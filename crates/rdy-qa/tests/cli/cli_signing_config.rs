//! sig2: the four signing settings are valid in the user config and in .rdy/local.yaml only
//! (D-QVH4N). They merge, warn and show their origin like every other setting. Reading them
//! contacts no agent and no network.
use rdy_engine::config_levels::{Effective, LOCAL_FILE, Level, PROJECT_FILE, effective};
use rdy_engine::signing_config::{SignMode, SigningConfig};
use rdy_engine::signing_crypto::key_fingerprint;
use rdy_engine::*;
use rdy_qa::fixtures::{human_env, rdy_bin};
use serde_yaml_ng::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Two distinct ssh-ed25519 public keys (test keys; no private half is in the repo).
const KEY_A: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA11UHVOCACl0jfu9YJgNXZrmz5aFYaKlAqyiZWHiOOw rdy-sig3-test";
const KEY_A_OTHER_COMMENT: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA11UHVOCACl0jfu9YJgNXZrmz5aFYaKlAqyiZWHiOOw another comment";
const KEY_B: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti user@example.com";

fn proj() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    let p = d.path().to_path_buf();
    (d, p)
}

fn user(text: &str) -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("config.yaml");
    std::fs::write(&f, text).unwrap();
    (d, f)
}

fn append(p: &Path, rel: &str, text: &str) {
    let f = p.join(rel);
    if let Some(dir) = f.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut t = std::fs::read_to_string(&f).unwrap_or_default();
    t.push_str(text);
    std::fs::write(f, t).unwrap();
}

fn fp(selector: &str) -> String {
    key_fingerprint(selector).expect("a test key")
}

fn read(p: &Path, uf: Option<&Path>) -> (Effective, SigningConfig) {
    let e = effective(p, uf).expect("the settings read");
    let c = SigningConfig::from_effective(p, &e).expect("the signing settings read");
    (e, c)
}

fn origin<'a>(e: &'a Effective, key: &str) -> Option<&'a rdy_engine::config_levels::Origin> {
    e.origins.iter().find(|o| o.key == key)
}

fn warn_messages(e: &Effective) -> Vec<String> {
    e.warnings.iter().map(|w| w.message.clone()).collect()
}

#[test]
fn sig2_signing_uses_local_then_user_then_defaults() {
    // no setting anywhere: nothing set, sign is auto by default
    let (_d, p) = proj();
    let (e, c) = read(&p, None);
    assert_eq!(c, SigningConfig::default());
    assert_eq!(c.sign, SignMode::Auto);
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    let sign = origin(&e, "signing.sign").expect("sign has a default");
    assert_eq!(sign.value, Value::from("auto"));
    assert_eq!(sign.level, Level::Default);
    for k in [
        "signing.key",
        "signing.agent_key",
        "signing.allowed_signers",
    ] {
        assert!(origin(&e, k).is_none(), "{k} has no default");
    }
    // the four rows come in this order, after the keys before them
    let keys: Vec<&str> = e
        .origins
        .iter()
        .map(|o| o.key.as_str())
        .filter(|k| k.starts_with("signing."))
        .collect();
    assert_eq!(keys, ["signing.sign"]);

    // the user config alone sets all four
    let abs = std::env::temp_dir().join("sig2-allowed-user");
    let (_u, uf) = user(&format!(
        "signing:\n  key: {KEY_A}\n  agent_key: {KEY_B}\n  sign: never\n  allowed_signers: {}\n",
        abs.display()
    ));
    let (e, c) = read(&p, Some(&uf));
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    assert_eq!(c.key.as_deref(), Some(KEY_A));
    assert_eq!(c.agent_key.as_deref(), Some(KEY_B));
    assert_eq!(c.sign, SignMode::Never);
    assert_eq!(c.allowed_signers.as_deref(), Some(abs.as_path()));
    let order: Vec<&str> = e
        .origins
        .iter()
        .map(|o| o.key.as_str())
        .filter(|k| k.starts_with("signing."))
        .collect();
    assert_eq!(
        order,
        [
            "signing.key",
            "signing.agent_key",
            "signing.sign",
            "signing.allowed_signers"
        ]
    );
    for k in order {
        let o = origin(&e, k).unwrap();
        assert_eq!(o.level, Level::User, "{k}");
        assert_eq!(o.file.as_deref(), Some(uf.as_path()), "{k}");
    }

    // the local file wins key by key; a key it leaves alone comes from the user config
    append(
        &p,
        LOCAL_FILE,
        &format!(
            "signing:\n  key: {}\n  sign: always\n  allowed_signers: signers/allowed\n",
            fp(KEY_A)
        ),
    );
    let (e, c) = read(&p, Some(&uf));
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    assert_eq!(c.key, Some(fp(KEY_A)), "the raw selector is kept");
    assert_eq!(c.agent_key.as_deref(), Some(KEY_B));
    assert_eq!(c.sign, SignMode::Always);
    assert_eq!(
        c.allowed_signers.as_deref(),
        Some(p.join("signers/allowed").as_path())
    );
    let level_of = |k: &str| origin(&e, k).unwrap().level;
    assert_eq!(level_of("signing.key"), Level::Local);
    assert_eq!(level_of("signing.agent_key"), Level::User);
    assert_eq!(level_of("signing.sign"), Level::Local);
    assert_eq!(level_of("signing.allowed_signers"), Level::Local);
    assert_eq!(
        origin(&e, "signing.key").unwrap().file.as_deref(),
        Some(p.join(LOCAL_FILE).as_path())
    );

    // a project-level setting never applies
    let (_d2, p2) = proj();
    append(&p2, PROJECT_FILE, &format!("signing:\n  key: {KEY_A}\n"));
    let (e, c) = read(&p2, None);
    assert_eq!(c.key, None);
    assert!(origin(&e, "signing.key").is_none());
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);

    // a null selector and a null path mean unset
    let (_d3, p3) = proj();
    append(
        &p3,
        LOCAL_FILE,
        "signing:\n  key: null\n  agent_key: ~\n  allowed_signers: null\n",
    );
    let (e, c) = read(&p3, Some(&uf));
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    assert_eq!(c.key, None);
    assert_eq!(c.agent_key, None);
    assert_eq!(c.allowed_signers, None);
    assert_eq!(c.sign, SignMode::Never, "sign still comes from the user");
    assert_eq!(origin(&e, "signing.key").unwrap().value, Value::Null);
    assert_eq!(origin(&e, "signing.key").unwrap().level, Level::Local);

    // an empty selector is kept for the signing path's own error
    let (_d4, p4) = proj();
    append(
        &p4,
        LOCAL_FILE,
        "signing:\n  key: ''\n  allowed_signers: ''\n",
    );
    let (_, c) = read(&p4, None);
    assert_eq!(c.key.as_deref(), Some(""));
    assert_eq!(c.allowed_signers.as_deref(), Some(Path::new("")));

    // a wrong YAML type keeps the existing E_CONFIG text
    let (_d5, p5) = proj();
    append(&p5, LOCAL_FILE, "signing:\n  key: 5\n");
    let err = effective(&p5, None).expect_err("a number is not a selector");
    assert_eq!(err.code, E_CONFIG);
    assert!(
        err.message.starts_with("signing.key in .rdy/local.yaml: "),
        "{}",
        err.message
    );
    assert_eq!(err.fix, "fix signing.key in .rdy/local.yaml");
}

#[test]
fn sig2_project_signing_settings_warn_and_never_apply() {
    let home = ".rdy/local.yaml or the user config";
    // a scalar section
    let (_d, p) = proj();
    append(&p, PROJECT_FILE, "signing: 5\n");
    let (e, c) = read(&p, None);
    assert_eq!(c, SigningConfig::default());
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert_eq!(e.warnings[0].code, "W_CONFIG_UNKNOWN");
    assert_eq!(
        e.warnings[0].message,
        format!("signing in .rdy.yaml belongs in {home}; rdy ignores it here")
    );
    assert_eq!(e.warnings[0].fix, format!("move signing to {home}"));

    // each leaf
    let (_d, p) = proj();
    append(
        &p,
        PROJECT_FILE,
        &format!(
            "signing:\n  key: {KEY_A}\n  agent_key: {KEY_B}\n  sign: always\n  allowed_signers: /x/y\n"
        ),
    );
    let (e, c) = read(&p, None);
    assert_eq!(c, SigningConfig::default(), "no project value applies");
    assert_eq!(e.warnings.len(), 4, "{:?}", e.warnings);
    for (w, leaf) in e.warnings.iter().zip([
        "signing.key",
        "signing.agent_key",
        "signing.sign",
        "signing.allowed_signers",
    ]) {
        assert_eq!(w.code, "W_CONFIG_UNKNOWN");
        assert_eq!(
            w.message,
            format!("{leaf} in .rdy.yaml belongs in {home}; rdy ignores it here")
        );
        assert_eq!(w.fix, format!("move {leaf} to {home}"));
    }
    // the project file never gives an origin
    assert_eq!(
        origin(&e, "signing.sign").unwrap().level,
        Level::Default,
        "{:?}",
        e.origins
    );
}

#[test]
fn sig2_old_keys_and_prompt_are_not_compatibility_paths() {
    let (_d, p) = proj();
    append(
        &p,
        LOCAL_FILE,
        "signing:\n  require: true\n  sign: prompt\n",
    );
    let (e, c) = read(&p, None);
    assert_eq!(c.sign, SignMode::Auto);
    assert_eq!(e.warnings.len(), 2, "{:?}", e.warnings);
    // signing.require is an unknown key like any other: no migration text
    let unknown = &e.warnings[0];
    assert_eq!(unknown.code, "W_CONFIG_UNKNOWN");
    assert!(
        unknown.message.starts_with(
            "unknown key signing.require in .rdy/local.yaml (local level); rdy ignores it"
        ),
        "{}",
        unknown.message
    );
    assert!(
        !unknown.message.contains("migrat") && !unknown.fix.contains("migrat"),
        "{unknown:?}"
    );
    // prompt is an invalid choice
    let choice = &e.warnings[1];
    assert_eq!(choice.code, "W_CONFIG_UNKNOWN");
    assert_eq!(
        choice.message,
        "signing.sign must be never, auto or always; using auto"
    );
    assert_eq!(choice.fix, "set signing.sign to never, auto or always");
    let sign = origin(&e, "signing.sign").unwrap();
    assert_eq!(sign.value, Value::from("auto"));
    assert_eq!(sign.level, Level::Local);

    // a bad word at the local level does not let the user level answer
    let (_u, uf) = user("signing:\n  sign: never\n");
    let (_, c) = read(&p, Some(&uf));
    assert_eq!(c.sign, SignMode::Auto);

    // a top-level `prompt` is unknown too
    let (_d, p) = proj();
    append(&p, LOCAL_FILE, "prompt: true\n");
    let (e, c) = read(&p, None);
    assert_eq!(c.sign, SignMode::Auto);
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert!(
        e.warnings[0]
            .message
            .starts_with("unknown key prompt in .rdy/local.yaml"),
        "{}",
        e.warnings[0].message
    );
}

#[test]
fn sig2_equal_public_lines_and_fingerprints_ignore_the_agent_key() {
    let equal = "signing.agent_key equals signing.key; rdy ignores signing.agent_key".to_string();
    let fix = "set signing.agent_key to a different key";

    // two public lines of one key that differ only in the comment
    let (_d, p) = proj();
    let (_u, uf) = user(&format!(
        "signing:\n  key: {KEY_A}\n  agent_key: {KEY_A_OTHER_COMMENT}\n"
    ));
    let (e, c) = read(&p, Some(&uf));
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert_eq!(e.warnings[0].code, "W_CONFIG_UNKNOWN");
    assert_eq!(e.warnings[0].message, equal);
    assert_eq!(e.warnings[0].fix, fix);
    assert_eq!(c.key.as_deref(), Some(KEY_A));
    assert_eq!(c.agent_key, None);
    // the origin stays for show-origin
    let o = origin(&e, "signing.agent_key").expect("the origin is kept");
    assert_eq!(o.value, Value::from(KEY_A_OTHER_COMMENT));
    assert_eq!(o.level, Level::User);

    // a fingerprint and a public line
    let (_d, p) = proj();
    let (_u, uf) = user(&format!(
        "signing:\n  key: {}\n  agent_key: {KEY_A}\n",
        fp(KEY_A)
    ));
    let (e, c) = read(&p, Some(&uf));
    assert_eq!(warn_messages(&e), std::slice::from_ref(&equal));
    assert_eq!(c.agent_key, None);

    // a local override that equals the key gives one warning, and the user's own agent key
    // does not answer instead
    let (_d, p) = proj();
    let (_u, uf) = user(&format!("signing:\n  key: {KEY_A}\n  agent_key: {KEY_B}\n"));
    let (e, c) = read(&p, Some(&uf));
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    assert_eq!(c.agent_key.as_deref(), Some(KEY_B));
    append(
        &p,
        LOCAL_FILE,
        &format!("signing:\n  agent_key: {}\n", fp(KEY_A)),
    );
    let (e, c) = read(&p, Some(&uf));
    assert_eq!(warn_messages(&e), std::slice::from_ref(&equal));
    assert_eq!(c.key.as_deref(), Some(KEY_A));
    assert_eq!(c.agent_key, None, "no fallback to the lower level");
    let o = origin(&e, "signing.agent_key").unwrap();
    assert_eq!(o.level, Level::Local);

    // a local key that makes the user's agent key equal
    let (_d, p) = proj();
    let (_u, uf) = user(&format!("signing:\n  agent_key: {KEY_B}\n"));
    append(&p, LOCAL_FILE, &format!("signing:\n  key: {KEY_B}\n"));
    let (e, c) = read(&p, Some(&uf));
    assert_eq!(warn_messages(&e), std::slice::from_ref(&equal));
    assert_eq!(c.agent_key, None);

    // different keys, and one malformed selector: no comparison, nothing is dropped
    let (_d, p) = proj();
    append(
        &p,
        LOCAL_FILE,
        &format!("signing:\n  key: not a key\n  agent_key: {KEY_A}\n"),
    );
    let (e, c) = read(&p, None);
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    assert_eq!(c.key.as_deref(), Some("not a key"));
    assert_eq!(c.agent_key.as_deref(), Some(KEY_A));
    // two equal malformed selectors are not compared by fingerprint
    let (_d, p) = proj();
    append(
        &p,
        LOCAL_FILE,
        "signing:\n  key: not a key\n  agent_key: not a key\n",
    );
    let (e, c) = read(&p, None);
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    assert_eq!(c.agent_key.as_deref(), Some("not a key"));
}

#[test]
fn sig2_allowed_signers_paths_follow_the_setting_origin() {
    let abs_local = std::env::temp_dir().join("sig2-local-abs");
    let abs_user = std::env::temp_dir().join("sig2-user-abs");

    // a local relative path resolves from the project root, with no expansion
    for text in ["signers/allowed", "~/allowed", "$HOME/allowed", "*.signers"] {
        let (_d, p) = proj();
        append(
            &p,
            LOCAL_FILE,
            &format!("signing:\n  allowed_signers: '{text}'\n"),
        );
        let (e, c) = read(&p, None);
        assert!(e.warnings.is_empty(), "{text}: {:?}", e.warnings);
        assert_eq!(c.allowed_signers, Some(p.join(text)), "{text}");
        // the origin shows the value as written
        assert_eq!(
            origin(&e, "signing.allowed_signers").unwrap().value,
            Value::from(text)
        );
    }

    // a local absolute path is kept
    let (_d, p) = proj();
    append(
        &p,
        LOCAL_FILE,
        &format!("signing:\n  allowed_signers: {}\n", abs_local.display()),
    );
    let (_, c) = read(&p, None);
    assert_eq!(c.allowed_signers.as_deref(), Some(abs_local.as_path()));

    // a user absolute path is kept, and the local file still wins
    let (_u, uf) = user(&format!(
        "signing:\n  allowed_signers: {}\n",
        abs_user.display()
    ));
    let (_d, p) = proj();
    let (e, c) = read(&p, Some(&uf));
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    assert_eq!(c.allowed_signers.as_deref(), Some(abs_user.as_path()));
    append(&p, LOCAL_FILE, "signing:\n  allowed_signers: rel\n");
    let (_, c) = read(&p, Some(&uf));
    assert_eq!(c.allowed_signers, Some(p.join("rel")));

    // a user relative path is refused with a warning and removed before precedence
    for text in ["allowed", "~/allowed", "$HOME/allowed"] {
        let (_u, uf) = user(&format!("signing:\n  allowed_signers: '{text}'\n"));
        let (_d, p) = proj();
        let (e, c) = read(&p, Some(&uf));
        assert_eq!(e.warnings.len(), 1, "{text}: {:?}", e.warnings);
        assert_eq!(e.warnings[0].code, "W_CONFIG_UNKNOWN");
        assert_eq!(
            e.warnings[0].message,
            format!(
                "signing.allowed_signers in {} must be absolute; rdy ignores it",
                uf.display()
            )
        );
        assert_eq!(
            e.warnings[0].fix,
            "set signing.allowed_signers to an absolute path"
        );
        assert_eq!(c.allowed_signers, None, "{text}");
        assert!(origin(&e, "signing.allowed_signers").is_none(), "{text}");
    }
    // a local value is not touched by the user rule
    let (_u, uf) = user("signing:\n  allowed_signers: allowed\n");
    let (_d, p) = proj();
    append(&p, LOCAL_FILE, "signing:\n  allowed_signers: mine\n");
    let (e, c) = read(&p, Some(&uf));
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    assert_eq!(c.allowed_signers, Some(p.join("mine")));
    assert_eq!(
        origin(&e, "signing.allowed_signers").unwrap().level,
        Level::Local
    );
}

/// A command that runs with networking disabled by the operating system, not by convention:
/// a macOS sandbox profile that denies `network*`, or a fresh Linux network namespace with
/// no interface up. Proxy variables do not disable networking: a direct socket, a client
/// that ignores proxies, or an inherited `NO_PROXY` still connects.
fn offline(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[cfg(target_os = "macos")]
    {
        let mut c = Command::new("/usr/bin/sandbox-exec");
        c.args(["-p", "(version 1)(allow default)(deny network*)"])
            .arg(program);
        c
    }
    #[cfg(target_os = "linux")]
    {
        let mut c = Command::new("unshare");
        c.args(["--map-root-user", "--net", "--"]).arg(program);
        c
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = program;
        panic!("no network guard for this platform")
    }
}

#[test]
fn sig2_offline_guard_blocks_a_direct_local_connection() {
    // a listener on the loopback address accepts a plain connection ...
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let probe = format!("exec 3<>/dev/tcp/127.0.0.1/{port}");
    let open = Command::new("bash").args(["-c", &probe]).output().unwrap();
    assert!(open.status.success(), "control connection failed");
    // ... and the guarded command cannot reach it, with or without proxy variables
    let blocked = offline("bash")
        .args(["-c", &probe])
        .env("NO_PROXY", "*")
        .output()
        .unwrap();
    assert!(
        !blocked.status.success(),
        "the guard did not block a direct connection: {}",
        String::from_utf8_lossy(&blocked.stderr)
    );
}

#[test]
fn sig2_config_does_not_need_an_agent_or_network() {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    append(
        d.path(),
        LOCAL_FILE,
        &format!(
            "signing:\n  key: {KEY_A}\n  agent_key: {KEY_A_OTHER_COMMENT}\n  sign: always\n  allowed_signers: signers\n"
        ),
    );
    let (cfg, env) = human_env("human:qa");
    // the operating system denies every socket to the child (see `offline`)
    let out = offline(rdy_bin())
        .args(["config", "--show-origin"])
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("SSH_AUTH_SOCK")
        .env("RDY_NO_DAEMON", "1")
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(d.path())
        .output()
        .expect("run rdy");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{text}\n{err}");
    // the CLI shows the project's canonical path (macOS temp dirs are symlinks)
    let root = d.path().canonicalize().unwrap();
    let local = format!("  # {}", root.join(LOCAL_FILE).display());
    let line = |k: &str| {
        text.lines()
            .find(|l| l.starts_with(&format!("{k} = ")))
            .unwrap_or_else(|| panic!("{k}: {text}"))
            .to_string()
    };
    assert!(line("signing.key").ends_with(&local), "{text}");
    assert!(line("signing.agent_key").ends_with(&local), "{text}");
    assert!(line("signing.sign").starts_with("signing.sign = always"));
    assert!(line("signing.allowed_signers").ends_with(&local), "{text}");
    assert!(
        text.lines().any(|l| l
            == "warning W_CONFIG_UNKNOWN: signing.agent_key equals signing.key; rdy ignores signing.agent_key; set signing.agent_key to a different key"),
        "{text}"
    );
    drop(cfg);
}
