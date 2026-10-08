//! bent1: briefings carry the entity work (FW-EN3, FW-EN4, FW-EN5): the blocks that mention a new
//! doc, graph coverage for what they cover, and related decisions for a covered decision.
use rdy_engine::brief::{Briefing, Coverage};
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

fn brief(r: &Rdy, ids: &[&str], path: Option<&str>) -> Briefing {
    r.read()
        .brief(
            &Scope::View("work".into()),
            &Coverage {
                ids: ids.iter().map(|s| s.to_string()).collect(),
                path: path.map(String::from),
            },
            None,
        )
        .unwrap()
}

fn json_of(b: &Briefing) -> Value {
    serde_json::to_value(b).unwrap()
}

#[test]
fn bent1_a_briefing_on_a_new_doc_lists_the_blocks_that_mention_it() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "claim", "id": "$m", "parent": "$o", "body": "Every entry is copied to the Archive nightly."},
        {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "archive", "title": "Archive", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "parent": "$a", "body": "See {{$m}}."},
        {"op": "create", "type": "claim", "parent": "$a", "body": "Ops owns this."}
    ]))
    .unwrap();
    let v = json_of(&brief(&r, &[], Some("archive")));
    assert_eq!(
        v["mentioned_by"],
        json!([{"doc": w.created["$a"], "block": w.created["$m"], "field": "body", "mention": "Archive"}]),
        "{v}"
    );
    // ops is linked from the archive, so it is not new, though "Ops owns this." names it
    let v = json_of(&brief(&r, &[], Some("ops")));
    assert!(v.get("mentioned_by").is_none(), "{v}");
}

#[test]
fn bent1_a_briefing_carries_coverage_for_what_it_covers() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$o", "fields": {"name": "Ledger service", "aliases": ["ledger"]}},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$o", "body": "The ledger is append-only.", "links": {"describes": [{"target": "$e"}]}},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$o", "body": "The ledger closes daily."}
    ]))
    .unwrap();
    let b = brief(
        &r,
        &[w.created["$c1"].as_str(), w.created["$c2"].as_str()],
        None,
    );
    let c = &b.coverage;
    let g = &c.grounded;
    assert_eq!(
        (
            c.scope.as_str(),
            g.facts,
            g.facts_tied,
            g.content,
            g.content_tied,
            g.mentions,
            g.mentions_linked
        ),
        ("covered", 2, 1, 2, 1, 2, 1)
    );
    let v = json_of(&b);
    assert!(
        v["coverage"]["grounded"].is_object() && v.get("health").is_none(),
        "FW-CV1: {v}"
    );
}

#[test]
fn bent1_related_decisions_in_a_briefing_stay_within_the_cap() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$o", "fields": {"name": "Ledger service"}}
    ]))
    .unwrap();
    let (o, e) = (w.created["$o"].clone(), w.created["$e"].clone());
    let mut ds = vec![];
    for i in 0..22 {
        let d = r
            .decide(
                &writer(),
                "work",
                DecisionInput {
                    parent: o.clone(),
                    statement: format!("Rule {i}"),
                    affects: vec![e.clone()],
                    ..Default::default()
                },
            )
            .unwrap()
            .created["$decision"]
            .clone();
        ds.push(d);
    }
    // each of the two covered decisions has 21 proposed decisions on the same entity: 20 each
    // after the per-decision cap (2 dropped), 40 in all, 20 shown (20 more dropped)
    let v = json_of(&brief(&r, &[ds[0].as_str(), ds[1].as_str()], None));
    assert_eq!(
        v["related_decisions"].as_array().map(Vec::len),
        Some(rdy_engine::related::RELATED_CAP),
        "{v}"
    );
    assert_eq!(v["related_decisions_omitted"], 22, "{v}");
}

#[test]
fn bent1_a_briefing_on_a_decision_lists_related_decisions() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$o", "fields": {"name": "Ledger service"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$o", "body": "Plain."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let (o, e) = (w.created["$o"].clone(), w.created["$e"].clone());
    let a = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: o.clone(),
                statement: "The ledger is append-only".into(),
                affects: vec![e.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "work").unwrap();
    let b = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: o,
                statement: "Entries are kept forever".into(),
                affects: vec![e.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let v = json_of(&brief(&r, &[b.as_str()], None));
    assert_eq!(
        v["related_decisions"],
        json!([{"for": b, "decision": a, "status": "live", "statement": "The ledger is append-only", "relation": "same_entity", "entities": [e]}])
    );
    let v = json_of(&brief(&r, &[w.created["$c"].as_str()], None));
    assert!(v.get("related_decisions").is_none(), "{v}");
}

/// enperf2: a briefing reads the text of its covered blocks and of the few blocks that hold
/// every word of a covered new doc's name, never the scope's: the counter of block text scans
/// on this thread moves by at most that many. The lexicon (and its word index) is carried from
/// the last write, so the briefing builds nothing over the scope either.
#[test]
fn enperf2_a_briefing_reads_no_text_outside_its_coverage_and_the_mentions() {
    let (_d, mut r) = project();
    // 300 claims in other docs that never name the new doc, and 2 that do
    let mut ops = vec![];
    for d in 0..3 {
        ops.push(json!({"op": "create", "type": "doc", "id": format!("$d{d}"), "fields": {"name": format!("other-{d}"), "title": format!("Other {d}"), "doc_type": "overview"}}));
        for c in 0..100 {
            ops.push(json!({"op": "create", "type": "claim", "parent": format!("$d{d}"), "body": format!("Plain claim {c} of other doc {d}.")}));
        }
    }
    ops.push(json!({"op": "create", "type": "claim", "id": "$m1", "parent": "$d0", "body": "The Ledger archive holds it."}));
    ops.push(json!({"op": "create", "type": "claim", "id": "$m2", "parent": "$d1", "body": "See the ledger archive."}));
    // a claim that holds one word of the name only: not read
    ops.push(json!({"op": "create", "type": "claim", "parent": "$d2", "body": "The ledger closes daily."}));
    let w0 = write(&mut r, "work", json!(ops)).unwrap();
    let scope = Scope::View("work".into());
    // built once here (an earlier read built it); the next write carries it
    r.read().lexicon(&scope).unwrap();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "ledger-archive", "title": "Ledger archive", "doc_type": "overview"}},
        {"op": "create", "type": "claim", "parent": "$n", "body": "Entries are kept for a year."},
        {"op": "create", "type": "claim", "parent": "$n", "body": "Reads are served from the replica."}
    ]))
    .unwrap();
    assert!(
        r.read().lexicon_built(&scope),
        "the write carried the lexicon"
    );
    let before = rdy_engine::lexicon::text_scans();
    let b = brief(&r, &[], Some("ledger-archive"));
    let scanned = rdy_engine::lexicon::text_scans() - before;
    // the doc and its two claims, read for coverage and for the doc's name; the two mentions
    assert_eq!(b.covers, 3);
    let mut mentions: Vec<&str> = b.mentioned_by.iter().map(|m| m.block.as_str()).collect();
    mentions.sort();
    let mut expected = vec![w0.created["$m1"].as_str(), w0.created["$m2"].as_str()];
    expected.sort();
    assert_eq!(mentions, expected);
    assert!(
        scanned <= 2 * b.covers + mentions.len(),
        "the briefing read the text of {scanned} blocks; the scope holds over 300"
    );
}

/// enperf2: the mention scan a briefing made is carried across writes: the next briefing reads
/// only the blocks written since, and finds what a fresh scan finds, whether the writes add a
/// mention, change one away, delete one, or move a section of mentions into the doc.
#[test]
fn enperf2_a_carried_mention_scan_equals_a_fresh_one() {
    let (_d, mut r) = project();
    let scope = Scope::View("work".into());
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "Other", "doc_type": "overview"}},
        {"op": "create", "type": "claim", "id": "$m1", "parent": "$o", "body": "The ledger archive holds it."},
        {"op": "create", "type": "claim", "id": "$m2", "parent": "$o", "body": "Ask the Ledger Archive."},
        {"op": "create", "type": "claim", "id": "$m3", "parent": "$o", "body": "Plain."},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Moved"}},
        {"op": "create", "type": "claim", "id": "$m4", "parent": "$s", "body": "Ledger archive, again."},
        {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "ledger-archive", "title": "Ledger archive", "doc_type": "overview"}},
        {"op": "create", "type": "claim", "parent": "$n", "body": "Kept a year."}
    ]))
    .unwrap();
    let id = |k: &str| w.created[k].clone();
    let n = id("$n");
    let mentions = |r: &Rdy| {
        let b = brief(r, &[], Some("ledger-archive"));
        (json_of(&b)["mentioned_by"].clone(), b.mentioned_by_omitted)
    };
    // the first briefing makes the scan
    let (first, _) = mentions(&r);
    assert_eq!(first.as_array().map(Vec::len), Some(3), "{first}");
    let steps: Vec<Value> = vec![
        // a new mention and a mention changed away, in two writes
        json!([{"op": "create", "type": "claim", "parent": id("$o"), "body": "The ledger archive is small."}]),
        json!([{"op": "update", "id": id("$m1"), "body": "Nothing here."}]),
        // a plain block made a mention
        json!([{"op": "update", "id": id("$m3"), "body": "The LEDGER ARCHIVE."}]),
        // a mention deleted
        json!([{"op": "delete", "id": id("$m2")}]),
        // a section of mentions moved into the doc: its claim no longer counts
        json!([{"op": "move", "id": id("$s"), "parent": n}]),
    ];
    let mut added = String::new();
    for (i, ops) in steps.into_iter().enumerate() {
        let res = write(&mut r, "work", ops).unwrap();
        if i == 0 {
            added = res.created["op0"].clone();
        }
        let before = rdy_engine::lexicon::text_scans();
        let carried = mentions(&r);
        let read = rdy_engine::lexicon::text_scans() - before;
        // the doc and its claim (coverage), plus the written block
        assert!(
            read <= 5,
            "step {i}: the briefing read the text of {read} blocks"
        );
        r.invalidate();
        let fresh = mentions(&r);
        assert_eq!(carried, fresh, "step {i}");
    }
    // the claim added and the plain block made a mention; the rest changed away, deleted or moved
    let (last, _) = mentions(&r);
    let mut got: Vec<&str> = last
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["block"].as_str().unwrap())
        .collect();
    got.sort();
    let mut expected = vec![added.as_str(), w.created["$m3"].as_str()];
    expected.sort();
    assert_eq!(got, expected, "{last}");
    assert!(r.read().lexicon_built(&scope));
}
