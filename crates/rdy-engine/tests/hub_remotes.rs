#![cfg(unix)]
// Unix only: it reads and sets file modes. FW-XP1 Phase 4 ports it.
//! hsa6: remote configuration with no network or credential access (D-B8N21, p-38V30, p-PCXT7).
//! A remote mutation locks the user config first, then its destination's sibling lock, rereads
//! every settings level under the locks and saves only the destination. A row's level is its
//! effective source; removal and mode changes touch only the selected level's entry.

use rdy_engine::config_levels::{Level, effective};
use rdy_engine::hub_config::{HubConfig, RemoteMode};
use rdy_engine::hub_remotes::{RemoteLevel, RemoteReply, RemoteRow, RemoteService};
use rdy_engine::hub_types::Origin;
use rdy_engine::{
    E_ALREADY_EXISTS, E_BAD_INPUT, E_CONFIG, E_IO, E_LIMIT, E_NO_PROJECT, E_NOT_FOUND,
    E_REMOTE_EXISTS, Rdy, RdyError,
};
use rdy_store::fsutil::{PrivateWriteStage, with_private_write_probe_for_tests};
use serde_yaml_ng::{Mapping, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};

const EXISTS_MESSAGE: &str = "remote origin already exists at this config level";
const EXISTS_FIX: &str = "change that remote, or choose another name";
const MISSING_MESSAGE: &str = "remote origin does not exist at this config level";
const MISSING_FIX: &str = "list the remotes and use the level that defines it";
const SHARE_MESSAGE: &str = "a share remote already exists";
const SHARE_FIX: &str = "change its mode before adding another share remote";
const USER_MESSAGE: &str = "the user config path is not available";
const USER_FIX: &str = "set XDG_CONFIG_HOME or HOME before changing remotes";
const SAVE: &str = "the config file could not be saved durably";

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// A directory that group and others cannot write, whatever the umask.
fn private_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

fn mkdir(path: &Path) {
    fs::DirBuilder::new().mode(0o700).create(path).unwrap();
}

/// A project (`.rdy.yaml` and an entry at `.rdy/store`) and a user config home whose `rdy`
/// directory exists; the user config itself is `<home>/rdy/config.yaml`.
struct Fx {
    project: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Fx {
    fn new() -> Fx {
        let project = private_dir();
        mkdir(&project.path().join(".rdy"));
        mkdir(&project.path().join(".rdy/store"));
        fs::write(project.path().join(".rdy.yaml"), "pack: rdy-standard\n").unwrap();
        Fx::with_project(project)
    }

    fn with_project(project: tempfile::TempDir) -> Fx {
        let home = private_dir();
        mkdir(&home.path().join("rdy"));
        Fx { project, home }
    }

    fn project(&self) -> &Path {
        self.project.path()
    }

    fn project_file(&self) -> PathBuf {
        self.project().join(".rdy.yaml")
    }

    fn local_file(&self) -> PathBuf {
        self.project().join(".rdy/local.yaml")
    }

    fn user_file(&self) -> PathBuf {
        self.home.path().join("rdy/config.yaml")
    }

    fn credential_file(&self) -> PathBuf {
        self.home.path().join("rdy/credentials.yaml")
    }

    fn svc(&self) -> RemoteService {
        RemoteService::new(self.project(), &self.user_file())
    }

    fn files(&self) -> [PathBuf; 3] {
        [self.project_file(), self.local_file(), self.user_file()]
    }

    fn load(&self) -> HubConfig {
        HubConfig::load(Some(self.project()), Some(&self.user_file())).unwrap()
    }
}

/// Bytes and mode of a file, `None` when it is missing.
fn stamp(path: &Path) -> Option<(Vec<u8>, u32)> {
    fs::read(path).ok().map(|b| (b, mode(path)))
}

fn yaml(path: &Path) -> Mapping {
    serde_yaml_ng::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

/// `path`'s settings without the remotes section.
fn without_remotes(path: &Path) -> Mapping {
    let mut m = yaml(path);
    m.shift_remove("remotes");
    m
}

fn rows(svc: &RemoteService) -> Vec<(String, String, RemoteMode, RemoteLevel)> {
    svc.list()
        .unwrap()
        .remotes
        .into_iter()
        .map(|r| (r.name, r.url, r.mode, r.level))
        .collect()
}

fn row(
    name: &str,
    url: &str,
    mode: RemoteMode,
    level: RemoteLevel,
) -> (String, String, RemoteMode, RemoteLevel) {
    (name.to_string(), url.to_string(), mode, level)
}

fn err_of<T: std::fmt::Debug>(r: Result<T, RdyError>) -> RdyError {
    r.expect_err("the call must be refused")
}

fn assert_err(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix)
    );
}

/// Every file under `dir`, by path, with its bytes.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let m = fs::symlink_metadata(&p).unwrap();
            if m.is_dir() {
                todo.push(p);
            } else {
                out.insert(p.clone(), fs::read(&p).unwrap());
            }
        }
    }
    out
}

#[test]
fn hsa6_all_remote_operations_are_offline() {
    let fx = Fx::new();
    // an install with an unusable CA file and no saved credential: a credential sibling no one can
    // read, and a directory where its lock would be, so any credential access fails
    fs::write(
        fx.user_file(),
        "hub:\n  installs:\n    \"https://unreachable.invalid\":\n      ca_file: /nonexistent/hsa6-ca.pem\n",
    )
    .unwrap();
    let cred = fx.credential_file();
    fs::write(&cred, "installs: {}\n").unwrap();
    chmod(&cred, 0o000);
    let cred_lock = fx.home.path().join("rdy/credentials.yaml.lock");
    mkdir(&cred_lock);
    let svc = fx.svc();

    let added = svc
        .add(
            "origin",
            "https://unreachable.invalid/team/",
            RemoteMode::Share,
            RemoteLevel::Project,
        )
        .unwrap();
    assert_eq!(
        added.remote,
        RemoteRow {
            name: "origin".into(),
            url: "https://unreachable.invalid/team".into(),
            mode: RemoteMode::Share,
            level: RemoteLevel::Project,
            install: Origin::parse("https://unreachable.invalid").unwrap(),
        }
    );
    assert!(!added.removed);
    // the reply's wire shape: no token, root, transport or server-derived member
    let json = serde_json::to_value(&added).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "remote": {
                "name": "origin",
                "url": "https://unreachable.invalid/team",
                "mode": "share",
                "level": "project",
                "install": "https://unreachable.invalid",
            },
            "removed": false,
            "warnings": [],
        })
    );
    svc.add(
        "mirror",
        "https://127.0.0.1:1/m",
        RemoteMode::Publish,
        RemoteLevel::Local,
    )
    .unwrap();
    svc.add(
        "me",
        "https://[::1]:9/x",
        RemoteMode::Publish,
        RemoteLevel::User,
    )
    .unwrap();
    let changed = svc
        .set_mode("origin", RemoteMode::Publish, RemoteLevel::Project)
        .unwrap();
    assert_eq!(changed.remote.mode, RemoteMode::Publish);
    svc.set_mode("mirror", RemoteMode::Share, RemoteLevel::Local)
        .unwrap();
    let list = svc.list().unwrap();
    assert!(list.warnings.is_empty(), "{:?}", list.warnings);
    assert_eq!(
        rows(&svc),
        [
            row(
                "me",
                "https://[::1]:9/x",
                RemoteMode::Publish,
                RemoteLevel::User
            ),
            row(
                "mirror",
                "https://127.0.0.1:1/m",
                RemoteMode::Share,
                RemoteLevel::Local
            ),
            row(
                "origin",
                "https://unreachable.invalid/team",
                RemoteMode::Publish,
                RemoteLevel::Project
            ),
        ]
    );
    let list_json = serde_json::to_value(&list).unwrap();
    let keys: Vec<&String> = list_json.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["remotes", "warnings"]);
    let gone = svc.remove("me", RemoteLevel::User).unwrap();
    assert!(gone.removed);
    assert_eq!(gone.remote.level, RemoteLevel::User);

    // nothing reached the credential side
    assert_eq!(mode(&cred), 0o000, "the credential file is untouched");
    assert!(fs::symlink_metadata(&cred_lock).unwrap().is_dir());
    chmod(&cred, 0o600);
    assert_eq!(fs::read(&cred).unwrap(), b"installs: {}\n");
    // adding a remote confirms nothing for sign-in: no pinned origin, the CA setting stays
    let c = fx.load();
    let install = c
        .install(&Origin::parse("https://unreachable.invalid").unwrap())
        .unwrap();
    assert_eq!(
        install.ca_file,
        Some(PathBuf::from("/nonexistent/hsa6-ca.pem"))
    );
    assert!(install.pinned_origins.is_empty());
    for f in fx.files() {
        if let Ok(text) = fs::read_to_string(&f) {
            assert!(!text.contains("pinned_origins"), "{}: {text}", f.display());
        }
    }
}

#[test]
fn hsa6_remote_levels_and_share_uniqueness_are_explicit() {
    assert_eq!(RemoteLevel::default(), RemoteLevel::Project);
    let fx = Fx::new();
    let svc = fx.svc();
    let p = "https://hub.example.com/p";
    let l = "https://hub.example.com/l";
    let u = "https://hub.example.com/u";

    // add at the default level, then override it locally: the row's level is its source
    svc.add("origin", p, RemoteMode::Publish, RemoteLevel::default())
        .unwrap();
    let local = svc
        .add("origin", l, RemoteMode::Share, RemoteLevel::Local)
        .unwrap();
    assert_eq!(local.remote.level, RemoteLevel::Local);
    assert_eq!(
        rows(&svc),
        [row("origin", l, RemoteMode::Share, RemoteLevel::Local)]
    );
    // the same name at the same level is refused; the effective share blocks a second one
    let before = fx.files().map(|f| stamp(&f));
    let e = err_of(svc.add("origin", u, RemoteMode::Publish, RemoteLevel::Local));
    assert_err(&e, E_ALREADY_EXISTS, EXISTS_MESSAGE, EXISTS_FIX);
    let e = err_of(svc.add("mirror", u, RemoteMode::Share, RemoteLevel::User));
    assert_err(&e, E_REMOTE_EXISTS, SHARE_MESSAGE, SHARE_FIX);
    // a selected level without the entry is refused, even though another level has it
    let e = err_of(svc.remove("origin", RemoteLevel::User));
    assert_err(&e, E_NOT_FOUND, MISSING_MESSAGE, MISSING_FIX);
    let e = err_of(svc.set_mode("origin", RemoteMode::Share, RemoteLevel::User));
    assert_err(&e, E_NOT_FOUND, MISSING_MESSAGE, MISSING_FIX);
    assert_eq!(
        fx.files().map(|f| stamp(&f)),
        before,
        "a refusal saves nothing"
    );

    // a mode change touches only the selected level's entry
    svc.set_mode("origin", RemoteMode::Publish, RemoteLevel::Local)
        .unwrap();
    svc.add("mirror", u, RemoteMode::Share, RemoteLevel::User)
        .unwrap();
    assert_eq!(
        rows(&svc),
        [
            row("mirror", u, RemoteMode::Share, RemoteLevel::User),
            row("origin", l, RemoteMode::Publish, RemoteLevel::Local),
        ]
    );
    // the project entry is shadowed, but it is checked as the effective config would be after
    // the change: a project share would not be effective, so it is allowed
    let shadowed = svc
        .set_mode("origin", RemoteMode::Share, RemoteLevel::Project)
        .unwrap();
    assert_eq!(
        (shadowed.remote.level, shadowed.remote.mode),
        (RemoteLevel::Local, RemoteMode::Publish),
        "the reply is the effective row"
    );
    // removal would uncover the project's share beside the user's: refused, nothing saved
    let before = fx.files().map(|f| stamp(&f));
    let e = err_of(svc.remove("origin", RemoteLevel::Local));
    assert_err(&e, E_REMOTE_EXISTS, SHARE_MESSAGE, SHARE_FIX);
    assert_eq!(fx.files().map(|f| stamp(&f)), before);
    svc.set_mode("mirror", RemoteMode::Publish, RemoteLevel::User)
        .unwrap();
    let gone = svc.remove("origin", RemoteLevel::Local).unwrap();
    assert!(gone.removed);
    assert_eq!(
        gone.remote,
        RemoteRow {
            name: "origin".into(),
            url: l.into(),
            mode: RemoteMode::Publish,
            level: RemoteLevel::Local,
            install: Origin::parse("https://hub.example.com").unwrap(),
        },
        "the removed row is the selected level's"
    );
    // the less specific row is effective again
    assert_eq!(
        rows(&svc),
        [
            row("mirror", u, RemoteMode::Publish, RemoteLevel::User),
            row("origin", p, RemoteMode::Share, RemoteLevel::Project),
        ]
    );
    assert!(
        !fs::read_to_string(fx.local_file())
            .unwrap()
            .contains("remotes"),
        "an emptied remotes section is removed"
    );

    // input checks come first
    let e = err_of(svc.add("-bad", u, RemoteMode::Publish, RemoteLevel::User));
    assert_eq!(e.code, E_BAD_INPUT);
    let e = err_of(svc.add(
        "ok",
        "http://hub.example.com/x",
        RemoteMode::Publish,
        RemoteLevel::User,
    ));
    assert_eq!(e.code, E_BAD_INPUT);
    let e = err_of(svc.remove(&"a".repeat(65), RemoteLevel::User));
    assert_eq!(e.code, E_BAD_INPUT);

    // at most 64 effective remotes: an override keeps the count, a new name passes it
    let fx = Fx::new();
    let many: String = (0..64)
        .map(|i| format!("  r{i}: {{url: \"https://hub.example.com/{i}\", mode: publish}}\n"))
        .collect();
    fs::write(fx.user_file(), format!("remotes:\n{many}")).unwrap();
    let svc = fx.svc();
    svc.add("r7", p, RemoteMode::Publish, RemoteLevel::Project)
        .unwrap();
    let e = err_of(svc.add("r64", p, RemoteMode::Publish, RemoteLevel::Project));
    assert_eq!(e.code, E_LIMIT);

    // no project, or no user config path
    let none = private_dir();
    let e = err_of(RemoteService::new(none.path(), &fx.user_file()).list());
    assert_eq!(e.code, E_NO_PROJECT);
    let e = err_of(RemoteService::new(none.path(), &fx.user_file()).add(
        "x",
        u,
        RemoteMode::Publish,
        RemoteLevel::User,
    ));
    assert_eq!(e.code, E_NO_PROJECT);
    let e = err_of(RemoteService::new(fx.project(), Path::new("")).add(
        "x",
        u,
        RemoteMode::Publish,
        RemoteLevel::User,
    ));
    assert_err(&e, E_CONFIG, USER_MESSAGE, USER_FIX);
}

/// Two remote edits at different levels, each adding a share remote, run at once: the user
/// config lock orders them, so the second rereads the first's save and is refused.
#[test]
fn hsa6_concurrent_config_edits_are_rechecked_under_lock() {
    let pairs = [
        (RemoteLevel::User, RemoteLevel::Local),
        (RemoteLevel::Local, RemoteLevel::Project),
        (RemoteLevel::User, RemoteLevel::Project),
        (RemoteLevel::Local, RemoteLevel::User),
    ];
    for round in 0..16 {
        let (la, lb) = pairs[round % pairs.len()];
        let fx = Fx::new();
        let barrier = Arc::new(Barrier::new(2));
        let run = |name: &'static str, level: RemoteLevel| {
            let svc = fx.svc();
            let barrier = Arc::clone(&barrier);
            move || {
                barrier.wait();
                svc.add(name, "https://hub.example.com/s", RemoteMode::Share, level)
            }
        };
        let (a, b) = std::thread::scope(|s| {
            let a = s.spawn(run("a", la));
            let b = s.spawn(run("b", lb));
            (a.join().unwrap(), b.join().unwrap())
        });
        let wins = usize::from(a.is_ok()) + usize::from(b.is_ok());
        assert_eq!(wins, 1, "round {round}: {a:?} / {b:?}");
        let lost = a.err().or(b.err()).unwrap();
        assert_eq!(lost.code, E_REMOTE_EXISTS, "round {round}");
        let shares = fx
            .load()
            .remotes()
            .iter()
            .filter(|r| r.mode == RemoteMode::Share)
            .count();
        assert_eq!(shares, 1, "round {round}");
    }

    // one edit is held inside its save; the other cannot finish until it is released, then
    // rereads the saved share and is refused
    let fx = Fx::new();
    let (held_tx, held_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (b_tx, b_rx) = mpsc::channel::<Result<RemoteReply, RdyError>>();
    std::thread::scope(|s| {
        let svc_a = fx.svc();
        let a = s.spawn(move || {
            let mut go = Some(go_rx);
            with_private_write_probe_for_tests(
                move |stage, _| {
                    if stage == PrivateWriteStage::Created
                        && let Some(rx) = go.take()
                    {
                        held_tx.send(()).unwrap();
                        rx.recv().unwrap();
                    }
                    Ok(())
                },
                || {
                    svc_a.add(
                        "a",
                        "https://hub.example.com/a",
                        RemoteMode::Share,
                        RemoteLevel::User,
                    )
                },
            )
        });
        held_rx.recv().unwrap();
        let svc_b = fx.svc();
        s.spawn(move || {
            let r = svc_b.add(
                "b",
                "https://hub.example.com/b",
                RemoteMode::Share,
                RemoteLevel::Local,
            );
            b_tx.send(r).unwrap();
        });
        assert!(
            matches!(b_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "the second edit waits for the user config lock"
        );
        go_tx.send(()).unwrap();
        a.join().unwrap().unwrap();
        let b = b_rx.recv().unwrap();
        assert_eq!(err_of(b).code, E_REMOTE_EXISTS);
    });
    assert!(!fx.local_file().exists(), "the refused edit saved nothing");
}

#[test]
fn hsa6_only_the_destination_settings_file_is_saved() {
    let fx = Fx::new();
    let signers = fx.home.path().join("allowed_signers");
    fs::write(&signers, "nick@example.com ssh-ed25519 AAAA\n").unwrap();
    fs::write(
        fx.user_file(),
        format!(
            "# user settings\nactor: human:nick\nsigning:\n  sign: auto\n  allowed_signers: {}\nhub:\n  installs:\n    \"https://hub.example.com\":\n      ca_file: /etc/hsa6/hub.pem\n      pinned_origins: [\"https://cdn.example.com\"]\n",
            signers.display()
        ),
    )
    .unwrap();
    fs::write(
        fx.local_file(),
        "signing:\n  sign: never\nhub:\n  installs:\n    \"https://hub.example.com\":\n      pinned_origins: [\"https://cdn2.example.com\"]\n",
    )
    .unwrap();
    fs::write(fx.project_file(), "pack: rdy-standard\nname: hsa6\n").unwrap();
    chmod(&fx.user_file(), 0o644);
    chmod(&fx.local_file(), 0o640);
    chmod(&fx.project_file(), 0o664);
    fs::write(fx.credential_file(), "installs: {}\n").unwrap();
    chmod(&fx.credential_file(), 0o600);
    let fixed = || {
        (
            stamp(&fx.credential_file()),
            stamp(&signers),
            tree(&fx.project().join(".rdy/store")),
        )
    };
    let untouched = fixed();
    let settings_before = effective(fx.project(), Some(&fx.user_file())).unwrap();
    let semantic = fx.files().map(|f| without_remotes(&f));
    let svc = fx.svc();
    let url = "https://hub.example.com/team";

    // each call saves exactly its destination, keeps its mode and its other values
    type Call<'a> = Box<dyn Fn() -> Result<RemoteReply, RdyError> + 'a>;
    let steps: Vec<(usize, Call)> = vec![
        (
            0,
            Box::new(|| svc.add("origin", url, RemoteMode::Share, RemoteLevel::Project)),
        ),
        (
            1,
            Box::new(|| svc.add("origin", url, RemoteMode::Publish, RemoteLevel::Local)),
        ),
        (
            1,
            Box::new(|| svc.set_mode("origin", RemoteMode::Share, RemoteLevel::Local)),
        ),
        (
            2,
            Box::new(|| svc.add("mirror", url, RemoteMode::Publish, RemoteLevel::User)),
        ),
        (
            0,
            Box::new(|| svc.set_mode("origin", RemoteMode::Publish, RemoteLevel::Project)),
        ),
        (2, Box::new(|| svc.remove("mirror", RemoteLevel::User))),
        (1, Box::new(|| svc.remove("origin", RemoteLevel::Local))),
    ];
    let modes = [0o664, 0o640, 0o644];
    for (i, (dest, call)) in steps.iter().enumerate() {
        let before = fx.files().map(|f| stamp(&f));
        call().unwrap_or_else(|e| panic!("step {i}: {e}"));
        let after = fx.files().map(|f| stamp(&f));
        for f in 0..3 {
            if f == *dest {
                assert_ne!(after[f], before[f], "step {i}: the destination is saved");
                assert_eq!(
                    after[f].as_ref().unwrap().1,
                    modes[f],
                    "step {i}: mode kept"
                );
            } else {
                assert_eq!(after[f], before[f], "step {i}: file {f} untouched");
            }
        }
        assert_eq!(
            fx.files().map(|f| without_remotes(&f)),
            semantic,
            "step {i}"
        );
        assert_eq!(fixed(), untouched, "step {i}");
    }
    // a refusal saves nothing at all
    let before = fx.files().map(|f| stamp(&f));
    err_of(svc.add("origin", url, RemoteMode::Share, RemoteLevel::Project));
    assert_eq!(fx.files().map(|f| stamp(&f)), before);

    // the CA and origin lists, the actor and the signing settings are as they were
    let c = fx.load();
    let install = c
        .install(&Origin::parse("https://hub.example.com").unwrap())
        .unwrap();
    assert_eq!(install.ca_file, Some(PathBuf::from("/etc/hsa6/hub.pem")));
    assert_eq!(
        install.pinned_origins,
        [Origin::parse("https://cdn2.example.com").unwrap()],
        "the local list still replaces the user list"
    );
    let user_hub = &yaml(&fx.user_file())["hub"]["installs"]["https://hub.example.com"];
    assert_eq!(
        user_hub["pinned_origins"][0],
        Value::from("https://cdn.example.com")
    );
    let settings_after = effective(fx.project(), Some(&fx.user_file())).unwrap();
    for key in [
        "actor",
        "signing.sign",
        "signing.allowed_signers",
        "pack",
        "name",
    ] {
        let pick = |e: &rdy_engine::config_levels::Effective| {
            e.origins
                .iter()
                .find(|o| o.key == key)
                .map(|o| (o.value.clone(), o.level))
        };
        assert_eq!(pick(&settings_after), pick(&settings_before), "{key}");
    }
    assert_eq!(
        settings_after
            .origins
            .iter()
            .find(|o| o.key == "signing.sign")
            .map(|o| o.level),
        Some(Level::Local)
    );
}

#[test]
fn hsa6_remote_changes_never_touch_the_project_log() {
    let project = private_dir();
    drop(Rdy::init(project.path(), "rdy-standard").unwrap());
    let fx = Fx::with_project(project);
    let store = fx.project().join(".rdy/store");
    let log = tree(&store);
    assert!(!log.is_empty());
    let entries = |dir: &Path| {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with("local.yaml"))
            .collect();
        v.sort();
        v
    };
    let dot = entries(&fx.project().join(".rdy"));
    let svc = fx.svc();
    let url = "https://hub.example.com/team";
    let check = |what: &str| {
        assert_eq!(
            tree(&store),
            log,
            "{what}: the store's log and refs are unchanged"
        );
        assert_eq!(entries(&fx.project().join(".rdy")), dot, "{what}");
    };
    for level in [RemoteLevel::Project, RemoteLevel::Local, RemoteLevel::User] {
        svc.add("origin", url, RemoteMode::Publish, level).unwrap();
        check("add");
        err_of(svc.add("origin", url, RemoteMode::Publish, level));
        check("duplicate add");
        svc.set_mode("origin", RemoteMode::Share, level).unwrap();
        check("set-mode");
        err_of(svc.add("second", url, RemoteMode::Share, level));
        check("second share");
        svc.remove("origin", level).unwrap();
        check("remove");
        err_of(svc.remove("origin", level));
        check("missing remove");
    }
    // the project still opens, with no change in its history
    let r = Rdy::open(fx.project()).unwrap();
    drop(r);
}

#[test]
fn hsa6_failed_remote_save_never_claims_success() {
    let fx = Fx::new();
    let svc = fx.svc();
    let url = "https://hub.example.com/team";
    let fail = |at: PrivateWriteStage| {
        move |stage: PrivateWriteStage, _: &Path| {
            if stage == at {
                Err(io::Error::other("hsa6 injected"))
            } else {
                Ok(())
            }
        }
    };

    // before the rename: the old bytes stay and no temp file is left
    let before = stamp(&fx.project_file());
    let e = with_private_write_probe_for_tests(fail(PrivateWriteStage::Written), || {
        svc.add("origin", url, RemoteMode::Share, RemoteLevel::Project)
    });
    let e = err_of(e);
    assert_eq!((e.code, e.message.as_str()), (E_IO, SAVE));
    assert_eq!(stamp(&fx.project_file()), before);
    assert!(rows(&svc).is_empty());
    let temps: Vec<_> = fs::read_dir(fx.project())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(temps.is_empty(), "{temps:?}");

    // after the rename: an error, never a success; the complete new file is read next time
    for (stage, level) in [
        (PrivateWriteStage::Renamed, RemoteLevel::Project),
        (PrivateWriteStage::DirSynced, RemoteLevel::Local),
    ] {
        let name = if level == RemoteLevel::Project {
            "origin"
        } else {
            "mirror"
        };
        let r = with_private_write_probe_for_tests(fail(stage), || {
            svc.add(name, url, RemoteMode::Publish, level)
        });
        let e = err_of(r);
        assert_eq!((e.code, e.message.as_str()), (E_IO, SAVE), "{stage:?}");
        assert!(
            rows(&svc).iter().any(|r| r.0 == name && r.3 == level),
            "{stage:?}: the saved file is reread"
        );
        let e = err_of(svc.add(name, url, RemoteMode::Publish, level));
        assert_eq!(e.code, E_ALREADY_EXISTS, "{stage:?}");
    }
    // the locks were released: a later edit at every level goes through
    for level in [RemoteLevel::Project, RemoteLevel::Local, RemoteLevel::User] {
        svc.add("later", url, RemoteMode::Publish, level).unwrap();
    }
}
