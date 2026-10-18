//! hsa3: remote and install settings show like every other setting, but the credential file
//! beside the user config is never read for them, and no setting value under the hub subtree
//! reaches a warning or error: not a dotted key, not one at the wrong level, not a file that
//! does not parse.
use rdy_qa::daemon::Served;
use rdy_qa::mcp::McpClient;
use std::path::Path;

const O: &str = "https://hub.example.com";
const SENTINEL: &str = "rdyh_a_SENTINELhsa3SENTINELhsa3SENTINELhsa3SENTINEL";

fn write(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

fn append(path: &Path, text: &str) {
    let mut t = std::fs::read_to_string(path).unwrap_or_default();
    t.push_str(text);
    std::fs::write(path, t).unwrap();
}

/// stdout and stderr of `rdy <args>`, after checking that neither holds the sentinel.
fn run_clean(s: &Served, args: &[&str]) -> (bool, String) {
    let o = s.rdy(args);
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        !out.contains(SENTINEL),
        "rdy {args:?} printed the sentinel:\n{out}"
    );
    assert!(!out.contains("SENTINELhsa3"), "rdy {args:?}:\n{out}");
    (o.status.success(), out)
}

#[test]
fn hsa3_config_cli_never_reads_or_displays_credentials() {
    let s = Served::new();
    let uf = s.user_config();
    // a credential file that no settings read may open: unreadable, and holding the sentinel
    let cred = uf.with_file_name("credentials.yaml");
    write(
        &cred,
        &format!("installs:\n  \"{O}\":\n    state: ready\n    access_token: {SENTINEL}\n"),
    );
    rdy_qa::platform::set_mode(&cred, 0o000).unwrap();
    // valid settings, a secret spelling and an unknown key under an install, and a dotted key
    write(
        &uf,
        &format!(
            "actor: human:qa\nremotes:\n  origin: {{url: \"{O}/team\", mode: share}}\nhub:\n  installs:\n    \"{O}\":\n      ca_file: /etc/rdy/ca.pem\n      access_token: {SENTINEL}\n      tokn: {SENTINEL}\n\"hub.installs.{O}.refresh_token\": {SENTINEL}\n"
        ),
    );
    // the wrong level: no hub setting comes from the shared project config
    let project = s.dir.path();
    append(
        &project.join(".rdy.yaml"),
        &format!("hub:\n  installs:\n    \"{O}\":\n      ca_file: {SENTINEL}\n"),
    );
    write(
        &project.join(".rdy/local.yaml"),
        &format!("\"remotes.origin.url\": {SENTINEL}\n"),
    );

    let (ok, text) = run_clean(&s, &["config"]);
    assert!(ok, "{text}");
    assert!(
        text.contains(&format!("remotes.origin = {{url: {O}/team, mode: share}}")),
        "{text}"
    );
    assert!(
        text.contains(&format!("hub.installs.{O}.ca_file = /etc/rdy/ca.pem")),
        "{text}"
    );
    let (ok, json) = run_clean(&s, &["config", "--json"]);
    assert!(ok, "{json}");
    assert!(json.contains("\"remotes.origin\""), "{json}");
    let (ok, shown) = run_clean(&s, &["config", "--show-origin"]);
    assert!(ok, "{shown}");
    for expected in [
        format!("hub.installs.{O}.ca_file = /etc/rdy/ca.pem  # {}", uf.display()),
        "warning W_CONFIG_UNKNOWN: unknown hub setting hub.installs.credential; remove hub.installs.credential".to_string(),
        format!("warning W_CONFIG_UNKNOWN: unknown hub setting hub.installs.{O}.tokn; remove hub.installs.{O}.tokn"),
        format!("warning W_CONFIG_UNKNOWN: hub.installs.{O}.ca_file is not allowed in .rdy.yaml; set hub.installs.{O}.ca_file in user config and remove it from .rdy.yaml"),
        "warning W_CONFIG_UNKNOWN: unknown hub setting remotes; remove remotes".to_string(),
    ] {
        assert!(shown.contains(&expected), "{expected}\n---\n{shown}");
    }
    let (_, status) = run_clean(&s, &["status"]);
    assert!(
        status.contains("unknown hub setting hub.installs.credential"),
        "{status}"
    );
    let (_, status) = run_clean(&s, &["status", "--json"]);
    assert!(status.contains("hub.installs.credential"), "{status}");
    // the session's warnings too
    let mut c = McpClient::start_proxy(&s);
    let r = c
        .call(
            "rdy_session",
            serde_json::json!({"actor": "agent:m", "role": "writer"}),
        )
        .ok();
    let session = r.to_string();
    assert!(!session.contains("SENTINELhsa3"), "{session}");
    assert!(
        session.contains("unknown hub setting hub.installs.credential"),
        "{session}"
    );
    drop(c);

    // a user config that does not parse names its file and nothing it holds
    for bad in [
        format!("hub:\n  installs:\n    \"{O}\": {{ca_file: {SENTINEL}\n"),
        format!("hub:\n  {SENTINEL}: 1\n  {SENTINEL}: 2\n"),
        format!("remotes: &r {{x: {SENTINEL}}}\nother: *r\n"),
    ] {
        write(&uf, &format!("actor: human:qa\n{bad}"));
        for args in [
            &["config"][..],
            &["config", "--json"],
            &["config", "--show-origin"],
        ] {
            let (ok, out) = run_clean(&s, args);
            assert!(!ok, "{out}");
            assert!(
                out.contains(&format!(
                    "config is not a valid bounded mapping in {}",
                    uf.display()
                )),
                "{args:?}: {out}"
            );
            assert!(
                out.contains("fix the config syntax, duplicate keys or aliases"),
                "{out}"
            );
        }
        run_clean(&s, &["status"]);
    }
    write(&uf, "actor: human:qa\n");
    rdy_qa::platform::set_mode(&cred, 0o600).unwrap();
}
