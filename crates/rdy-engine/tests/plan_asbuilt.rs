//! ab3 (D-WJ0SJ, D-YAY1G): the as-built confirm items of a change plan. A block whose override
//! disagrees with its decisions after a version gets one confirm item, with edge `built`, for
//! each cause the version gives it (the block, the override's holder, a decision whose `built`
//! changed); its subject is the holder. A reasoned confirm, sealed when the version lands,
//! silences the warning until an input changes.
mod common;

use rdy_engine::as_built::{AS_BUILT_FIX_NO_ITEM, AS_BUILT_FIX_OPEN_ITEM};
use rdy_engine::plan::compute::{BUILT_CONFIRM_DOMAIN, BUILT_EDGE, built_cause_hash};
use rdy_engine::plan::{ItemState, PlanAnswerInput, PlanQuery, PlanRow, W_PLAN_ANSWER_LAPSED};
use rdy_engine::*;
use rdy_model::plan::{PlanAct, PlanKind, PlanOutcome, item_display, item_hash};
use rdy_model::{BlockId, EditorialClass, Hash, Justification, Role};
use serde_json::{Value, json};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A pack whose decisions have one more tracking field, `disposition` (trk1), beside `built`.
const TRK_PACK: &str = "\
name: trk
version: \"1\"
extends: rdy-standard
block_types:
  decision:
    prefix: D
    traits: [normative]
    fields:
      disposition: { kind: str }
    tracking: [disposition]
";

/// A project on [`TRK_PACK`].
fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), TRK_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: trk\n").unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    (dir, r)
}

fn writer() -> Session {
    Session::human("human:qa".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}

fn write_why(
    r: &mut Rdy,
    view: &str,
    ops: Value,
    reason: Option<&str>,
) -> Result<WriteResult, RdyError> {
    let ops: Vec<Value> = serde_json::from_value(ops).unwrap();
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: parse_ops(ops).unwrap(),
            justification: reason.map(|w| Justification::Editorial {
                class: EditorialClass::Clarify,
                reason: w.into(),
            }),
            message: String::new(),
        },
    )
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    write_why(r, view, ops, None).unwrap_or_else(|e| panic!("write {view}: {e}"))
}

/// An editorial write: a tracking edit of an accepted decision needs one.
fn edit(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    write_why(r, view, ops, Some("the record follows the release"))
        .unwrap_or_else(|e| panic!("write {view}: {e}"))
}

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    common::answer_open_items(r, &writer(), view);
    r.propose(&writer(), view, view, Some(common::TEST_SUMMARY))
        .unwrap()
}

fn land(r: &mut Rdy, view: &str) -> String {
    let p = propose(r, view);
    accept_deferrals(r, &p);
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id).unwrap()
}

/// Lands `view` straight through the store: no acceptance change, no change plan, no seal.
fn land_raw(r: &mut Rdy, view: &str) {
    let by = approver().provenance();
    let store = r.store_mut();
    let prop = store.propose(view, view, by.clone()).unwrap();
    store
        .land_atomic(rdy_store::LandRequest {
            proposal: prop,
            accept: None,
            approver: by,
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap();
    r.invalidate();
}

/// Accepts the deferral card of version `p` when it has one (D-AAQ9Z), as the approver.
fn accept_deferrals(r: &mut Rdy, p: &ProposalInfo) {
    let card = format!("defer:{}", p.r#ref);
    let q = r
        .read()
        .pending(&rdy_engine::pending::PendingQuery {
            person: approver().actor.clone(),
            session_ref: None,
            max_tokens: rdy_engine::pending::CARD_MAX_TOKENS,
        })
        .unwrap();
    if !q.items.iter().any(|i| i.r#ref == card) {
        return;
    }
    let s = approver();
    r.answer(
        &s,
        &s.actor,
        rdy_engine::answer::AnswerRequest {
            r#ref: card.clone(),
            option: "o1".into(),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("accept {card}: {e}"));
}

/// One as-built item as a live plan row shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    cause: String,
    subject: String,
    state: ItemState,
    how: Option<PlanOutcome>,
}

fn row(cause: &str, subject: &str, state: ItemState, how: Option<PlanOutcome>) -> Row {
    Row {
        cause: cause.into(),
        subject: subject.into(),
        state,
        how,
    }
}

fn open(cause: &str, subject: &str) -> Row {
    row(cause, subject, ItemState::Open, None)
}

fn updated(cause: &str, subject: &str) -> Row {
    row(
        cause,
        subject,
        ItemState::Closed,
        Some(PlanOutcome::Updated),
    )
}

/// The as-built rows of `rows`, in item order. Each is a confirm item with edge `built` and the
/// item id of (cause, subject, confirm).
fn built_rows(rows: &[PlanRow]) -> Vec<Row> {
    rows.iter()
        .filter_map(|r| match r {
            PlanRow::Live(i) if i.edge == BUILT_EDGE => {
                assert_eq!(i.kind, PlanKind::Confirm, "{i:?}");
                assert_eq!(
                    i.id,
                    item_display(&item_hash(&id(&i.cause), &i.subject, PlanKind::Confirm)),
                    "{i:?}"
                );
                Some(Row {
                    cause: i.cause.clone(),
                    subject: i.subject.clone(),
                    state: i.state,
                    how: i.how,
                })
            }
            _ => None,
        })
        .collect()
}

fn view_rows(r: &Rdy, view: &str) -> Vec<PlanRow> {
    r.read().view_plan(view).unwrap().items
}

fn built_of(r: &Rdy, view: &str) -> Vec<Row> {
    built_rows(&view_rows(r, view))
}

fn sorted(mut v: Vec<Row>) -> Vec<Row> {
    v.sort_by_key(|r| {
        (
            r.cause.clone(),
            r.subject.clone(),
            format!("{:?}", (r.state, r.how)),
        )
    });
    v
}

/// The full `pi-` id of the as-built item (cause, subject).
fn item(cause: &str, subject: &str) -> String {
    format!("pi-{}", item_hash(&id(cause), subject, PlanKind::Confirm))
}

fn answer(r: &mut Rdy, view: &str, answers: &[(&str, &str, PlanAct, Option<&str>)]) {
    let inputs: Vec<PlanAnswerInput> = answers
        .iter()
        .map(|(c, s, act, reason)| PlanAnswerInput {
            item: item(c, s),
            act: *act,
            reason: reason.map(str::to_string),
        })
        .collect();
    r.answer_plan(&writer(), view, &inputs)
        .unwrap_or_else(|e| panic!("answer {view}: {e}"));
}

fn live<'a>(rows: &'a [PlanRow], cause: &str, subject: &str) -> &'a rdy_engine::plan::LiveItem {
    rows.iter()
        .find_map(|r| match r {
            PlanRow::Live(i)
                if i.cause == cause && i.subject == subject && i.edge == BUILT_EDGE =>
            {
                Some(i)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no as-built item ({cause}, {subject}): {rows:#?}"))
}

#[test]
fn ab3_built_changes_create_only_direct_override_confirms() {
    let (_dir, mut r) = project();
    let seed = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Plain"}},
            {"op": "create", "type": "section", "id": "$p", "parent": "$doc", "fields": {"title": "Planned", "planned": true}},
            {"op": "create", "type": "section", "id": "$q", "parent": "$doc", "fields": {"title": "Later"}},
            {"op": "create", "type": "component", "id": "$ent", "parent": "$s", "fields": {"name": "engine"}},
            {"op": "create", "type": "decision", "id": "$dp", "fields": {"statement": "Plan P.", "status": "proposed"}},
            {"op": "create", "type": "decision", "id": "$dv", "fields": {"statement": "Ship V.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$dn", "fields": {"statement": "Process N.", "status": "proposed", "built": "none"}},
            {"op": "create", "type": "decision", "id": "$da", "fields": {"statement": "Old A.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$di", "fields": {"statement": "Ship I.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$dd", "fields": {"statement": "Ship D.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$dt1", "fields": {"statement": "Ship T1.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$dt2", "fields": {"statement": "Ship T2.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$ds", "fields": {"statement": "Ship S.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$du", "fields": {"statement": "Ship U.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$dx", "fields": {"statement": "Ship X.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$dr", "fields": {"statement": "Reach R.", "status": "proposed", "built": "1.0.0"}, "links": {"affects": ["$ent"]}},
            {"op": "create", "type": "decision", "id": "$dn2", "fields": {"statement": "Process N2.", "status": "proposed", "built": "none"}},
            {"op": "create", "type": "decision", "id": "$dk", "fields": {"statement": "Keep K.", "status": "proposed", "built": "1.0.0"}},
            // planned-to-version: own planned over planned, quiet now
            {"op": "create", "type": "claim", "id": "$ca", "parent": "$s", "body": "A.", "fields": {"planned": true}, "links": {"decided_by": ["$dp"]}},
            // version-to-version: own since 1.0.0 over since 1.0.0, quiet now
            {"op": "create", "type": "claim", "id": "$cb", "parent": "$s", "body": "B.", "fields": {"since": "1.0.0"}, "links": {"decided_by": ["$dv"]}},
            // none-to-version and absent-to-version: an unversioned derivation, quiet now
            {"op": "create", "type": "claim", "id": "$cc", "parent": "$s", "body": "C.", "fields": {"planned": true}, "links": {"decided_by": ["$dn"]}},
            {"op": "create", "type": "claim", "id": "$cd", "parent": "$s", "body": "D.", "fields": {"planned": true}, "links": {"decided_by": ["$da"]}},
            // implements, and a duplicate decided_by and implements link
            {"op": "create", "type": "claim", "id": "$ch", "parent": "$s", "body": "H.", "fields": {"since": "0.1.0"}, "links": {"implements": ["$di"]}},
            {"op": "create", "type": "claim", "id": "$cdup", "parent": "$s", "body": "Dup.", "fields": {"planned": true}, "links": {"decided_by": ["$dd"], "implements": ["$dd"]}},
            // two causes
            {"op": "create", "type": "claim", "id": "$cj", "parent": "$s", "body": "J.", "fields": {"planned": true}, "links": {"decided_by": ["$dt1", "$dt2"]}},
            // same value, changes undone and disposition only: a mismatch that stays as it is
            {"op": "create", "type": "claim", "id": "$ce", "parent": "$s", "body": "E.", "fields": {"planned": true}, "links": {"decided_by": ["$ds", "$du"]}},
            // inherited-only content: two children of the planned section
            {"op": "create", "type": "claim", "id": "$k1", "parent": "$p", "body": "K1.", "links": {"decided_by": ["$dx"]}},
            {"op": "create", "type": "claim", "id": "$k2", "parent": "$p", "body": "K2.", "links": {"implements": ["$dx"]}},
            // unrelated reach: describes an entity the changed decision affects
            {"op": "create", "type": "claim", "id": "$cm", "parent": "$s", "body": "M.", "fields": {"planned": true}, "links": {"describes": ["$ent"], "decided_by": ["$dk"]}},
            // under a section that turns planned in the version
            {"op": "create", "type": "claim", "id": "$cq", "parent": "$q", "body": "Q.", "links": {"decided_by": ["$ds"]}},
            // a claim that gains an own override in the version
            {"op": "create", "type": "claim", "id": "$co", "parent": "$s", "body": "O.", "links": {"decided_by": ["$ds"]}},
            // overrides over an unversioned derivation: quiet whatever the version does
            {"op": "create", "type": "note", "id": "$np", "parent": "$s", "body": "N.", "fields": {"planned": true}},
            {"op": "create", "type": "claim", "id": "$cn2", "parent": "$s", "body": "N2.", "fields": {"planned": true}, "links": {"decided_by": ["$dn2"]}}
        ]),
    );
    let c = |k: &str| seed.created[k].clone();
    // an old record: a decision with no built
    write(
        &mut r,
        "seed",
        json!([{"op": "update", "id": c("$da"), "unset": ["built"]}]),
    );
    land(&mut r, "seed");

    // the version: one editorial write per decision
    for (k, field, value) in [
        ("$dp", "built", "1.2.0"),
        ("$dv", "built", "1.1.0"),
        ("$dn", "built", "1.0.0"),
        ("$da", "built", "v1.0.0"),
        ("$di", "built", "2.0.0"),
        ("$dd", "built", "2.0.0"),
        ("$dt1", "built", "1.1.0"),
        ("$dt2", "built", "1.2.0"),
        ("$du", "built", "2.0.0"),
        ("$dx", "built", "1.1.0"),
        ("$dr", "built", "3.0.0"),
        // disposition only
        ("$dn2", "disposition", "Kept as process."),
    ] {
        write_why(
            &mut r,
            "v",
            json!([{"op": "update", "id": c(k), "fields": {field: value}}]),
            Some("the record follows the release"),
        )
        .unwrap_or_else(|e| panic!("{k}: {e}"));
    }
    // the same value is no tracking edit: an accepted decision refuses it, so it gives nothing
    let same = write_why(
        &mut r,
        "v",
        json!([{"op": "update", "id": c("$ds"), "fields": {"built": "1.0.0"}}]),
        Some("the record follows the release"),
    )
    .unwrap_err();
    assert_eq!(same.code, E_FROZEN);
    edit(
        &mut r,
        "v",
        json!([{"op": "update", "id": c("$ds"), "fields": {"disposition": "Built in 1.0.0."}}]),
    );
    // the undone change
    edit(
        &mut r,
        "v",
        json!([{"op": "update", "id": c("$du"), "fields": {"built": "1.0.0"}}]),
    );
    // content: an own override written, a section that turns planned, an unversioned override
    // edited, and a new decision cited by a new claim
    let w = write(
        &mut r,
        "v",
        json!([
            {"op": "update", "id": c("$co"), "fields": {"since": "0.5.0"}},
            {"op": "update", "id": c("$q"), "fields": {"planned": true}},
            {"op": "update", "id": c("$np"), "body": "N, edited."},
            {"op": "update", "id": c("$cn2"), "body": "N2, edited."},
            {"op": "create", "type": "decision", "id": "$dnew", "fields": {"statement": "New.", "status": "proposed", "built": "3.0.0"}},
            {"op": "create", "type": "claim", "id": "$cnew", "parent": c("$s"), "body": "New claim.", "fields": {"planned": true}, "links": {"decided_by": ["$dnew"]}}
        ]),
    );
    let cnew = w.created["$cnew"].clone();
    let want = sorted(vec![
        // planned-to-version
        open(&c("$dp"), &c("$ca")),
        // version-to-version
        open(&c("$dv"), &c("$cb")),
        // none-to-version and absent-to-version
        open(&c("$dn"), &c("$cc")),
        open(&c("$da"), &c("$cd")),
        // implements; one item for the duplicate links
        open(&c("$di"), &c("$ch")),
        open(&c("$dd"), &c("$cdup")),
        // two causes, two items
        open(&c("$dt1"), &c("$cj")),
        open(&c("$dt2"), &c("$cj")),
        // inherited: one item for both children, the section its subject
        open(&c("$dx"), &c("$p")),
        // an own override written: the block is its cause and subject
        open(&c("$co"), &c("$co")),
        // a section turned planned: the section is the cause and subject
        open(&c("$q"), &c("$q")),
        // a new decision gives no decision cause: the new claim does
        open(&cnew, &cnew),
    ]);
    let rows = view_rows(&r, "v");
    let got = built_rows(&rows);
    assert_eq!(sorted(got.clone()), want);
    // deterministic order: the rows are in full item-hash order, every read the same
    let ids: Vec<Hash> = got
        .iter()
        .map(|x| item_hash(&id(&x.cause), &x.subject, PlanKind::Confirm))
        .collect();
    let mut by_hash = ids.clone();
    by_hash.sort();
    assert_eq!(ids, by_hash);
    assert_eq!(built_of(&r, "v"), got);
    // built alone gives no other item: no update, suspect, reach or code item from a decision
    // whose content is unchanged
    let changed: Vec<String> = [
        "$dp", "$dv", "$dn", "$da", "$di", "$dd", "$dt1", "$dt2", "$ds", "$du", "$dx", "$dr",
        "$dn2",
    ]
    .iter()
    .map(|k| c(k))
    .collect();
    for row in &rows {
        let PlanRow::Live(i) = row else { continue };
        if i.edge != BUILT_EDGE {
            assert!(
                !changed.contains(&i.cause),
                "a built change gave a normal item: {i:?}"
            );
        }
    }
    // the cause hash binds the decision's built: domain, content hash, the exact string
    assert_eq!(BUILT_CONFIRM_DOMAIN, b"rdy-asbuilt-confirm-1\0");
    let content = Hash::of(b"content");
    let mut bytes = b"rdy-asbuilt-confirm-1\0".to_vec();
    bytes.extend_from_slice(&content.0);
    bytes.push(0);
    assert_eq!(built_cause_hash(&content, None), Hash::of(&bytes));
    let mut bytes = b"rdy-asbuilt-confirm-1\0".to_vec();
    bytes.extend_from_slice(&content.0);
    bytes.push(1);
    bytes.extend_from_slice(&6u32.to_le_bytes());
    bytes.extend_from_slice(b"v1.0.0");
    assert_eq!(built_cause_hash(&content, Some("v1.0.0")), Hash::of(&bytes));
    assert_ne!(
        built_cause_hash(&content, Some("v1.0.0")),
        built_cause_hash(&content, Some("1.0.0"))
    );
}

#[test]
fn ab3_confirm_answers_bind_build_fact_and_override() {
    let (_dir, mut r) = project();
    let seed = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Plain"}},
            {"op": "create", "type": "decision", "id": "$d", "fields": {"statement": "Ship D.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$e", "fields": {"statement": "Ship E.", "status": "proposed", "built": "1.0.0"}},
            // a mismatch in canon: own planned over since 1.0.0
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C.", "fields": {"planned": true}, "links": {"decided_by": ["$d"]}},
            {"op": "create", "type": "claim", "id": "$f", "parent": "$s", "body": "F.", "fields": {"planned": true}, "links": {"decided_by": ["$e"]}}
        ]),
    );
    let k = |x: &str| seed.created[x].clone();
    let (d, e, c, f) = (k("$d"), k("$e"), k("$c"), k("$f"));
    land(&mut r, "seed");

    // a built change opens the item; a confirm needs a reason and closes it
    edit(
        &mut r,
        "v",
        json!([{"op": "update", "id": d, "fields": {"built": "1.1.0"}}]),
    );
    assert_eq!(built_of(&r, "v"), vec![open(&d, &c)]);
    let err = r
        .answer_plan(
            &writer(),
            "v",
            &[PlanAnswerInput {
                item: item(&d, &c),
                act: PlanAct::Confirm,
                reason: None,
            }],
        )
        .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT);
    assert_eq!(
        err.message,
        "confirm needs a reason of 1 to 2000 characters"
    );
    // done does not answer a confirm item
    let err = r
        .answer_plan(
            &writer(),
            "v",
            &[PlanAnswerInput {
                item: item(&d, &c),
                act: PlanAct::Done,
                reason: None,
            }],
        )
        .unwrap_err();
    assert_eq!(err.message, "done does not answer a confirm item");
    answer(
        &mut r,
        "v",
        &[(
            &d,
            &c,
            PlanAct::Confirm,
            Some("the page stays planned on purpose"),
        )],
    );
    let rows = view_rows(&r, "v");
    let i = live(&rows, &d, &c);
    assert_eq!(i.state, ItemState::Closed);
    assert_eq!(i.answer.as_ref().unwrap().act, PlanAct::Confirm);

    // an unrelated disposition edit keeps the answer
    edit(
        &mut r,
        "v",
        json!([{"op": "update", "id": d, "fields": {"disposition": "Shipped."}}]),
    );
    assert_eq!(live(&view_rows(&r, "v"), &d, &c).state, ItemState::Closed);
    // a repeated built change lapses it
    edit(
        &mut r,
        "v",
        json!([{"op": "update", "id": d, "fields": {"built": "1.2.0"}}]),
    );
    let reply = r.read().view_plan("v").unwrap();
    assert_eq!(live(&reply.items, &d, &c).state, ItemState::Open);
    assert!(
        reply
            .notes
            .iter()
            .any(|n| n.code == W_PLAN_ANSWER_LAPSED && n.message.contains(&d)),
        "{:?}",
        reply.notes
    );
    // a changed override moves the subject: the answer lapses, and the block is a new cause
    write(
        &mut r,
        "v",
        json!([{"op": "update", "id": c, "unset": ["planned"], "fields": {"since": "0.9.0"}}]),
    );
    assert_eq!(
        sorted(built_of(&r, "v")),
        sorted(vec![open(&d, &c), open(&c, &c)])
    );
    // removing the override closes both as updated
    write(
        &mut r,
        "v",
        json!([{"op": "update", "id": c, "unset": ["since"]}]),
    );
    assert_eq!(
        sorted(built_of(&r, "v")),
        sorted(vec![updated(&d, &c), updated(&c, &c)])
    );

    // an unlink closes as updated
    write(
        &mut r,
        "u",
        json!([{"op": "update", "id": f, "links": {"decided_by": []}}]),
    );
    assert_eq!(built_of(&r, "u"), vec![updated(&f, &f)]);
    // a deleted subject closes as updated
    edit(
        &mut r,
        "x",
        json!([{"op": "update", "id": e, "fields": {"built": "2.0.0"}}]),
    );
    assert_eq!(built_of(&r, "x"), vec![open(&e, &f)]);
    write(&mut r, "x", json!([{"op": "delete", "id": f}]));
    assert_eq!(
        sorted(built_of(&r, "x")),
        sorted(vec![updated(&e, &f), updated(&f, &f)])
    );

    // a normal confirm of the same id merges: one item, edge built, open until a reasoned
    // answer, and a repin alone cannot clear it. A decision a landing straight through the store
    // left proposed can change its words, so the citing block gets a normal item from it too
    let raw = write(
        &mut r,
        "raw",
        json!([
            {"op": "create", "type": "decision", "id": "$dm", "fields": {"statement": "Ship M.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "claim", "id": "$cm", "parent": k("$s"), "body": "M builds on {{$dm}}.", "fields": {"planned": true}, "links": {"decided_by": ["$dm"]}}
        ]),
    );
    let (dm, cm) = (raw.created["$dm"].clone(), raw.created["$cm"].clone());
    land_raw(&mut r, "raw");
    // the words alone: a normal confirm item of the same id, which a repin closes
    write(
        &mut r,
        "n",
        json!([{"op": "update", "id": dm, "fields": {"statement": "Ship M in full."}}]),
    );
    let normal = |r: &Rdy, view: &str| {
        view_rows(r, view)
            .into_iter()
            .find_map(|row| match row {
                PlanRow::Live(i)
                    if i.cause == dm && i.subject == cm && i.kind == PlanKind::Confirm =>
                {
                    Some(i)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no normal confirm in {view}"))
    };
    let n = normal(&r, "n");
    assert_ne!(n.edge, BUILT_EDGE);
    assert_eq!(
        n.id,
        item_display(&item_hash(&id(&dm), &cm, PlanKind::Confirm))
    );
    write(
        &mut r,
        "n",
        json!([{"op": "repin", "id": cm, "targets": [dm]}]),
    );
    assert_eq!(normal(&r, "n").how, Some(PlanOutcome::Repinned));
    write(
        &mut r,
        "m",
        json!([{"op": "update", "id": dm, "fields": {"statement": "Ship M in full.", "built": "2.0.0"}}]),
    );
    let rows = view_rows(&r, "m");
    let same: Vec<_> = rows
        .iter()
        .filter_map(|r| match r {
            PlanRow::Live(i) if i.cause == dm && i.subject == cm && i.kind == PlanKind::Confirm => {
                Some(i.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(same.len(), 1, "{rows:#?}");
    assert_eq!(same[0].edge, BUILT_EDGE);
    assert_eq!(same[0].state, ItemState::Open);
    // the normal item alone closes on a repin; the merged one stays open
    write(
        &mut r,
        "m",
        json!([{"op": "repin", "id": cm, "targets": [dm]}]),
    );
    let i = live(&view_rows(&r, "m"), &dm, &cm).clone();
    assert_eq!((i.state, i.how), (ItemState::Open, None));
    answer(&mut r, "m", &[(&dm, &cm, PlanAct::Defer, Some("later"))]);
    write(
        &mut r,
        "m",
        json!([{"op": "update", "id": dm, "fields": {"statement": "Ship M, all of it."}}]),
    );
    write(
        &mut r,
        "m",
        json!([{"op": "repin", "id": cm, "targets": [dm]}]),
    );
    let i = live(&view_rows(&r, "m"), &dm, &cm).clone();
    assert_eq!((i.state, i.how), (ItemState::Open, None));

    // selection subsets: a version of only the unrelated change has no as-built item
    let w1 = write(
        &mut r,
        "sub",
        json!([{"op": "create", "type": "claim", "parent": k("$s"), "body": "Unrelated."}]),
    );
    edit(
        &mut r,
        "sub",
        json!([{"op": "update", "id": d, "fields": {"built": "3.0.0"}}]),
    );
    let only = vec![w1.change.clone()];
    let p = r
        .propose_subset(
            &writer(),
            "sub",
            "sub",
            Some(&only),
            Some(common::TEST_SUMMARY),
        )
        .unwrap();
    let read = r.read();
    let reply = read
        .change_plan(
            &PlanQuery {
                proposal: Some(p.r#ref.clone()),
                ..PlanQuery::default()
            },
            "sub",
        )
        .unwrap();
    assert_eq!(built_rows(&reply.items), vec![]);
    // the whole set has it
    assert_eq!(built_of(&r, "sub"), vec![open(&d, &c)]);
}

/// The as-built warnings of `issues`: (block, message, fix).
fn warns(issues: &[rdy_schema::Issue]) -> Vec<(String, String, String)> {
    let mut v: Vec<_> = issues
        .iter()
        .filter(|i| i.code == W_AS_BUILT_OVERRIDE)
        .map(|i| {
            (
                i.block.as_ref().unwrap().to_string(),
                i.message.clone(),
                i.fix.clone(),
            )
        })
        .collect();
    v.sort();
    v
}

fn ctx(r: &Rdy, scope: &Scope, at: &str) -> Vec<(String, String, String)> {
    warns(&r.read().context(scope, &id(at)).unwrap().warnings)
}

fn warn(at: &str, eff: &str, der: &str, fix: &str) -> (String, String, String) {
    (
        at.to_string(),
        format!("Block {at} has override status {eff}, but its decisions derive {der}."),
        fix.to_string(),
    )
}

#[test]
fn ab3_a_confirm_silences_the_warning_until_its_inputs_change() {
    assert_eq!(
        AS_BUILT_FIX_OPEN_ITEM,
        "Confirm the as-built plan item with a reason, or remove the override."
    );
    let (dir, mut r) = project();
    let seed = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Plain"}},
            {"op": "create", "type": "section", "id": "$p", "parent": "$doc", "fields": {"title": "Planned", "planned": true}},
            {"op": "create", "type": "decision", "id": "$d", "fields": {"statement": "Ship D.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$d2", "fields": {"statement": "Ship D2.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "claim", "id": "$j1", "parent": "$p", "body": "J1.", "links": {"decided_by": ["$d"]}},
            {"op": "create", "type": "claim", "id": "$j2", "parent": "$p", "body": "J2.", "links": {"implements": ["$d"]}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A.", "fields": {"planned": true}, "links": {"decided_by": ["$d"]}},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B.", "fields": {"planned": true}, "links": {"decided_by": ["$d2"]}}
        ]),
    );
    let k = |x: &str| seed.created[x].clone();
    let (p, d, d2, j1, j2, a, b) = (
        k("$p"),
        k("$d"),
        k("$d2"),
        k("$j1"),
        k("$j2"),
        k("$a"),
        k("$b"),
    );
    land(&mut r, "seed");
    let canon = Scope::Canon;
    // canon warns, with no item to confirm
    assert_eq!(
        ctx(&r, &canon, &j1),
        vec![warn(&j1, "planned", "since 1.0.0", AS_BUILT_FIX_NO_ITEM)]
    );

    // the version: both decisions ship 1.1.0. In its view each warning names the open item
    let w = edit(
        &mut r,
        "v",
        json!([
            {"op": "update", "id": d, "fields": {"built": "1.1.0"}},
            {"op": "update", "id": d2, "fields": {"built": "1.1.0"}}
        ]),
    );
    let mut want: Vec<_> = [&j1, &j2, &a, &b]
        .iter()
        .map(|x| warn(x, "planned", "since 1.1.0", AS_BUILT_FIX_OPEN_ITEM))
        .collect();
    want.sort();
    assert_eq!(warns(&w.warnings), want);
    let view = Scope::View("v".into());
    assert_eq!(
        ctx(&r, &view, &j2),
        vec![warn(&j2, "planned", "since 1.1.0", AS_BUILT_FIX_OPEN_ITEM)]
    );
    assert_eq!(
        sorted(built_of(&r, "v")),
        sorted(vec![open(&d, &p), open(&d, &a), open(&d2, &b)])
    );
    // a confirm for the section and the claim, a defer for b
    answer(
        &mut r,
        "v",
        &[
            (&d, &p, PlanAct::Confirm, Some("the section ships later")),
            (&d, &a, PlanAct::Confirm, Some("this claim ships later")),
            (&d2, &b, PlanAct::Defer, Some("review with the release")),
        ],
    );
    // answered but not landed: the view still warns, with no open item now
    assert_eq!(
        ctx(&r, &view, &a),
        vec![warn(&a, "planned", "since 1.1.0", AS_BUILT_FIX_NO_ITEM)]
    );
    land(&mut r, "v");

    // landed: the confirmed subject and its inherited children are quiet, the deferred one not
    let quiet = |r: &Rdy, scope: &Scope| {
        for x in [&j1, &j2, &a] {
            assert_eq!(ctx(r, scope, x), vec![], "{x} in {scope:?}");
        }
        assert_eq!(
            ctx(r, scope, &b),
            vec![warn(&b, "planned", "since 1.1.0", AS_BUILT_FIX_NO_ITEM)]
        );
    };
    quiet(&r, &canon);
    // a write that reaches them gives no warning either
    let w = edit(
        &mut r,
        "w",
        json!([{"op": "update", "id": d, "fields": {"disposition": "Shipped in 1.1.0."}}]),
    );
    assert_eq!(warns(&w.warnings), vec![]);
    assert_eq!(built_of(&r, "w"), vec![]);
    quiet(&r, &Scope::View("w".into()));

    // an unrelated landing keeps the silence
    write(
        &mut r,
        "other",
        json!([{"op": "create", "type": "claim", "parent": k("$s"), "body": "Other."}]),
    );
    land(&mut r, "other");
    quiet(&r, &canon);
    // reopen and reindex read the same
    drop(r);
    let mut r = Rdy::open(dir.path()).unwrap();
    quiet(&r, &canon);
    r.reindex().unwrap();
    quiet(&r, &canon);

    // a changed child gets a new item for its holder: while it is open, the view warns again
    // on each block of that holder, and canon stays quiet
    write(
        &mut r,
        "child",
        json!([{"op": "update", "id": j1, "body": "J1, edited."}]),
    );
    let child = Scope::View("child".into());
    assert_eq!(built_of(&r, "child"), vec![open(&j1, &p)]);
    for x in [&j1, &j2] {
        assert_eq!(
            ctx(&r, &child, x),
            vec![warn(x, "planned", "since 1.1.0", AS_BUILT_FIX_OPEN_ITEM)]
        );
    }
    assert_eq!(ctx(&r, &child, &a), vec![]);
    quiet(&r, &canon);

    // an edit of the subject brings the warning back, and its version gets a new item
    write(
        &mut r,
        "t",
        json!([{"op": "update", "id": p, "fields": {"title": "Planned, renamed"}}]),
    );
    let t = Scope::View("t".into());
    assert_eq!(
        ctx(&r, &t, &j1),
        vec![warn(&j1, "planned", "since 1.1.0", AS_BUILT_FIX_OPEN_ITEM)]
    );
    assert_eq!(built_of(&r, "t"), vec![open(&p, &p)]);
    // canon is still quiet
    quiet(&r, &canon);

    // a later landing that changes a cited decision's built brings it back
    edit(
        &mut r,
        "later",
        json!([{"op": "update", "id": d, "fields": {"built": "1.2.0"}}]),
    );
    land(&mut r, "later");
    for x in [&j1, &j2, &a] {
        assert_eq!(
            ctx(&r, &canon, x),
            vec![warn(x, "planned", "since 1.2.0", AS_BUILT_FIX_NO_ITEM)]
        );
    }
}

/// A landing straight through the store, with no change plan and so no seal: a later landing
/// that changes a cited decision's `built` brings the warning back by the decision's value as
/// the confirming landing left it.
#[test]
fn ab3_a_built_change_with_no_seal_brings_the_warning_back() {
    let (_dir, mut r) = project();
    let seed = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Plain"}},
            {"op": "create", "type": "decision", "id": "$d", "fields": {"statement": "Ship D.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A.", "fields": {"planned": true}, "links": {"decided_by": ["$d"]}}
        ]),
    );
    let (d, a) = (seed.created["$d"].clone(), seed.created["$a"].clone());
    land(&mut r, "seed");
    edit(
        &mut r,
        "v",
        json!([{"op": "update", "id": d, "fields": {"built": "1.1.0"}}]),
    );
    answer(
        &mut r,
        "v",
        &[(&d, &a, PlanAct::Confirm, Some("kept planned"))],
    );
    land(&mut r, "v");
    assert_eq!(ctx(&r, &Scope::Canon, &a), vec![]);
    edit(
        &mut r,
        "raw",
        json!([{"op": "update", "id": d, "fields": {"built": "1.3.0"}}]),
    );
    land_raw(&mut r, "raw");
    assert_eq!(
        ctx(&r, &Scope::Canon, &a),
        vec![warn(&a, "planned", "since 1.3.0", AS_BUILT_FIX_NO_ITEM)]
    );
}
