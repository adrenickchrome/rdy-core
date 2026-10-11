//! cent1, cc15: `rdy status` reports the corpus coverage and `rdy search` prints fact sheets before the
//! matches (FW-EN1, FW-EN4).
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::json;

#[test]
fn cc15_status_reports_the_corpus_coverage() {
    let s = Served::new();
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
            {"op": "create", "type": "entity", "id": "$e", "parent": "$o", "fields": {"name": "Ledger service", "kind": "service", "aliases": ["ledger"]}},
            {"op": "create", "type": "entity", "parent": "$o", "fields": {"name": "Billing", "kind": "Services"}},
            {"op": "create", "type": "claim", "parent": "$o", "body": "The ledger is append-only.", "links": {"describes": [{"target": "$e"}]}},
            {"op": "create", "type": "claim", "parent": "$o", "body": "Billing runs nightly."}
        ]))
        .unwrap();
        land(&mut r, "work").unwrap();
        r.close().unwrap();
    }
    let out = s.rdy(&["status"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let cov: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("coverage: "))
        .collect();
    assert_eq!(cov.len(), 1, "{text}");
    assert!(cov[0].contains("(grounded "), "{text}");
    assert!(
        cov[0].ends_with(" coverage issues, listed by rdy coverage"),
        "{text}"
    );
    assert!(!cov[0].contains('/'), "no fraction: {text}");
    assert!(!text.contains("health"), "FW-CV1: {text}");
    assert!(text.contains("\nkinds: Services 1, service 1\n"), "{text}");
    assert!(
        text.contains("\nsimilar kinds: Services, service\n"),
        "{text}"
    );
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok, "{v}");
    assert!(v["coverage"]["score"].is_number(), "{v}");
    assert!(v["coverage"]["issues_total"].is_number(), "{v}");
    assert!(v["coverage"]["dimensions"].is_object(), "{v}");
    assert!(v["coverage"].get("grounded").is_none(), "{v}");
    assert!(v["coverage"].get("pages").is_none(), "{v}");
    assert!(v["coverage"].get("issues").is_none(), "{v}");
    assert_eq!(
        v["coverage"]["similar_kinds"],
        json!([["Services", "service"]])
    );
    s.stop();
}

#[test]
fn cent1_rdy_search_prints_the_fact_sheet_before_the_matches() {
    let s = Served::new();
    let (e, c, p) = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let w = write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
            {"op": "create", "type": "entity", "id": "$e", "parent": "$o", "body": "Keeps the books.", "fields": {"name": "Ledger service", "kind": "service", "aliases": ["ledger"]}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$o", "body": "The ledger is append-only.", "links": {"describes": [{"target": "$e"}]}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$o", "body": "Restart the ledger nightly."}
        ]))
        .unwrap();
        land(&mut r, "work").unwrap();
        let ids = (
            w.created["$e"].clone(),
            w.created["$c"].clone(),
            w.created["$p"].clone(),
        );
        r.close().unwrap();
        ids
    };
    let out = s.rdy(&["search", "ledger"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.starts_with(&format!(
            "1 matching blocks\nby doc: ops 1\nby type: prose 1\nentity Ledger service (entity, {e}) [service], matched by alias\n  definition: Keeps the books.\n  facts:\n    {c} (describes) The ledger is append-only.\nops  {p} (prose)\n  body:1: Restart the ledger nightly.\n  suggest: describes {e}"
        )),
        "totals, then the fact sheet, then the matches: {text}"
    );
    let (ok, v) = s.json(&["--json", "search", "ledger"]);
    assert!(ok, "{v}");
    assert_eq!(v["entities"][0]["id"], e);
    s.stop();
}
