//! logt1: rdy log shows each landing's time, proposer and approver, and pages with --limit and --since.
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::{Value, from_value, json};

const AT: &str = "2026-10-05T09:00:00Z";

/// Seeded with the clock pinned: landing 1 proposed and approved by the human; landing 2 proposed
/// by agent:a/ra, approved by the human, tagged v1.0.0. Returns (landing 1, landing 2), full hashes.
fn seeded() -> (Served, String, String) {
    let s = Served::new();
    let _clock = pin_clock(1_791_190_800_000);
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let res = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "One."}
    ]))
    .unwrap();
    land(&mut r, "seed").unwrap();
    let l1 = r.read().canon_head().unwrap().to_string();
    let a = agent("agent:a", Some("ra"));
    r.write(
        &a,
        WriteRequest {
            view: "ra".into(),
            ops: from_value(json!([{"op": "update", "id": res.created["$c"], "body": "Two."}]))
                .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let p = r.propose(&a, "ra", "Agent work", None).unwrap();
    r.land(&approver(), &p.id).unwrap();
    let l2 = r.read().canon_head().unwrap().to_string();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    r.close().unwrap();
    (s, l1, l2)
}

fn approved(row: &Value) -> String {
    match row["approved"].as_str() {
        Some(p) => format!("approved {p} by human:qa"),
        None => "approved by human:qa".to_string(),
    }
}

#[test]
fn logt1_log_text_shows_the_time_the_proposer_and_the_approver() {
    let (s, l1, l2) = seeded();
    let (ok, rows) = s.json(&["--json", "log"]);
    assert!(ok, "{rows}");
    assert_eq!(
        rows[0]["proposed_by"],
        json!({"actor": "agent:a", "run": "ra"})
    );
    assert_eq!(
        rows[1]["proposed_by"],
        json!({"actor": "human:qa", "run": "cli"})
    );
    let o = s.rdy(&["log"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    let heads: Vec<&str> = out.lines().filter(|l| !l.starts_with("    ")).collect();
    assert_eq!(
        heads[0],
        format!(
            "{}  {AT}  Agent work  proposed by agent:a/ra  {}  [v1.0.0]",
            &l2[..12],
            approved(&rows[0])
        )
    );
    assert_eq!(
        heads[1],
        format!(
            "{}  {AT}  seed  proposed by human:qa  {}",
            &l1[..12],
            approved(&rows[1])
        )
    );
    assert!(
        !out.contains("local"),
        "the default assurance is not printed: {out}"
    );
}

#[test]
fn logt1_limit_and_since_page_the_log_newest_first() {
    let (s, l1, l2) = seeded();
    let landings = |args: &[&str]| -> Vec<String> {
        let (ok, rows) = s.json(args);
        assert!(ok, "{args:?}: {rows}");
        rows.as_array()
            .unwrap()
            .iter()
            .map(|r| r["landing"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(landings(&["--json", "log"]), vec![l2.clone(), l1.clone()]);
    assert_eq!(
        landings(&["--json", "log", "--limit", "1"]),
        vec![l2.clone()]
    );
    assert_eq!(
        landings(&["--json", "log", "--since", &l1[..8]]),
        vec![l2.clone()]
    );
    assert_eq!(
        landings(&["--json", "log", "--since", &format!("landing:{l1}")]),
        vec![l2.clone()]
    );
    assert!(landings(&["--json", "log", "--since", "tag:v1.0.0"]).is_empty());
    let o = s.rdy(&["log", "--since", "tag:v1.0.0"]);
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim_end(), "no landings");
    for (args, code) in [
        (vec!["--json", "log", "--since", "000000"], "E_NOT_FOUND"),
        (
            vec!["--json", "log", "--since", "tag:v9.9.9"],
            "E_NOT_FOUND",
        ),
        (vec!["--json", "log", "--limit", "0"], "E_BAD_INPUT"),
    ] {
        let (ok, v) = s.json(&args);
        assert!(!ok, "{args:?}");
        assert_eq!(v["code"], code, "{args:?}: {v}");
    }
}
