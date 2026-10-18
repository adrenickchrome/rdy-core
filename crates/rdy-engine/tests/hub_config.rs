//! hsa3: remote and install settings, and the separate credential codec. Pure parsing and merging:
//! no lock, HTTP call or credential read. Remotes merge by name (user, then project, then local);
//! install fields merge user then local; the hub subtree never comes from `.rdy.yaml`. The
//! credential file is the user config's sibling, and only its own codec reads it.
use rdy_engine::config_levels::{Level, effective, effective_lenient};
use rdy_engine::hub_config::*;
use rdy_engine::hub_types::{Origin, RemoteUrl, Secret};
use rdy_engine::{E_BAD_INPUT, E_CONFIG, E_LIMIT, E_NOT_FOUND, E_REMOTE_EXISTS, E_REMOTE_REQUIRED};
use rdy_schema::Issue;
use serde_yaml_ng::{Mapping, Value};
use std::path::{Path, PathBuf};

const O: &str = "https://hub.example.com";
const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const REFRESH: &str = "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC";
const SENTINEL: &str = "SENTINEL-hsa3-do-not-print";

const SYNTAX_FIX: &str = "fix the config syntax, duplicate keys or aliases";
const LIMIT_FIX: &str = "reduce the config file to the stated limit";
const CRED_MESSAGE: &str = "the credential file is not a valid bounded mapping";
const CRED_FIX: &str = "remove the invalid credential file, then sign in again";

/// A project (`.rdy.yaml` holding `pack: p` plus `project`, and `.rdy/local.yaml` when given) and
/// a user config (when given) in its own directory.
struct Fx {
    dir: tempfile::TempDir,
    user_dir: tempfile::TempDir,
}

impl Fx {
    fn new(project: &str, local: Option<&str>, user: Option<&str>) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".rdy")).unwrap();
        std::fs::write(dir.path().join(".rdy.yaml"), format!("pack: p\n{project}")).unwrap();
        if let Some(l) = local {
            std::fs::write(dir.path().join(".rdy/local.yaml"), l).unwrap();
        }
        let user_dir = tempfile::tempdir().unwrap();
        if let Some(u) = user {
            std::fs::write(user_dir.path().join("config.yaml"), u).unwrap();
        }
        Fx { dir, user_dir }
    }

    fn project(&self) -> &Path {
        self.dir.path()
    }

    fn user_file(&self) -> PathBuf {
        self.user_dir.path().join("config.yaml")
    }

    fn load(&self) -> Result<HubConfig, rdy_engine::RdyError> {
        HubConfig::load(Some(self.project()), Some(&self.user_file()))
    }
}

fn texts(w: &[Issue]) -> Vec<(String, String)> {
    w.iter()
        .map(|i| (i.message.clone(), i.fix.clone()))
        .collect()
}

fn pair(m: &str, f: &str) -> (String, String) {
    (m.to_string(), f.to_string())
}

fn remote(url: &str, mode: &str) -> String {
    format!("{{url: \"{url}\", mode: {mode}}}")
}

#[test]
fn hsa3_remotes_merge_by_name_and_select_deterministically() {
    let user = format!(
        "remotes:\n  origin: {}\n  mirror: {}\n",
        remote("https://hub.example.com/u", "publish"),
        remote("https://Mirror.Example.com:443/m/", "publish"),
    );
    let project = format!(
        "remotes:\n  origin: {}\n  team: {}\n",
        remote("https://hub.example.com/p", "share"),
        remote("https://hub.example.com/t", "publish"),
    );
    // the local entry replaces the project's whole entry; an invalid local entry is ignored
    // before the merge, so the project's `team` stays
    let local = format!(
        "remotes:\n  origin: {}\n  team: {{url: \"http://hub.example.com/t\", mode: share}}\n",
        remote("https://hub.example.com/l", "share"),
    );
    let fx = Fx::new(&project, Some(&local), Some(&user));
    let c = fx.load().unwrap();
    let names: Vec<&str> = c.remotes().iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["mirror", "origin", "team"], "sorted by name");
    let origin = c.remote("origin").unwrap();
    assert_eq!(origin.url.as_str(), "https://hub.example.com/l");
    assert_eq!(origin.mode, RemoteMode::Share);
    assert_eq!(origin.level, Level::Local);
    assert_eq!(origin.file, Some(fx.project().join(".rdy/local.yaml")));
    let mirror = c.remote("mirror").unwrap();
    assert_eq!(
        mirror.url.as_str(),
        "https://mirror.example.com/m",
        "canonical"
    );
    assert_eq!(mirror.level, Level::User);
    assert_eq!(c.remote("team").unwrap().level, Level::Project);
    assert_eq!(
        texts(&c.warnings),
        [pair(
            "invalid value for remotes.team.url",
            "set remotes.team.url to an HTTPS URL"
        )]
    );
    // the display rows: one per remote, sorted by name, with the level and file that won
    let rows: Vec<(&str, Level)> = c
        .origins
        .iter()
        .map(|o| (o.key.as_str(), o.level))
        .collect();
    assert_eq!(
        rows,
        [
            ("remotes.mirror", Level::User),
            ("remotes.origin", Level::Local),
            ("remotes.team", Level::Project)
        ]
    );
    assert_eq!(
        c.origins[1].value_line(),
        "{url: https://hub.example.com/l, mode: share}"
    );
    // the general settings show the same rows and warnings
    let e = effective(fx.project(), Some(&fx.user_file())).unwrap();
    let keys: Vec<&str> = e
        .origins
        .iter()
        .map(|o| o.key.as_str())
        .filter(|k| k.starts_with("remotes"))
        .collect();
    assert_eq!(keys, ["remotes.mirror", "remotes.origin", "remotes.team"]);
    assert!(
        e.warnings
            .iter()
            .any(|w| w.message == "invalid value for remotes.team.url")
    );
    // selection: an explicit name, else the share remote
    assert_eq!(c.select_remote(Some("mirror")).unwrap().name, "mirror");
    assert_eq!(c.select_remote(None).unwrap().name, "origin");
    let e = c.select_remote(Some("nope")).unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND);
    assert_eq!(e.message, "remote nope does not exist");
    assert_eq!(e.fix, "list the remotes, then pass a listed name");
    assert_eq!(
        c.select_remote(Some("bad/name")).unwrap_err().code,
        E_BAD_INPUT
    );
    // else the sole remote
    let fx = Fx::new(
        "",
        None,
        Some(&format!("remotes:\n  only: {}\n", remote(O, "publish"))),
    );
    assert_eq!(fx.load().unwrap().select_remote(None).unwrap().name, "only");
    // two remotes and no share remote: E_REMOTE_REQUIRED; no remote at all, too
    let fx = Fx::new(
        "",
        None,
        Some(&format!(
            "remotes:\n  a: {}\n  b: {}\n",
            remote(O, "publish"),
            remote(O, "publish")
        )),
    );
    let e = fx.load().unwrap().select_remote(None).unwrap_err();
    assert_eq!(e.code, E_REMOTE_REQUIRED);
    assert_eq!(e.message, "no remote was selected");
    assert_eq!(
        e.fix,
        "pass --remote, or configure one share remote or one remote"
    );
    let fx = Fx::new("", None, None);
    let e = fx.load().unwrap().select_remote(None).unwrap_err();
    assert_eq!(e.code, E_REMOTE_REQUIRED);
    // with no project, only the user settings load
    let fx = Fx::new(
        &format!("remotes:\n  proj: {}\n", remote(O, "share")),
        None,
        Some(&format!("remotes:\n  mine: {}\n", remote(O, "publish"))),
    );
    let c = HubConfig::load(None, Some(&fx.user_file())).unwrap();
    let names: Vec<&str> = c.remotes().iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["mine"]);
    assert_eq!(c.select_remote(None).unwrap().name, "mine");
    // the same files load the same way
    let again = HubConfig::load(None, Some(&fx.user_file())).unwrap();
    assert_eq!(again.remotes(), c.remotes());
    // names and fields are checked; an unknown dynamic name is never printed
    let fx = Fx::new(
        "",
        None,
        Some(&format!(
            "remotes:\n  \"-bad\": {}\n  ok: {{url: \"{O}\", mode: both, extra: 1}}\n  half: {{url: \"{O}\"}}\n",
            remote(O, "share")
        )),
    );
    let c = fx.load().unwrap();
    assert!(c.remotes().is_empty(), "{:?}", c.remotes());
    assert_eq!(
        texts(&c.warnings),
        [
            pair("invalid value for remotes", "set remotes to a remote name"),
            pair(
                "invalid value for remotes.half.mode",
                "set remotes.half.mode to share or publish"
            ),
            pair(
                "unknown hub setting remotes.ok.extra",
                "remove remotes.ok.extra"
            ),
            pair(
                "invalid value for remotes.ok.mode",
                "set remotes.ok.mode to share or publish"
            ),
        ]
    );
    // at most 64 effective remotes
    let mut many = String::from("remotes:\n");
    for i in 0..65 {
        many.push_str(&format!("  r{i}: {}\n", remote(O, "publish")));
    }
    let fx = Fx::new("", None, Some(&many));
    let e = fx.load().unwrap_err();
    assert_eq!(e.code, E_LIMIT);
    assert_eq!(e.message, "config input exceeds 64 remotes");
    assert_eq!(e.fix, LIMIT_FIX);
}

#[test]
fn hsa3_two_share_remotes_are_refused() {
    let fx = Fx::new(
        &format!("remotes:\n  b: {}\n", remote(O, "share")),
        None,
        Some(&format!("remotes:\n  a: {}\n", remote(O, "share"))),
    );
    let e = fx.load().unwrap_err();
    assert_eq!(e.code, E_REMOTE_EXISTS);
    assert_eq!(e.message, "a share remote already exists");
    assert_eq!(e.fix, "change its mode before adding another share remote");
    // the same name at a more specific level replaces the entry: one share remote
    let fx = Fx::new(
        &format!("remotes:\n  a: {}\n", remote(O, "publish")),
        None,
        Some(&format!("remotes:\n  a: {}\n", remote(O, "share"))),
    );
    let c = fx.load().unwrap();
    assert_eq!(c.remotes().len(), 1);
    assert_eq!(c.remotes()[0].mode, RemoteMode::Publish);
    assert!(c.select_remote(None).is_ok(), "the sole remote");
    // the failure carries the warnings as issues
    let fx = Fx::new(
        &format!("remotes:\n  b: {}\n", remote(O, "share")),
        Some("remotes:\n  c: 5\n"),
        Some(&format!("remotes:\n  a: {}\n", remote(O, "share"))),
    );
    let e = fx.load().unwrap_err();
    assert_eq!(e.code, E_REMOTE_EXISTS);
    assert_eq!(
        texts(&e.issues),
        [pair(
            "invalid value for remotes.c",
            "set remotes.c to a map within the stated limits"
        )]
    );
    // the typed edit refuses a second share remote in one file
    let mut m =
        parse_settings(format!("remotes:\n  a: {}\n", remote(O, "share")).as_bytes()).unwrap();
    let before = m.clone();
    let entry = RemoteEntry {
        url: RemoteUrl::parse(O).unwrap(),
        mode: RemoteMode::Share,
    };
    let e = set_remote(&mut m, "b", Some(entry.clone())).unwrap_err();
    assert_eq!(e.code, E_REMOTE_EXISTS);
    assert_eq!(m, before, "a refused edit changes nothing");
    // the same remote can stay share
    set_remote(&mut m, "a", Some(entry)).unwrap();
}

#[test]
fn hsa3_ca_and_origin_list_levels_are_enforced() {
    let user = format!(
        "hub:\n  installs:\n    \"{O}\":\n      ca_file: /etc/rdy/ca.pem\n      pinned_origins: [\"https://b.example.com\", \"https://a.example.com\"]\n"
    );
    let project = format!(
        "hub:\n  installs:\n    \"{O}\":\n      ca_file: /etc/x.pem\n      pinned_origins: [\"https://p.example.com\"]\n"
    );
    let local = format!(
        "hub:\n  installs:\n    \"{O}\":\n      ca_file: /etc/y.pem\n      pinned_origins: [\"https://c.example.com\"]\n"
    );
    let fx = Fx::new(&project, Some(&local), Some(&user));
    let c = fx.load().unwrap();
    let o = Origin::parse(O).unwrap();
    let i = c.install(&o).unwrap();
    assert_eq!(
        i.ca_file,
        Some(PathBuf::from("/etc/rdy/ca.pem")),
        "user only"
    );
    assert_eq!(
        i.pinned_origins,
        [Origin::parse("https://c.example.com").unwrap()],
        "the local list replaces the user list"
    );
    let ca = format!("hub.installs.{O}.ca_file");
    let pins = format!("hub.installs.{O}.pinned_origins");
    // sorted by file (user, project, local), then by path
    assert_eq!(
        texts(&c.warnings),
        [
            pair(
                &format!("{ca} is not allowed in .rdy.yaml"),
                &format!("set {ca} in user config and remove it from .rdy.yaml")
            ),
            pair(
                &format!("{pins} is not allowed in .rdy.yaml"),
                &format!(
                    "set {pins} in user config or .rdy/local.yaml and remove it from .rdy.yaml"
                )
            ),
            pair(
                &format!("{ca} is not allowed in .rdy/local.yaml"),
                &format!("set {ca} in user config and remove it from .rdy/local.yaml")
            ),
        ]
    );
    let rows: Vec<(&str, Level)> = c
        .origins
        .iter()
        .map(|o| (o.key.as_str(), o.level))
        .collect();
    assert_eq!(
        rows,
        [(ca.as_str(), Level::User), (pins.as_str(), Level::Local)]
    );
    // the fence trusts exactly the install and its pinned origins
    let fence = c.fence(&o).unwrap();
    assert!(fence.check("https://c.example.com/x").is_ok());
    assert!(fence.check("https://a.example.com/x").is_err());
    // without the local list, the user list applies, sorted by canonical bytes
    let fx = Fx::new("", None, Some(&user));
    let c = fx.load().unwrap();
    assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    let pinned: Vec<&str> = c
        .install(&o)
        .unwrap()
        .pinned_origins
        .iter()
        .map(Origin::as_str)
        .collect();
    assert_eq!(pinned, ["https://a.example.com", "https://b.example.com"]);
    assert_eq!(
        c.origins[1].value_line(),
        "[https://a.example.com, https://b.example.com]"
    );
    // a hub subtree in .rdy.yaml never contributes, and the general settings agree
    let fx = Fx::new(&project, None, None);
    let c = fx.load().unwrap();
    assert!(c.installs().is_empty());
    let e = effective(fx.project(), Some(&fx.user_file())).unwrap();
    assert!(e.origins.iter().all(|o| !o.key.starts_with("hub")));
    assert!(e.merged.get("hub").is_none());
    assert_eq!(texts(&e.warnings), texts(&c.warnings));
    // invalid values and unknown keys warn and are ignored
    let bad = format!(
        "hub:\n  installs:\n    \"{O}\":\n      ca_file: etc/ca.pem\n      pinned_origins: [\"https://a.example.com\", \"https://a.example.com\"]\n      ca_fil: x\n    \"https://Hub.Example.com/\":\n      ca_file: /a\n    \"https://ok.example.com\":\n      pinned_origins: [\"http://a.example.com\"]\n  instals: 1\n"
    );
    let fx = Fx::new("", None, Some(&bad));
    let c = fx.load().unwrap();
    assert!(c.installs().is_empty(), "{:?}", c.installs());
    assert_eq!(
        texts(&c.warnings),
        [
            pair(
                "invalid value for hub.installs",
                "set hub.installs to a map within the stated limits"
            ),
            pair(
                &format!("unknown hub setting hub.installs.{O}.ca_fil"),
                &format!("did you mean {ca}? rename hub.installs.{O}.ca_fil")
            ),
            pair(
                &format!("invalid value for {ca}"),
                &format!("set {ca} to an absolute CA file path")
            ),
            pair(
                &format!("invalid value for {pins}"),
                &format!("set {pins} to a list of unique HTTPS origins")
            ),
            pair(
                "invalid value for hub.installs.https://ok.example.com.pinned_origins",
                "set hub.installs.https://ok.example.com.pinned_origins to a list of unique HTTPS origins"
            ),
            pair(
                "unknown hub setting hub.instals",
                "did you mean hub.installs? rename hub.instals"
            ),
        ]
    );
    // at most 16 origins in a list, and at most 32 installs
    let list: Vec<String> = (0..17)
        .map(|i| format!("\"https://p{i}.example.com\""))
        .collect();
    let fx = Fx::new(
        "",
        None,
        Some(&format!(
            "hub:\n  installs:\n    \"{O}\":\n      pinned_origins: [{}]\n",
            list.join(", ")
        )),
    );
    let e = fx.load().unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_LIMIT, "config input exceeds 16 origins")
    );
    let mut many = String::from("hub:\n  installs:\n");
    for i in 0..33 {
        many.push_str(&format!(
            "    \"https://h{i}.example.com\":\n      ca_file: /ca.pem\n"
        ));
    }
    let fx = Fx::new("", None, Some(&many));
    let e = fx.load().unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_LIMIT, "config input exceeds 32 installs")
    );
}

#[test]
fn hsa3_credentials_have_one_user_sibling_path() {
    let uf = Path::new("/home/someone/.config/rdy/config.yaml");
    assert_eq!(
        credential_file(uf),
        PathBuf::from("/home/someone/.config/rdy/credentials.yaml")
    );
    // no setting at any level moves it or adds one: the reserved spellings warn under one
    // fixed path that names no secret leaf and no install, and their values are never printed
    let attempt = format!(
        "hub:\n  credential_file: {SENTINEL}\n  installs:\n    \"{O}\":\n      access_token: {SENTINEL}\n      device_code: {SENTINEL}\n    {SENTINEL}:\n      refresh_token: {SENTINEL}\n    credential: {SENTINEL}\n"
    );
    let a = Fx::new("", Some(&attempt), Some(&attempt));
    let b = Fx::new("", None, None);
    for fx in [&a, &b] {
        // a directory where the credential file would be: loading never opens it
        std::fs::create_dir_all(credential_file(&fx.user_file())).unwrap();
        let c = fx.load().unwrap();
        assert!(c.installs().is_empty());
        assert!(c.origins.is_empty(), "{:?}", c.origins);
        for w in &c.warnings {
            assert!(
                !w.message.contains(SENTINEL) && !w.fix.contains(SENTINEL),
                "{w:?}"
            );
        }
        let e = effective_lenient(fx.project(), Some(&fx.user_file()));
        let shown = format!("{:?}{:?}{:?}", e.origins, e.merged, e.warnings);
        assert!(!shown.contains(SENTINEL), "{shown}");
        assert!(
            !shown.contains("access_token") && !shown.contains("device_code"),
            "{shown}"
        );
    }
    let c = a.load().unwrap();
    let hub: Vec<&str> = c.warnings.iter().map(|w| w.message.as_str()).collect();
    assert_eq!(
        hub,
        [
            "unknown hub setting hub.credential_file",
            "invalid value for hub.installs",
            "unknown hub setting hub.installs.credential",
            "unknown hub setting hub.installs.credential",
            "unknown hub setting hub.installs.credential",
            "unknown hub setting hub.credential_file",
            "invalid value for hub.installs",
            "unknown hub setting hub.installs.credential",
            "unknown hub setting hub.installs.credential",
            "unknown hub setting hub.installs.credential",
        ]
    );
    // the path is the user file's sibling whichever project is selected
    assert_eq!(
        credential_file(&a.user_file()),
        a.user_dir.path().join("credentials.yaml")
    );
}

fn pending() -> Credential {
    Credential::Pending(PendingSignIn {
        machine_name: "laptop".into(),
        device_code: Secret::new(format!("dc-{SENTINEL}")),
        user_code: "BCDFGHJK".into(),
        verification_uri: format!("{O}/app/device"),
        expires_at_ms: 1_000,
        next_poll_at_ms: 500,
        interval_seconds: 5,
    })
}

fn ready() -> Credential {
    Credential::Ready(ReadyCredential {
        access_token: Secret::new(ACCESS),
        refresh_token: Secret::new(REFRESH),
        expires_at_ms: 2_000,
        account: "acct_1".into(),
        handle: "ada".into(),
    })
}

fn ready_text(extra: &str) -> String {
    format!(
        "installs:\n  \"{O}\":\n    state: ready\n    access_token: {ACCESS}\n    refresh_token: {REFRESH}\n    expires_at_ms: 2000\n    account: acct_1\n    handle: ada\n{extra}"
    )
}

fn cred_err(bytes: &[u8]) -> rdy_engine::RdyError {
    parse_credentials(bytes).expect_err("refused")
}

fn assert_bad(bytes: &[u8]) {
    let e = cred_err(bytes);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (E_CONFIG, CRED_MESSAGE, CRED_FIX),
        "{}",
        String::from_utf8_lossy(bytes)
    );
}

#[test]
fn hsa3_credential_codec_is_closed_and_bounded() {
    // all three states round-trip, sorted by origin
    let mut f = CredentialFile::default();
    let a = Origin::parse("https://a.example.com").unwrap();
    let b = Origin::parse(O).unwrap();
    let z = Origin::parse("https://z.example.com").unwrap();
    f.set(z.clone(), Some(Credential::ReauthRequired));
    f.set(b.clone(), Some(pending()));
    f.set(a.clone(), Some(ready()));
    let bytes = encode_credentials(&f).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    let at = |s: &str| text.find(s).unwrap();
    assert!(at("https://a.") < at("https://hub.") && at("https://hub.") < at("https://z."));
    assert!(text.contains("state: reauth_required"), "{text}");
    let back = parse_credentials(&bytes).unwrap();
    assert_eq!(back, f);
    assert_eq!(encode_credentials(&back).unwrap(), bytes, "deterministic");
    assert_eq!(back.get(&a), Some(&ready()));
    assert_eq!(back.get(&z).map(Credential::state), Some("reauth_required"));
    // signing out removes the install; an absent install is signed out
    f.set(a.clone(), None);
    assert_eq!(f.get(&a), None);
    assert_eq!(f.len(), 2);
    // empty and missing files are an empty map
    for empty in ["", "installs: {}\n", "# nothing\n"] {
        assert!(parse_credentials(empty.as_bytes()).unwrap().is_empty());
    }
    assert_eq!(
        encode_credentials(&CredentialFile::default()).unwrap(),
        b"installs: {}\n"
    );
    // Debug never shows a secret
    let shown = format!("{f:?} {:?} {:?}", ready(), pending());
    assert!(
        !shown.contains(ACCESS) && !shown.contains(REFRESH) && !shown.contains(SENTINEL),
        "{shown}"
    );
    // an unknown key is named only by its nearest known field
    let e = cred_err(ready_text("    acess_token: x\n").as_bytes());
    assert_eq!(e.code, E_CONFIG);
    assert_eq!(e.message, "unknown credential field");
    assert_eq!(e.fix, "did you mean access_token? use that field");
    let e = cred_err(format!("instals: {{}}\n{SENTINEL}: 1\n").as_bytes());
    assert_eq!(e.fix, "did you mean installs? use that field");
    let e = cred_err(ready_text(&format!("    {SENTINEL}: x\n")).as_bytes());
    assert_eq!(
        (e.message.as_str(), e.fix.as_str()),
        ("unknown credential field", "remove the unknown field")
    );
    // explicit null, wrong state and field combinations, bad forms and duplicates fail closed
    for bad in [
        "installs:\n".to_string(),
        "installs: []\n".to_string(),
        "- installs\n".to_string(),
        ready_text("    device_code: x\n"),
        ready_text("").replace("state: ready", "state: done"),
        ready_text("").replace("state: ready", "state: pending"),
        ready_text("").replace("    handle: ada\n", ""),
        ready_text("").replace("handle: ada", "handle:"),
        ready_text("").replace("handle: ada", "handle: [ada]"),
        ready_text("").replace("expires_at_ms: 2000", "expires_at_ms: soon"),
        ready_text("").replace(ACCESS, &format!("{}A", &ACCESS[..52])),
        ready_text("").replace(REFRESH, "not-a-token"),
        ready_text("").replace("https://hub.example.com", "https://Hub.Example.com"),
        ready_text("").replace("https://hub.example.com", "http://hub.example.com"),
        format!("installs:\n  \"{O}\":\n    state: reauth_required\n    handle: ada\n"),
        format!(
            "installs:\n  \"{O}\":\n    state: reauth_required\n  \"{O}\":\n    state: reauth_required\n"
        ),
        format!(
            "installs:\n  \"{O}\": &x\n    state: reauth_required\n  \"https://a.example.com\": *x\n"
        ),
        "installs: {".to_string(),
    ] {
        assert_bad(bad.as_bytes());
    }
    // a pending device code must be nonempty, the user code eight letters
    let p = encode_credentials(&{
        let mut f = CredentialFile::default();
        f.set(b.clone(), Some(pending()));
        f
    })
    .unwrap();
    let p = String::from_utf8(p).unwrap();
    assert!(parse_credentials(p.as_bytes()).is_ok(), "{p}");
    for (from, to) in [
        ("user_code: BCDFGHJK", "user_code: ABC"),
        ("interval_seconds: 5", "interval_seconds: 0"),
    ] {
        assert!(p.contains(from), "{p}");
        assert_bad(p.replace(from, to).as_bytes());
    }
    let mut empty_code = CredentialFile::default();
    let Credential::Pending(mut ps) = pending() else {
        unreachable!()
    };
    ps.device_code = Secret::new("");
    empty_code.set(b.clone(), Some(Credential::Pending(ps)));
    let e = encode_credentials(&empty_code).unwrap_err();
    assert_eq!((e.code, e.message.as_str()), (E_CONFIG, CRED_MESSAGE));
    // no error names a secret, a path or the parser's prose
    for bytes in [
        ready_text("").replace(ACCESS, SENTINEL),
        format!("installs:\n  \"{O}\": {{state: ready, access_token: {SENTINEL}"),
    ] {
        let e = cred_err(bytes.as_bytes());
        let shown = format!("{e} {e:?}");
        assert!(
            !shown.contains(SENTINEL) && !shown.contains("line"),
            "{shown}"
        );
    }
    // the bounds: 32 installs, 1 MiB, depth 32, 4096 entries and 16384-byte scalars
    let mut many = String::from("installs:\n");
    for i in 0..33 {
        many.push_str(&format!(
            "  \"https://h{i}.example.com\": {{state: reauth_required}}\n"
        ));
    }
    let e = cred_err(many.as_bytes());
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (E_LIMIT, "config input exceeds 32 installs", LIMIT_FIX)
    );
    let mut too_many = CredentialFile::default();
    for i in 0..33 {
        too_many.set(
            Origin::parse(&format!("https://h{i}.example.com")).unwrap(),
            Some(Credential::ReauthRequired),
        );
    }
    assert_eq!(encode_credentials(&too_many).unwrap_err().code, E_LIMIT);
    let big = format!("# {}\n", "x".repeat(1_048_576));
    assert_eq!(
        cred_err(big.as_bytes()).message,
        "config input exceeds 1048576 bytes"
    );
    let deep = format!("{}1{}", "[".repeat(33), "]".repeat(33));
    assert_eq!(
        cred_err(format!("installs: {deep}\n").as_bytes()).message,
        "config input exceeds 32 levels"
    );
    let wide: Vec<String> = (0..4097).map(|i| i.to_string()).collect();
    assert_eq!(
        cred_err(format!("installs: [{}]\n", wide.join(", ")).as_bytes()).message,
        "config input exceeds 4096 entries"
    );
    assert_eq!(
        cred_err(format!("installs: {}\n", "x".repeat(16385)).as_bytes()).message,
        "config input exceeds 16384 bytes"
    );
}

fn settings_err(bytes: &[u8]) -> rdy_engine::RdyError {
    parse_settings(bytes).expect_err("refused")
}

#[test]
fn hsa3_config_bounds_duplicates_and_aliases_fail_closed() {
    // the pure parser: syntax, duplicate keys, aliases and a non-string-keyed map
    for bad in [
        format!("actor: [{SENTINEL}\n"),
        format!("{SENTINEL}: 1\n{SENTINEL}: 2\n"),
        format!("remotes:\n  a: {{url: x}}\n  a: {{url: {SENTINEL}}}\n"),
        format!("base: &b {{x: {SENTINEL}}}\nother: *b\n"),
        "- a\n- b\n".to_string(),
        "1: a\n".to_string(),
    ] {
        let e = settings_err(bad.as_bytes());
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_CONFIG,
                "config is not a valid bounded mapping",
                SYNTAX_FIX
            ),
            "{bad}"
        );
        assert!(format!("{e:?}").find(SENTINEL).is_none());
    }
    assert!(parse_settings(b"").unwrap().is_empty());
    // a settings file: the same text, naming the file and nothing from it
    for bad in [
        format!("actor: [{SENTINEL}\n"),
        format!("hub:\n  {SENTINEL}: 1\n  {SENTINEL}: 2\n"),
        format!("x: &a {SENTINEL}\ny: *a\n"),
    ] {
        let fx = Fx::new("", None, Some(&bad));
        let uf = fx.user_file();
        for e in [
            effective(fx.project(), Some(&uf)).unwrap_err(),
            fx.load().unwrap_err(),
        ] {
            assert_eq!(e.code, E_CONFIG);
            assert_eq!(
                e.message,
                format!("config is not a valid bounded mapping in {}", uf.display())
            );
            assert_eq!(e.fix, SYNTAX_FIX);
            assert!(!format!("{e:?}").contains(SENTINEL), "{e:?}");
        }
        // the project and local files too
        let fx = Fx::new("", Some(&bad), None);
        let e = effective(fx.project(), None).unwrap_err();
        assert_eq!(
            e.message,
            "config is not a valid bounded mapping in .rdy/local.yaml"
        );
    }
    // the bounds, read before the semantic parse
    let deep = format!("{}1{}", "[".repeat(33), "]".repeat(33));
    let wide: Vec<String> = (0..4097).map(|i| format!("k{i}: 1")).collect();
    for (text, message) in [
        (
            format!("# {}\n", "x".repeat(1_048_576)),
            "config input exceeds 1048576 bytes",
        ),
        (format!("later: {deep}\n"), "config input exceeds 32 levels"),
        (
            format!("later: {{{}}}\n", wide.join(", ")),
            "config input exceeds 4096 entries",
        ),
        (
            format!("later: {}\n", "x".repeat(16385)),
            "config input exceeds 16384 bytes",
        ),
    ] {
        let e = settings_err(text.as_bytes());
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (E_LIMIT, message, LIMIT_FIX)
        );
        let fx = Fx::new("", None, Some(&text));
        let e = effective(fx.project(), Some(&fx.user_file())).unwrap_err();
        assert_eq!((e.code, e.message.as_str()), (E_LIMIT, message));
    }
    // just inside the bounds reads
    let deep = format!("{}1{}", "[".repeat(31), "]".repeat(31));
    assert!(parse_settings(format!("later: {deep}\n").as_bytes()).is_ok());
    assert!(parse_settings(format!("later: {}\n", "x".repeat(16384)).as_bytes()).is_ok());
    // encoding is bounded too
    let mut m = Mapping::new();
    m.insert(Value::from("later"), Value::from("x".repeat(16385)));
    let e = encode_settings(&m).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_LIMIT, "config input exceeds 16384 bytes")
    );
    let mut v = Value::from(1);
    for _ in 0..32 {
        v = Value::Sequence(vec![v]);
    }
    let mut m = Mapping::new();
    m.insert(Value::from("later"), v);
    assert_eq!(
        encode_settings(&m).unwrap_err().message,
        "config input exceeds 32 levels"
    );
}

#[test]
fn hsa3_settings_updates_preserve_unrelated_values() {
    let text = format!(
        "# a comment\nactor: human:ada\nsigning:\n  key: ssh-ed25519 AAAA\nserve:\n  idle_timeout: 5\nlater: [1, 2]\nremotes:\n  a: {}\nhub:\n  installs:\n    \"{O}\":\n      ca_file: /ca.pem\n      later: x\n",
        remote(O, "publish")
    );
    let original = parse_settings(text.as_bytes()).unwrap();
    let mut m = original.clone();
    let entry = |url: &str, mode| RemoteEntry {
        url: RemoteUrl::parse(url).unwrap(),
        mode,
    };
    set_remote(
        &mut m,
        "b",
        Some(entry("https://hub.example.com/b", RemoteMode::Share)),
    )
    .unwrap();
    let o = Origin::parse(O).unwrap();
    let p = Origin::parse("https://p.example.com").unwrap();
    let q = Origin::parse("https://a.example.com").unwrap();
    set_install_field(
        &mut m,
        &o,
        InstallField::PinnedOrigins(Some(vec![p.clone(), q.clone(), p.clone()])),
    )
    .unwrap();
    let back = parse_settings(&encode_settings(&m).unwrap()).unwrap();
    assert_eq!(back, m, "the encoding reads back as the same values");
    for key in ["actor", "signing", "serve", "later"] {
        assert_eq!(back.get(key), original.get(key), "{key}");
    }
    let remotes = back.get("remotes").unwrap().as_mapping().unwrap();
    assert_eq!(remotes.get("a"), original.get("remotes").unwrap().get("a"));
    assert_eq!(
        remotes.get("b"),
        Some(
            &serde_yaml_ng::from_str::<Value>("{url: \"https://hub.example.com/b\", mode: share}")
                .unwrap()
        )
    );
    let install = back["hub"]["installs"][O].as_mapping().unwrap();
    assert_eq!(install.get("ca_file"), Some(&Value::from("/ca.pem")));
    assert_eq!(
        install.get("later"),
        Some(&Value::from("x")),
        "an unknown field stays"
    );
    assert_eq!(
        install.get("pinned_origins"),
        Some(
            &serde_yaml_ng::from_str::<Value>(
                "[\"https://a.example.com\", \"https://p.example.com\"]"
            )
            .unwrap()
        ),
        "sorted and unique"
    );
    // a field set to nothing goes, with a section it leaves empty; other installs stay
    set_install_field(&mut m, &o, InstallField::CaFile(None)).unwrap();
    set_install_field(&mut m, &o, InstallField::PinnedOrigins(None)).unwrap();
    assert_eq!(
        m["hub"]["installs"][O].as_mapping().unwrap().len(),
        1,
        "later stays"
    );
    set_install_field(
        &mut m,
        &p,
        InstallField::CaFile(Some(PathBuf::from("/p.pem"))),
    )
    .unwrap();
    assert_eq!(
        m["hub"]["installs"]["https://p.example.com"]["ca_file"],
        Value::from("/p.pem")
    );
    set_remote(&mut m, "a", None).unwrap();
    set_remote(&mut m, "b", None).unwrap();
    assert!(m.get("remotes").is_none(), "an empty remotes section goes");
    let mut bare = parse_settings(b"actor: human:ada\n").unwrap();
    set_install_field(
        &mut bare,
        &o,
        InstallField::CaFile(Some(PathBuf::from("/ca.pem"))),
    )
    .unwrap();
    set_install_field(&mut bare, &o, InstallField::CaFile(None)).unwrap();
    assert_eq!(
        bare,
        parse_settings(b"actor: human:ada\n").unwrap(),
        "an emptied hub goes"
    );
    // the typed edits check what they write
    let before = m.clone();
    assert_eq!(
        set_remote(&mut m, "bad/name", None).unwrap_err().code,
        E_BAD_INPUT
    );
    let e = set_install_field(
        &mut m,
        &o,
        InstallField::CaFile(Some(PathBuf::from("ca.pem"))),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    let seventeen: Vec<Origin> = (0..17)
        .map(|i| Origin::parse(&format!("https://p{i}.example.com")).unwrap())
        .collect();
    let e =
        set_install_field(&mut m, &o, InstallField::PinnedOrigins(Some(seventeen))).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_LIMIT, "config input exceeds 16 origins")
    );
    assert_eq!(m, before, "refused edits change nothing");
}
