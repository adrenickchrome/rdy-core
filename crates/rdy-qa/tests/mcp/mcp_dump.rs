//! dl7: `rdy_dump` and `rdy_load` give an agent the whole-project dump and the load into a
//! directory with no store (D-FR0C4 F4, F6 and F7; D-KQ4CA).
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::every_kind::facts;
use rdy_qa::golden::{FIXTURE_DIR, approver, land};
use rdy_qa::mcp::McpClient;
use serde_json::json;
use std::path::Path;

/// A served project holding the golden corpus, landed and tagged `v1.0.0`.
fn golden_served() -> Served {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    r.import(&approver(), "import", Path::new(FIXTURE_DIR))
        .unwrap();
    land(&mut r, "import").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    r.close().unwrap();
    s
}

fn agent(s: &Served) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    c
}

#[test]
fn dl7_an_agent_dumps_and_loads_through_the_tools() {
    let s = golden_served();
    let mut c = agent(&s);
    let d = c.call("rdy_dump", json!({"out": "p.rdydump"})).ok();
    let file = s.dir.path().join("p.rdydump");
    assert!(
        file.is_file(),
        "the dump lands in the project directory: {d}"
    );
    assert_eq!(d["dump_format"], 3, "{d}");
    assert!(d["batches"].as_u64().unwrap() > 0, "{d}");
    let again = c.call("rdy_dump", json!({"out": "p.rdydump"}));
    assert_eq!(
        again.code(),
        Some("E_ALREADY_EXISTS"),
        "it never overwrites: {again:?}"
    );

    let scratch = tempfile::tempdir().unwrap();
    let to = scratch.path().join("new");
    let l = c
        .call(
            "rdy_load",
            json!({"file": file.to_str().unwrap(), "dir": to.to_str().unwrap()}),
        )
        .ok();
    assert_eq!(l["fsck"]["unlogged_moves"], 0, "{l}");
    assert!(
        l["files_written"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == ".rdy.yaml"),
        "{l}"
    );
    assert_eq!(l["batches"], d["batches"], "{l}");

    // a relative file and dir resolve against the project directory
    let rel = c
        .call("rdy_load", json!({"file": "p.rdydump", "dir": "loaded"}))
        .ok();
    assert!(s.dir.path().join("loaded/.rdy/store").exists(), "{rel}");

    // the session wrote its inbox cursor before the dump, so the source is read once the
    // daemon has stopped
    drop(c);
    s.stop();
    let source = Rdy::open(s.dir.path()).unwrap();
    let loaded = Rdy::open(&to).unwrap();
    assert_eq!(facts(&loaded), facts(&source));
    loaded.close().unwrap();
    source.close().unwrap();
}

#[test]
fn dl7_the_tools_need_a_session() {
    let s = golden_served();
    let mut c = McpClient::start_proxy(&s);
    let r = c.call("rdy_dump", json!({"out": "p.rdydump"}));
    assert_eq!(r.code(), Some("E_NO_SESSION"), "{r:?}");
    let r = c.call("rdy_load", json!({"file": "p.rdydump", "dir": "new"}));
    assert_eq!(r.code(), Some("E_NO_SESSION"), "{r:?}");
    assert!(!s.dir.path().join("p.rdydump").exists());
}

#[test]
fn dl7_rdy_load_into_this_project_is_refused() {
    let s = golden_served();
    let mut c = agent(&s);
    c.call("rdy_dump", json!({"out": "p.rdydump"})).ok();
    let r = c.call("rdy_load", json!({"file": "p.rdydump", "dir": "."}));
    let Some("E_ALREADY_EXISTS") = r.code() else {
        panic!("expected E_ALREADY_EXISTS, got {r:?}")
    };
    let rdy_qa::mcp::Reply::ToolError(e) = r else {
        unreachable!()
    };
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .ends_with(" already has a store"),
        "{e}"
    );
}

#[test]
fn dl7_a_load_into_dot_rdy_is_refused() {
    let s = golden_served();
    let mut c = agent(&s);
    c.call("rdy_dump", json!({"out": "p.rdydump"})).ok();
    for dir in [".rdy/packs", ".rdy", ".rdy/store/../new"] {
        let r = c.call("rdy_load", json!({"file": "p.rdydump", "dir": dir}));
        let Some("E_BAD_INPUT") = r.code() else {
            panic!("{dir}: expected E_BAD_INPUT, got {r:?}")
        };
        let rdy_qa::mcp::Reply::ToolError(e) = r else {
            unreachable!()
        };
        assert_eq!(
            e["message"],
            format!(
                "{} is inside a project's .rdy directory",
                s.dir.path().join(dir).display()
            ),
            "{dir}"
        );
        assert_eq!(
            e["fix"],
            "load into a new directory outside any .rdy, for example a sibling of the project"
        );
    }
    assert!(!s.dir.path().join(".rdy/packs/.rdy.yaml").exists());
    assert!(!s.dir.path().join(".rdy/new").exists());
    // the session's own project still works
    let st = c.call("rdy_tags", json!({}));
    assert!(st.is_ok(), "{st:?}");
}

/// `text` with every run of white space made one space.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn dl7_rdy_load_describes_the_in_place_move_in_order() {
    let s = Served::new();
    let mut c = McpClient::start_proxy(&s);
    let tools = c.tools();
    let load = tools
        .iter()
        .find(|t| t["name"] == "rdy_load")
        .expect("rdy_load is listed");
    let text = load["description"].as_str().unwrap();
    assert!(
        one_line(text).contains(&one_line(&rdy_store::format::move_with_dump("<n>"))),
        "{text}"
    );
}
