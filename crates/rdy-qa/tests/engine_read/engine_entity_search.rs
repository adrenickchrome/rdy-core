//! fsh1, esrch1: a literal query that names an entity (key, name or alias) returns its fact
//! sheet; text matches the sheet already holds drop out, and the rest come back as unlinked
//! mentions with a suggestion to link them (FW-EN1).
use rdy_engine::factsheet::FactSheet;
use rdy_engine::lexicon::NameKind;
use rdy_engine::search::SearchResult;
use rdy_engine::suggest::{Suggestion, suggestion_id};
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

struct Ops {
    s: String,
    e: String,
    v: String,
    c1: String,
    r1: String,
    p1: String,
    m: String,
    d1: String,
    d2: String,
}

/// Doc `ops`, section Summary: entity Ledger service (key SVC-LEDGER, alias ledger, kind
/// service), value Request timeout = 5 ms; decision D1 affects the entity, decision D2 has no
/// links; claim c1 describes the entity; requirement r1 references both and is decided by D2;
/// prose p1 names the ledger without a link; component Billing depends on the entity. Landed.
fn ops(r: &mut Rdy) -> Ops {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "body": "Keeps the books.", "fields": {"name": "Ledger service", "kind": "service", "aliases": ["ledger"], "key": "SVC-LEDGER"}},
        {"op": "create", "type": "value", "id": "$v", "parent": "$s", "fields": {"name": "Request timeout", "value": "5", "unit": "ms"}}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    let (s, e, v) = (
        w.created["$s"].clone(),
        w.created["$e"].clone(),
        w.created["$v"].clone(),
    );
    let d1 = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: s.clone(),
                statement: "The ledger is the source of truth".into(),
                affects: vec![e.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let d2 = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: s.clone(),
                statement: "Writes are synchronous".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(r, "work").unwrap();
    let w = write(r, "work", json!([
        {"op": "create", "type": "claim", "id": "$c1", "parent": s, "body": "The ledger is append-only.", "links": {"describes": [{"target": e}]}},
        {"op": "create", "type": "requirement", "id": "$r1", "parent": s, "body": format!("Writes to {{{{{e}}}}} finish within {{{{{v}}}}}."), "links": {"decided_by": [{"target": d2}]}},
        {"op": "create", "type": "prose", "id": "$p1", "parent": s, "body": "Ops runbook: restart the ledger nightly."},
        {"op": "create", "type": "component", "id": "$m", "parent": s, "fields": {"name": "Billing"}, "links": {"depends_on": [{"target": e}]}}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    let c = |k: &str| w.created[k].clone();
    Ops {
        s,
        e,
        v,
        c1: c("$c1"),
        r1: c("$r1"),
        p1: c("$p1"),
        m: c("$m"),
        d1,
        d2,
    }
}

fn find(r: &Rdy, text: &str, regex: bool, page: Option<&str>, max: usize) -> SearchResult {
    r.read()
        .search(
            &Scope::Canon,
            &SearchQuery {
                text: text.into(),
                regex,
                ..Default::default()
            },
            page,
            Some(max),
        )
        .unwrap()
}

fn sheets(r: &Rdy, text: &str) -> Vec<FactSheet> {
    find(r, text, false, None, 100_000).entities
}

fn sorted(mut v: Vec<(String, String)>) -> Vec<(String, String)> {
    v.sort();
    v
}

#[test]
fn fsh1_a_name_returns_the_entity_with_its_facts_decisions_and_dependents() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let all = sheets(&r, "Ledger Service");
    assert_eq!(all.len(), 1);
    let s = &all[0];
    assert_eq!(
        (s.id.as_str(), s.block_type.as_str(), s.matched_by),
        (o.e.as_str(), "entity", NameKind::Name)
    );
    assert_eq!(
        (s.name.as_str(), s.key.as_deref(), s.kind.as_deref()),
        ("Ledger service", Some("SVC-LEDGER"), Some("service"))
    );
    assert_eq!(s.aliases, vec!["ledger"]);
    assert_eq!(
        (s.definition.as_str(), s.path.as_deref(), s.value.as_deref()),
        ("Keeps the books.", Some("ops"), None)
    );
    assert_eq!(
        sorted(
            s.facts
                .iter()
                .map(|f| (f.id.clone(), f.via.clone()))
                .collect()
        ),
        sorted(vec![
            (o.c1.clone(), "describes".into()),
            (o.r1.clone(), "references".into())
        ])
    );
    assert_eq!(
        sorted(
            s.decisions
                .iter()
                .map(|d| (d.id.clone(), d.via.clone()))
                .collect()
        ),
        sorted(vec![
            (o.d1.clone(), "affects".into()),
            (o.d2.clone(), "governs".into())
        ])
    );
    let d2 = s.decisions.iter().find(|d| d.id == o.d2).unwrap();
    assert_eq!(
        (d2.on.clone(), d2.status.as_str(), d2.statement.as_str()),
        (vec![o.r1.clone()], "live", "Writes are synchronous")
    );
    assert_eq!(
        s.dependents
            .iter()
            .map(|d| (d.id.as_str(), d.via.as_str()))
            .collect::<Vec<_>>(),
        vec![(o.m.as_str(), "depends_on")]
    );
    assert!(s.also_described_in.is_empty());
    assert!(s.omitted.is_empty());
}

#[test]
fn fsh1_a_value_sheet_carries_the_current_value() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let all = sheets(&r, "request timeout");
    let s = &all[0];
    assert_eq!(
        (s.id.as_str(), s.value.as_deref(), s.matched_by),
        (o.v.as_str(), Some("5 ms"), NameKind::Name)
    );
    assert_eq!(
        s.facts.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        vec![o.r1.as_str()]
    );
    assert!(
        s.facts[0].summary.contains("within 5 ms"),
        "{}",
        s.facts[0].summary
    );
}

#[test]
fn fsh1_a_key_and_an_alias_resolve_but_a_claim_key_does_not() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    assert_eq!(sheets(&r, "svc-ledger")[0].matched_by, NameKind::Key);
    assert_eq!(sheets(&r, "LEDGER")[0].matched_by, NameKind::Alias);
    write(&mut r, "work", json!([{"op": "create", "type": "claim", "parent": o.s, "body": "Keyed.", "fields": {"key": "REQ-9"}}])).unwrap();
    land(&mut r, "work").unwrap();
    assert!(sheets(&r, "REQ-9").is_empty());
    let e = r
        .read()
        .fact_sheet(
            &Scope::Canon,
            &rdy_model::BlockId::parse(&o.c1).unwrap(),
            NameKind::Name,
        )
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
}

#[test]
fn esrch1_text_matches_the_sheet_holds_drop_out_and_the_rest_carry_a_link_suggestion() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let res = find(&r, "ledger", false, None, 100_000);
    assert_eq!(res.entities.len(), 1);
    assert_eq!(res.total, 1, "{:?}", res.matches);
    assert_eq!(res.matches[0].id, o.p1);
    assert_eq!(
        res.matches[0].suggestions,
        vec![Suggestion::Link {
            id: suggestion_id("describes", &o.p1, &o.e),
            block: o.p1.clone(),
            link: "describes".into(),
            target: o.e.clone(),
            mention: "ledger".into(),
            field: "body".into(),
        }]
    );
    // a regex never resolves entities: every text match comes back, with no suggestion
    let rx = find(&r, "ledger", true, None, 100_000);
    assert!(rx.entities.is_empty());
    let mut ids: Vec<String> = rx.matches.iter().map(|m| m.id.clone()).collect();
    ids.sort();
    let mut want = vec![o.e.clone(), o.c1.clone(), o.p1.clone(), o.d1.clone()];
    want.sort();
    assert_eq!(ids, want);
    assert!(rx.matches.iter().all(|m| m.suggestions.is_empty()));
    let v: Value = serde_json::to_value(&rx).unwrap();
    assert!(
        v.get("entities").is_none() && v["matches"][0].get("suggestions").is_none(),
        "{v}"
    );
}

#[test]
fn esrch1_a_shared_alias_resolves_to_both_entities() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "entity", "id": "$x", "parent": o.s, "fields": {"name": "General ledger", "aliases": ["ledger"]}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let x = w.created["$x"].clone();
    let res = find(&r, "ledger", false, None, 100_000);
    let mut got: Vec<String> = res.entities.iter().map(|s| s.id.clone()).collect();
    got.sort();
    let mut want = vec![o.e.clone(), x.clone()];
    want.sort();
    assert_eq!(got, want);
    assert!(res.entities.iter().all(|s| s.matched_by == NameKind::Alias));
    let p = res.matches.iter().find(|m| m.id == o.p1).unwrap();
    let targets: Vec<&str> = p
        .suggestions
        .iter()
        .map(|s| match s {
            Suggestion::Link { target, .. } => target.as_str(),
            Suggestion::Entity { .. } => panic!("a search suggests links only"),
        })
        .collect();
    assert_eq!(
        targets,
        res.entities
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn esrch1_entities_come_on_the_first_page_only_and_paging_covers_every_mention() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let more: Vec<Value> = (0..30)
        .map(|i| json!({"op": "create", "type": "prose", "parent": o.s, "body": format!("Restart the ledger, step {i}.")}))
        .collect();
    write(&mut r, "work", Value::Array(more)).unwrap();
    land(&mut r, "work").unwrap();
    let first = find(&r, "ledger", false, None, 600);
    assert_eq!(first.entities.len(), 1);
    assert!(
        first.truncated && first.next.is_some(),
        "{} matches on the first page",
        first.matches.len()
    );
    assert_eq!(first.total, 31);
    let mut seen: Vec<String> = first.matches.iter().map(|m| m.id.clone()).collect();
    let mut next = first.next.clone();
    let mut pages = 1;
    while let Some(cursor) = next {
        let p = find(&r, "ledger", false, Some(&cursor), 600);
        assert!(p.entities.is_empty(), "sheets come on the first page only");
        seen.extend(p.matches.iter().map(|m| m.id.clone()));
        next = p.next.clone();
        pages += 1;
        assert!(pages < 100, "paging does not end");
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 31);
}

#[test]
fn fsh1_informative_blocks_come_after_the_facts_as_also_described_in() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "prose", "id": "$p2", "parent": o.s, "body": "Background reading.", "links": {"describes": [{"target": o.e}]}},
        {"op": "create", "type": "note", "id": "$n", "parent": o.s, "body": format!("See {{{{{}}}}}.", o.e)}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let (p2, n) = (w.created["$p2"].clone(), w.created["$n"].clone());
    let s = &sheets(&r, "Ledger service")[0];
    assert_eq!(
        sorted(
            s.also_described_in
                .iter()
                .map(|b| (b.id.clone(), b.via.clone()))
                .collect()
        ),
        sorted(vec![
            (p2.clone(), "describes".into()),
            (n.clone(), "references".into())
        ])
    );
    assert!(
        s.facts.iter().all(|f| f.id != p2 && f.id != n),
        "informative blocks are not facts"
    );
    assert!(
        s.dependents.iter().all(|d| d.id != p2 && d.id != n),
        "nor dependents"
    );
    assert_eq!(
        sorted(
            s.facts
                .iter()
                .map(|f| (f.id.clone(), f.via.clone()))
                .collect()
        ),
        sorted(vec![
            (o.c1.clone(), "describes".into()),
            (o.r1.clone(), "references".into())
        ]),
        "guard: the normative facts stay"
    );
}

#[test]
fn fsh1_proposed_decisions_are_listed_and_labelled_proposed() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let p1 = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: o.s.clone(),
                statement: "The ledger is sharded".into(),
                affects: vec![o.e.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let p2 = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: o.s.clone(),
                statement: "Shards rebalance weekly".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "id": "$c3", "parent": o.s, "body": "Each shard holds a range.", "links": {"describes": [{"target": o.e}], "decided_by": [{"target": p2}]}}
    ]))
    .unwrap();
    let c3 = w.created["$c3"].clone();
    let res = r
        .read()
        .search(
            &Scope::View("work".into()),
            &SearchQuery {
                text: "Ledger service".into(),
                ..Default::default()
            },
            None,
            Some(100_000),
        )
        .unwrap();
    let s = &res.entities[0];
    let got = |id: &str| {
        s.decisions
            .iter()
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("{id} not on the sheet: {:?}", s.decisions))
            .clone()
    };
    let (a, b) = (got(&p1), got(&p2));
    assert_eq!((a.status.as_str(), a.via.as_str()), ("proposed", "affects"));
    assert_eq!(
        (b.status.as_str(), b.via.as_str(), b.on.clone()),
        ("proposed", "governs", vec![c3])
    );
    assert_eq!(got(&o.d1).status, "live", "guard: live decisions stay");
    let canon = sheets(&r, "Ledger service");
    assert!(
        canon[0].decisions.iter().all(|d| d.id != p1 && d.id != p2),
        "canon does not hold the view's proposals"
    );
}

#[test]
fn esrch1_an_all_words_hit_carries_no_suggestion() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "prose", "id": "$q", "parent": o.s, "body": "Ledger books need a service window."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let q = w.created["$q"].clone();
    let res = find(&r, "Ledger service", false, None, 100_000);
    assert_eq!(res.entities.len(), 1);
    let m = res
        .matches
        .iter()
        .find(|m| m.id == q)
        .expect("an all-words hit");
    assert!(m.all_words && m.suggestions.is_empty(), "{m:?}");
}

#[test]
fn esrch1_a_collapsed_hit_keeps_its_own_suggestion() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "prose", "id": "$q", "parent": o.s, "body": "Ops runbook: restart the ledger nightly."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let q = w.created["$q"].clone();
    let res = find(&r, "ledger", false, None, 100_000);
    assert_eq!(res.total, 2, "{:?}", res.matches);
    let m = res.matches.iter().find(|m| m.id == o.p1).unwrap();
    let a = m
        .also
        .iter()
        .find(|a| a.id == q)
        .expect("identical lines collapse");
    assert_eq!(
        a.suggestions,
        vec![Suggestion::Link {
            id: suggestion_id("describes", &q, &o.e),
            block: q.clone(),
            link: "describes".into(),
            target: o.e.clone(),
            mention: "ledger".into(),
            field: "body".into(),
        }]
    );
}

// Fix round 1: a hit drops out only when a returned sheet lists it (the sheet's entity, or a
// block tied to it that a list holds inside its cap); a filtered search keeps its tied hits; a
// link suggestion needs the entity's name or alias as whole words in the block's mention text.

fn link_targets(m: &rdy_engine::search::SearchMatch) -> Vec<String> {
    m.suggestions
        .iter()
        .map(|s| match s {
            Suggestion::Link { target, .. } => target.clone(),
            Suggestion::Entity { .. } => panic!("a search suggests links only"),
        })
        .collect()
}

#[test]
fn esrch1_a_decision_the_sheet_does_not_list_stays_a_match() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let w = rdy_qa::golden::write_as(&approver(), &mut r, "work", json!([
        {"op": "create", "type": "decision", "id": "$x", "parent": o.s,
         "fields": {"statement": "The ledger may be eventually consistent", "status": "rejected",
                    "decided_at": "2021-01-10T09:00:00Z", "recorded_at": "2021-01-10T09:00:00Z", "options": ["yes", "no"]},
         "links": {"affects": [{"target": o.e}]}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let x = w.created["$x"].clone();
    let res = find(&r, "ledger", false, None, 100_000);
    assert!(
        res.entities[0].decisions.iter().all(|d| d.id != x),
        "a rejected decision is not listed"
    );
    let m = res
        .matches
        .iter()
        .find(|m| m.id == x)
        .unwrap_or_else(|| panic!("{:?}", res.matches));
    assert!(m.history && m.suggestions.is_empty(), "{m:?}");
    assert_eq!(
        res.total, 2,
        "p1 and the rejected decision: {:?}",
        res.matches
    );
}

#[test]
fn esrch1_entities_past_max_resolved_and_their_blocks_stay_matches() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let more: Vec<Value> = (0..9)
        .map(|i| json!({"op": "create", "type": "entity", "parent": o.s, "fields": {"name": format!("Ledger {i}"), "aliases": ["ledger"]}}))
        .collect();
    let w = write(&mut r, "work", Value::Array(more)).unwrap();
    land(&mut r, "work").unwrap();
    let mut all: Vec<String> = w.created.values().cloned().collect();
    all.push(o.e.clone());
    let res = find(&r, "ledger", false, None, 100_000);
    assert_eq!((res.entities.len(), res.entities_omitted), (8, 2));
    let unsheeted: Vec<String> = all
        .iter()
        .filter(|id| res.entities.iter().all(|s| s.id != **id))
        .cloned()
        .collect();
    assert_eq!(unsheeted.len(), 2);
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "id": "$c", "parent": o.s, "body": "The ledger posts here.", "links": {"describes": [{"target": unsheeted[0]}]}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let c = w.created["$c"].clone();
    let res = find(&r, "ledger", false, None, 100_000);
    // the two entities' alias lines are identical, so one may sit in the other's `also`
    let ids: Vec<&str> = res
        .matches
        .iter()
        .flat_map(|m| std::iter::once(m.id.as_str()).chain(m.also.iter().map(|a| a.id.as_str())))
        .collect();
    for want in [&c, &unsheeted[0], &unsheeted[1]] {
        assert!(
            ids.contains(&want.as_str()),
            "{want} is on no sheet, so it stays a match: {ids:?}"
        );
    }
    let cm = res.matches.iter().find(|m| m.id == c).unwrap();
    let targets = link_targets(cm);
    assert_eq!(
        targets.len(),
        9,
        "every resolved entity but the one it describes: {targets:?}"
    );
    assert!(!targets.contains(&unsheeted[0]));
}

#[test]
fn esrch1_blocks_past_a_sheet_cap_stay_matches() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let more: Vec<Value> = (0..25)
        .map(|i| json!({"op": "create", "type": "claim", "parent": o.s, "body": format!("The ledger keeps entry {i}."), "links": {"describes": [{"target": o.e}]}}))
        .collect();
    let w = write(&mut r, "work", Value::Array(more)).unwrap();
    land(&mut r, "work").unwrap();
    let mut claims: Vec<String> = w.created.values().cloned().collect();
    claims.push(o.c1.clone());
    let res = find(&r, "ledger", false, None, 100_000);
    let s = &res.entities[0];
    assert_eq!(
        (s.facts.len(), s.omitted.get("facts").copied()),
        (20, Some(7))
    );
    let mut cut: Vec<String> = claims
        .iter()
        .filter(|c| s.facts.iter().all(|f| f.id != **c))
        .cloned()
        .collect();
    cut.sort();
    assert!(!cut.is_empty());
    let mut got: Vec<String> = res
        .matches
        .iter()
        .map(|m| m.id.clone())
        .filter(|id| *id != o.p1)
        .collect();
    got.sort();
    assert_eq!(got, cut, "the claims the cap cut stay matches");
    assert_eq!(res.total, cut.len() + 1);
    assert!(
        res.matches
            .iter()
            .filter(|m| m.id != o.p1)
            .all(|m| m.suggestions.is_empty()),
        "tied blocks get no suggestion"
    );
}

#[test]
fn esrch1_a_filtered_search_keeps_tied_blocks_and_the_sheet() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let q = SearchQuery {
        text: "ledger".into(),
        filters: rdy_engine::search::SearchFilters {
            types: vec!["claim".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let res = r
        .read()
        .search(&Scope::Canon, &q, None, Some(100_000))
        .unwrap();
    assert_eq!(res.entities.len(), 1, "the sheet still comes first");
    assert_eq!(
        res.matches
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec![o.c1.as_str()]
    );
    assert!(
        res.matches[0].suggestions.is_empty(),
        "c1 describes the entity already"
    );
}

#[test]
fn esrch1_a_suggestion_needs_the_name_as_whole_words() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "prose", "id": "$sub", "parent": o.s, "body": "The subledger reconciles nightly."},
        {"op": "create", "type": "code", "id": "$code", "parent": o.s, "body": "ledger_client.connect()", "fields": {"lang": "py"}},
        {"op": "create", "type": "note", "id": "$note", "parent": o.s, "body": "Notes.", "fields": {"key": "LEDGER-NOTES"}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let id = |k: &str| w.created[k].clone();
    let q = SearchQuery {
        text: "ledger".into(),
        ignore_case: true,
        ..Default::default()
    };
    let res = r
        .read()
        .search(&Scope::Canon, &q, None, Some(100_000))
        .unwrap();
    let all: Vec<&rdy_engine::search::SearchMatch> = res.matches.iter().collect();
    let get = |k: &str| {
        *all.iter()
            .find(|m| m.id == id(k))
            .unwrap_or_else(|| panic!("{k} is a hit: {all:?}"))
    };
    for k in ["$sub", "$code", "$note"] {
        assert!(
            get(k).suggestions.is_empty(),
            "{k}: {:?}",
            get(k).suggestions
        );
    }
    let p1 = res.matches.iter().find(|m| m.id == o.p1).unwrap();
    assert_eq!(
        link_targets(p1),
        vec![o.e.clone()],
        "guard: a whole-word mention keeps its suggestion"
    );
}

/// Final review fix round 1: a literal that names an entity reads text the way resolution does,
/// in any case and spacing, so every unlinked mention comes back with its suggestion.
#[test]
fn esrch1_a_name_query_lists_every_unlinked_mention_in_any_case() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "prose", "id": "$p", "parent": o.s, "body": "The ledger service is restarted nightly."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let p = w.created["$p"].clone();
    for q in ["Ledger service", "  LEDGER   SERVICE "] {
        let res = find(&r, q, false, None, 100_000);
        assert_eq!(res.entities.len(), 1, "{q}");
        let m = res.matches.iter().find(|m| m.id == p).unwrap_or_else(|| {
            panic!("{q:?}: the lowercase mention is a match: {:?}", res.matches)
        });
        assert!(!m.all_words, "{q:?}: a phrase hit");
        assert_eq!(link_targets(m), vec![o.e.clone()], "{q:?}");
        assert!(
            res.ignore_case,
            "{q:?}: the result says the text pass ignored case"
        );
    }
    // a query that names no entity keeps its case
    let res = find(&r, "Restarted", false, None, 100_000);
    assert!(res.matches.is_empty() && !res.ignore_case);
}

/// Final review fix round 1: a sheet's definition reads references the way its summaries do.
#[test]
fn fsh1_the_definition_reads_references_as_their_summaries() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    write(&mut r, "work", json!([
        {"op": "update", "id": o.e, "body": format!("Keeps the books.\nAnswers within {{{{{}}}}}.", o.v)}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    assert_eq!(
        sheets(&r, "ledger service")[0].definition,
        "Keeps the books.\nAnswers within 5 ms."
    );
}

#[test]
fn msw18_a_long_definition_is_cut_and_counted() {
    let (_d, mut r) = project();
    let o = ops(&mut r);
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": o.e, "body": "word ".repeat(20_000)}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let s = &sheets(&r, "ledger service")[0];
    assert!(
        s.definition.chars().count() <= 1_001,
        "{}",
        s.definition.chars().count()
    );
    assert!(s.omitted.contains_key("definition"), "{:?}", s.omitted);
}

#[test]
fn msw18_a_long_on_list_is_cut_and_counted() {
    use rdy_engine::factsheet::SHEET_LIST_CAP;
    let (_d, mut r) = project();
    let o = ops(&mut r);
    // 25 claims describe the entity and are decided by D2: D2 governs them and r1 (26 blocks)
    let claims: Vec<Value> = (0..25)
        .map(|i| {
            json!({"op": "create", "type": "claim", "id": format!("$k{i}"), "parent": o.s,
            "body": format!("Rule {i} holds."),
            "links": {"describes": [{"target": o.e}], "decided_by": [{"target": o.d2}]}})
        })
        .collect();
    write(&mut r, "work", Value::Array(claims)).unwrap();
    land(&mut r, "work").unwrap();
    let s = &sheets(&r, "ledger service")[0];
    let d2 = s.decisions.iter().find(|d| d.id == o.d2).unwrap();
    assert_eq!(d2.on.len(), SHEET_LIST_CAP, "{:?}", d2.on);
    assert_eq!(s.omitted.get("on"), Some(&6), "{:?}", s.omitted);
}
