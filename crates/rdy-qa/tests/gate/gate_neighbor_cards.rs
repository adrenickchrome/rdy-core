//! nb11 (D-YXBXH, D-ME2HD, O10): the decision intake shows what the neighbor work found. The
//! confidence line of an approve card names the words of an unlinked mention, and each card that
//! shows decisions lists their related decisions in one `Related decisions` fact. Both are
//! derived from the version's own state at propose, stored with the card's other facts, and
//! bound by the card's hash. The tests build real proposals and read the queue as a person does.
use crate::gate_check_verdicts::{REASON, meaning_verdict};
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::brief::Coverage;
use rdy_engine::consequence::NEIGHBOR_EDGE;
use rdy_engine::engine::Local;
use rdy_engine::neighbors::neighbors_in;
use rdy_engine::pending::{Item, ItemKind, Pending, PendingQuery, card_hash};
use rdy_engine::plan::PlanRow;
use rdy_engine::{
    DecisionInput, E_ITEM_MOVED, Engine, ProposalInfo, Rdy, ReadState, Scope, Session,
};
use rdy_model::assessment::Assessment;
use rdy_model::plan::PlanKind;
use rdy_model::{BlockId, Hash, Role};
use rdy_qa::fixtures::{land, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, decide_in};
use rdy_qa::tensions::{ag, created, j_typo, put, put_j, upd};
use serde_json::json;
use std::collections::BTreeSet;

/// Canon: draft doc `notes` with section `Ideas`, entity `Ledger service`, term `Store`,
/// component `Merge Conflict`, claim `x`, two claims that describe the entity, and two decisions
/// on the entity: `a` ties to it, `b` names it in its statement.
struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
    s: BlockId,
    e: BlockId,
    t: BlockId,
    x: BlockId,
    cl1: BlockId,
    cl2: BlockId,
    a: BlockId,
    b: BlockId,
}

const A_STATEMENT: &str = "Entries are append-only";
const B_STATEMENT: &str = "The ledger service keeps every entry";

fn world() -> World {
    let (dir, mut r) = rdy_qa::fixtures::project();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger service"}},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Store"}, "body": "Holds data."},
            {"op": "create", "type": "component", "id": "$m", "parent": "$s", "fields": {"name": "Merge Conflict"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."},
            {"op": "create", "type": "claim", "id": "$cl1", "parent": "$s", "body": "Entries stay.",
             "links": {"describes": [{"target": "$e"}]}},
            {"op": "create", "type": "claim", "id": "$cl2", "parent": "$s", "body": "Reads are exact.",
             "links": {"describes": [{"target": "$e"}]}}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let id = |k: &str| BlockId::parse(&w.created[k]).unwrap();
    let (s, e) = (id("$s"), id("$e"));
    let a = decide_in(&mut r, "seed2", decision(&s, A_STATEMENT, &[&e], false));
    let b = decide_in(&mut r, "seed2", decision(&s, B_STATEMENT, &[], false));
    land(&mut r, "seed2").unwrap();
    World {
        t: id("$t"),
        x: id("$x"),
        cl1: id("$cl1"),
        cl2: id("$cl2"),
        s,
        e,
        a,
        b,
        _dir: dir,
        r,
    }
}

/// A decision under `s` with card words; `one_way` gives it a confirm card.
fn decision(s: &BlockId, statement: &str, affects: &[&BlockId], one_way: bool) -> DecisionInput {
    DecisionInput {
        parent: s.to_string(),
        statement: statement.into(),
        title: Some(statement.into()),
        why: Some(format!("{statement}: the person decides it")),
        reversibility: Some(if one_way { "one_way" } else { "two_way" }.into()),
        affects: affects.iter().map(|b| b.to_string()).collect(),
        ..Default::default()
    }
}

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    // the change-plan items of the version are answered first, as a landing does
    rdy_qa::intake::answer_open_items(r, &writer(), view);
    r.propose(&writer(), view, view, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"))
}

fn pending_at(st: &ReadState, max_tokens: usize) -> Pending {
    st.pending(&PendingQuery {
        person: PERSON.into(),
        session_ref: None,
        max_tokens,
    })
    .unwrap()
}

fn items(r: &Rdy) -> Vec<Item> {
    pending_at(&r.read(), rdy_engine::pending::CARD_MAX_TOKENS).items
}

fn approve_item(r: &Rdy, pref: &str) -> Item {
    let q = pending_at(&r.read(), rdy_engine::pending::CARD_MAX_TOKENS);
    q.items
        .into_iter()
        .find(|i| i.r#ref == pref && i.kind == ItemKind::Approve)
        .unwrap_or_else(|| panic!("an approve item for {pref}; not ready: {:#?}", q.not_ready))
}

/// The confirm card of decision `d`.
fn confirm_of(r: &Rdy, d: &BlockId) -> Item {
    let at = format!("{d}@");
    let found: Vec<Item> = items(r)
        .into_iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.r#ref.starts_with(&at))
        .collect();
    assert_eq!(found.len(), 1, "one confirm card for {d}");
    found.into_iter().next().unwrap()
}

fn names(i: &Item) -> Vec<String> {
    i.facts.iter().map(|f| f.name.to_string()).collect()
}

fn lines(i: &Item) -> Vec<String> {
    i.facts
        .iter()
        .map(|f| format!("{}: {}", f.name, f.value))
        .collect()
}

fn fact<'a>(i: &'a Item, name: &str) -> &'a str {
    i.facts
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("fact {name} in {:#?}", i.facts))
        .value
        .as_str()
}

fn count_of(i: &Item, name: &str) -> usize {
    i.facts.iter().filter(|f| f.name == name).count()
}

fn stored(r: &Rdy, p: &ProposalInfo) -> Assessment {
    let st = r.read();
    let v: Hash = p.id.parse().unwrap();
    let h = st
        .reader()
        .assessment_of_proposal(&v, st.seq())
        .unwrap()
        .unwrap_or_else(|| panic!("{} stores an assessment", p.r#ref));
    st.reader().assessment(&h).unwrap()
}

fn agent() -> Session {
    Session::agent("agent:relay".into(), Some("r1".into()), Role::Writer).expect("agent")
}

/// One related row as a card prints it.
fn row(for_: &BlockId, relation: &str, d: &BlockId, status: &str, via: &str, text: &str) -> String {
    format!("{for_} {relation} {d} [{status}] via {via}: {text}")
}

/// The rows of anchor `for_` for the `(decision, status, via, statement)` list, each a
/// `same_entity` row, in decision id order.
fn same_entity_rows(for_: &BlockId, list: &[(&BlockId, &str, &str, &str)]) -> Vec<String> {
    let mut sorted: Vec<&(&BlockId, &str, &str, &str)> = list.iter().collect();
    sorted.sort_by_key(|(d, ..)| (*d).clone());
    sorted
        .into_iter()
        .map(|(d, status, via, text)| row(for_, "same_entity", d, status, via, text))
        .collect()
}

/// The `(linked, text)` counts a `Related decisions` value leads with, read from its head; the
/// value must say `omitted` omitted rows.
fn split_counts(value: &str, omitted: usize) -> (usize, usize) {
    let rest = value.split(" listed by link, ").collect::<Vec<_>>();
    assert_eq!(rest.len(), 2, "{value}");
    let linked: usize = rest[0].parse().unwrap();
    let (text, tail) = rest[1].split_once(" found by text, ").expect(value);
    assert!(tail.starts_with(&format!("{omitted} omitted")), "{value}");
    (linked, text.parse().unwrap())
}

/// Lands a decision `statement` on the entity in its own view: canon moves on.
fn land_decision_on(w: &mut World, view: &str, statement: &str) -> BlockId {
    let d = decide_in(&mut w.r, view, decision(&w.s, statement, &[&w.e], false));
    land(&mut w.r, view).unwrap();
    d
}

#[test]
fn nb11_confidence_names_the_original_mention_words() {
    let mut w = world();
    let (s, e) = (w.s.to_string(), w.e.to_string());
    // plural and case are kept as stored; the claim describes the entity, so the mention is its
    // only open issue
    let cases = [
        (
            "plural",
            json!({"op": "create", "type": "claim", "id": "$c", "parent": s,
                   "body": "The Stores are fast.", "links": {"describes": [{"target": e}]}}),
            "\"Stores\"",
        ),
        (
            "upper",
            json!({"op": "create", "type": "claim", "id": "$c", "parent": s,
                   "body": "STORE rules apply.", "links": {"describes": [{"target": e}]}}),
            "\"STORE\"",
        ),
        (
            "nested",
            json!({"op": "create", "type": "flow", "id": "$c", "parent": s,
                   "fields": {"name": "Refund flow", "steps": ["Open a case", "Check the\nStore twice"]},
                   "links": {"describes": [{"target": e}]}}),
            "\"Store\"",
        ),
        (
            "multiline",
            json!({"op": "create", "type": "claim", "id": "$c", "parent": s,
                   "body": "A merge\r\nconflict stops it.", "links": {"describes": [{"target": e}]}}),
            // a line break shows escaped, as JSON quotes it
            "\"merge\\r\\nconflict\"",
        ),
    ];
    for (view, op, words) in cases {
        let c = created(&put(&mut w.r, view, json!([op])), "$c");
        let p = propose(&mut w.r, view);
        let item = approve_item(&w.r, &p.r#ref);
        assert_eq!(
            fact(&item, "Confidence"),
            format!("medium: unlinked_mention on {c} ({words})"),
            "{view}: {:#?}",
            item.facts
        );
        // the card text and the stored fact say the same
        assert!(item.card.contains(&format!("({words})")), "{}", item.card);
        let line = lines(&item)
            .into_iter()
            .find(|l| l.starts_with("Confidence: "))
            .unwrap();
        assert!(stored(&w.r, &p).card.facts.contains(&line), "{line}");
    }
    // a first reason that is no mention keeps its old text
    let c = created(
        &put(
            &mut w.r,
            "plain",
            json!([{"op": "create", "type": "claim", "id": "$c", "parent": s, "body": "A plain rule."}]),
        ),
        "$c",
    );
    let p = propose(&mut w.r, "plain");
    let item = approve_item(&w.r, &p.r#ref);
    assert_eq!(
        fact(&item, "Confidence"),
        format!("medium: untied_content on {c}")
    );
}

#[test]
fn nb11_everyday_removes_only_unlinked_card_inference() {
    let mut w = world();
    let s = w.s.to_string();
    let cl1 = w.cl1.clone();
    let ends = |r: &Rdy| {
        let st = r.read();
        let m = st.materialized(&Scope::Canon).unwrap();
        let g = st.graph(&Scope::Canon).unwrap();
        let covered = BTreeSet::from([cl1.clone()]);
        neighbors_in(&m, &g, st.pack(), &covered)
            .unwrap()
            .into_iter()
            .map(|n| (n.block, n.via))
            .collect::<Vec<_>>()
    };
    let neighbors_before = ends(&w.r);
    assert!(
        neighbors_before
            .iter()
            .any(|(b, _)| *b == w.cl2.to_string()),
        "{neighbors_before:?}"
    );

    // a claim that mentions Store unlinked, and two decisions on the entity: n names it in text,
    // l is tied to it
    let build = |w: &mut World, view: &str| {
        let c = created(
            &put(
                &mut w.r,
                &format!("{view}-claim"),
                json!([{"op": "create", "type": "claim", "id": "$c", "parent": s,
                        "body": "The Stores are fast.",
                        "links": {"describes": [{"target": w.e.to_string()}]}}]),
            ),
            "$c",
        );
        let pc = propose(&mut w.r, &format!("{view}-claim"));
        let n = decide_in(
            &mut w.r,
            &format!("{view}-dec"),
            decision(&w.s, "The ledger service rotates keys", &[], true),
        );
        let l = decide_in(
            &mut w.r,
            &format!("{view}-dec"),
            decision(&w.s, "Entries are signed", &[&w.e], false),
        );
        let pd = propose(&mut w.r, &format!("{view}-dec"));
        (c, pc, n, l, pd)
    };
    let (c, pc, n, _l, pd) = build(&mut w, "before");
    assert_eq!(
        fact(&approve_item(&w.r, &pc.r#ref), "Confidence"),
        format!("medium: unlinked_mention on {c} (\"Stores\")")
    );
    let card = confirm_of(&w.r, &n);
    // n names the entity: a, b and the proposed l are found by text, none listed by link
    assert_eq!(
        fact(&card, "Related decisions"),
        "0 listed by link, 3 found by text, 0 omitted"
    );
    // the batch: l is tied to a (one linked row); the five other pairs are counted text rows
    assert_eq!(
        fact(&approve_item(&w.r, &pd.r#ref), "Related decisions"),
        format!(
            "1 listed by link, 5 found by text, 0 omitted: {}",
            row(&_l, "same_entity", &w.a, "live", "link", A_STATEMENT)
        )
    );

    // everyday on the term and the entity, landed
    put_j(
        &mut w.r,
        "flag",
        json!([{"op": "update", "id": w.t.to_string(), "fields": {"everyday": true}},
               {"op": "update", "id": w.e.to_string(), "fields": {"everyday": true}}]),
        j_typo(),
    );
    land(&mut w.r, "flag").unwrap();
    let (_c2, pc2, n2, l2, pd2) = build(&mut w, "after");
    let conf = approve_item(&w.r, &pc2.r#ref);
    assert!(
        !fact(&conf, "Confidence").contains("unlinked_mention"),
        "the unlinked mention of an everyday word gives no reason: {}",
        fact(&conf, "Confidence")
    );
    // the text-only decision lists nothing: its words do not count; the tied decision still
    // lists the tied one and counts nothing by text
    let card = confirm_of(&w.r, &n2);
    assert_eq!(fact(&card, "Related decisions"), "none");
    let batch = fact(&approve_item(&w.r, &pd2.r#ref), "Related decisions").to_string();
    assert_eq!(
        batch,
        format!(
            "1 listed by link, 0 found by text, 0 omitted: {}",
            row(&l2, "same_entity", &w.a, "live", "link", A_STATEMENT)
        ),
        "{batch}"
    );
    // the neighbor query reads ties: the flag moves none of its rows
    assert_eq!(ends(&w.r), neighbors_before);
    // nb8: explicit ties keep their neighbor items too: a text edit of cl1 asks each section
    // neighbor to confirm in the net plan
    put(&mut w.r, "nb8", upd(&w.cl1, "Entries stay longer."));
    let reply = w.r.read().view_plan("nb8").unwrap();
    let items: BTreeSet<(String, String, PlanKind)> = reply
        .items
        .iter()
        .filter_map(|row| match row {
            PlanRow::Live(i) if i.edge == NEIGHBOR_EDGE => {
                Some((i.cause.clone(), i.subject.clone(), i.kind))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        items,
        BTreeSet::from([
            (w.cl1.to_string(), w.cl2.to_string(), PlanKind::Confirm),
            (w.cl1.to_string(), w.x.to_string(), PlanKind::Confirm),
        ]),
        "{reply:#?}"
    );
}

#[test]
fn nb11_decision_and_batch_cards_show_the_related_list() {
    let mut w = world();
    let a = w.a.clone();
    // two decisions on the entity: n1 is tied to it, n2 names it
    let n1_statement = "Entries are signed";
    let n2_statement = "The ledger service stores signatures";
    let n1 = decide_in(&mut w.r, "a", decision(&w.s, n1_statement, &[&w.e], true));
    let n2 = decide_in(&mut w.r, "a", decision(&w.s, n2_statement, &[], true));
    let p = propose(&mut w.r, "a");
    let batch = approve_item(&w.r, &p.r#ref);

    // n1 is tied to a: one linked row and two counted text rows; n2 only names the entity: three
    // counted text rows
    let r1 = same_entity_rows(&n1, &[(&a, "live", "link", A_STATEMENT)]);
    let r2: Vec<String> = vec![];
    // one aggregate fact on the batch card, after Decisions and before Supersedes: anchors in id
    // order, each with its rows
    assert_eq!(count_of(&batch, "Related decisions"), 1);
    let n = names(&batch);
    let at = n.iter().position(|x| x == "Decisions").unwrap();
    assert_eq!(
        &n[at..at + 3],
        ["Decisions", "Related decisions", "Supersedes"]
    );
    assert_eq!(
        fact(&batch, "Related decisions"),
        format!(
            "1 listed by link, 5 found by text, 0 omitted: {}",
            r1.join("; ")
        )
    );
    assert!(!fact(&batch, "Related decisions").contains(B_STATEMENT));
    let _ = &w.b;
    // each decision confirm card: its own rows, one fact, last
    for (d, linked, rows) in [(&n1, 1, &r1), (&n2, 0, &r2)] {
        let card = confirm_of(&w.r, d);
        assert_eq!(count_of(&card, "Related decisions"), 1, "{:#?}", card.facts);
        assert_eq!(card.facts.last().unwrap().name, "Related decisions");
        let tail = if rows.is_empty() {
            String::new()
        } else {
            format!(": {}", rows.join("; "))
        };
        assert_eq!(
            fact(&card, "Related decisions"),
            format!(
                "{linked} listed by link, {} found by text, 0 omitted{tail}",
                3 - linked
            )
        );
        assert!(card.card.contains("Related decisions"), "{}", card.card);
    }

    // a decision with nothing related says none, on its confirm card and on the batch
    let z = decide_in(
        &mut w.r,
        "z",
        decision(&w.s, "Claims stay short", &[&w.x], true),
    );
    let pz = propose(&mut w.r, "z");
    assert_eq!(
        fact(&approve_item(&w.r, &pz.r#ref), "Related decisions"),
        "none"
    );
    assert_eq!(fact(&confirm_of(&w.r, &z), "Related decisions"), "none");
    // a version that writes no decision has no related fact
    put(
        &mut w.r,
        "q",
        json!([{"op": "create", "type": "claim", "parent": w.s.to_string(), "body": "Q holds."}]),
    );
    let pq = propose(&mut w.r, "q");
    assert_eq!(
        count_of(&approve_item(&w.r, &pq.r#ref), "Related decisions"),
        0
    );

    // the 20-row cap, in all, with exact counts: 22 decisions on the entity each list 21, cut to 20
    // (1 omitted each), and the aggregate is cut to one 20
    let mut w = world();
    for i in 0..22 {
        decide_in(
            &mut w.r,
            "many",
            decision(&w.s, &format!("Entry class {i} is kept"), &[&w.e], false),
        );
    }
    let p = propose(&mut w.r, "many");
    let item = approve_item(&w.r, &p.r#ref);
    let value = fact(&item, "Related decisions");
    // each of the 22 anchors sees the 21 others and the two canon decisions: 23, cut to 20
    // the omitted count is unchanged; the 20 retained rows are split by route
    let omitted = 22 * 3 + (22 * 20 - 20);
    let (l, t) = split_counts(value, omitted);
    assert_eq!(l + t, 20, "{}", &value[..value.len().min(120)]);
    let head = format!("{l} listed by link, {t} found by text, {omitted} omitted");
    assert!(
        value.starts_with(&head),
        "{}",
        &value[..value.len().min(120)]
    );
    if l > 0 {
        assert_eq!(value[head.len() + 2..].split("; ").count(), l);
    } else {
        assert_eq!(value, head);
    }
}

#[test]
fn nb11_approve_facts_are_stored_and_old_cards_do_not_recompute() {
    let mut w = world();
    let n = decide_in(
        &mut w.r,
        "a",
        decision(&w.s, "The ledger service stores signatures", &[], true),
    );
    put(
        &mut w.r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": w.s.to_string(),
                "body": "The Stores are fast.",
                "links": {"describes": [{"target": w.e.to_string()}]}}]),
    );
    let p = propose(&mut w.r, "a");
    let shown = approve_item(&w.r, &p.r#ref);
    let record = stored(&w.r, &p);
    assert_eq!(
        lines(&shown),
        record.card.facts,
        "the stored strings are the shown facts, the words and the list included"
    );
    assert!(
        record
            .card
            .facts
            .iter()
            .any(|l| l == "Related decisions: 0 listed by link, 2 found by text, 0 omitted"),
        "{:#?}",
        record.card.facts
    );
    assert!(
        record
            .card
            .facts
            .iter()
            .any(|l| l.contains("unlinked_mention") && l.contains("(\"ledger service\")")),
        "{:#?}",
        record.card.facts
    );
    let confirm = confirm_of(&w.r, &n);

    // an unrelated landing that would change a recomputed list: another decision names the entity
    let late = land_decision_on(&mut w, "late", "Ledger service entries expire never");
    let after = approve_item(&w.r, &p.r#ref);
    let others = |i: &Item| -> Vec<String> {
        lines(i)
            .into_iter()
            .filter(|l| !l.starts_with("Base: "))
            .collect()
    };
    assert_eq!(
        others(&after),
        others(&shown),
        "a later landing leaves the stored facts as they were"
    );
    assert!(fact(&after, "Base").ends_with(", moved"));
    assert!(
        !fact(&after, "Related decisions").contains(&late.to_string()),
        "{}",
        fact(&after, "Related decisions")
    );
    // the decision's card is read from the proposal's own state: it does not move
    let confirm_after = confirm_of(&w.r, &n);
    assert_eq!(
        fact(&confirm_after, "Related decisions"),
        fact(&confirm, "Related decisions")
    );
    // reopening the store gives the same cards
    w.r.invalidate();
    let again = approve_item(&w.r, &p.r#ref);
    assert_eq!(lines(&again), lines(&after));
    assert_eq!(stored(&w.r, &p), record, "the stored record is untouched");
}

#[test]
fn nb11_card_hash_binds_related_rows_beyond_the_text_budget() {
    let mut w = world();
    let n1 = decide_in(
        &mut w.r,
        "a",
        decision(&w.s, "Entries are signed", &[&w.e], true),
    );
    // n2 is tied to the entity too, so its row is listed by link with its summary
    let n2 = decide_in(
        &mut w.r,
        "a",
        decision(&w.s, "The ledger service stores signatures", &[&w.e], false),
    );
    let p1 = propose(&mut w.r, "a");
    let card1 = confirm_of(&w.r, &n1);
    let batch1 = approve_item(&w.r, &p1.r#ref);
    assert!(
        fact(&card1, "Related decisions").contains("The ledger service stores signatures"),
        "{}",
        fact(&card1, "Related decisions")
    );
    // a small budget cuts the facts out of the text, not out of the hash
    let small = pending_at(&w.r.read(), 40);
    let cut = small
        .items
        .iter()
        .find(|i| i.r#ref == card1.r#ref)
        .expect("the confirm card");
    assert!(!cut.card.contains("Related decisions"), "{}", cut.card);
    assert!(cut.card != card1.card);
    assert_eq!(
        cut.card_hash, card1.card_hash,
        "the hash ignores the budget"
    );
    let cut_batch = small.items.iter().find(|i| i.r#ref == p1.r#ref).unwrap();
    assert_eq!(cut_batch.card_hash, batch1.card_hash);
    // the hash is of the whole bounded fact: a row changed in the item moves it
    for item in [&card1, &batch1] {
        let mut moved = item.clone();
        for f in &mut moved.facts {
            if f.name == "Related decisions" {
                f.value = f.value.replace("stores signatures", "keeps signatures");
            }
        }
        assert_ne!(moved.facts, item.facts);
        assert_ne!(
            card_hash(&moved).to_string(),
            item.card_hash,
            "{} binds its related rows",
            item.r#ref
        );
        assert_eq!(card_hash(item).to_string(), item.card_hash);
    }

    // an edit of a row within the bounded fact: n2's statement still names the entity and n2 is
    // still tied to it
    put(
        &mut w.r,
        "a",
        json!([{"op": "update", "id": n2.to_string(),
                "fields": {"statement": "The ledger service stores every signature"}}]),
    );
    let p2 = propose(&mut w.r, "a");
    assert_ne!(p2.r#ref, p1.r#ref);
    let card2 = confirm_of(&w.r, &n1);
    assert_eq!(card2.r#ref, card1.r#ref, "n1 itself did not change");
    assert!(
        fact(&card2, "Related decisions").contains("stores every signature"),
        "{}",
        fact(&card2, "Related decisions")
    );
    assert_ne!(
        card2.card_hash, card1.card_hash,
        "the related row moved the hash"
    );
    assert_ne!(
        approve_item(&w.r, &p2.r#ref).card_hash,
        batch1.card_hash,
        "and the batch's"
    );
    // the old hash is refused
    let e =
        w.r.answer(
            &agent(),
            PERSON,
            AnswerRequest {
                r#ref: card1.r#ref.clone(),
                option: "confirm".into(),
                card_hash: Some(card1.card_hash.clone()),
                note: None,
                relay: RelayInput {
                    relayed_by: None,
                    evidence: Some("yes".into()),
                    question: Some("Shall I confirm it?".into()),
                    session_ref: Some("chat-7".into()),
                    kind: None,
                },
                brief: None,
            },
        )
        .unwrap_err();
    assert_eq!(e.code, E_ITEM_MOVED, "{e:?}");
}

#[test]
fn nb11_long_words_and_related_rows_fit_existing_fact_bounds() {
    let mut w = world();
    // 21 decisions on the entity whose summaries are 80 three-byte characters: 20 rows of about
    // 250 bytes each pass the 4,096-byte bound of one fact
    let long = format!("ledger service {}", "数据库 ".repeat(30));
    for i in 0..21 {
        decide_in(
            &mut w.r,
            "long",
            DecisionInput {
                // distinct statements, equal summaries
                statement: format!("{long}{i}"),
                ..decision(&w.s, &format!("Long {i}"), &[&w.e], false)
            },
        );
    }
    let p = propose(&mut w.r, "long");
    let item = approve_item(&w.r, &p.r#ref);
    let n = names(&item);
    let at = n.iter().position(|x| x == "Related decisions").unwrap();
    assert_eq!(n[at - 1], "Decisions");
    assert_eq!(n[at + 1], "Supersedes");
    for later in ["Tensions", "Coverage", "Confidence", "Base", "Exceptions"] {
        assert!(n.iter().position(|x| x == later).unwrap() > at, "{later}");
    }
    let line = format!("Related decisions: {}", fact(&item, "Related decisions"));
    assert!(line.len() <= 4096, "{} bytes", line.len());
    assert!(line.ends_with("..."), "the row tail is cut: {line}");
    // 20 rows are kept, split by route (the canon decision that only names the entity may be
    // among them); the omitted count is the one it was
    let value = fact(&item, "Related decisions");
    let (linked, text) = split_counts(value, 21 * 2 + (21 * 20 - 20));
    assert_eq!(linked + text, 20, "the counts lead: {}", &line[..90]);
    assert!(linked >= 19, "{}", &line[..90]);
    // the stored card holds the same bounded line
    let record = stored(&w.r, &p);
    assert_eq!(record.card.facts, lines(&item));
    // a long unlinked mention fits too: an entity name of eight long words
    let word = "x".repeat(20);
    let name = (0..8)
        .map(|i| format!("{word}{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    put_j(
        &mut w.r,
        "names",
        json!([{"op": "create", "type": "term", "parent": w.s.to_string(),
                "fields": {"name": name.clone()}, "body": "A long name."}]),
        j_typo(),
    );
    land(&mut w.r, "names").unwrap();
    let c = created(
        &put(
            &mut w.r,
            "words",
            json!([{"op": "create", "type": "claim", "id": "$c", "parent": w.s.to_string(),
                    "body": format!("See {name} here."),
                    "links": {"describes": [{"target": w.e.to_string()}]}}]),
        ),
        "$c",
    );
    let p = propose(&mut w.r, "words");
    let item = approve_item(&w.r, &p.r#ref);
    let conf = fact(&item, "Confidence");
    assert!(
        conf.starts_with(&format!("medium: unlinked_mention on {c} (\"")),
        "{conf}"
    );
    assert!(format!("Confidence: {conf}").len() <= 4096);
}

#[test]
fn nb11_m3d_accepted_verdict_and_source_rules_survive() {
    let mut w = world();
    put_j(
        &mut w.r,
        "a",
        upd(&w.cl1, "Entries stay for good."),
        j_typo(),
    );
    let n = decide_in(
        &mut w.r,
        "a",
        decision(&w.s, "The ledger service stores signatures", &[], true),
    );
    let p = propose(&mut w.r, "a");
    let before = approve_item(&w.r, &p.r#ref);
    assert_eq!(count_of(&before, "Related decisions"), 1);
    // the stored confidence stays free of machine-dependent source findings
    assert!(!fact(&before, "Confidence").contains("source_not_found"));
    let cl1 = w.cl1.clone();
    let World { _dir, r, .. } = w;
    let e = Local::new(r);
    meaning_verdict(&e, &ag("a"), &p.r#ref, "fail", &cl1);
    let shown = pending_at(&e.read(), rdy_engine::pending::CARD_MAX_TOKENS)
        .items
        .into_iter()
        .find(|i| i.r#ref == p.r#ref && i.kind == ItemKind::Approve)
        .unwrap();
    let k = before.facts.len();
    assert_eq!(
        lines(&shown)[..k],
        lines(&before)[..],
        "the stored prefix, related list included"
    );
    assert_eq!(
        lines(&shown)[k..],
        [format!(
            "meaning preservation: fail (agent:r, independent): {REASON}"
        )],
        "the verdict suffix follows the stored facts"
    );
    assert!(
        shown
            .options
            .iter()
            .any(|o| o.id.starts_with("accept_verdict")),
        "the accepted-verdict option stays"
    );
    assert_ne!(
        shown.card_hash, before.card_hash,
        "the verdict moved the hash"
    );
    let _ = n;
}

// ---- mi2 (D-KVQ7F): link rows are listed, text rows are counted ----

/// An empty project with doc `notes`, section `Ideas` and entity `Ledger service`, landed.
struct Mini {
    _dir: tempfile::TempDir,
    r: Rdy,
    s: BlockId,
    e: BlockId,
}

fn mini() -> Mini {
    let (dir, mut r) = rdy_qa::fixtures::project();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger service"}}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let id = |k: &str| BlockId::parse(&w.created[k]).unwrap();
    Mini {
        s: id("$s"),
        e: id("$e"),
        _dir: dir,
        r,
    }
}

impl Mini {
    /// A decision in `view`: tied to the entity, or only naming it in its statement.
    fn dec(&mut self, view: &str, statement: &str, tied: bool, one_way: bool) -> BlockId {
        let e = self.e.clone();
        let affects: Vec<&BlockId> = if tied { vec![&e] } else { vec![] };
        decide_in(
            &mut self.r,
            view,
            decision(&self.s, statement, &affects, one_way),
        )
    }

    /// Lands one decision as canon.
    fn seed(&mut self, statement: &str, tied: bool) -> BlockId {
        let d = self.dec("seed2", statement, tied, false);
        land(&mut self.r, "seed2").unwrap();
        d
    }
}

/// The briefing rows of `ids` in view `view`: `(related_decisions, omitted)` as JSON.
fn briefed(r: &Rdy, view: &str, ids: &[&BlockId]) -> (Vec<serde_json::Value>, usize) {
    let b = r
        .read()
        .brief(
            &Scope::View(view.into()),
            &Coverage {
                ids: ids.iter().map(|b| b.to_string()).collect(),
                path: None,
            },
            None,
        )
        .unwrap();
    let v = serde_json::to_value(&b).unwrap();
    (
        v["related_decisions"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
        v["related_decisions_omitted"].as_u64().unwrap_or(0) as usize,
    )
}

/// Both cards of a version with the confirm anchor `n` and the batch: their related facts.
fn related_on_cards(m: &mut Mini, view: &str, n: &BlockId) -> (String, String) {
    let p = propose(&mut m.r, view);
    let batch = fact(&approve_item(&m.r, &p.r#ref), "Related decisions").to_string();
    let confirm = fact(&confirm_of(&m.r, n), "Related decisions").to_string();
    (confirm, batch)
}

#[test]
fn mi2_cards_list_links_and_count_text() {
    // link only: both ends tied
    let mut m = mini();
    let a1 = m.seed("Entries are append-only", true);
    let n = m.dec("v", "Entries are signed", true, true);
    let (confirm, batch) = related_on_cards(&mut m, "v", &n);
    let linked = row(
        &n,
        "same_entity",
        &a1,
        "live",
        "link",
        "Entries are append-only",
    );
    let want = format!("1 listed by link, 0 found by text, 0 omitted: {linked}");
    assert_eq!(confirm, want);
    assert_eq!(batch, want);

    // text only: nothing is tied; the anchor and the canon decision only name the entity
    let mut m = mini();
    m.seed("The ledger service keeps entries", false);
    let n = m.dec("v", "The ledger service signs entries", false, true);
    let (confirm, batch) = related_on_cards(&mut m, "v", &n);
    assert_eq!(confirm, "0 listed by link, 1 found by text, 0 omitted");
    assert_eq!(batch, confirm);
    assert!(!confirm.contains("keeps entries"), "{confirm}");

    // mixed: one end named is text, both ends tied is link; the tail lists the link row only
    let mut m = mini();
    let a1 = m.seed("Entries are append-only", true);
    m.seed("The ledger service keeps entries", false);
    let n = m.dec("v", "Entries are signed", true, true);
    let (confirm, batch) = related_on_cards(&mut m, "v", &n);
    let linked = row(
        &n,
        "same_entity",
        &a1,
        "live",
        "link",
        "Entries are append-only",
    );
    let want = format!("1 listed by link, 1 found by text, 0 omitted: {linked}");
    assert_eq!(confirm, want);
    assert_eq!(batch, want);

    // the anchor only names the entity while the canon decision is tied: one end named, text
    let mut m = mini();
    m.seed("Entries are append-only", true);
    let n = m.dec("v", "The ledger service signs entries", false, true);
    let (confirm, batch) = related_on_cards(&mut m, "v", &n);
    assert_eq!(confirm, "0 listed by link, 1 found by text, 0 omitted");
    assert_eq!(batch, confirm);

    // empty
    let mut m = mini();
    let n = m.dec("v", "Entries are signed", true, true);
    let (confirm, batch) = related_on_cards(&mut m, "v", &n);
    assert_eq!(confirm, "none");
    assert_eq!(batch, "none");

    // several anchors in one batch; a repeated decision is counted once per anchor row
    let mut m = mini();
    let t1 = m.seed("The ledger service keeps entries", false);
    let n1 = m.dec("v", "The ledger service signs entries", false, true);
    let n2 = m.dec("v", "The ledger service seals entries", false, true);
    let (c1, batch) = related_on_cards(&mut m, "v", &n1);
    // four rows: n1-t1, n1-n2, n2-t1, n2-n1; t1 stands in two of them
    assert_eq!(batch, "0 listed by link, 4 found by text, 0 omitted");
    assert_eq!(c1, "0 listed by link, 2 found by text, 0 omitted");
    assert_eq!(
        fact(&confirm_of(&m.r, &n2), "Related decisions"),
        "0 listed by link, 2 found by text, 0 omitted"
    );
    // a briefing still returns the text rows and the unchanged omitted count
    let (rows, omitted) = briefed(&m.r, "v", &[&n1, &n2]);
    let text_rows = rows.iter().filter(|r| r["via"] == "text").count();
    assert_eq!((rows.len(), text_rows, omitted), (4, 4, 0), "{rows:?}");
    assert!(rows.iter().any(|r| r["decision"] == t1.to_string()));

    // anchors with linked rows list them in anchor order, then row order
    let mut m = mini();
    let a1 = m.seed("Entries are append-only", true);
    let n1 = m.dec("v", "Entries are signed", true, true);
    let n2 = m.dec("v", "Entries are sealed", true, true);
    let (_, batch) = related_on_cards(&mut m, "v", &n1);
    let mut want_rows = vec![];
    let mut anchors = vec![
        (n1.clone(), n2.clone(), "Entries are sealed"),
        (n2.clone(), n1.clone(), "Entries are signed"),
    ];
    anchors.sort_by(|x, y| x.0.cmp(&y.0));
    for (n, other, text) in anchors {
        let mut rows = vec![
            (a1.clone(), "live", "Entries are append-only"),
            (other, "proposed", text),
        ];
        rows.sort_by(|x, y| x.0.cmp(&y.0));
        for (d, status, t) in rows {
            want_rows.push(row(&n, "same_entity", &d, status, "link", t));
        }
    }
    assert_eq!(
        batch,
        format!(
            "4 listed by link, 0 found by text, 0 omitted: {}",
            want_rows.join("; ")
        )
    );
}

#[test]
fn mi2_related_counts_keep_existing_caps() {
    // 20 rows fit: 10 tied and 10 named; 21 rows cut one, whichever route it had
    for total in [20usize, 21] {
        let mut m = mini();
        for i in 0..total {
            // text first, as the statements are made; the order of the rows is the engine's
            let tied = i % 2 == 0;
            m.seed(&format!("Class {i} is kept by the ledger service"), tied);
        }
        let n = m.dec("v", "Entries are signed", true, true);
        let (rows, omitted) = briefed_after_propose(&mut m, "v", &n);
        assert_eq!(rows.len() + omitted, total, "{total}");
        let (confirm, batch) = related_on_cards(&mut m, "v", &n);
        let link: Vec<String> = rows
            .iter()
            .filter(|r| r["via"] == "link")
            .map(|r| {
                row(
                    &n,
                    "same_entity",
                    &BlockId::parse(r["decision"].as_str().unwrap()).unwrap(),
                    r["status"].as_str().unwrap(),
                    "link",
                    r["statement"].as_str().unwrap(),
                )
            })
            .collect();
        let text = rows.iter().filter(|r| r["via"] == "text").count();
        let want = format!(
            "{} listed by link, {text} found by text, {omitted} omitted: {}",
            link.len(),
            link.join("; ")
        );
        assert_eq!(confirm, want, "{total}");
        assert_eq!(batch, want, "{total}");
        // omitted rows are neither printed nor guessed: the tail holds only the linked rows
        assert_eq!(confirm.split("; ").count(), link.len());
        assert_eq!(omitted, total - 20.min(total));
        // text comes before link somewhere in the engine's order
        let first_link = rows.iter().position(|r| r["via"] == "link");
        let last_text = rows.iter().rposition(|r| r["via"] == "text");
        if let (Some(f), Some(l)) = (first_link, last_text) {
            assert!(f != l);
        }
    }

    // all-text retained rows: 21 named decisions; 20 are counted, one is omitted, none printed
    let mut m = mini();
    for i in 0..21 {
        m.seed(&format!("Class {i} is kept by the ledger service"), false);
    }
    let n = m.dec("v", "The ledger service signs entries", false, true);
    let (confirm, batch) = related_on_cards(&mut m, "v", &n);
    assert_eq!(confirm, "0 listed by link, 20 found by text, 1 omitted");
    assert_eq!(batch, confirm);

    // several anchors: 22 tied decisions in one version, each sees the 21 others; each is cut to
    // 20 (1 omitted each), then the aggregate is cut to one 20. Counts are over the retained set
    let mut m = mini();
    let mut ds = vec![];
    for i in 0..22 {
        ds.push(m.dec("many", &format!("Entry class {i} is kept"), true, false));
    }
    let p = propose(&mut m.r, "many");
    let value = fact(&approve_item(&m.r, &p.r#ref), "Related decisions").to_string();
    let refs: Vec<&BlockId> = ds.iter().collect();
    let (rows, omitted) = briefed(&m.r, "many", &refs);
    assert_eq!(rows.len(), 20);
    assert_eq!(omitted, 22 + (22 * 20 - 20), "{omitted}");
    assert!(
        value.starts_with(&format!(
            "20 listed by link, 0 found by text, {omitted} omitted: "
        )),
        "{}",
        &value[..value.len().min(120)]
    );
    let head = format!("20 listed by link, 0 found by text, {omitted} omitted: ");
    assert_eq!(
        value[head.len()..].split("; ").count(),
        20,
        "no omitted row becomes a printed row"
    );

    // long summaries: 20 linked rows with long text fit the bound, the counts lead, the tail is
    // cut and the cut tail does not change the counts
    let mut m = mini();
    let long = "x".repeat(10);
    for i in 0..20 {
        let d = DecisionInput {
            statement: format!("{long} {} {i}", "数据库 ".repeat(30)),
            ..decision(&m.s, &format!("Long {i}"), &[&m.e.clone()], false)
        };
        decide_in(&mut m.r, "seed2", d);
    }
    land(&mut m.r, "seed2").unwrap();
    let n = m.dec("v", "Entries are signed", true, true);
    let (confirm, _) = related_on_cards(&mut m, "v", &n);
    let line = format!("Related decisions: {confirm}");
    assert!(line.len() <= 4096, "{} bytes", line.len());
    assert!(line.ends_with("..."), "the tail is cut");
    assert!(
        confirm.starts_with("20 listed by link, 0 found by text, 0 omitted: "),
        "the counts are not invented after the cut: {}",
        &confirm[..80]
    );
}

/// The briefing rows of `n` in the proposal view `view` (proposed first, as the card is).
fn briefed_after_propose(m: &mut Mini, view: &str, n: &BlockId) -> (Vec<serde_json::Value>, usize) {
    briefed(&m.r, view, &[n])
}

#[test]
fn mi2_text_summary_is_not_a_card_fact() {
    // n1 is tied; n2 only names the entity. Editing n2's words changes no card fact.
    let mut m = mini();
    let n1 = m.dec("v", "Entries are signed", true, true);
    let n2 = m.dec("v", "The ledger service stores signatures", false, false);
    let p1 = propose(&mut m.r, "v");
    let card1 = confirm_of(&m.r, &n1);
    let batch1 = approve_item(&m.r, &p1.r#ref);
    assert_eq!(
        fact(&card1, "Related decisions"),
        "0 listed by link, 1 found by text, 0 omitted"
    );
    put(
        &mut m.r,
        "v",
        json!([{"op": "update", "id": n2.to_string(),
                "fields": {"statement": "The ledger service stores every signature"}}]),
    );
    let p2 = propose(&mut m.r, "v");
    assert_ne!(p2.r#ref, p1.r#ref, "n2 changed, so the batch did");
    let card2 = confirm_of(&m.r, &n1);
    let batch2 = approve_item(&m.r, &p2.r#ref);
    assert_eq!(card2.r#ref, card1.r#ref);
    for (a, b) in [(&card1, &card2), (&batch1, &batch2)] {
        assert_eq!(
            fact(a, "Related decisions"),
            fact(b, "Related decisions"),
            "the text row's words are no card fact"
        );
    }
    assert_eq!(
        card1.facts, card2.facts,
        "so no fact of the confirm card moves"
    );
    // the hash may still move on other card parts (the plan binding of the edit); the related
    // fact adds nothing to it, as its bytes are the same

    // a linked row's summary is in the fact and in the hash
    let mut m = mini();
    let n1 = m.dec("v", "Entries are signed", true, true);
    let n2 = m.dec("v", "The ledger service stores signatures", true, false);
    let p1 = propose(&mut m.r, "v");
    let card1 = confirm_of(&m.r, &n1);
    assert!(
        fact(&card1, "Related decisions").contains("stores signatures"),
        "{}",
        fact(&card1, "Related decisions")
    );
    let batch1 = approve_item(&m.r, &p1.r#ref);
    put(
        &mut m.r,
        "v",
        json!([{"op": "update", "id": n2.to_string(),
                "fields": {"statement": "The ledger service stores every signature"}}]),
    );
    let p2 = propose(&mut m.r, "v");
    let card2 = confirm_of(&m.r, &n1);
    assert_ne!(
        fact(&card1, "Related decisions"),
        fact(&card2, "Related decisions")
    );
    assert_ne!(card1.card_hash, card2.card_hash);
    assert_ne!(batch1.card_hash, approve_item(&m.r, &p2.r#ref).card_hash);

    // a changed retained text count moves the fact and the hash
    let mut m = mini();
    let n1 = m.dec("v", "Entries are signed", true, true);
    m.dec("v", "The ledger service stores signatures", false, false);
    let p1 = propose(&mut m.r, "v");
    let card1 = confirm_of(&m.r, &n1);
    let batch1 = approve_item(&m.r, &p1.r#ref);
    m.dec("v", "The ledger service seals signatures", false, false);
    let p2 = propose(&mut m.r, "v");
    let card2 = confirm_of(&m.r, &n1);
    assert_eq!(
        fact(&card1, "Related decisions"),
        "0 listed by link, 1 found by text, 0 omitted"
    );
    assert_eq!(
        fact(&card2, "Related decisions"),
        "0 listed by link, 2 found by text, 0 omitted"
    );
    assert_ne!(card1.card_hash, card2.card_hash);
    assert_ne!(batch1.card_hash, approve_item(&m.r, &p2.r#ref).card_hash);

    // stored approve facts stay as stored: a fresh record holds the new form, and reopening the
    // store does not recompute it
    let record = stored(&m.r, &p2);
    // the batch holds three anchors: six text rows
    let line = "Related decisions: 0 listed by link, 6 found by text, 0 omitted";
    assert!(
        record.card.facts.iter().any(|l| l == line),
        "{:#?}",
        record.card.facts
    );
    m.r.invalidate();
    assert_eq!(stored(&m.r, &p2), record);
}
