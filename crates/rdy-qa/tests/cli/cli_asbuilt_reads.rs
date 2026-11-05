//! ab2 (D-WJ0SJ, D-YAY1G): the fallback commands print the as-built status and the override
//! warnings that the chat tools compute, through the same daemon. No new flag.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{land, write};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const FIX: &str = "Remove the override, or change it in a view and confirm its as-built plan item.";

fn stdout(s: &Served, args: &[&str]) -> String {
    let out = s.rdy(args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn cli_json(s: &Served, args: &[&str]) -> Value {
    let (ok, v) = s.json(args);
    assert!(ok, "{args:?}: {v}");
    v
}

#[test]
fn ab2_cli_status_and_queries_match_mcp() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let w = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Shipped"}},
        {"op": "create", "type": "section", "id": "$p", "parent": "$d", "fields": {"title": "Planned", "planned": true}},
        {"op": "create", "type": "decision", "id": "$dv", "fields": {"statement": "Ship V.", "status": "proposed", "built": "v1.2.0"}},
        {"op": "create", "type": "decision", "id": "$dp", "fields": {"statement": "Plan P.", "status": "proposed"}},
        {"op": "create", "type": "decision", "id": "$d2", "fields": {"statement": "Ship two.", "status": "proposed", "built": "2.0.0"}},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "Shipped in one two.", "links": {"decided_by": ["$dv"]}},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Still planned.", "links": {"implements": ["$dp"]}},
        {"op": "create", "type": "claim", "id": "$c3", "parent": "$p", "body": "Under the planned section.", "links": {"decided_by": ["$d2"]}},
        {"op": "create", "type": "claim", "id": "$c4", "parent": "$s", "body": "Said to ship early.", "fields": {"since": "v1.0.0"}, "links": {"decided_by": ["$d2"]}}
    ]))
    .unwrap();
    land(&mut r, "seed").unwrap();
    r.close().unwrap();
    let c: BTreeMap<String, String> = w.created.clone();
    let (c1, c2, c3, c4, p) = (&c["$c1"], &c["$c2"], &c["$c3"], &c["$c4"], &c["$p"]);

    // the chat path, through the same daemon
    let mut mcp = McpClient::start_proxy(&s);
    mcp.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "r1", "role": "reader"}),
    )
    .ok();
    for id in [c1, c2, c3, c4, p, &c["$d2"]] {
        let ctx = mcp
            .call("rdy_context", json!({"id": id, "scope": "canon"}))
            .ok();
        let shown = cli_json(&s, &["--json", "show", id]);
        assert_eq!(shown["as_built"], ctx["as_built"], "{id}");
        assert_eq!(shown["warnings"], ctx["warnings"], "{id}");
        assert_eq!(shown["status"], ctx["status"], "{id}");
    }
    // the computed values: normalized versions, the authored field kept as written
    let shown = cli_json(&s, &["--json", "show", c4]);
    assert_eq!(shown["fields"]["since"], "v1.0.0");
    assert_eq!(shown["as_built"]["since"], "1.0.0");
    assert_eq!(shown["as_built"]["derived"]["since"], "2.0.0");
    assert_eq!(
        cli_json(&s, &["--json", "show", c1])["as_built"]["since"],
        "1.2.0"
    );
    // the text: one as-built line beside the header, then the warning with its fix
    let t = stdout(&s, &["show", c1]);
    assert!(t.contains("\nas-built: since 1.2.0\n"), "{t}");
    assert!(!t.contains("warning"), "{t}");
    let t = stdout(&s, &["show", c2]);
    assert!(t.contains("\nas-built: planned\n"), "{t}");
    let t = stdout(&s, &["show", c3]);
    assert!(
        t.contains(&format!(
            "\nas-built: planned (override planned on {p}; its decisions derive since 2.0.0)\n"
        )),
        "{t}"
    );
    assert!(
        t.contains(&format!(
            "\nwarning W_AS_BUILT_OVERRIDE: Block {c3} has override status planned, but its decisions derive since 2.0.0.\n  fix: {FIX}"
        )),
        "{t}"
    );
    let t = stdout(&s, &["show", c4]);
    assert!(
        t.contains(&format!(
            "\nas-built: since 1.0.0 (override since on {c4}; its decisions derive since 2.0.0)\n"
        )),
        "{t}"
    );
    assert!(
        t.contains(&format!(
            "\nwarning W_AS_BUILT_OVERRIDE: Block {c4} has override status since 1.0.0, but its decisions derive since 2.0.0.\n  fix: {FIX}"
        )),
        "{t}"
    );
    // a decision keeps its retirement status and gets no as-built line
    let t = stdout(&s, &["show", &c["$d2"]]);
    assert!(t.contains("\nstatus: live\n"), "{t}");
    assert!(!t.contains("as-built"), "{t}");

    // the same filter through rdy q and rdy_query
    for filter in [
        "planned = true",
        "planned = false",
        "has since",
        "since >= v1.2.0",
        "since < 2.0.0",
        "fields.since = v1.0.0",
    ] {
        let cli = cli_json(&s, &["--json", "q", filter]);
        let chat = mcp
            .call(
                "rdy_query",
                json!({"filter": filter, "scope": "canon", "max_tokens": 100000}),
            )
            .ok();
        assert_eq!(cli["matches"], chat["matches"], "{filter}");
        assert_eq!(cli["total"], chat["total"], "{filter}");
    }
    let t = stdout(&s, &["q", "planned = true"]);
    assert!(t.starts_with("3 matching blocks\n"), "{t}");
    let t = stdout(&s, &["q", "since >= v1.2.0"]);
    assert!(
        t.starts_with("1 matching block\n") && t.contains(c1.as_str()),
        "{t}"
    );
    let out = s.rdy(&["q", "since > soon"]);
    assert!(!out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim_end(),
        "error E_BAD_INPUT: filter: since takes a release version at column 9\nfix: read the filter guide in rdy_schema, or run rdy q --help"
    );
    // the filter guide in rdy q --help names the new fields
    let help = stdout(&s, &["q", "--help"]);
    assert!(help.contains("planned, since and fields.<name>"), "{help}");

    // YAML round-trip: the source YAML holds the authored fields only, and reads back to the
    // stored fields
    let yaml = mcp
        .call(
            "rdy_read",
            json!({"id": c4, "scope": "canon", "format": "yaml"}),
        )
        .ok();
    let text = yaml["text"].as_str().unwrap();
    assert!(
        !text.contains("as_built") && !text.contains("derived"),
        "{text}"
    );
    let back = rdy_engine::block_from_yaml(text).unwrap();
    let fields: serde_json::Map<String, Value> = back
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), rdy_engine::field_json(v)))
        .collect();
    assert_eq!(Value::Object(fields), shown_fields(&s, c4));
    assert_eq!(yaml["as_built"][c4.as_str()]["since"], "1.0.0");
    s.stop();
}

fn shown_fields(s: &Served, id: &str) -> Value {
    cli_json(s, &["--json", "show", id])["fields"].clone()
}
