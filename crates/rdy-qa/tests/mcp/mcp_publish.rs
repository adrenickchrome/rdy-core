//! hpb15 (D-Q0AVZ, D-KQ4CA, D-8QV9G, D-50JXR; hub i-6ZXW0): `rdy_publish`'s published shape.
//! Its input is closed to `{tag, to, site, keep_latest}`: no caller, whatever its role, names a
//! profile or an audience, on MCP or on the CLI. The tool is in the remote family, with the
//! one description the chat path reads. The cross-surface runs against a hub are in the cli
//! binary's `cli_publish`.
use rdy_qa::fixtures::{project, rdy_bin};
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::process::{Command, Stdio};

const DESCRIPTION: &str = "Upload a tagged publication to a publish remote and create a hub candidate. The site selects the profile. Relay warnings, the candidate link and the disclosure review to the person. The hub authorizes publication.";
const KEEP_LATEST: &str = "Optional: true or false. rdy sends it to the hub beside the record. Omit it to use the hub's default.";

fn publish_tool(c: &mut McpClient) -> Value {
    c.tools()
        .into_iter()
        .find(|t| t["name"] == "rdy_publish")
        .expect("rdy_publish is listed")
}

#[test]
fn hpb15_the_publish_tool_is_listed_with_its_description() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let t = publish_tool(&mut c);
    assert_eq!(t["description"], DESCRIPTION);
    assert_eq!(
        t["inputSchema"]["properties"]["keep_latest"]["description"],
        KEEP_LATEST
    );
    c.alive();
}

/// The closed schema: exactly four members, three required strings and an optional boolean, no
/// other member allowed. Every role's session meets the same refusal for a profile or an
/// audience, and so does the CLI, before a session, a config or a hub is read.
#[test]
fn hpb15_no_role_can_override_profile_or_audience() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let t = publish_tool(&mut c);
    let schema = &t["inputSchema"];
    let mut props: Vec<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    props.sort();
    assert_eq!(props, ["keep_latest", "site", "tag", "to"]);
    let mut required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    required.sort();
    assert_eq!(required, ["site", "tag", "to"]);
    assert_eq!(schema["additionalProperties"], json!(false), "{schema}");
    for role in ["reader", "writer", "reviewer"] {
        let r = c.call(
            "rdy_session",
            json!({"actor": "agent:claude", "run": "r7", "role": role}),
        );
        assert!(r.is_ok(), "{r:?}");
        for (key, value) in [
            ("profile", json!("p-AAAAA")),
            ("audience", json!("public")),
            ("selector", json!("implicit:org")),
        ] {
            let mut args = json!({"tag": "v1.0.0", "to": "pub", "site": "docs"});
            args[key] = value;
            match c.call("rdy_publish", args) {
                Reply::ToolError(e) => {
                    assert_eq!(e["code"], "E_BAD_INPUT", "{role} {key}");
                    assert_eq!(
                        e["message"],
                        format!("rdy_publish: unknown parameter \"{key}\""),
                        "{role}"
                    );
                    assert!(
                        e["fix"]
                            .as_str()
                            .unwrap()
                            .ends_with("the parameters here are keep_latest, site, tag, to"),
                        "{e}"
                    );
                }
                other => panic!("{role} {key}: {other:?}"),
            }
        }
    }
    c.alive();
    // the CLI has no such option either: clap's own refusal
    for flag in [
        "--profile",
        "--audience",
        "--ratify",
        "--authorize",
        "--token",
        "--origin",
        "--format",
        "--out",
        "--force",
        "--resume",
    ] {
        let o = Command::new(rdy_bin())
            .env_clear()
            .args(["--project"])
            .arg(dir.path())
            .args([
                "publish", "v1.0.0", "--to", "pub", "--site", "docs", flag, "x",
            ])
            .stdin(Stdio::null())
            .output()
            .expect("run rdy");
        assert!(!o.status.success(), "{flag}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            err.contains(&format!("unexpected argument '{flag}'")),
            "{flag}: {err}"
        );
    }
}
