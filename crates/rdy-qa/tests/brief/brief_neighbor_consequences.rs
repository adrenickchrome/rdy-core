//! nb8 (D-YXBXH; TASK-978 I1, X2, C2, O3): a write that changes a content block's text asks
//! each unchanged section neighbor to confirm, with any removed literal it states as evidence,
//! and an entity neighbor in the same doc to update only when it states a literal the write
//! removed. A create or a delete asks the unchanged section neighbors to confirm. One neighbor
//! rule gives the write reports and the net proposal's plan items; a declared relation wins
//! over `neighbor` for one item identity, and no net cause is lost.
use rdy_engine::brief::FollowUp;
use rdy_engine::consequence::*;
use rdy_engine::plan::{ItemState, PlanAnswerInput, PlanQuery, PlanReply, PlanRow};
use rdy_engine::*;
use rdy_model::plan::{PlanAct, PlanKind, item_display, item_hash};
use rdy_model::{BlockId, Hash};
use rdy_qa::edit_loop::{brief_of, write_cited};
use rdy_qa::fixtures::*;
use rdy_qa::intake::TEST_SUMMARY;
use serde_json::{Value, json};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// The consequences of `change` in view `view`, uncut.
fn report(r: &Rdy, view: &str, change: &str) -> Consequences {
    r.read()
        .consequences(
            &Scope::View(view.into()),
            &change.parse::<Hash>().unwrap(),
            Some(1_000_000),
        )
        .unwrap()
}

/// The neighbor entries of `c`: (block, follow-up, changed block), in list order.
fn nrows(c: &Consequences) -> Vec<(String, FollowUp, String)> {
    c.items
        .iter()
        .filter(|x| x.because.edge == NEIGHBOR_EDGE)
        .map(|x| (x.block.clone(), x.follow_up, x.because.changed.clone()))
        .collect()
}

fn entry<'a>(c: &'a Consequences, block: &str) -> &'a Consequence {
    c.items
        .iter()
        .find(|x| x.block == block)
        .unwrap_or_else(|| panic!("no entry for {block}: {c:#?}"))
}

/// Every live item of `reply`: (cause, subject, kind, edge), sorted.
fn items_of(reply: &PlanReply) -> Vec<(String, String, PlanKind, String)> {
    let mut v: Vec<_> = reply
        .items
        .iter()
        .filter_map(|row| match row {
            PlanRow::Live(i) => Some((i.cause.clone(), i.subject.clone(), i.kind, i.edge.clone())),
            _ => None,
        })
        .collect();
    v.sort();
    v
}

/// The net plan of view `view`: every item.
fn plan(r: &Rdy, view: &str) -> Vec<(String, String, PlanKind, String)> {
    items_of(&r.read().view_plan(view).unwrap())
}

/// The plan items of `view` with edge `neighbor`: (cause, subject, kind), sorted.
fn nitems(r: &Rdy, view: &str) -> Vec<(String, String, PlanKind)> {
    plan(r, view)
        .into_iter()
        .filter(|i| i.3 == NEIGHBOR_EDGE)
        .map(|i| (i.0, i.1, i.2))
        .collect()
}

fn ni(cause: &str, subject: &str, kind: PlanKind) -> (String, String, PlanKind) {
    (cause.to_string(), subject.to_string(), kind)
}

fn pi(cause: &str, subject: &str, kind: PlanKind) -> String {
    item_display(&item_hash(&id(cause), subject, kind))
}

fn sorted<T: Ord>(mut v: Vec<T>) -> Vec<T> {
    v.sort();
    v
}

/// Neighbor entries in block order, as a report lists them.
fn by_block(mut v: Vec<(String, FollowUp, String)>) -> Vec<(String, FollowUp, String)> {
    v.sort_by(|x, y| x.0.cmp(&y.0));
    v
}

/// Seeds `ops` in view `seed`, lands it and returns the created ids by placeholder.
fn seed(r: &mut Rdy, ops: Value) -> Ids {
    let w = write(r, "seed", ops).unwrap();
    land(r, "seed").unwrap();
    Ids(w.created)
}

/// The ids a write created, by placeholder.
struct Ids(std::collections::BTreeMap<String, String>);

impl Ids {
    fn get(&self, k: &str) -> String {
        self.0[k].clone()
    }
}

fn update(r: &mut Rdy, view: &str, id: &str, body: &str) -> WriteResult {
    write(r, view, json!([{"op": "update", "id": id, "body": body}]))
        .unwrap_or_else(|e| panic!("update {id} in {view}: {e}"))
}

fn doc(name: &str) -> Value {
    json!({"op": "create", "type": "doc", "id": format!("${name}"),
        "fields": {"name": name, "title": name, "doc_type": "page"}})
}

fn section(key: &str, parent: &str) -> Value {
    json!({"op": "create", "type": "section", "id": key, "parent": parent,
        "fields": {"title": key.trim_start_matches('$')}})
}

fn claim(key: &str, parent: &str, body: &str) -> Value {
    json!({"op": "create", "type": "claim", "id": key, "parent": parent, "body": body})
}

fn tied(key: &str, parent: &str, body: &str, entity: &str) -> Value {
    json!({"op": "create", "type": "claim", "id": key, "parent": parent, "body": body,
        "links": {"describes": [{"target": entity}]}})
}

/// A body past the 16384-byte evidence budget that states `tail` only after it.
fn long(tail: &str) -> String {
    format!("Long {}{tail}", "pad ".repeat(4_200))
}

#[test]
fn nb8_section_confirm_keeps_literal_evidence() {
    let (_d, mut r) = project();
    let ids = seed(
        &mut r,
        json!([
            doc("nb"),
            section("$s", "$nb"),
            claim("$a", "$s", "A waits 30 days."),
            claim("$n1", "$s", "N1 stands."),
            claim("$n2", "$s", "N2 also waits 30 days."),
            claim("$n3", "$s", &long("waits 30 days.")),
        ]),
    );
    let (a, n1, n2, n3) = (
        ids.get("$a"),
        ids.get("$n1"),
        ids.get("$n2"),
        ids.get("$n3"),
    );
    let res = update(&mut r, "w", &a, "A waits 60 days.");
    let c = report(&r, "w", &res.change);
    let mut want = vec![n1.clone(), n2.clone(), n3.clone()];
    want.sort();
    assert_eq!(
        c.items.iter().map(|x| x.block.clone()).collect::<Vec<_>>(),
        want
    );
    let change: Hash = res.change.parse().unwrap();
    for x in &c.items {
        // no literal, a matching removed literal and a cut scan: each one confirms
        assert_eq!(
            (x.kind, x.follow_up),
            (ConsequenceKind::Dependent, FollowUp::Confirm),
            "{x:#?}"
        );
        assert_eq!(
            x.because,
            Because {
                edge: NEIGHBOR_EDGE.into(),
                changed: a.clone(),
                edges: Some(vec![NEIGHBOR_EDGE.into()]),
                values: Some(vec!["30 days".into()]),
                values_omitted: 0,
            }
        );
        assert_eq!(x.fix.as_deref(), Some(NEIGHBOR_SECTION_FIX));
        assert_eq!(x.id, consequence_id(&change, "dependent", &id(&x.block)));
        assert!(!x.already_written);
    }
    assert!(entry(&c, &n1).matched.is_empty());
    assert!(
        entry(&c, &n3).matched.is_empty(),
        "the cut scan matched nothing"
    );
    let m = &entry(&c, &n2).matched;
    assert_eq!(m.len(), 1, "{m:#?}");
    assert_eq!(
        (
            m[0].value.as_str(),
            m[0].field.as_str(),
            m[0].line,
            m[0].span
        ),
        ("30 days", "body", 1, [14, 21])
    );
    assert_eq!(entry(&c, &n2).matched_omitted, 0);
    assert_eq!(
        NEIGHBOR_SECTION_FIX,
        "a block in its section changed: check that it still holds, and update it if not; no repin is needed"
    );

    // the net proposal: one confirm item each, never an update; a reasoned confirm closes each
    // with no repin and no change to the neighbor
    let want_items = sorted(vec![
        ni(&a, &n1, PlanKind::Confirm),
        ni(&a, &n2, PlanKind::Confirm),
        ni(&a, &n3, PlanKind::Confirm),
    ]);
    assert_eq!(nitems(&r, "w"), want_items);
    let w = Scope::View("w".into());
    let versions = |r: &Rdy| {
        [&n1, &n2, &n3]
            .iter()
            .map(|b| version_of(r, &w, b))
            .collect::<Vec<_>>()
    };
    let (before, changes) = (versions(&r), r.read().included("w").len());
    let answers: Vec<PlanAnswerInput> = [&n1, &n2, &n3]
        .iter()
        .map(|b| PlanAnswerInput {
            item: pi(&a, b, PlanKind::Confirm),
            act: PlanAct::Confirm,
            reason: Some("it still holds".into()),
        })
        .collect();
    r.answer_plan(&writer(), "w", &answers).unwrap();
    let reply = r.read().view_plan("w").unwrap();
    for row in &reply.items {
        let PlanRow::Live(i) = row else { continue };
        assert_eq!(i.state, ItemState::Closed, "{i:#?}");
    }
    assert_eq!(versions(&r), before, "no repin, no rewrite");
    assert_eq!(r.read().included("w").len(), changes, "no change written");
}

#[test]
fn nb8_entity_update_requires_a_positive_removed_literal() {
    let (_d, mut r) = project();
    let ids = seed(
        &mut r,
        json!([
            doc("nb"),
            section("$s1", "$nb"),
            {"op": "create", "type": "entity", "id": "$e", "parent": "$s1", "fields": {"name": "Ledger"}},
            tied("$a", "$s1", "A keeps 30 days.", "$e"),
            section("$s2", "$nb"),
            tied("$p", "$s2", "P keeps 30 days.", "$e"),
            tied("$q", "$s2", "Q is fine.", "$e"),
            tied("$cut", "$s2", &long("keeps 30 days."), "$e"),
            doc("far"),
            section("$s3", "$far"),
            tied("$x", "$s3", "X keeps 30 days.", "$e"),
        ]),
    );
    let (a, p) = (ids.get("$a"), ids.get("$p"));
    let res = update(&mut r, "w", &a, "A keeps 60 days.");
    let c = report(&r, "w", &res.change);
    // only p: a positive match; q states nothing, the cut scan matched nothing, x is in another doc
    assert_eq!(
        nrows(&c),
        vec![(p.clone(), FollowUp::Update, a.clone())],
        "{c:#?}"
    );
    let e = entry(&c, &p);
    assert_eq!(e.kind, ConsequenceKind::Dependent);
    assert_eq!(e.fix.as_deref(), Some(NEIGHBOR_ENTITY_FIX));
    assert_eq!(
        NEIGHBOR_ENTITY_FIX,
        "update the block for the removed values"
    );
    assert_eq!(e.because.values, Some(vec!["30 days".to_string()]));
    assert_eq!(e.because.edges, Some(vec![NEIGHBOR_EDGE.to_string()]));
    assert_eq!(e.matched.len(), 1);
    assert_eq!(
        (e.matched[0].value.as_str(), e.matched[0].span),
        ("30 days", [8, 15])
    );
    assert_eq!(c.items.len(), 1, "{c:#?}");
    assert_eq!(nitems(&r, "w"), vec![ni(&a, &p, PlanKind::Update)]);
}

#[test]
fn nb8_neighbor_trigger_is_net_text_change() {
    let (_d, mut r) = project();
    let ids = seed(
        &mut r,
        json!([
            doc("trig"),
            section("$s", "$trig"),
            claim("$a", "$s", "A holds."),
            claim("$n", "$s", "N stands."),
            {"op": "create", "type": "code", "id": "$k", "parent": "$s", "body": "let a = 1;", "fields": {"lang": "rust"}},
            {"op": "create", "type": "flow", "id": "$f", "parent": "$s", "fields": {"name": "Flow one", "steps": ["Step one"]}},
            {"op": "create", "type": "requirement", "id": "$q", "parent": "$s", "body": "Q holds.", "fields": {"priority": "must"}},
            section("$s2", "$trig"),
            claim("$m", "$s2", "M holds."),
            section("$s3", "$trig"),
            {"op": "create", "type": "term", "id": "$t1", "parent": "$s3", "fields": {"name": "Thirty"}, "body": "A span."},
            {"op": "create", "type": "term", "id": "$t2", "parent": "$s3", "fields": {"name": "Sixty"}, "body": "Another span."},
            claim("$p", "$s3", "P waits 30 days per {{$t1}}."),
            claim("$p2", "$s3", "P2 notes {{$t2}}."),
            section("$s4", "$trig"),
            claim("$a2", "$s4", "A2 waits {{$t1|30 days}}."),
            claim("$n2", "$s4", "N2 stands."),
            section("$s5", "$trig"),
            claim("$a3", "$s5", "A3 is {{$t1|x}}."),
            claim("$n3", "$s5", "N3 stands."),
        ]),
    );
    let (a, n, k, f, q) = (
        ids.get("$a"),
        ids.get("$n"),
        ids.get("$k"),
        ids.get("$f"),
        ids.get("$q"),
    );
    let (s, s2, t1, t2, p) = (
        ids.get("$s"),
        ids.get("$s2"),
        ids.get("$t1"),
        ids.get("$t2"),
        ids.get("$p"),
    );
    let (a2, n2, a3, n3) = (
        ids.get("$a2"),
        ids.get("$n2"),
        ids.get("$a3"),
        ids.get("$n3"),
    );
    let section_of_a = |cause: &str| {
        let mut v: Vec<String> = [&a, &n, &k, &f, &q]
            .into_iter()
            .filter(|b| b.as_str() != cause)
            .cloned()
            .collect();
        v.sort();
        v
    };

    // equal text: no neighbor entry in the write's reply, no neighbor item for the block
    let quiet = |r: &mut Rdy, view: &str, ops: Value, edited: &str| {
        let res = write(r, view, ops).unwrap_or_else(|e| panic!("{view}: {e}"));
        let c = report(r, view, &res.change);
        assert_eq!(nrows(&c), vec![], "{view}: {c:#?}");
        assert!(
            nitems(r, view).iter().all(|i| i.0 != edited),
            "{view}: {:#?}",
            nitems(r, view)
        );
    };
    quiet(
        &mut r,
        "move",
        json!([{"op": "move", "id": a, "parent": s2}]),
        &a,
    );
    quiet(
        &mut r,
        "order",
        json!([{"op": "move", "id": n, "parent": s, "before": a}]),
        &n,
    );
    quiet(
        &mut r,
        "links",
        json!([{"op": "update", "id": a, "links": {"describes": [{"target": t2}]}}]),
        &a,
    );
    quiet(
        &mut r,
        "enum",
        json!([{"op": "update", "id": q, "fields": {"priority": "should"}}]),
        &q,
    );
    let t = brief_of(&r, &Scope::Canon, &[&id(&s)]).base.token;
    let res = write_cited(
        &mut r,
        "life",
        json!([{"op": "update", "id": s, "fields": {"lifecycle": "deprecated"}}]),
        Some(&t),
    )
    .unwrap();
    assert_eq!(nrows(&report(&r, "life", &res.change)), vec![]);
    assert_eq!(nitems(&r, "life"), vec![]);
    // a repin: t1 changes (a term takes no part), then p's pin follows it
    update(&mut r, "repin", &t1, "A longer span.");
    quiet(
        &mut r,
        "repin",
        json!([{"op": "repin", "id": p, "targets": [t1]}]),
        &p,
    );

    // body, code body and visible string-leaf edits count
    for (view, edited, ops) in [
        (
            "body",
            &a,
            json!([{"op": "update", "id": a, "body": "A holds firm."}]),
        ),
        (
            "code",
            &k,
            json!([{"op": "update", "id": k, "body": "let a = 2;"}]),
        ),
        (
            "steps",
            &f,
            json!([{"op": "update", "id": f, "fields": {"steps": ["Step two"]}}]),
        ),
        (
            "name",
            &f,
            json!([{"op": "update", "id": f, "fields": {"name": "Flow two"}}]),
        ),
    ] {
        let res = write(&mut r, view, ops).unwrap();
        let c = report(&r, view, &res.change);
        let got: Vec<String> = nrows(&c).into_iter().map(|x| x.0).collect();
        assert_eq!(got, section_of_a(edited), "{view}: {c:#?}");
        assert!(
            nrows(&c)
                .iter()
                .all(|x| x.1 == FollowUp::Confirm && x.2 == *edited)
        );
        let want: Vec<_> = section_of_a(edited)
            .iter()
            .map(|b| ni(edited, b, PlanKind::Confirm))
            .collect();
        assert_eq!(nitems(&r, view), want, "{view}");
    }

    // a display label change: a section confirm and an entity-only update that states it
    let res = update(
        &mut r,
        "label",
        &a2,
        "A2 waits {{T1|60 days}}.".replace("T1", &t1).as_str(),
    );
    let c = report(&r, "label", &res.change);
    assert_eq!(
        nrows(&c),
        by_block(vec![
            (n2.clone(), FollowUp::Confirm, a2.clone()),
            (p.clone(), FollowUp::Update, a2.clone()),
        ]),
        "{c:#?}"
    );
    let e = entry(&c, &p);
    assert_eq!(e.fix.as_deref(), Some(NEIGHBOR_ENTITY_FIX));
    assert_eq!(e.matched[0].value, "30 days");
    assert_eq!(
        entry(&c, &n2).because.values,
        Some(vec!["30 days".to_string()])
    );
    assert_eq!(
        nitems(&r, "label"),
        sorted(vec![
            ni(&a2, &n2, PlanKind::Confirm),
            ni(&a2, &p, PlanKind::Update),
        ])
    );
    // a reference that points elsewhere with the same label: a section confirm, no entity entry
    let res = update(
        &mut r,
        "target",
        &a3,
        "A3 is {{T2|x}}.".replace("T2", &t2).as_str(),
    );
    let c = report(&r, "target", &res.change);
    assert_eq!(
        nrows(&c),
        vec![(n3.clone(), FollowUp::Confirm, a3.clone())],
        "{c:#?}"
    );
    assert_eq!(entry(&c, &n3).because.values, Some(vec![]));
    assert_eq!(nitems(&r, "target"), vec![ni(&a3, &n3, PlanKind::Confirm)]);

    // a subject the write rewrites is no neighbor; nor in the net plan
    let res = write(
        &mut r,
        "both",
        json!([
            {"op": "update", "id": a, "body": "A holds both."},
            {"op": "update", "id": n, "body": "N stands both."}
        ]),
    )
    .unwrap();
    let c = report(&r, "both", &res.change);
    let mut want: Vec<String> = vec![k.clone(), f.clone(), q.clone()];
    want.sort();
    assert_eq!(nrows(&c).into_iter().map(|x| x.0).collect::<Vec<_>>(), want);
    assert!(
        nitems(&r, "both")
            .iter()
            .all(|i| i.1 != a && i.1 != n && (i.0 == a || i.0 == n))
    );
    assert_eq!(nitems(&r, "both").len(), 6);

    // a text edit reverted before propose: the net change has no item
    let res = update(&mut r, "rev", &a, "A holds for now.");
    assert_eq!(nrows(&report(&r, "rev", &res.change)).len(), 4);
    update(&mut r, "rev", &a, "A holds.");
    assert_eq!(nitems(&r, "rev"), vec![]);

    // a tracking field is store-only metadata: its edit is no text change
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), TRK_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: trk\n").unwrap();
    let mut t = Rdy::open(dir.path()).unwrap();
    let ids = seed(
        &mut t,
        json!([
            doc("trk"),
            section("$s", "$trk"),
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds.", "fields": {"owner": "ana"}},
            claim("$n", "$s", "N stands."),
        ]),
    );
    let ta = ids.get("$a");
    quiet(
        &mut t,
        "track",
        json!([{"op": "update", "id": ta, "fields": {"owner": "bo"}}]),
        &ta,
    );
}

/// A pack whose claims track `owner` (trk1): left out of the content hash and of the text.
const TRK_PACK: &str = "\
name: trk
version: \"1\"
extends: rdy-standard
block_types:
  claim:
    prefix: c
    traits: [normative, describes]
    body: required
    fields:
      owner: { kind: str }
    tracking: [owner]
";

#[test]
fn nb8_create_confirms_only_unchanged_baseline_section_neighbors() {
    let (_d, mut r) = project();
    let ids = seed(
        &mut r,
        json!([
            doc("nb"),
            section("$s", "$nb"),
            {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger"}},
            tied("$a", "$s", "A keeps 30 days.", "$e"),
            claim("$b", "$s", "B stands."),
            section("$s2", "$nb"),
            tied("$p", "$s2", "P keeps 30 days.", "$e"),
            section("$empty", "$nb"),
        ]),
    );
    let (s, e, a, b, p, empty) = (
        ids.get("$s"),
        ids.get("$e"),
        ids.get("$a"),
        ids.get("$b"),
        ids.get("$p"),
        ids.get("$empty"),
    );
    let res = write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "claim", "id": "$x", "parent": s, "body": "X keeps 30 days.",
             "links": {"describes": [{"target": e}]}},
            {"op": "create", "type": "claim", "parent": empty, "body": "In an empty section."},
            doc("fresh"),
            section("$fs", "$fresh"),
            claim("$f1", "$fs", "F1 stands."),
            claim("$f2", "$fs", "F2 stands."),
        ]),
    )
    .unwrap();
    let x = res.created["$x"].clone();
    let c = report(&r, "w", &res.change);
    // a and b confirm; p, tied to the same entity in another section, has no entry
    assert_eq!(
        nrows(&c),
        by_block(vec![
            (a.clone(), FollowUp::Confirm, x.clone()),
            (b.clone(), FollowUp::Confirm, x.clone()),
        ]),
        "{c:#?}"
    );
    assert!(c.items.iter().all(|i| i.block != p));
    for i in &c.items {
        // TASK-1067 (b): every neighbor row carries values; a create's are empty
        assert_eq!(
            i.because.values,
            Some(vec![]),
            "a create carries no literal evidence"
        );
        assert!(i.matched.is_empty());
        assert_eq!(i.fix.as_deref(), Some(NEIGHBOR_SECTION_FIX));
    }
    assert_eq!(
        nitems(&r, "w"),
        sorted(vec![
            ni(&x, &a, PlanKind::Confirm),
            ni(&x, &b, PlanKind::Confirm),
        ])
    );
}

#[test]
fn nb8_deletion_confirms_baseline_section_neighbors_only() {
    let (_d, mut r) = project();
    let ids = seed(
        &mut r,
        json!([
            doc("nb"),
            section("$s", "$nb"),
            {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger"}},
            tied("$a", "$s", "A keeps 30 days.", "$e"),
            claim("$b", "$s", "B stands."),
            claim("$c", "$s", "C stands."),
            section("$s2", "$nb"),
            tied("$p", "$s2", "P keeps 30 days.", "$e"),
        ]),
    );
    let (s, a, b, c, p) = (
        ids.get("$s"),
        ids.get("$a"),
        ids.get("$b"),
        ids.get("$c"),
        ids.get("$p"),
    );
    let res = write(&mut r, "w", json!([{"op": "delete", "id": a}])).unwrap();
    let rep = report(&r, "w", &res.change);
    assert_eq!(
        nrows(&rep),
        by_block(vec![
            (b.clone(), FollowUp::Confirm, a.clone()),
            (c.clone(), FollowUp::Confirm, a.clone()),
        ]),
        "{rep:#?}"
    );
    assert!(
        rep.items.iter().all(|i| i.block != p),
        "p states the literal, but no entity entry"
    );
    for i in &rep.items {
        // TASK-1067 (b): a delete's values are empty, never absent
        assert_eq!(i.because.values, Some(vec![]));
        assert!(i.matched.is_empty());
    }
    assert_eq!(
        nitems(&r, "w"),
        sorted(vec![
            ni(&a, &b, PlanKind::Confirm),
            ni(&a, &c, PlanKind::Confirm),
        ])
    );
    // a block deleted with one sibling: only the survivor confirms, from the least cause
    let res = write(
        &mut r,
        "two",
        json!([{"op": "delete", "id": a}, {"op": "delete", "id": b}]),
    )
    .unwrap();
    assert_eq!(
        nrows(&report(&r, "two", &res.change)),
        vec![(c.clone(), FollowUp::Confirm, a.clone().min(b.clone()))]
    );
    assert_eq!(
        nitems(&r, "two"),
        sorted(vec![
            ni(&a, &c, PlanKind::Confirm),
            ni(&b, &c, PlanKind::Confirm),
        ])
    );
    // create then delete: the delete's write asks the siblings, the net change owes nothing
    let x = write(&mut r, "cd", json!([claim("$x", &s, "X is brief.")]))
        .unwrap()
        .created["$x"]
        .clone();
    let res = write(&mut r, "cd", json!([{"op": "delete", "id": x}])).unwrap();
    assert_eq!(nrows(&report(&r, "cd", &res.change)).len(), 3);
    assert_eq!(nitems(&r, "cd"), vec![]);
}

#[test]
fn nb8_declared_edges_win_without_losing_plan_causes() {
    let (_d, mut r) = project();
    let ids = seed(
        &mut r,
        json!([
            doc("nb"),
            section("$s", "$nb"),
            claim("$a", "$s", "A waits 30 days."),
            claim("$c", "$s", "C stands."),
            claim("$b", "$s", "B builds on {{$a}}."),
            claim("$p", "$s", "P waits 30 days per {{$a}}."),
        ]),
    );
    let (a, b, c, p) = (ids.get("$a"), ids.get("$b"), ids.get("$c"), ids.get("$p"));
    let res = write(
        &mut r,
        "w",
        json!([
            {"op": "update", "id": a, "body": "A waits 60 days."},
            {"op": "update", "id": c, "body": "C stands firm."}
        ]),
    )
    .unwrap();
    let rep = report(&r, "w", &res.change);
    // one entry per block: the declared one, its id and its cause
    assert_eq!(rep.items.len(), 2, "{rep:#?}");
    assert_eq!(nrows(&rep), vec![]);
    let change: Hash = res.change.parse().unwrap();
    let eb = entry(&rep, &b);
    assert_eq!(
        (
            eb.kind,
            eb.because.edge.as_str(),
            eb.because.changed.as_str()
        ),
        (ConsequenceKind::Suspect, "references", a.as_str())
    );
    assert_eq!(eb.id, consequence_id(&change, "suspect", &id(&b)));
    let ep = entry(&rep, &p);
    assert_eq!(ep.follow_up, FollowUp::Update);
    assert_eq!(ep.because.edge, "references");

    // the plan keeps every net cause: the declared edge wins one identity (though `neighbor`
    // sorts first), a different kind keeps its own item, and c's neighbor items stand
    assert!(NEIGHBOR_EDGE < "references");
    let items = plan(&r, "w");
    let edge_of = |cause: &str, subject: &str, kind: PlanKind| {
        items
            .iter()
            .find(|i| i.0 == cause && i.1 == subject && i.2 == kind)
            .unwrap_or_else(|| panic!("no item ({cause}, {subject}, {kind:?}): {items:#?}"))
            .3
            .clone()
    };
    assert_eq!(edge_of(&a, &b, PlanKind::Confirm), "references");
    assert_eq!(edge_of(&a, &p, PlanKind::Update), "references");
    assert_eq!(edge_of(&a, &p, PlanKind::Confirm), NEIGHBOR_EDGE);
    assert_eq!(edge_of(&c, &b, PlanKind::Confirm), NEIGHBOR_EDGE);
    assert_eq!(edge_of(&c, &p, PlanKind::Confirm), NEIGHBOR_EDGE);
    assert_eq!(items.len(), 5, "{items:#?}");
}

#[test]
fn nb8_net_subset_and_canonical_plans_use_the_same_neighbor_rule() {
    let (_d, mut r) = project();
    let ids = seed(
        &mut r,
        json!([
            doc("nb"),
            section("$s", "$nb"),
            claim("$a", "$s", "A holds."),
            claim("$b", "$s", "B stands."),
            section("$t", "$nb"),
            claim("$c", "$t", "C holds."),
            claim("$d", "$t", "D stands."),
        ]),
    );
    let (s, a, b, c, d) = (
        ids.get("$s"),
        ids.get("$a"),
        ids.get("$b"),
        ids.get("$c"),
        ids.get("$d"),
    );
    let ca = update(&mut r, "w", &a, "A holds firm.").change;
    update(&mut r, "w", &c, "C holds firm.");
    let full = sorted(vec![
        ni(&a, &b, PlanKind::Confirm),
        ni(&c, &d, PlanKind::Confirm),
    ]);
    assert_eq!(nitems(&r, "w"), full);
    // the subset proposal: only its own change's neighbors
    let subset = [ca.clone()];
    let info = r
        .propose_plan(
            &writer(),
            rdy_engine::plan::ProposePlanRequest {
                changes: Some(&subset),
                summary: Some(TEST_SUMMARY),
                ..rdy_engine::plan::ProposePlanRequest::new("w", "only a")
            },
        )
        .unwrap();
    let read = |r: &Rdy, pref: &str| {
        let q = PlanQuery {
            proposal: Some(pref.to_string()),
            max_tokens: Some(100_000),
            ..Default::default()
        };
        items_of(&r.read().change_plan(&q, "w").unwrap())
            .into_iter()
            .filter(|i| i.3 == NEIGHBOR_EDGE)
            .map(|i| (i.0, i.1, i.2))
            .collect::<Vec<_>>()
    };
    assert_eq!(read(&r, &info.r#ref), vec![ni(&a, &b, PlanKind::Confirm)]);

    // a view whose base is older than canon: its write's reply read the state it published,
    // which had no z; its net change compares canon, where z now stands, with the final
    // candidate
    let old = update(&mut r, "old", &a, "A holds anew.");
    let rep = report(&r, "old", &old.change);
    assert_eq!(
        nrows(&rep),
        vec![(b.clone(), FollowUp::Confirm, a.clone())],
        "{rep:#?}"
    );
    let z = write(&mut r, "z", json!([claim("$z", &s, "Z joins.")]))
        .unwrap()
        .created["$z"]
        .clone();
    land(&mut r, "z").unwrap();
    assert_eq!(
        nitems(&r, "old"),
        sorted(vec![
            ni(&a, &b, PlanKind::Confirm),
            ni(&a, &z, PlanKind::Confirm),
        ])
    );

    // create then edit: the new block's neighbors confirm, as for a create
    let x = write(&mut r, "ce", json!([claim("$x", &s, "X is new.")]))
        .unwrap()
        .created["$x"]
        .clone();
    update(&mut r, "ce", &x, "X is newer.");
    let want = sorted(vec![
        ni(&x, &a, PlanKind::Confirm),
        ni(&x, &b, PlanKind::Confirm),
        ni(&x, &z, PlanKind::Confirm),
    ]);
    assert_eq!(nitems(&r, "ce"), want);
    // edit then revert: nothing
    update(&mut r, "er", &b, "B moves.");
    update(&mut r, "er", &b, "B stands.");
    assert_eq!(nitems(&r, "er"), vec![]);
}

/// TASK-1067 (a): a neighbor row names the least edited cause when the change edits a block that
/// owes the subject a neighbor item; a create beside it never wins, whatever the id order. The
/// edited cause's id is seeded both below and above the created one.
#[test]
fn nb8_an_edit_beside_a_create_names_the_edit_whatever_the_ids() {
    for edit_low in [true, false] {
        let (_d, mut r) = project();
        let ids = seed(
            &mut r,
            json!([
                doc("nb"),
                section("$s", "$nb"),
                claim("$a", "$s", "A stands.")
            ]),
        );
        let (s, a) = (ids.get("$s"), ids.get("$a"));
        let prefix = a.split_once('-').unwrap().0.to_string();
        let (low, high) = (format!("{prefix}-00000"), format!("{prefix}-ZZZZZ"));
        let (edited, created) = if edit_low { (low, high) } else { (high, low) };
        seed(&mut r, json!([claim(&edited, &s, "E keeps 30 days.")]));
        let res = write(
            &mut r,
            "w",
            json!([
                {"op": "update", "id": edited, "body": "E keeps 60 days."},
                claim(&created, &s, "N stands."),
            ]),
        )
        .unwrap();
        assert_eq!(res.created.values().next(), Some(&created), "{res:#?}");
        let c = report(&r, "w", &res.change);
        assert_eq!(
            nrows(&c),
            vec![(a.clone(), FollowUp::Confirm, edited.clone())],
            "edit_low {edit_low}: {c:#?}"
        );
        assert!(
            entry(&c, &a).because.values.is_some(),
            "edit_low {edit_low}: an edited cause carries its values"
        );
        // the plan keeps both causes
        assert_eq!(
            nitems(&r, "w"),
            sorted(vec![
                ni(&edited, &a, PlanKind::Confirm),
                ni(&created, &a, PlanKind::Confirm),
            ]),
            "edit_low {edit_low}"
        );
    }
}
