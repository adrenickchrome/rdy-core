//! rf9 (D-4WC6R, D-Z8P09, TASK-507 (3)): the two approval-card facts the supporting types add. A
//! superseding decision's exception card lists the findings it depends on directly (Findings),
//! and every proposal's batch card gives the claim of its changes' justifications
//! (Justification, after Changes). Reflects values are typed store commits (the normalized model
//! variant), not raw input.
mod common;

use rdy_engine::pending::{Item, ItemKind, PendingQuery};
use rdy_engine::{
    DecisionInput, ProposalInfo, Rdy, Scope, Session, WriteRequest, WriteResult, parse_ops,
};
use rdy_model::{BlockId, Change, EditorialClass, Hash, Justification, Op, Provenance, Role};
use serde_json::{Value, json};

fn writer() -> Session {
    Session::human("human:qa".into(), Role::Writer)
}

fn approver() -> Session {
    Session::human("human:qa".into(), Role::Approver)
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn view(v: &str) -> Scope {
    Scope::View(v.into())
}

fn put(r: &mut Rdy, v: &str, ops: Value, j: Option<Justification>) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: v.into(),
            ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: j,
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("write to {v}: {e}"))
}

fn land(r: &mut Rdy, v: &str) {
    let p = r
        .propose(&writer(), v, v, Some(common::TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {v}: {e}"));
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("land {v}: {e}"));
}

fn decide(r: &mut Rdy, v: &str, d: DecisionInput) -> BlockId {
    let w = r
        .decide(&writer(), v, d)
        .unwrap_or_else(|e| panic!("decide in {v}: {e}"));
    id(&w.created["$decision"])
}

fn supersede(r: &mut Rdy, v: &str, old: &BlockId, d: DecisionInput) -> BlockId {
    let w = r
        .supersede(&writer(), v, &old.to_string(), None, d)
        .unwrap_or_else(|e| panic!("supersede {old} in {v}: {e}"));
    id(&w.created["$decision"])
}

/// A typed store commit of block `b` in `v`, rewritten by `edit`, justified by `j`.
fn commit(
    r: &mut Rdy,
    v: &str,
    b: &BlockId,
    j: Option<Justification>,
    edit: impl FnOnce(&mut rdy_model::Block),
) -> Hash {
    let mut next = r.read().block(&view(v), b).unwrap();
    let base = next.version_hash();
    edit(&mut next);
    let deps = r.read().included(v).to_vec();
    let h = r
        .store_mut()
        .commit_to_view(
            v,
            Change {
                view: v.into(),
                ops: vec![Op::Update { base, block: next }],
                deps,
                provenance: Provenance {
                    actor: "human:qa".into(),
                    run: None,
                    role: Role::Writer,
                    at_ms: 5,
                },
                justification: j,
                message: String::new(),
                links: Default::default(),
                read_identity: None,
            },
        )
        .unwrap();
    r.invalidate();
    h
}

/// The queue of the approver.
fn queue(r: &Rdy) -> Vec<Item> {
    r.read()
        .pending(&PendingQuery {
            person: approver().actor,
            session_ref: None,
            max_tokens: rdy_engine::pending::CARD_MAX_TOKENS,
        })
        .expect("pending")
        .items
}

fn facts(item: &Item) -> Vec<(String, String)> {
    item.facts
        .iter()
        .map(|f| (f.name.to_string(), f.value.clone()))
        .collect()
}

fn fact<'a>(item: &'a Item, name: &str) -> Option<&'a str> {
    item.facts
        .iter()
        .find(|f| f.name == name)
        .map(|f| f.value.as_str())
}

/// The exception card of decision `d` beside proposal `p`.
fn exception(r: &Rdy, p: &ProposalInfo, d: &BlockId) -> Item {
    queue(r)
        .into_iter()
        .find(|i| {
            i.kind == ItemKind::Confirm
                && i.batch.as_deref() == Some(p.r#ref.as_str())
                && i.r#ref.starts_with(&format!("{d}@"))
        })
        .unwrap_or_else(|| panic!("no exception card for {d}"))
}

fn batch(r: &Rdy, p: &ProposalInfo) -> Item {
    queue(r)
        .into_iter()
        .find(|i| i.kind == ItemKind::Approve && i.r#ref == p.r#ref)
        .unwrap_or_else(|| panic!("no batch card {}", p.r#ref))
}

#[test]
fn rf9_superseding_card_carries_findings() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C holds."},
            {"op": "create", "type": "finding", "id": "$fa", "parent": "$s", "fields": {"statement": "Alpha holds.", "evidence": [{"quote": "qa"}]}},
            {"op": "create", "type": "finding", "id": "$fb", "parent": "$s", "fields": {"statement": "Beta holds.", "evidence": [{"quote": "qb"}]}},
            {"op": "create", "type": "finding", "id": "$fc", "parent": "$s", "fields": {"statement": "Gamma holds.", "evidence": [{"quote": "qc"}]}}
        ]),
        None,
    );
    let g = |k: &str| id(&w.created[k]);
    let (sec, c, fa, fb, fc) = (g("$s"), g("$c"), g("$fa"), g("$fb"), g("$fc"));
    let stmt = |s: &str| DecisionInput {
        parent: sec.to_string(),
        statement: s.into(),
        affects: vec![c.to_string()],
        ..Default::default()
    };
    let (d0, d0b, d0c) = (
        decide(&mut r, "seed", stmt("Old one")),
        decide(&mut r, "seed", stmt("Old two")),
        decide(&mut r, "seed", stmt("Old three")),
    );
    land(&mut r, "seed");

    // d1 supersedes d0 and depends on the findings (stored beta then alpha), conditional on
    // gamma and on d0b (a decision, not a finding); d2 supersedes d0b and depends on none; d3 is
    // a one_way decision of its own; d4 only revokes d0c
    let d1 = supersede(
        &mut r,
        "w",
        &d0,
        DecisionInput {
            depends_on: vec![fb.to_string(), fa.to_string(), d0b.to_string()],
            conditional_on: vec![fc.to_string()],
            ..stmt("New one")
        },
    );
    let d2 = supersede(&mut r, "w", &d0b, stmt("New two"));
    let d3 = decide(
        &mut r,
        "w",
        DecisionInput {
            reversibility: Some("one_way".into()),
            depends_on: vec![fa.to_string()],
            ..stmt("Never undo")
        },
    );
    let d4 = {
        let x = r
            .revoke(
                &writer(),
                "w",
                &d0c.to_string(),
                "Gone.",
                &sec.to_string(),
                None,
            )
            .unwrap();
        id(&x.created["$decision"])
    };
    let p = r
        .propose(&writer(), "w", "w", Some(common::TEST_SUMMARY))
        .unwrap();

    let mut ordered = [fa.to_string(), fb.to_string()];
    ordered.sort();
    let text = |f: &str| match f {
        f if f == fa.to_string() => "Alpha holds.",
        _ => "Beta holds.",
    };
    let want = format!(
        "{}: {}; {}: {}",
        ordered[0],
        text(&ordered[0]),
        ordered[1],
        text(&ordered[1])
    );
    let card1 = exception(&r, &p, &d1);
    // after the existing exception-kind facts, as the last one
    assert_eq!(
        card1.facts.last().map(|f| f.name.as_ref()),
        Some("Findings")
    );
    assert_eq!(
        fact(&card1, "Findings"),
        Some(want.as_str()),
        "{:?}",
        facts(&card1)
    );
    assert!(card1.facts.len() >= 2, "{:?}", facts(&card1));
    assert_eq!(card1.facts[0].name, "Goes against");
    // no finding, none
    let card2 = exception(&r, &p, &d2);
    assert_eq!(
        fact(&card2, "Findings"),
        Some("none"),
        "{:?}",
        facts(&card2)
    );
    // a one_way card of an unrelated decision and a revocation-only card carry no Findings
    for d in [&d3, &d4] {
        let card = exception(&r, &p, d);
        assert!(fact(&card, "Findings").is_none(), "{d}: {:?}", facts(&card));
    }
    // the card text shows the fact, and the hash follows the displayed statement
    assert!(card1.card.contains("Alpha holds."), "{}", card1.card);
    assert_eq!(
        card1.card_hash,
        rdy_engine::pending::card_hash(&card1).to_string()
    );

    // a displayed statement changes: the card and its hash change
    put(
        &mut r,
        "w",
        json!([{"op": "update", "id": fa.to_string(), "fields": {"statement": "Alpha holds, newly."}}]),
        None,
    );
    let p2 = r
        .propose(&writer(), "w", "w", Some(common::TEST_SUMMARY))
        .unwrap();
    let again = exception(&r, &p2, &d1);
    assert_ne!(again.card_hash, card1.card_hash);
    assert!(
        fact(&again, "Findings").is_some_and(|f| f.contains("Alpha holds, newly.")),
        "{:?}",
        facts(&again)
    );
    // the same facts give the same hash
    let twice = exception(&r, &p2, &d1);
    assert_eq!(twice.card_hash, again.card_hash);
}

/// Only the displayed Findings value differs: the item identity, batch reference and every other
/// fact are held fixed, and both the rendered card and its hash follow that one value.
#[test]
fn rf9_card_hash_follows_the_findings_value_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C holds."},
            {"op": "create", "type": "finding", "id": "$f", "parent": "$s", "fields": {"statement": "Alpha holds.", "evidence": [{"quote": "qa"}]}}
        ]),
        None,
    );
    let (s, c, f) = (
        id(&w.created["$s"]),
        id(&w.created["$c"]),
        id(&w.created["$f"]),
    );
    let d0 = decide(
        &mut r,
        "seed",
        DecisionInput {
            parent: s.to_string(),
            statement: "Old".into(),
            affects: vec![c.to_string()],
            ..Default::default()
        },
    );
    land(&mut r, "seed");
    let d1 = supersede(
        &mut r,
        "w",
        &d0,
        DecisionInput {
            parent: s.to_string(),
            statement: "New".into(),
            affects: vec![c.to_string()],
            depends_on: vec![f.to_string()],
            ..Default::default()
        },
    );
    let p = r
        .propose(&writer(), "w", "w", Some(common::TEST_SUMMARY))
        .unwrap();
    let base = exception(&r, &p, &d1);
    let shown = fact(&base, "Findings").unwrap().to_string();
    assert!(shown.contains("Alpha holds."), "{shown}");
    // the same item with one changed Findings value
    let mut changed = base.clone();
    for fa in changed.facts.iter_mut().filter(|fa| fa.name == "Findings") {
        fa.value = shown.replace("Alpha holds.", "Alpha holds, newly.");
    }
    assert_ne!(fact(&changed, "Findings"), fact(&base, "Findings"));
    assert_eq!(changed.batch, base.batch);
    assert_eq!(changed.r#ref, base.r#ref);
    let (before, after) = (
        rdy_engine::pending::render(&base, rdy_engine::pending::CARD_MAX_TOKENS),
        rdy_engine::pending::render(&changed, rdy_engine::pending::CARD_MAX_TOKENS),
    );
    assert_ne!(
        before, after,
        "the rendered text follows the Findings value"
    );
    assert!(after.contains("Alpha holds, newly."), "{after}");
    assert_ne!(
        rdy_engine::pending::card_hash(&base),
        rdy_engine::pending::card_hash(&changed),
        "the hash follows the Findings value"
    );
    // restoring the value restores both
    let mut same = changed.clone();
    for fa in same.facts.iter_mut().filter(|fa| fa.name == "Findings") {
        fa.value = shown.clone();
    }
    assert_eq!(
        rdy_engine::pending::card_hash(&same),
        rdy_engine::pending::card_hash(&base)
    );
}

/// Canon: claims a to f, sources s1 "Release 1" and s2 "Spec 2", and a live decision dx.
struct Fx {
    claims: Vec<BlockId>,
    s1: BlockId,
    s2: BlockId,
    dx: BlockId,
}

fn fixture(r: &mut Rdy) -> Fx {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}}),
        json!({"op": "create", "type": "source", "id": "$s1", "parent": "$s", "fields": {"title": "Release 1", "kind": "release", "locator": "v1"}, "body": "One."}),
        json!({"op": "create", "type": "source", "id": "$s2", "parent": "$s", "fields": {"title": "Spec 2", "kind": "spec", "locator": "v2"}, "body": "Two."}),
    ];
    for i in 0..6 {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s", "body": format!("Claim {i} holds.")}));
    }
    let w = put(r, "seed", Value::Array(ops), None);
    let sec = id(&w.created["$s"]);
    let dx = decide(
        r,
        "seed",
        DecisionInput {
            parent: sec.to_string(),
            statement: "Claims hold".into(),
            ..Default::default()
        },
    );
    land(r, "seed");
    Fx {
        claims: (0..6).map(|i| id(&w.created[&format!("$c{i}")])).collect(),
        s1: id(&w.created["$s1"]),
        s2: id(&w.created["$s2"]),
        dx,
    }
}

fn edit(r: &mut Rdy, v: &str, b: &BlockId, j: Option<Justification>) {
    commit(r, v, b, j, |blk| blk.body = format!("{} Edited.", blk.body));
}

fn justification_fact(r: &mut Rdy, view_name: &str) -> (Vec<(String, String)>, String) {
    let p = r
        .propose(&writer(), view_name, view_name, Some(common::TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view_name}: {e}"));
    let card = batch(r, &p);
    let value = fact(&card, "Justification")
        .unwrap_or_else(|| panic!("no Justification fact: {:?}", facts(&card)))
        .to_string();
    (facts(&card), value)
}

#[test]
fn rf9_justification_fact_names_all_three_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let fx = fixture(&mut r);
    let [a, b, c, d, e, f] = &fx.claims[..] else {
        unreachable!()
    };
    let both = vec![fx.s2.clone(), fx.s1.clone()];
    let mixed_sources = vec![fx.s1.clone()];

    // changes in stored order: implements, editorial, two-source reflects, code-only reflects,
    // mixed reflects, then a repeat of the editorial claim and of the implements claim
    edit(
        &mut r,
        "j",
        a,
        Some(Justification::Implements(vec![fx.dx.clone()])),
    );
    edit(
        &mut r,
        "j",
        b,
        Some(Justification::Editorial {
            class: EditorialClass::Clarify,
            reason: "Reworded".into(),
        }),
    );
    edit(
        &mut r,
        "j",
        c,
        Some(Justification::Reflects {
            sources: both.clone(),
            code: false,
        }),
    );
    edit(
        &mut r,
        "j",
        d,
        Some(Justification::Reflects {
            sources: vec![],
            code: true,
        }),
    );
    edit(
        &mut r,
        "j",
        e,
        Some(Justification::Reflects {
            sources: mixed_sources,
            code: true,
        }),
    );
    edit(
        &mut r,
        "j",
        f,
        Some(Justification::Editorial {
            class: EditorialClass::Clarify,
            reason: "Reworded".into(),
        }),
    );
    edit(
        &mut r,
        "j",
        a,
        Some(Justification::Implements(vec![fx.dx.clone()])),
    );

    let mut titled = [
        (fx.s1.to_string(), "Release 1"),
        (fx.s2.to_string(), "Spec 2"),
    ];
    titled.sort();
    let both_claim = format!("reflects {}, {}", titled[0].1, titled[1].1);
    let want = format!(
        "implements {}; editorial clarify: Reworded; {both_claim}; reflects code; reflects Release 1, code",
        fx.dx
    );
    let (all, value) = justification_fact(&mut r, "j");
    assert_eq!(value, want);
    // after Changes, before Decisions
    let names: Vec<&str> = all.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        &names[..3],
        ["Changes", "Justification", "Decisions"],
        "{names:?}"
    );
}

#[test]
fn rf9_a_proposal_without_justification_says_none() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let fx = fixture(&mut r);
    put(
        &mut r,
        "plain",
        json!([{"op": "update", "id": fx.claims[0].to_string(), "body": "Claim 0, reworded."}]),
        None,
    );
    let (_, value) = justification_fact(&mut r, "plain");
    assert_eq!(value, "none");
}

#[test]
fn rf9_a_reflected_source_with_no_title_is_named_by_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let fx = fixture(&mut r);
    let ghost = id("S-ZZZZZ");
    edit(
        &mut r,
        "g",
        &fx.claims[0],
        Some(Justification::Reflects {
            sources: vec![ghost.clone()],
            code: false,
        }),
    );
    let (_, value) = justification_fact(&mut r, "g");
    assert_eq!(value, format!("reflects {ghost}"));
}
