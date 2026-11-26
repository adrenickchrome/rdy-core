//! r5909 (D-8QV9G, D-KQ4CA, D-TXFHR, D-4JMPH): a refused call names a step that works. Each test
//! pins a refusal's code and its exact recovery text, then performs the corrected call. The MCP
//! and CLI routes of the same texts are pinned in rdy-qa.
mod common;

use rdy_engine::answer::option_fix;
use rdy_engine::did_you_mean::{KNOWN_WRONG_NAMES, fields_fix, suggest};
use rdy_engine::pending::{Item, ItemKind, ItemOption};
use rdy_engine::plan::PlanAnswerInput;
use rdy_engine::plan::api::{ACTS_BY_KIND, act_fix};
use rdy_engine::{
    E_ALREADY_LANDED, E_BAD_INPUT, E_BAD_TAG_NAME, E_EXCEPTION_OPEN, E_HAS_CHILDREN, E_PERMISSION,
    E_PLAN_ITEM, E_UNKNOWN_BLOCK, E_UNKNOWN_REF, MISSING_BLOCK_BYTES, MISSING_BLOCK_FRAMES,
    MISSING_BLOCK_OPS, MISSING_BLOCK_VIEWS, Rdy, RdyError, Scope, SearchLimits, Session,
    WriteRequest, WriteResult, parse_op, parse_ops,
};
use rdy_model::plan::{PlanAct, PlanKind};
use rdy_model::{BlockId, Role};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn writer() -> Session {
    Session::human("human:qa".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn try_write(r: &mut Rdy, view: &str, ops: Value) -> Result<WriteResult, RdyError> {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: parse_ops(ops.as_array().unwrap().clone())?,
            justification: None,
            message: String::new(),
        },
    )
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    try_write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e}"))
}

fn land(r: &mut Rdy, view: &str) {
    let p = r
        .propose(&writer(), view, view, Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id).unwrap();
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// The code, message and fix of a refusal.
fn triple(e: &RdyError) -> (&'static str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

/// Canon: doc `spec`, its section and the claims `a` and `b`, where `b` cites `a`.
struct Spec {
    doc: String,
    sec: String,
    a: String,
    b: String,
}

fn spec(r: &mut Rdy) -> Spec {
    let w = put(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."}
        ]),
    );
    land(r, "seed");
    let get = |k: &str| w.created[k].clone();
    Spec {
        doc: get("$d"),
        sec: get("$s"),
        a: get("$a"),
        b: get("$b"),
    }
}

// ---- W-core-1: the act a plan item does not take ----

/// r5909: the six acts-by-kind fixes read as the plan words them, a group keeps the one-line
/// table, and a real item refuses an act its kind does not take with that kind's fix and appends
/// nothing; a stale item and an empty group report the open count.
#[test]
fn r5909_plan_acts_and_stale_items_give_recovery() {
    let table = [
        (
            PlanKind::Update,
            "update items take: defer. An update item also closes when you edit its block's content; a repin does not close it",
        ),
        (
            PlanKind::Rejustify,
            "rejustify items take: defer. A rejustify item also closes when the block no longer links to or references the cause",
        ),
        (
            PlanKind::Reaffirm,
            "reaffirm items take: defer. A reaffirm item also closes with an rdy_write reaffirm op",
        ),
        (PlanKind::Confirm, "confirm items take: confirm or defer"),
        (
            PlanKind::UpdateImplementation,
            "update_implementation items take: done or defer",
        ),
        (PlanKind::Tension, "tension items take: confirm or dismiss"),
    ];
    for (kind, want) in table {
        assert_eq!(act_fix(kind), want, "{kind:?}");
    }
    assert_eq!(
        ACTS_BY_KIND,
        "update, rejustify and reaffirm items take defer; confirm items take confirm or defer; update_implementation items take done or defer; tension items take confirm or dismiss"
    );

    // a real project: editing a makes b a confirm item
    let (_d, mut r) = project();
    let s = spec(&mut r);
    put(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.a, "body": "A holds 60 days."}]),
    );
    let rows = r.read().view_plan("w").unwrap();
    let confirm = rows
        .items
        .iter()
        .find_map(|row| match row {
            rdy_engine::plan::PlanRow::Live(i) if i.kind == PlanKind::Confirm => Some(i.id.clone()),
            _ => None,
        })
        .expect("a confirm item");
    let answer = |item: &str, act| PlanAnswerInput {
        item: item.into(),
        act,
        reason: Some("because".into()),
    };
    let before = r.store().log_end();
    let e = r
        .answer_plan(&writer(), "w", &[answer(&confirm, PlanAct::Done)])
        .unwrap_err();
    assert_eq!(
        triple(&e),
        (
            E_BAD_INPUT,
            "done does not answer a confirm item",
            "confirm items take: confirm or defer"
        )
    );
    assert_eq!(
        r.store().log_end(),
        before,
        "a refused call appends nothing"
    );

    // a stale item: the full open count rides on the fix
    let open = rows.counts.open;
    let e = r
        .answer_plan(
            &writer(),
            "w",
            &[answer("pi-00000000000000000000", PlanAct::Defer)],
        )
        .unwrap_err();
    assert_eq!(e.code, E_PLAN_ITEM);
    assert_eq!(
        e.fix,
        format!(
            "read the plan again after each write. Check counts.open and read every page before answering. An already_written consequence may have no item. One unmatched item or group refuses the whole call. Open items: {open}."
        )
    );
    assert_eq!(r.store().log_end(), before);
    // the corrected call: the act the kind takes
    r.answer_plan(&writer(), "w", &[answer(&confirm, PlanAct::Confirm)])
        .unwrap();
}

// ---- W-core-2: the card's own options ----

fn item(kind: ItemKind, options: &[(&str, &str)]) -> Item {
    Item {
        r#ref: "P-x@1".into(),
        kind,
        title: String::new(),
        why: String::new(),
        blocking: false,
        summary: None,
        summary_by: None,
        facts: vec![],
        members: vec![],
        options: options
            .iter()
            .map(|(id, label)| ItemOption {
                id: (*id).into(),
                label: (*label).into(),
                consequences: vec![],
                recommended: None,
            })
            .collect(),
        raised_by: String::new(),
        raised_at: String::new(),
        age: String::new(),
        batch: None,
        exception: None,
        card: String::new(),
        card_hash: String::new(),
        removal: None,
        removal_reports: BTreeMap::new(),
        plan_binding: None,
        check_verdicts: vec![],
    }
}

/// r5909: the fix of a wrong option lists the card's own options in display order, for each of
/// the seven kinds, and the list and each entry are bounded.
#[test]
fn r5909_card_fix_uses_current_options() {
    let kinds = [
        (ItemKind::Approve, "approve"),
        (ItemKind::Confirm, "confirm"),
        (ItemKind::Answer, "answer"),
        (ItemKind::Resolve, "resolve"),
        (ItemKind::Defer, "defer"),
        (ItemKind::Reaffirm, "reaffirm"),
        (ItemKind::Adopt, "adopt"),
    ];
    for (kind, word) in kinds {
        let i = item(
            kind,
            &[("o1", "first"), ("o2", "second"), ("later", "Later")],
        );
        assert_eq!(
            option_fix(&i),
            format!(
                "this {word} card takes: o1 (first), o2 (second), later (Later). Copy an option id from the current card; put the person's exact words in evidence."
            )
        );
    }
    // an approve card with an accepted check verdict lists that option, in its place
    let i = item(
        ItemKind::Approve,
        &[
            ("approve", "Approve"),
            ("accept_verdict:rv-1:abc", "Accept the verdict"),
            ("reject", "Reject"),
            ("later", "Later"),
        ],
    );
    assert_eq!(
        option_fix(&i),
        "this approve card takes: approve (Approve), accept_verdict:rv-1:abc (Accept the verdict), reject (Reject), later (Later). Copy an option id from the current card; put the person's exact words in evidence."
    );
    // 32 entries are listed whole; a 33rd is counted, not listed
    let many = |n: usize| -> Vec<(String, String)> {
        (1..=n)
            .map(|k| (format!("o{k}"), format!("L{k}")))
            .collect()
    };
    let with = |n: usize| {
        let owned = many(n);
        let refs: Vec<(&str, &str)> = owned
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        option_fix(&item(ItemKind::Answer, &refs))
    };
    let at32 = with(32);
    assert!(at32.contains("o32 (L32). Copy"), "{at32}");
    assert!(!at32.contains("First 32 of"), "{at32}");
    let at33 = with(33);
    assert!(!at33.contains("o33 (L33)"), "{at33}");
    assert!(
        at33.ends_with(" First 32 of 33; read the full list before retrying."),
        "{at33}"
    );
    // each entry is at most 256 scalars: 255 and 256 stay, 257 is cut after 255 and ends with …
    let entry = |scalars: usize| -> String {
        // "o1 (" is 4 scalars and ")" is 1
        let label = "é".repeat(scalars - 5);
        let fix = option_fix(&item(ItemKind::Answer, &[("o1", &label)]));
        let start = fix.find("takes: ").unwrap() + "takes: ".len();
        let end = fix.find(". Copy an option id").unwrap();
        fix[start..end].to_string()
    };
    assert_eq!(entry(255).chars().count(), 255);
    assert_eq!(entry(256).chars().count(), 256);
    let cut = entry(257);
    assert_eq!(cut.chars().count(), 256, "255 scalars and the ellipsis");
    assert!(cut.ends_with('…'), "{cut}");
}

// ---- W-core-3: known wrong names ----

/// r5909: a known wrong name is a hint only when the field it stands for is known there; the
/// nearest-name rule and the unknown-field order are unchanged, and an alias is never accepted.
#[test]
fn r5909_name_hints_never_accept_aliases() {
    let pairs = [
        ("action", "op"),
        ("id", "alias"),
        ("proposal", "ref"),
        ("card", "ref"),
        ("token", "brief"),
        ("type", "block_type"),
        ("text", "evidence"),
        ("answer", "evidence"),
        ("words", "evidence"),
    ];
    assert_eq!(KNOWN_WRONG_NAMES, pairs);
    for (given, field) in pairs {
        let known = ["zzz", field, "yyy"];
        assert_eq!(suggest(given, &known), Some(field), "{given}");
        assert_eq!(
            fields_fix(given, &known),
            format!("did you mean \"{field}\"? the fields are zzz, {field}, yyy")
        );
        // the field is not known here: no hint from the table, the nearest-name rule alone
        let other = ["zzz", "yyy"];
        assert_eq!(suggest(given, &other), None, "{given}");
        assert_eq!(
            fields_fix(given, &other),
            format!("remove \"{given}\"; the fields are zzz, yyy")
        );
    }
    // the table goes before edit distance, and a name that is not in it keeps the old rule
    assert_eq!(suggest("id", &["ida", "alias"]), Some("alias"));
    assert_eq!(suggest("bodyy", &["body", "fields"]), Some("body"));
    // a write op: the op value `edit` has its own fix; any other unknown op keeps the old one
    let e = parse_op(0, json!({"op": "edit", "id": "c-AAAAA", "body": "x"})).unwrap_err();
    assert_eq!(
        triple(&e).2,
        "use op update to edit an existing block; copy the op fields from rdy_schema"
    );
    let e = parse_op(0, json!({"op": "frob", "id": "c-AAAAA"})).unwrap_err();
    assert_eq!(
        triple(&e).2,
        "each op needs \"op\" (rdy_schema lists them under \"ops\"); create needs \"type\""
    );
    // an unknown field of an op names its nearest field as before
    let e = parse_op(0, json!({"op": "update", "id": "c-AAAAA", "bdy": "x"})).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.fix.starts_with("did you mean \"body\"?"), "{}", e.fix);
}

// ---- W-core-10, 11, 25: placeholders, repins, small cases ----

/// r5909: a placeholder used before its create refuses the whole write with the create-first
/// fix, a placeholder never carries into another write, and the returned id works there.
#[test]
fn r5909_placeholders_and_repin_recover_without_partial_write() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    let before = r.store().log_end();
    let e = try_write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "claim", "parent": s.sec, "body": "Uses {{$intro}} early."},
            {"op": "create", "type": "claim", "id": "$intro", "parent": s.sec, "body": "Intro."}
        ]),
    )
    .unwrap_err();
    assert_eq!(
        triple(&e),
        (
            E_BAD_INPUT,
            "placeholder $intro is used before it is created",
            "create $intro earlier in this write, then use it in a later op. A placeholder does not carry into another write; use the returned block id there."
        )
    );
    assert_eq!(r.store().log_end(), before, "no partial write");
    // create-before-use works
    let w = put(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "claim", "id": "$intro", "parent": s.sec, "body": "Intro."},
            {"op": "create", "type": "claim", "parent": s.sec, "body": "Uses {{$intro}}."}
        ]),
    );
    let intro = w.created["$intro"].clone();
    // a placeholder of the earlier write does not carry into this one
    let e = try_write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "parent": s.sec, "body": "Uses {{$intro}} again."}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.fix.starts_with("create $intro earlier in this write"));
    // the returned block id works there
    put(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "parent": s.sec, "body": format!("Uses {{{{{intro}}}}} again.")}]),
    );

    // a repin of pins that are already current is refused whole, with the read-first fix
    let current = "read rdy_suspects and the block's own pins before retrying. Drop this repin from the write. A superseded or revoked target needs a changed citation; a reaffirm does not change the target's content version.";
    let before = r.store().log_end();
    let e = try_write(
        &mut r,
        "r",
        json!([
            {"op": "create", "type": "claim", "parent": s.sec, "body": "Extra."},
            {"op": "repin", "id": s.b, "targets": [s.a]}
        ]),
    )
    .unwrap_err();
    assert_eq!(
        triple(&e),
        (
            E_BAD_INPUT,
            format!(
                "{} already pins the current version of {}; nothing to repin",
                s.b, s.a
            )
            .as_str(),
            current
        )
    );
    assert_eq!(r.store().log_end(), before, "the whole write refuses");
    // the wrong side: a does not cite b
    let e = try_write(
        &mut r,
        "r",
        json!([{"op": "repin", "id": s.a, "targets": [s.b]}]),
    )
    .unwrap_err();
    assert_eq!(
        triple(&e),
        (
            E_BAD_INPUT,
            format!("{} does not reference or link to {}", s.a, s.b).as_str(),
            "repin the block that holds the citation, with targets from that block's own pins. Read both blocks with rdy_context before retrying."
        )
    );
    // the corrected selection: edit a, then repin b to it
    put(
        &mut r,
        "r",
        json!([{"op": "update", "id": s.a, "body": "A holds 60 days."}]),
    );
    put(
        &mut r,
        "r",
        json!([{"op": "repin", "id": s.b, "targets": [s.a]}]),
    );
}

/// r5909: the hash of a pin is no version name; the local missing reference and link refusals
/// point at another project's alias form; a relaying person and the human relay refusal keep
/// their codes and give the one fix; a list in a fix shows 32 entries.
#[test]
fn r5909_small_text_cases_preserve_conditions() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    // a 64-character lowercase hex string is no version name
    let hash = "ab".repeat(32);
    let e = rdy_engine::parse_version(&hash).unwrap_err();
    assert_eq!(e.code, E_BAD_TAG_NAME);
    assert!(
        e.message
            .starts_with(&format!("{hash:?} is not a version: ")),
        "{}",
        e.message
    );
    assert_eq!(
        e.fix,
        "a pin hash names content, not a release or landing. Read the block with rdy_context; choose a version name or a landing hash from rdy_log."
    );
    // a name of another length keeps the version fix
    let e = rdy_engine::parse_version("abc").unwrap_err();
    assert_eq!(
        e.fix,
        "name versions like v1.2.0 or 2.0.0-rc.1 (Semantic Versioning, one optional leading v, no +build metadata)"
    );
    // a missing local reference and a missing local link
    let ghost = "c-ZZZZZ";
    let other = " If the block belongs to another project, use {{alias:id}} in text with a registered project alias, or use plain text.";
    let e = try_write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "parent": s.sec, "body": format!("See {{{{{ghost}}}}}.")}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_UNKNOWN_REF);
    assert!(e.fix.ends_with(other), "{}", e.fix);
    let e = try_write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "parent": s.sec, "body": "Linked.",
                "links": {"depends_on": [{"target": ghost}]}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_UNKNOWN_REF);
    assert_eq!(e.fix, format!("fix the link target id{other}"));
    // the human relay refusal: its code and message stay, its fix is the one text
    let e = rdy_engine::answer::relay::RelayRefusal::HumanRelays.error();
    assert_eq!(
        triple(&e),
        (
            E_PERMISSION,
            "a person answers as themselves; only the relaying agent records a relay",
            "if you are the person, drop the relay flags. If you relay for the person, set RDY_ACTOR=agent:<name> to your own actor and pass that actor in --relayed-by with the person's words in --evidence"
        )
    );
    // a relayed-by that is not the session actor still names the actual actor
    let e = rdy_engine::answer::relay::RelayRefusal::NotItself {
        actor: "agent:real".into(),
        run: None,
        by: "agent:other".into(),
    }
    .error();
    assert_eq!(e.code, E_PERMISSION);
    assert_eq!(
        e.fix,
        "pass --relayed-by agent:real with the person's words as evidence"
    );
}

// ---- W-core-23: children before their parent ----

/// r5909: deleting a parent that has children names them, in tree order and at most 32; the
/// corrected write deletes the children first, in the same write.
#[test]
fn r5909_conflicts_null_and_children_give_valid_operations() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    // null does not clear a field; unset does
    let e = try_write(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.doc, "fields": {"doc_type": null}}]),
    )
    .unwrap_err();
    assert!(
        e.issues.iter().any(|i| i.code == "E_FIELD_KIND"
            && i.fix
                == "null does not clear a field; list it in the update op's unset: [\"doc_type\"]"),
        "{e:?}"
    );
    // an edit collision's way out depends on the block: an editable block is written from the
    // chosen version, a frozen decision takes the landed side
    assert_eq!(
        rdy_engine::tensions::resolve_hint(rdy_engine::tensions::TensionKind::EditCollision),
        "read the conflict with rdy_context. For an editable block, write from the chosen version with base set to that version. For an accepted or rejected decision, use rdy_view resolve with landed true, or exclude a colliding reaffirm change."
    );
    // children: the section holds a and b
    let before = r.store().log_end();
    let e = try_write(&mut r, "w", json!([{"op": "delete", "id": s.sec}])).unwrap_err();
    assert_eq!(e.code, E_HAS_CHILDREN);
    assert_eq!(e.message, format!("{} still has children", s.sec));
    assert_eq!(
        e.fix,
        format!(
            "delete or move these children before their parent in the same write: {}, {}",
            s.a, s.b
        )
    );
    assert_eq!(r.store().log_end(), before);
    // the corrected write: the children first, then the parent
    put(
        &mut r,
        "w",
        json!([
            {"op": "delete", "id": s.b},
            {"op": "delete", "id": s.a},
            {"op": "delete", "id": s.sec}
        ]),
    );
    // more than 32 children: 32 are listed and the rest counted
    let (_d2, mut r2) = project();
    let s2 = spec(&mut r2);
    let ops: Vec<Value> = (0..33)
        .map(|n| json!({"op": "create", "type": "claim", "parent": s2.sec, "body": format!("Claim {n}.")}))
        .collect();
    put(&mut r2, "w", Value::Array(ops));
    let e = try_write(&mut r2, "w", json!([{"op": "delete", "id": s2.sec}])).unwrap_err();
    assert_eq!(e.code, E_HAS_CHILDREN);
    assert!(
        e.fix
            .ends_with(" First 32 of 35; read the full list before retrying."),
        "{}",
        e.fix
    );
    assert_eq!(
        e.fix
            .split(": ")
            .nth(1)
            .unwrap()
            .split(" First")
            .next()
            .unwrap()
            .split(", ")
            .count(),
        32
    );
}

// ---- W-core-22: a block held only in an open view ----

fn read_missing(r: &Rdy, scope: &Scope, block: &str) -> RdyError {
    r.read().block(scope, &id(block)).unwrap_err()
}

/// Canon of [`spec`], and three open views: `c` creates a claim, `b` includes that change, `a`
/// edits another block. Returns the spec, the new block's id and its change.
fn three_views(r: &mut Rdy) -> (Spec, String, String) {
    let s = spec(r);
    let made = put(
        r,
        "c",
        json!([{"op": "create", "type": "claim", "parent": s.sec, "body": "Only in c."}]),
    );
    let ghost = made.created.values().next().unwrap().clone();
    put(
        r,
        "a",
        json!([{"op": "update", "id": s.a, "body": "A in view a."}]),
    );
    r.view_create(&writer(), "b", None, false).unwrap();
    r.view_include(&writer(), "b", std::slice::from_ref(&made.change))
        .unwrap();
    (s, ghost, made.change)
}

/// r5909: a block that only an open view holds is named in the fix of a read that missed it. The
/// first open view by name is the one confirmed, with one materialization and no block reads in
/// the search; a deleted first candidate gives the fallback; history and closed views are not
/// searched; the 33rd view and each search bound add their sentence.
#[test]
fn r5909_missing_block_names_an_open_view() {
    let fallback = "check the id with rdy_read";
    let hint = "this id is in open view b; read with scope view:b";

    // discovery reads change frames only: no block is hydrated and one view is materialized
    let (d0, mut r0) = project();
    let (_s0, ghost0, _) = three_views(&mut r0);
    // a reopened project holds no built view
    r0.close().unwrap();
    let r0 = Rdy::open(d0.path()).unwrap();
    let st = r0.read();
    for v in ["a", "b", "c"] {
        assert_eq!(st.is_built(&Scope::View(v.into())), (false, false), "{v}");
    }
    let reader = st.reader().clone();
    let frames = reader.frames_read();
    assert_eq!(
        st.missing_block_fix(&Scope::Canon, &id(&ghost0), fallback),
        hint
    );
    // a holds one change (read), b holds c's change (read, the match); c is never read; the
    // one materialization, of b, reads its blocks and nothing else of the search does
    assert_eq!(st.is_built(&Scope::View("a".into())), (false, false));
    assert_eq!(st.is_built(&Scope::View("c".into())), (false, false));
    assert!(st.is_built(&Scope::View("b".into())).0);
    assert!(reader.frames_read() - frames >= 2);
    // with b built, the search reads exactly the two change frames
    let frames = reader.frames_read();
    assert_eq!(
        st.missing_block_fix(&Scope::Canon, &id(&ghost0), fallback),
        hint
    );
    assert_eq!(
        reader.frames_read() - frames,
        2,
        "a's change and b's change"
    );

    let (_d, mut r) = project();
    let (_s, ghost, _) = three_views(&mut r);
    // the reads of canon and of another view give the hint; the in-flight scope holds the block
    assert!(r.read().block(&Scope::Inflight, &id(&ghost)).is_ok());
    for scope in [Scope::Canon, Scope::View("a".into())] {
        let e = read_missing(&r, &scope, &ghost);
        assert_eq!(e.code, E_UNKNOWN_BLOCK);
        assert_eq!(e.message, format!("no block {ghost}"));
        assert_eq!(e.fix, hint, "{scope:?}");
    }
    // the path read and the context read share it
    assert_eq!(
        r.read()
            .path_of(&Scope::Canon, &id(&ghost))
            .unwrap_err()
            .fix,
        hint
    );
    assert_eq!(
        r.read()
            .context(&Scope::Canon, &id(&ghost))
            .unwrap_err()
            .fix,
        hint
    );
    // the view that holds it reads it
    assert!(
        r.read()
            .block(&Scope::View("b".into()), &id(&ghost))
            .is_ok()
    );
    // a block no view holds keeps the fallback
    let e = read_missing(&r, &Scope::Canon, "c-ZZZZZ");
    assert_eq!(e.fix, fallback);

    // a deleted first candidate gives the fallback; the second view is not tried
    let (d3, mut r3) = project();
    let s3 = spec(&mut r3);
    let made = put(
        &mut r3,
        "c",
        json!([{"op": "create", "type": "claim", "parent": s3.sec, "body": "Only in c."}]),
    );
    let gone = made.created.values().next().unwrap().clone();
    r3.view_create(&writer(), "b", None, false).unwrap();
    r3.view_include(&writer(), "b", std::slice::from_ref(&made.change))
        .unwrap();
    put(&mut r3, "b", json!([{"op": "delete", "id": gone}]));
    r3.close().unwrap();
    let r3 = Rdy::open(d3.path()).unwrap();
    let e = read_missing(&r3, &Scope::Canon, &gone);
    assert_eq!(e.fix, fallback, "b deleted it: no hint, and c is not tried");
    assert_eq!(r3.read().is_built(&Scope::View("c".into())), (false, false));

    // a closed view is never a candidate: with b closed, c is the first
    r.view_close(&writer(), &["b".to_string()], "done").unwrap();
    let e = read_missing(&r, &Scope::Canon, &ghost);
    assert_eq!(e.fix, "this id is in open view c; read with scope view:c");
    // historical scopes keep the fallback whatever the views hold
    let landing = r.read().canon_head().unwrap();
    let e = r
        .read()
        .block(&Scope::At(landing), &id(&ghost))
        .unwrap_err();
    assert_eq!(e.fix, fallback);
}

/// r5909: the bounds of the search: the constants, the 33rd open view, and each cap.
#[test]
fn r5909_missing_block_search_is_bounded() {
    assert_eq!(MISSING_BLOCK_VIEWS, 32);
    assert_eq!(MISSING_BLOCK_FRAMES, 4_096);
    assert_eq!(MISSING_BLOCK_OPS, 100_000);
    assert_eq!(MISSING_BLOCK_BYTES, 67_108_864);
    assert_eq!(SearchLimits::DEFAULT.views, 32);
    let (_d, mut r) = project();
    let s = spec(&mut r);
    let fallback = "check the id with rdy_read";
    // 33 open views: the last one holds the block, so the first 32 give no candidate
    let mut ghost = String::new();
    for n in 0..33 {
        let name = format!("v{n:02}");
        let w = put(
            &mut r,
            &name,
            json!([{"op": "create", "type": "claim", "parent": s.sec, "body": format!("In {name}.")}]),
        );
        if n == 32 {
            ghost = w.created.values().next().unwrap().clone();
        }
    }
    let e = read_missing(&r, &Scope::Canon, &ghost);
    assert_eq!(
        e.fix,
        format!(
            "{fallback} Only the first 32 open views were checked; list views with rdy_view before retrying."
        )
    );
    // fewer views than the limit: no sentence
    let st = r.read();
    let small = SearchLimits {
        views: 40,
        ..SearchLimits::DEFAULT
    };
    assert_eq!(
        st.missing_block_fix_within(&Scope::Canon, &id(&ghost), fallback, small),
        "this id is in open view v32; read with scope view:v32"
    );
    // each cap ends the search with its sentence
    let capped = format!(
        "{fallback} The open-view hint reached its search limit; list views with rdy_view before retrying."
    );
    for limits in [
        SearchLimits { frames: 1, ..small },
        SearchLimits { ops: 1, ..small },
        SearchLimits { bytes: 1, ..small },
    ] {
        assert_eq!(
            st.missing_block_fix_within(&Scope::Canon, &id(&ghost), fallback, limits),
            capped,
            "{limits:?}"
        );
    }
}

// ---- W-core-14: the cards an approval waits for ----

/// r5909: an approval of a version with an exception card names the cards to answer, in the
/// order of `corrected.exceptions`, and records nothing; the answer of each card unlocks it.
#[test]
fn r5909_approval_fix_names_required_cards() {
    let (_d, mut r) = project();
    let s = spec(&mut r);
    put(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.a, "body": "A holds 90 days."}]),
    );
    // a one-way decision stands beside the batch as an exception card
    r.decide(
        &writer(),
        "w",
        rdy_engine::DecisionInput {
            parent: s.doc.clone(),
            statement: "Keep 90 days".into(),
            body: "Because.".into(),
            reversibility: Some("one_way".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let p = r
        .propose(&writer(), "w", "w", Some(common::TEST_SUMMARY))
        .unwrap();
    let before = r.store().log_end();
    let e = r.land(&approver(), &p.id).unwrap_err();
    assert_eq!(e.code, E_EXCEPTION_OPEN, "{e}");
    let cards = e.corrected.as_ref().unwrap()["exceptions"]
        .as_array()
        .unwrap()
        .clone();
    assert!(!cards.is_empty());
    let refs: Vec<&str> = cards.iter().map(|c| c["ref"].as_str().unwrap()).collect();
    assert_eq!(
        e.fix,
        format!(
            "answer these exception cards first, in order: {}. Then relay the approval.",
            refs.join(", ")
        )
    );
    assert_eq!(
        r.store().log_end(),
        before,
        "a refused approval records nothing"
    );
    // the cards' own answers unlock the existing approval path
    common::confirm_exceptions(&mut r, &approver(), &p);
    r.land(&approver(), &p.id).unwrap();
    assert_eq!(
        r.land(&approver(), &p.id).unwrap_err().code,
        E_ALREADY_LANDED,
        "a landed proposal stays E_ALREADY_LANDED"
    );
}
