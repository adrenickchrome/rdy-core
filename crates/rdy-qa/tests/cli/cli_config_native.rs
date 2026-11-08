//! wx26c: the config and instruction paths are native. A CA file is an absolute path of this
//! platform in single quotes, an unreadable credential file is made unreadable by the platform's
//! own access rules, and a name that is not Unicode comes from the platform's seam.
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::yaml_path;
use rdy_qa::instructions::World;
use rdy_qa::platform::{not_unicode, set_mode, symlink};
use std::ffi::OsStr;

const O: &str = "https://hub.example.com";
const SENTINEL: &str = "rdyh_a_SENTINELwx26cSENTINELwx26cSENTINELwx26c";

fn both(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn wx26c_config_and_instruction_paths_are_native() {
    // a directory name with a space and a quote must survive the YAML round trip
    let s = Served::new();
    let dir = s.dir.path().join("it's a dir");
    std::fs::create_dir_all(&dir).unwrap();
    let ca = dir.join("ca.pem");
    assert!(ca.is_absolute(), "{}", ca.display());
    let quoted = yaml_path(&ca);
    assert!(
        quoted.starts_with('\'') && quoted.ends_with('\''),
        "{quoted}"
    );
    assert!(quoted.contains("it''s a dir"), "{quoted}");
    let uf = s.user_config();
    std::fs::write(
        &uf,
        format!("actor: human:qa\nhub:\n  installs:\n    \"{O}\":\n      ca_file: {quoted}\n"),
    )
    .unwrap();
    let shown = both(&s.rdy(&["config", "--show-origin"]));
    assert!(
        shown.contains(&format!(
            "hub.installs.{O}.ca_file = {}  # {}",
            ca.display(),
            uf.display()
        )),
        "{shown}"
    );
    assert!(!shown.contains("W_CONFIG_UNKNOWN"), "{shown}");

    // setup refuses a native file link and leaves its target alone
    let w = World::new();
    let project = w.project();
    let target = dir.join("shared.md");
    std::fs::write(&target, "Shared rules.\n").unwrap();
    symlink(&target, &project.path().join("AGENTS.md")).unwrap();
    let out = w.rdy(project.path(), &["--json", "setup"]);
    assert!(!out.status.success(), "{}", both(&out));
    assert!(both(&out).contains("E_IO"), "{}", both(&out));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "Shared rules.\n");
    assert!(!project.path().join("CLAUDE.md").exists());
}

#[test]
fn wx26c_unreadable_credentials_never_leak() {
    let s = Served::new();
    let cred = s.user_config().with_file_name("credentials.yaml");
    std::fs::write(
        &cred,
        format!("installs:\n  \"{O}\":\n    state: ready\n    access_token: {SENTINEL}\n"),
    )
    .unwrap();
    set_mode(&cred, 0o000).unwrap();
    // the fixture is real: this account cannot open the file now
    let denied = std::fs::read(&cred);
    assert!(
        denied.is_err(),
        "the credential file must be unreadable; run the suite as a standard account"
    );
    for args in [
        &["config"][..],
        &["config", "--json"],
        &["config", "--show-origin"],
        &["status"],
        &["--json", "status"],
    ] {
        let text = both(&s.rdy(args));
        assert!(!text.contains("SENTINELwx26c"), "{args:?}: {text}");
        assert!(
            !text.to_lowercase().contains("permission denied")
                && !text.contains("credentials.yaml"),
            "{args:?}: the unreadable file is never opened or named: {text}"
        );
    }
    set_mode(&cred, 0o600).unwrap();
    assert!(std::fs::read(&cred).unwrap().len() > 10);
}

#[test]
fn wx26c_non_unicode_actor_and_run_stay_refused() {
    let s = Served::new();
    let actor = not_unicode("agent:");
    let run = not_unicode("r");
    assert!(actor.to_str().is_none() && run.to_str().is_none());
    for (a, r) in [
        (actor.as_os_str(), None),
        (OsStr::new("agent:x"), Some(run.as_os_str())),
    ] {
        for args in [
            &["--no-daemon", "--json", "status"][..],
            &["--json", "status"][..],
        ] {
            let (ok, v) = s.json_as_os(a, r, args);
            assert!(!ok, "{a:?} {r:?} {args:?}: {v}");
            assert_eq!(v["code"], "E_BAD_INPUT", "{a:?} {r:?} {args:?}: {v}");
        }
    }
    assert!(
        s.daemon_pid().is_none(),
        "a refused command starts no daemon"
    );
}
