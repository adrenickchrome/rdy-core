//! ab2 (D-WJ0SJ, D-YAY1G): the as-built status a content block has at the scope being read, how
//! it is cached, and when an override warns.
mod common;

use rdy_engine::as_built::{AS_BUILT_FIX_NO_ITEM, AsBuilt, Derived, Override, Resolver, Status};
use rdy_engine::*;
use rdy_model::redaction::RedactedBlockIdentity;
use rdy_model::{Block, BlockId, EditorialClass, FieldValue, Hash, Justification, Link, Op, Role};
use rdy_schema::{Pack, PackSet};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn standard() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

/// The standard pack with one more content type, `memo`.
fn with_memo() -> Pack {
    let mut set = PackSet::with_builtin();
    let name = set
        .add_yaml(
            "name: team\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  memo:\n    prefix: mm\n    traits: [informative]\n    body: required\n",
        )
        .unwrap();
    set.resolve(&name).unwrap()
}

fn block(at: &str, ty: &str, parent: Option<&str>) -> Block {
    Block {
        id: id(at),
        block_type: ty.into(),
        parent: parent.map(id),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: "Text.".into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn set(mut b: Block, field: &str, v: FieldValue) -> Block {
    b.fields.insert(field.into(), v);
    b
}

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
}

fn linked(mut b: Block, kind: &str, target: &str, part: Option<&str>) -> Block {
    b.links.entry(kind.into()).or_default().push(Link {
        target: id(target),
        part: part.map(str::to_string),
    });
    b
}

fn decision(at: &str, status: &str, built: Option<&str>) -> Block {
    let mut d = set(block(at, "decision", None), "statement", s("Ship it."));
    d = set(d, "status", s(status));
    match built {
        Some(b) => set(d, "built", s(b)),
        None => d,
    }
}

fn stub(at: &str, ty: &str, parent: Option<&str>) -> Block {
    Block::redacted(
        id(at),
        ty.into(),
        parent.map(id),
        "V".into(),
        RedactedBlockIdentity {
            version: Hash::of(b"v"),
            content_hash: Hash::of(b"c"),
            frozen: false,
            request: Hash::of(b"r"),
        },
    )
}

fn state(blocks: Vec<Block>) -> Materialized {
    let ops: Vec<Op> = blocks
        .into_iter()
        .map(|block| Op::Create { block })
        .collect();
    Materialized::default().with(&ops)
}

fn derived(status: Status, since: Option<&str>) -> Derived {
    Derived {
        status,
        since: since.map(str::to_string),
    }
}

fn owner(block: &str, field: &str) -> Option<Override> {
    Some(Override {
        block: block.into(),
        field: field.into(),
    })
}

fn st(status: Status, since: Option<&str>, d: Derived, o: Option<Override>) -> AsBuilt {
    AsBuilt {
        status,
        since: since.map(str::to_string),
        derived: d,
        owner: o,
    }
}

#[test]
fn ab2_direct_decisions_and_precedence_are_exact() {
    use Status::*;
    let doc = "d-D0C01";
    let sec = "s-SEC01";
    let mut blocks = vec![
        block(doc, "doc", None),
        block(sec, "section", Some(doc)),
        // decisions sit outside the doc: a target outside any selection still gives its version
        decision("D-PXAN1", "proposed", Some("planned")),
        decision("D-V1200", "accepted", Some("1.2.0")),
        decision("D-V2000", "accepted", Some("v2.0.0")),
        decision("D-V0100", "accepted", Some("0.1.0")),
        decision("D-N0NE1", "accepted", Some("none")),
        decision("D-ABS01", "accepted", None),
        decision("D-PR0P1", "proposed", Some("1.5.0")),
        decision("D-REJ01", "rejected", Some("3.0.0")),
        decision("D-SVP01", "superseded", Some("1.9.0")),
        decision("D-PRE01", "accepted", Some("2.0.0-rc.1")),
        stub("D-RED01", "decision", None),
        // an entity a decision affects: entity reach gives no build fact
        block("m-ENT01", "component", Some(sec)),
        linked(
            decision("D-ENT01", "accepted", Some("4.0.0")),
            "affects",
            "m-ENT01",
            None,
        ),
    ];
    let under = |at: &str, ty: &str| block(at, ty, Some(sec));
    let claim = |at: &str| under(at, "claim");
    blocks.extend([
        claim("c-N0DEC"),
        linked(claim("c-ABS01"), "decided_by", "D-ABS01", None),
        linked(claim("c-N0NE1"), "decided_by", "D-N0NE1", None),
        linked(
            linked(claim("c-VERS1"), "decided_by", "D-V1200", None),
            "implements",
            "D-V2000",
            None,
        ),
        linked(
            linked(claim("c-PXAN1"), "decided_by", "D-V2000", None),
            "implements",
            "D-PXAN1",
            None,
        ),
        linked(
            linked(
                linked(claim("c-DVP01"), "decided_by", "D-V1200", None),
                "decided_by",
                "D-V1200",
                None,
            ),
            "implements",
            "D-V1200",
            None,
        ),
        linked(claim("c-PART1"), "decided_by", "D-V1200", Some("the CLI")),
        linked(claim("c-PR0P1"), "decided_by", "D-PR0P1", None),
        linked(claim("c-REJ01"), "decided_by", "D-REJ01", None),
        linked(claim("c-SVP01"), "decided_by", "D-SVP01", None),
        // semantic order: 2.0.0-rc.1 is before 2.0.0, and 1.10 after 1.9
        linked(
            linked(claim("c-PRE01"), "decided_by", "D-PRE01", None),
            "decided_by",
            "D-V1200",
            None,
        ),
        linked(claim("c-M1SS1"), "decided_by", "D-G0NE1", None),
        linked(claim("c-RED01"), "implements", "D-RED01", None),
        linked(claim("c-REACH"), "describes", "m-ENT01", None),
        // planned: false alone is no override
        set(
            linked(claim("c-PF001"), "decided_by", "D-V1200", None),
            "planned",
            FieldValue::Bool(false),
        ),
        // own since, normalized
        set(
            linked(claim("c-S1NCE"), "decided_by", "D-PXAN1", None),
            "since",
            s("v0.9.0"),
        ),
    ]);
    // a planned section: its own planned is its own status; it covers the blocks under it
    let plan_sec = "s-PXN01";
    let in_plan = |at: &str, ty: &str| block(at, ty, Some(plan_sec));
    blocks.extend([
        set(
            block(plan_sec, "section", Some(doc)),
            "planned",
            FieldValue::Bool(true),
        ),
        linked(in_plan("c-NH001", "claim"), "decided_by", "D-V1200", None),
        set(
            linked(in_plan("c-0WN01", "claim"), "decided_by", "D-V1200", None),
            "since",
            s("1.0.0"),
        ),
        set(
            set(
                in_plan("c-0WNP1", "claim"),
                "planned",
                FieldValue::Bool(true),
            ),
            "since",
            s("1.0.0"),
        ),
        set(
            in_plan("c-FA15E", "claim"),
            "planned",
            FieldValue::Bool(false),
        ),
        stub("c-REDC1", "claim", Some(plan_sec)),
        stub("c-REDC2", "claim", Some(sec)),
    ]);
    // a planned doc: a nearer section with no true flag does not cancel it; a nearer planned
    // section owns its blocks
    let pdoc = "d-PXD01";
    blocks.extend([
        set(block(pdoc, "doc", None), "planned", FieldValue::Bool(true)),
        set(
            block("s-N0F01", "section", Some(pdoc)),
            "planned",
            FieldValue::Bool(false),
        ),
        block("s-NST01", "section", Some("s-N0F01")),
        linked(
            block("c-DEEP1", "claim", Some("s-NST01")),
            "decided_by",
            "D-PXAN1",
            None,
        ),
        set(
            block("s-PXN02", "section", Some(pdoc)),
            "planned",
            FieldValue::Bool(true),
        ),
        block("n-NEAR1", "note", Some("s-PXN02")),
    ]);
    // since does not inherit
    blocks.extend([
        set(block("s-S1N01", "section", Some(doc)), "since", s("1.0.0")),
        linked(
            block("c-S1N01", "claim", Some("s-S1N01")),
            "decided_by",
            "D-V2000",
            None,
        ),
    ]);
    let pack = standard();
    let m = state(blocks.clone());
    let r = Resolver::new(&pack, &m);
    let unv = || derived(Unversioned, None);
    let cases: Vec<(&str, AsBuilt)> = vec![
        ("c-N0DEC", st(Unversioned, None, unv(), None)),
        ("c-ABS01", st(Unversioned, None, unv(), None)),
        ("c-N0NE1", st(Unversioned, None, unv(), None)),
        (
            "c-VERS1",
            st(Since, Some("2.0.0"), derived(Since, Some("2.0.0")), None),
        ),
        ("c-PXAN1", st(Planned, None, derived(Planned, None), None)),
        (
            "c-DVP01",
            st(Since, Some("1.2.0"), derived(Since, Some("1.2.0")), None),
        ),
        (
            "c-PART1",
            st(Since, Some("1.2.0"), derived(Since, Some("1.2.0")), None),
        ),
        (
            "c-PR0P1",
            st(Since, Some("1.5.0"), derived(Since, Some("1.5.0")), None),
        ),
        (
            "c-REJ01",
            st(Since, Some("3.0.0"), derived(Since, Some("3.0.0")), None),
        ),
        (
            "c-SVP01",
            st(Since, Some("1.9.0"), derived(Since, Some("1.9.0")), None),
        ),
        (
            "c-PRE01",
            st(
                Since,
                Some("2.0.0-rc.1"),
                derived(Since, Some("2.0.0-rc.1")),
                None,
            ),
        ),
        ("c-M1SS1", st(Unversioned, None, unv(), None)),
        ("c-RED01", st(Unversioned, None, unv(), None)),
        ("c-REACH", st(Unversioned, None, unv(), None)),
        ("m-ENT01", st(Unversioned, None, unv(), None)),
        (
            "c-PF001",
            st(Since, Some("1.2.0"), derived(Since, Some("1.2.0")), None),
        ),
        (
            "c-S1NCE",
            st(
                Since,
                Some("0.9.0"),
                derived(Planned, None),
                owner("c-S1NCE", "since"),
            ),
        ),
        (
            plan_sec,
            st(Planned, None, unv(), owner(plan_sec, "planned")),
        ),
        (
            "c-NH001",
            st(
                Planned,
                None,
                derived(Since, Some("1.2.0")),
                owner(plan_sec, "planned"),
            ),
        ),
        (
            "c-0WN01",
            st(
                Since,
                Some("1.0.0"),
                derived(Since, Some("1.2.0")),
                owner("c-0WN01", "since"),
            ),
        ),
        (
            "c-0WNP1",
            st(Planned, None, unv(), owner("c-0WNP1", "planned")),
        ),
        (
            "c-FA15E",
            st(Planned, None, unv(), owner(plan_sec, "planned")),
        ),
        // a stub's own fields are not read, its container still covers it
        (
            "c-REDC1",
            st(Planned, None, unv(), owner(plan_sec, "planned")),
        ),
        ("c-REDC2", st(Unversioned, None, unv(), None)),
        ("s-N0F01", st(Planned, None, unv(), owner(pdoc, "planned"))),
        ("s-NST01", st(Planned, None, unv(), owner(pdoc, "planned"))),
        (
            "c-DEEP1",
            st(
                Planned,
                None,
                derived(Planned, None),
                owner(pdoc, "planned"),
            ),
        ),
        (
            "n-NEAR1",
            st(Planned, None, unv(), owner("s-PXN02", "planned")),
        ),
        (
            "s-S1N01",
            st(Since, Some("1.0.0"), unv(), owner("s-S1N01", "since")),
        ),
        (
            "c-S1N01",
            st(Since, Some("2.0.0"), derived(Since, Some("2.0.0")), None),
        ),
    ];
    for (at, want) in &cases {
        assert_eq!(r.status(&id(at)).as_ref(), Some(want), "{at}");
    }
    // a doc, a decision, a missing block: no status
    for at in [doc, pdoc, "D-V1200", "D-RED01", "c-ABSNT"] {
        assert_eq!(r.status(&id(at)), None, "{at}");
    }
    // the JSON shape and the labels
    let inh = r.status(&id("c-NH001")).unwrap();
    assert_eq!(
        serde_json::to_value(&inh).unwrap(),
        json!({"status": "planned", "since": null,
            "derived": {"status": "since", "since": "1.2.0"},
            "override": {"block": plan_sec, "field": "planned"}})
    );
    assert_eq!(inh.label(), "planned");
    assert_eq!(inh.derived.label(), "since 1.2.0");
    assert_eq!(r.status(&id("c-N0DEC")).unwrap().label(), "unversioned");
    // a custom content type takes both overrides and derives like any other
    let memo_pack = with_memo();
    let mut custom = blocks.clone();
    custom.push(linked(
        block("mm-MEM01", "memo", Some(sec)),
        "decided_by",
        "D-V1200",
        None,
    ));
    custom.push(set(
        block("mm-MEM02", "memo", Some(sec)),
        "planned",
        FieldValue::Bool(true),
    ));
    let cm = state(custom);
    let cr = Resolver::new(&memo_pack, &cm);
    assert_eq!(
        cr.status(&id("mm-MEM01")),
        Some(st(
            Since,
            Some("1.2.0"),
            derived(Since, Some("1.2.0")),
            None
        ))
    );
    assert_eq!(
        cr.status(&id("mm-MEM02")),
        Some(st(Planned, None, unv(), owner("mm-MEM02", "planned")))
    );
    // the standard pack knows no memo: no status
    let sr = Resolver::new(&pack, &cm);
    assert_eq!(sr.status(&id("mm-MEM01")), None);
    // version order is semantic: 1.10.0 is after 1.9.0
    let order = state(vec![
        block(doc, "doc", None),
        block(sec, "section", Some(doc)),
        decision("D-V1900", "accepted", Some("1.9.0")),
        decision("D-V1100", "accepted", Some("1.10.0")),
        linked(
            linked(claim("c-0RDER"), "decided_by", "D-V1900", None),
            "implements",
            "D-V1100",
            None,
        ),
    ]);
    assert_eq!(
        Resolver::new(&pack, &order)
            .status(&id("c-0RDER"))
            .unwrap()
            .since
            .as_deref(),
        Some("1.10.0")
    );
}

fn writer() -> Session {
    Session {
        actor: "human:qa".into(),
        run: None,
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

fn approver() -> Session {
    Session {
        actor: "human:qa".into(),
        run: None,
        role: Role::Approver,
        kind: SessionKind::Human,
    }
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    write_why(r, view, ops, None).unwrap()
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

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    common::answer_open_items(r, &writer(), view);
    r.propose(&writer(), view, view, Some(common::TEST_SUMMARY))
        .unwrap()
}

fn land(r: &mut Rdy, p: &ProposalInfo) -> String {
    common::confirm_exceptions(r, &approver(), p);
    r.land(&approver(), &p.id).unwrap()
}

fn label_at(st: &ReadState, scope: &Scope, at: &str) -> String {
    st.as_built(scope, [&id(at)]).unwrap()[0].1.label()
}

#[test]
fn ab2_scope_snapshots_do_not_reuse_tracking_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let seeded = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$a", "parent": "$d", "fields": {"title": "Alpha"}},
            {"op": "create", "type": "section", "id": "$b", "parent": "$d", "fields": {"title": "Beta"}},
            {"op": "create", "type": "decision", "id": "$x", "fields": {"statement": "Ship X.", "status": "proposed"}},
            {"op": "create", "type": "decision", "id": "$y", "fields": {"statement": "Ship Y.", "status": "proposed", "built": "2.0.0"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$a", "body": "X ships.", "links": {"decided_by": ["$x"]}}
        ]),
    );
    let (a, b, x, y, c) = (
        seeded.created["$a"].clone(),
        seeded.created["$b"].clone(),
        seeded.created["$x"].clone(),
        seeded.created["$y"].clone(),
        seeded.created["$c"].clone(),
    );
    let p = propose(&mut r, "seed");
    let l1 = land(&mut r, &p);
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    // a new decision defaults to planned, so the claim is planned in canon
    let old = r.read();
    assert_eq!(label_at(&old, &Scope::Canon, &c), "planned");
    let pack_tracking = old.pack().tracking();
    let x_before = old.block(&Scope::Canon, &id(&x)).unwrap();

    // a built-only edit of the accepted decision, in a view: an editorial tracking write
    write_why(
        &mut r,
        "edit",
        json!([{"op": "update", "id": x, "fields": {"built": "v1.1.0"}}]),
        Some("it shipped in 1.1.0"),
    )
    .unwrap();
    let new = r.read();
    let view = Scope::View("edit".into());
    let x_after = new.block(&view, &id(&x)).unwrap();
    // the hash a pin records leaves tracking out: it did not move, and the claim's version neither
    assert_eq!(
        x_before.content_hash_with(&pack_tracking),
        x_after.content_hash_with(&pack_tracking)
    );
    assert_eq!(
        new.materialized(&Scope::Canon).unwrap().version_of(&id(&c)),
        new.materialized(&view).unwrap().version_of(&id(&c))
    );
    // each scope reads its own decision; the old snapshot stays frozen
    assert_eq!(label_at(&new, &view, &c), "since 1.1.0");
    assert_eq!(label_at(&new, &Scope::Canon, &c), "planned");
    assert_eq!(label_at(&old, &Scope::Canon, &c), "planned");
    // a proposal version, its landing, the tag and canon after it
    let p2 = propose(&mut r, "edit");
    let proposal = r
        .read()
        .resolve_scope(&format!("proposal:{}", p2.r#ref))
        .unwrap();
    assert_eq!(label_at(&r.read(), &proposal, &c), "since 1.1.0");
    let l2 = land(&mut r, &p2);
    let now = r.read();
    assert_eq!(label_at(&now, &Scope::Canon, &c), "since 1.1.0");
    let at_l2 = now.resolve_scope(&format!("landing:{l2}")).unwrap();
    let at_l1 = now.resolve_scope(&format!("landing:{l1}")).unwrap();
    let tag = now.resolve_scope("tag:v1.0.0").unwrap();
    assert_eq!(label_at(&now, &at_l2, &c), "since 1.1.0");
    assert_eq!(label_at(&now, &at_l1, &c), "planned");
    assert_eq!(label_at(&now, &tag, &c), "planned");
    // id@version reads under that tag
    let pinned = rdy_model::reference::parse_target(&format!("{c}@v1.0.0")).unwrap();
    let text = pinned.scope_text(None).unwrap().unwrap();
    assert_eq!(
        label_at(&now, &now.resolve_scope(&text).unwrap(), &c),
        "planned"
    );
    // the old snapshots still read what they read
    assert_eq!(label_at(&old, &Scope::Canon, &c), "planned");
    assert_eq!(label_at(&new, &view, &c), "since 1.1.0");

    // a parent flag move: the section turns planned, then the claim moves out of it
    write(
        &mut r,
        "flag",
        json!([{"op": "update", "id": a, "fields": {"planned": true}}]),
    );
    let flag = Scope::View("flag".into());
    let read = r.read();
    let flagged = read.as_built(&flag, [&id(&c)]).unwrap()[0].1.clone();
    assert_eq!(flagged.label(), "planned");
    assert_eq!(flagged.owner.as_ref().unwrap().block, a);
    write(
        &mut r,
        "flag",
        json!([{"op": "move", "id": c, "parent": b}]),
    );
    assert_eq!(label_at(&r.read(), &flag, &c), "since 1.1.0");
    // a changed link
    write(
        &mut r,
        "flag",
        json!([{"op": "update", "id": c, "links": {"decided_by": [], "implements": [y]}}]),
    );
    assert_eq!(label_at(&r.read(), &flag, &c), "since 2.0.0");
    assert_eq!(label_at(&r.read(), &Scope::Canon, &c), "since 1.1.0");

    // warm and cold: one resolver asked many times equals a new resolver per block
    let st = r.read();
    for scope in [Scope::Canon, flag.clone(), tag.clone(), at_l1.clone()] {
        let m = st.materialized(&scope).unwrap();
        let ids: Vec<BlockId> = m.ids().cloned().collect();
        let warm = Resolver::new(st.pack(), &m);
        let first: Vec<Option<AsBuilt>> = ids.iter().map(|i| warm.status(i)).collect();
        let again: Vec<Option<AsBuilt>> = ids.iter().rev().map(|i| warm.status(i)).collect();
        let cold: Vec<Option<AsBuilt>> = ids
            .iter()
            .map(|i| Resolver::new(st.pack(), &m).status(i))
            .collect();
        assert_eq!(first, cold, "{scope:?}");
        assert_eq!(
            first,
            again.into_iter().rev().collect::<Vec<_>>(),
            "{scope:?}"
        );
    }
}

/// The as-built warnings of one write.
fn as_built_rows(w: &WriteResult) -> Vec<(String, String, String)> {
    w.warnings
        .iter()
        .filter(|i| i.code == W_AS_BUILT_OVERRIDE)
        .map(|i| {
            (
                i.block.as_ref().unwrap().to_string(),
                i.message.clone(),
                i.fix.clone(),
            )
        })
        .collect()
}

fn issue_rows(issues: &[rdy_schema::Issue]) -> Vec<(String, String, String)> {
    issues
        .iter()
        .filter(|i| i.code == W_AS_BUILT_OVERRIDE)
        .map(|i| {
            (
                i.block.as_ref().unwrap().to_string(),
                i.message.clone(),
                i.fix.clone(),
            )
        })
        .collect()
}

#[test]
fn ab2_override_warnings_follow_the_recorded_ruling() {
    assert_eq!(W_AS_BUILT_OVERRIDE, "W_AS_BUILT_OVERRIDE");
    assert_eq!(
        AS_BUILT_FIX_NO_ITEM,
        "Remove the override, or change it in a view and confirm its as-built plan item."
    );
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let base = write(
        &mut r,
        "v",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Alpha"}},
            {"op": "create", "type": "section", "id": "$p", "parent": "$d", "fields": {"title": "Planned", "planned": true}},
            {"op": "create", "type": "section", "id": "$q", "parent": "$d", "fields": {"title": "Later"}},
            {"op": "create", "type": "decision", "id": "$d2", "fields": {"statement": "Two.", "status": "proposed", "built": "2.0.0"}},
            {"op": "create", "type": "decision", "id": "$d12", "fields": {"statement": "One two.", "status": "proposed", "built": "v1.2.0"}},
            {"op": "create", "type": "decision", "id": "$dp", "fields": {"statement": "Later.", "status": "proposed"}},
            {"op": "create", "type": "decision", "id": "$dn", "fields": {"statement": "Process.", "status": "proposed", "needs_content": false}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Plain.", "links": {"decided_by": ["$d2"]}},
        ]),
    );
    let ids = |k: &str| base.created[k].clone();
    let (s, p, q, d2, d12, dp, dn, plain) = (
        ids("$s"),
        ids("$p"),
        ids("$q"),
        ids("$d2"),
        ids("$d12"),
        ids("$dp"),
        ids("$dn"),
        ids("$c"),
    );
    // nothing warns before any override
    assert!(as_built_rows(&base).is_empty(), "{:?}", base.warnings);
    let view = Scope::View("v".into());
    let before = r.read();
    let suspects_before = before.suspects(&view).unwrap();
    let coverage_before = before.coverage_issues(&view).unwrap();
    let plain_cov_before = coverage_before.blocks.get(&id(&plain)).cloned();

    let w = write(
        &mut r,
        "v",
        json!([
            // own planned over a since derivation: warns
            {"op": "create", "type": "claim", "id": "$a", "parent": s, "body": "A.", "fields": {"planned": true}, "links": {"decided_by": [d2]}},
            // own since over a planned derivation: warns
            {"op": "create", "type": "claim", "id": "$b", "parent": s, "body": "B.", "fields": {"since": "1.0.0"}, "links": {"decided_by": [dp]}},
            // own since 1.0.0 over since 2.0.0: warns
            {"op": "create", "type": "claim", "id": "$e", "parent": s, "body": "E.", "fields": {"since": "1.0.0"}, "links": {"implements": [d2]}},
            // equal after normalization: quiet
            {"op": "create", "type": "claim", "id": "$f", "parent": s, "body": "F.", "fields": {"since": "1.2.0"}, "links": {"decided_by": [d12]}},
            // overrides over an unversioned derivation: quiet
            {"op": "create", "type": "note", "id": "$g", "parent": s, "body": "G.", "fields": {"planned": true}},
            {"op": "create", "type": "claim", "id": "$h", "parent": s, "body": "H.", "fields": {"planned": true}, "links": {"decided_by": [dn]}},
            {"op": "create", "type": "claim", "id": "$i", "parent": s, "body": "I.", "fields": {"since": "1.0.0"}, "links": {"decided_by": [dn]}},
            // inherited planned over since: warns on the child and names the section
            {"op": "create", "type": "claim", "id": "$j", "parent": p, "body": "J.", "links": {"decided_by": [d2]}},
            // inherited planned over planned: quiet
            {"op": "create", "type": "claim", "id": "$k", "parent": p, "body": "K.", "links": {"decided_by": [dp]}},
            // planned: false alone: quiet
            {"op": "create", "type": "claim", "id": "$l", "parent": s, "body": "L.", "fields": {"planned": false}, "links": {"decided_by": [d2]}},
            // under a section with no flag yet
            {"op": "create", "type": "claim", "id": "$m", "parent": q, "body": "M.", "links": {"decided_by": [d2]}}
        ]),
    );
    let c = |k: &str| w.created[k].clone();
    let fix = AS_BUILT_FIX_NO_ITEM.to_string();
    let row = |at: &str, eff: &str, der: &str| {
        (
            at.to_string(),
            format!("Block {at} has override status {eff}, but its decisions derive {der}."),
            fix.clone(),
        )
    };
    let mut want = vec![
        row(&c("$a"), "planned", "since 2.0.0"),
        row(&c("$b"), "since 1.0.0", "planned"),
        row(&c("$e"), "since 1.0.0", "since 2.0.0"),
        row(&c("$j"), "planned", "since 2.0.0"),
    ];
    want.sort();
    let mut got = as_built_rows(&w);
    got.sort();
    assert_eq!(got, want);
    // the inherited owner is structured, not in the text
    let st = r.read();
    let j = st.as_built(&view, [&id(&c("$j"))]).unwrap()[0].1.clone();
    assert_eq!(
        j.owner,
        Some(Override {
            block: p.clone(),
            field: "planned".into()
        })
    );
    // read parity: context and brief give the same warnings for the same blocks
    let mut ctx_rows = vec![];
    for k in [
        "$a", "$b", "$e", "$f", "$g", "$h", "$i", "$j", "$k", "$l", "$m",
    ] {
        let ctx = st.context(&view, &id(&c(k))).unwrap();
        assert_eq!(
            ctx.as_built.as_ref(),
            Some(&st.as_built(&view, [&id(&c(k))]).unwrap()[0].1),
            "{k}"
        );
        ctx_rows.extend(issue_rows(&ctx.warnings));
    }
    ctx_rows.sort();
    assert_eq!(ctx_rows, want);
    let brief = st
        .brief(
            &view,
            &Coverage {
                ids: vec![],
                path: Some(s.clone()),
            },
            Some(100_000),
        )
        .unwrap();
    let mut brief_rows = issue_rows(&brief.warnings);
    brief_rows.sort();
    let mut in_s: Vec<_> = want
        .iter()
        .filter(|(b, ..)| *b != c("$j"))
        .cloned()
        .collect();
    in_s.sort();
    assert_eq!(brief_rows, in_s);
    // the decision itself is no content block: no status
    assert!(st.context(&view, &id(&d2)).unwrap().as_built.is_none());

    // a written decision warns for its direct citers: d2 is now 1.0.0, so e matches, a and j
    // still disagree, and the plain claim derives 1.0.0 with no override
    let w2 = write(
        &mut r,
        "v",
        json!([{"op": "update", "id": d2, "fields": {"built": "1.0.0"}}]),
    );
    let mut got = as_built_rows(&w2);
    got.sort();
    let mut want2 = vec![
        row(&c("$a"), "planned", "since 1.0.0"),
        row(&c("$j"), "planned", "since 1.0.0"),
    ];
    want2.sort();
    assert_eq!(got, want2);
    // a written section warns for the blocks under it
    let w3 = write(
        &mut r,
        "v",
        json!([{"op": "update", "id": q, "fields": {"planned": true}}]),
    );
    assert_eq!(
        as_built_rows(&w3),
        vec![row(&c("$m"), "planned", "since 1.0.0")]
    );

    // no suspect and no coverage change from an override alone
    let after = r.read();
    let plain_cov_after = after
        .coverage_issues(&view)
        .unwrap()
        .blocks
        .get(&id(&plain))
        .cloned();
    assert_eq!(
        plain_cov_before.map(|e| e.issues),
        plain_cov_after.map(|e| e.issues)
    );
    let suspects_after = after.suspects(&view).unwrap();
    assert_eq!(suspects_before.len(), suspects_after.len());
    let _ = (dn, d12);
}
