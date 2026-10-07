//! blame1: rdy show names the change that last wrote a block and its justification; --blame lists every version.
use rdy_engine::*;
use rdy_model::{BlockId, Justification};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::{from_value, json};

/// Every stamp the seed writes reads as this (the clock is pinned while seeding).
const AT: &str = "2026-10-05T09:00:00Z";

struct Seed {
    claim: String,
    dec: String,
    change: String,
    landing: String,
}

/// Seeded in this process with the clock pinned: a claim created and landed (tagged v0.1.0), then
/// updated by agent:a/ra implementing a decision, and landed.
fn seeded() -> (Served, Seed) {
    let s = Served::new();
    let _clock = pin_clock(1_791_190_800_000);
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let res = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "One."}
    ]))
    .unwrap();
    let dec = r
        .decide(
            &writer(),
            "seed",
            DecisionInput {
                parent: res.created["$s"].clone(),
                statement: "Claims are short".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "seed").unwrap();
    r.tag(&approver(), "v0.1.0", None, None).unwrap();
    let claim = res.created["$c"].clone();
    let w = r
        .write(
            &agent("agent:a", Some("ra")),
            WriteRequest {
                view: "ra".into(),
                ops: from_value(json!([{"op": "update", "id": claim, "body": "Two."}])).unwrap(),
                justification: Some(Justification::Implements(BlockId::parse(&dec).unwrap())),
                message: "tighten".into(),
            },
        )
        .unwrap();
    land(&mut r, "ra").unwrap();
    let landing = r.read().canon_head().unwrap().to_string();
    r.close().unwrap();
    (
        s,
        Seed {
            claim,
            dec,
            change: w.change,
            landing,
        },
    )
}

fn text(s: &Served, args: &[&str]) -> String {
    let o = s.rdy(args);
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn blame1_show_prints_the_last_change_with_its_justification() {
    let (s, x) = seeded();
    let out = text(&s, &["show", &x.claim]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[1],
        format!(
            "last change: {} by agent:a/ra at {AT}, implements {}, landed {}",
            &x.change[..12],
            x.dec,
            &x.landing[..12]
        )
    );
    assert!(!out.contains("history:"), "{out}");
    let (ok, v) = s.json(&["--json", "show", &x.claim]);
    assert!(ok, "{v}");
    assert_eq!(v["justification"], json!({"implements": x.dec}));
    assert_eq!(v["history"].as_array().unwrap().len(), 1);
    assert_eq!(v["history_more"], true);
    // a decision: the status line, then the last change
    let out = text(&s, &["show", &x.dec]);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[1].starts_with("status: "), "{out}");
    assert!(
        lines[2].starts_with("last change: ")
            && lines[2].contains(&format!(" by human:qa at {AT}")),
        "{out}"
    );
}

#[test]
fn blame1_show_blame_lists_every_version_newest_first() {
    let (s, x) = seeded();
    let (ok, v) = s.json(&["--json", "show", &x.claim, "--blame"]);
    assert!(ok, "{v}");
    let h = v["history"].as_array().unwrap().clone();
    assert_eq!((h.len(), v["history_more"].clone()), (2, json!(false)));
    assert_eq!(h[0]["author"]["run"], "ra");
    assert_eq!(h[0]["message"], "tighten");
    let (v2, v1) = (
        h[0]["version"].as_str().unwrap(),
        h[1]["version"].as_str().unwrap(),
    );
    let c1 = h[1]["change"].as_str().unwrap();
    let l1 = h[1]["landing"]["id"].as_str().unwrap();
    let out = text(&s, &["show", &x.claim, "--blame"]);
    let lines: Vec<&str> = out.lines().collect();
    let i = lines
        .iter()
        .position(|l| *l == "history:")
        .unwrap_or_else(|| panic!("{out}"));
    let want = [
        format!(
            "  {}  update  {}  agent:a/ra  {AT}  implements {}",
            &v2[..12],
            &x.change[..12],
            x.dec
        ),
        format!(
            "    landed {}, approved by human:qa at {AT}",
            &x.landing[..12]
        ),
        format!("  {}  create  {}  human:qa  {AT}", &v1[..12], &c1[..12]),
        format!("    landed {}, approved by human:qa at {AT}", &l1[..12]),
    ];
    assert_eq!(
        lines[i + 1..].to_vec(),
        want.iter().map(String::as_str).collect::<Vec<_>>(),
        "{out}"
    );
}

#[test]
fn blame1_blame_at_a_version_starts_from_that_version() {
    let (s, x) = seeded();
    let pinned = format!("{}@v0.1.0", x.claim);
    for args in [
        vec!["--json", "show", pinned.as_str(), "--blame"],
        vec![
            "--json",
            "show",
            x.claim.as_str(),
            "--scope",
            "tag:v0.1.0",
            "--blame",
        ],
    ] {
        let (ok, v) = s.json(&args);
        assert!(ok, "{args:?}: {v}");
        let h = v["history"].as_array().unwrap();
        assert_eq!(h.len(), 1, "{args:?}: {v}");
        assert_eq!(h[0]["op"], "create");
        assert_eq!(v["history_more"], false);
        assert_eq!(v["justification"], serde_json::Value::Null);
    }
}

#[test]
fn blame1_an_unlanded_write_reads_not_landed() {
    let (s, x) = seeded();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": x.claim, "body": "Three."}]),
    )
    .unwrap();
    r.close().unwrap();
    let out = text(&s, &["show", &x.claim, "--scope", "work"]);
    let line = out.lines().nth(1).unwrap();
    assert!(
        line.starts_with("last change: ")
            && line.contains(" by human:qa at ")
            && line.ends_with(", not landed"),
        "{out}"
    );
    let (ok, v) = s.json(&["--json", "show", &x.claim, "--scope", "work"]);
    assert!(ok);
    assert_eq!(v["justification"], serde_json::Value::Null);
}

#[test]
fn blame1_a_landed_revert_is_the_last_change_and_blame_lists_it() {
    // fix round 1 (I1): writing "One." back and landing it names the revert, not the create
    let (s, x) = seeded();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let w = write(
        &mut r,
        "revert",
        json!([{"op": "update", "id": x.claim, "body": "One."}]),
    )
    .unwrap();
    land(&mut r, "revert").unwrap();
    let l3 = r.read().canon_head().unwrap().to_string();
    r.close().unwrap();
    let out = text(&s, &["show", &x.claim]);
    let line = out.lines().nth(1).unwrap();
    assert!(
        line.starts_with(&format!("last change: {} by human:qa at ", &w.change[..12]))
            && line.ends_with(&format!(", landed {}", &l3[..12])),
        "{out}"
    );
    let (ok, v) = s.json(&["--json", "show", &x.claim, "--blame"]);
    assert!(ok, "{v}");
    let h = v["history"].as_array().unwrap();
    let got: Vec<&str> = h.iter().map(|e| e["change"].as_str().unwrap()).collect();
    assert_eq!(got.len(), 3, "{v}");
    assert_eq!((got[0], got[1]), (w.change.as_str(), x.change.as_str()));
    assert_eq!(h[2]["op"], "create");
    assert_eq!(v["history_more"], false);
}
