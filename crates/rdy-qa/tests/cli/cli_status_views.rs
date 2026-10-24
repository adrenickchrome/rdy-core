//! df9 (F11): `rdy status` and `rdy_overview` list a view only when it is open and holds at least
//! one included change. A landed view stays open and empty (nothing closes it), so it is hidden,
//! not closed. `rdy view list` stays the full list.
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

fn names(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap().to_string())
        .collect()
}

fn overview_views(s: &Served) -> Value {
    let mut c = McpClient::start_proxy(s);
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:ov", "role": "reader"}),
    );
    assert!(r.code().is_none(), "{:?}", r.ok());
    c.call("rdy_overview", json!({})).ok()["views"].clone()
}

#[test]
fn df9_landed_view_stays_listed() {
    let s = Served::new();
    // a tree to import: a doc with a section and a claim, exported from a scratch project
    let tree = tempfile::tempdir().unwrap();
    {
        let (_p, mut r) = project();
        seed_doc(&mut r, "work");
        land(&mut r, "work").unwrap();
        r.read().export(&Scope::Canon, tree.path()).unwrap();
    }
    let dir = tree.path().to_str().unwrap().to_string();
    let (ok, out) = s.json(&["--json", "import", &dir, "--view", "import"]);
    assert!(ok, "{out}");
    // before the landing the view holds the import: listed
    let (ok, st) = s.json(&["--json", "status"]);
    assert!(ok, "{st}");
    assert_eq!(names(&st["views"]), ["import"], "{st}");
    assert_eq!(
        overview_views(&s),
        json!([{"name": "import", "changes": 1}])
    );
    // the human lands it
    let (ok, p) = s.json(&[
        "--json",
        "propose",
        "--title",
        "import",
        "--view",
        "import",
        "--summary",
        "test summary",
    ]);
    assert!(ok, "{p}");
    let pref = p["ref"].as_str().unwrap().to_string();
    let o = s.rdy(&["approve", &pref]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // the view is open and empty: neither status nor the overview lists it
    let (ok, st) = s.json(&["--json", "status"]);
    assert!(ok, "{st}");
    assert_eq!(st["views"], json!([]), "{st}");
    assert_eq!(overview_views(&s), json!([]));
    // `view list` is the full list: the landed view is still there with no changes
    let (ok, v) = s.json(&["--json", "view", "ls"]);
    assert!(ok, "{v}");
    let row = v["views"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == "import")
        .unwrap_or_else(|| panic!("{v}"));
    assert_eq!(row["changes"], 0, "{v}");
    assert_eq!(row["closed"], false, "{v}");
}

#[test]
fn df9_a_view_with_changes_is_listed_and_a_closed_one_is_not() {
    let s = Served::new();
    s.stop();
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let (_doc, section, _claim) = seed_doc(&mut r, "seed");
        land(&mut r, "seed").unwrap();
        write_as(
            &mut r,
            &as_agent("agent:a", "r1"),
            "w",
            json!([{"op": "create", "type": "claim", "parent": section, "body": "y"}]),
        )
        .unwrap();
        r.close().unwrap();
    }
    let (ok, st) = s.json(&["--json", "status"]);
    assert!(ok, "{st}");
    let w: Vec<&Value> = st["views"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["name"] == "w")
        .collect();
    assert_eq!(w.len(), 1, "{st}");
    assert_eq!(w[0]["changes"], 1, "{st}");
    assert!(w[0].get("owner").is_some() && w[0].get("version_position").is_some());
    assert_eq!(
        overview_views(&s)
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["name"] == "w")
            .count(),
        1
    );
    // the human closes it: a closed view keeps its changes but is no longer listed
    let (ok, c) = s.json(&["--json", "view", "close", "w"]);
    assert!(ok, "{c}");
    let (ok, st) = s.json(&["--json", "status"]);
    assert!(ok, "{st}");
    assert!(!names(&st["views"]).contains(&"w".to_string()), "{st}");
    assert!(!names(&overview_views(&s)).contains(&"w".to_string()));
    // `view list --all` still shows it, closed, with its change
    let (ok, v) = s.json(&["--json", "view", "ls", "--all"]);
    assert!(ok, "{v}");
    let row = v["views"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == "w")
        .unwrap_or_else(|| panic!("{v}"));
    assert_eq!(
        (row["closed"].clone(), row["changes"].clone()),
        (json!(true), json!(1))
    );
}
