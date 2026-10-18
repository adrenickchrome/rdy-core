//! The holder's structural check (D-BPYN8, D-DAGNB, O11): a holder has no canon, so its fsck
//! walks each tag's own chain and the objects the tags reach, never the global change -> landing
//! index. These tests build the objects by hand, with block types no pack knows.
use rdy_model::*;
use rdy_store::check_holder::{MAX_EDGES, MAX_LANDING_DEPTH, MAX_OBJECTS, holder_check_limit};
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::{Store, StoreError};
use std::collections::BTreeMap;

fn holder() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::create_holder(&dir.path().join("h"), RepoId([7; 16])).unwrap();
    (dir, s)
}

fn prov(at: i64) -> Provenance {
    Provenance {
        actor: "human:a".into(),
        run: None,
        role: Role::Approver,
        at_ms: at,
    }
}

fn block(id: &str, ty: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: ty.into(),
        parent: None,
        order: "a".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn change(ops: Vec<Op>, deps: Vec<Hash>, message: &str) -> Change {
    Change {
        view: "work".into(),
        ops,
        deps,
        provenance: prov(1),
        justification: None,
        message: message.into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// Writes `b` and returns its version.
fn put_block(s: &mut Store, b: &Block) -> Hash {
    s.put(kinds::BLOCK, b).unwrap()
}

fn put_change(s: &mut Store, c: &Change) -> Hash {
    s.put(kinds::CHANGE, c).unwrap()
}

/// Lands `changes` on `prev` with the state `blocks` (id -> version): writes the snapshot, a
/// proposal titled `title` and the landing. Returns the landing.
fn land(
    s: &mut Store,
    prev: Option<Hash>,
    blocks: &[&Block],
    changes: &[Hash],
    title: &str,
) -> Hash {
    let state = StateSnapshot {
        blocks: blocks
            .iter()
            .map(|b| (b.id.clone(), b.version_hash()))
            .collect(),
    };
    let state = s.put(kinds::SNAPSHOT, &state).unwrap();
    let proposal = Proposal {
        stable: None,
        prev: None,
        title: title.into(),
        view: "work".into(),
        changes: changes.to_vec(),
        alternative: None,
        provenance: prov(2),
        canon_at: prev,
        summary: None,
    };
    let proposal = s.put(kinds::PROPOSAL, &proposal).unwrap();
    let rec = LandingRecord {
        proposal,
        prev,
        state,
        approved_by: prov(3),
        changes: changes.to_vec(),
        chosen: None,
        checks: vec![],
        assurance: Assurance::Local,
        relayed_by: None,
        relay_evidence: None,
        approved_version: None,
        rebased_from: vec![],
        evidence: None,
    };
    s.put(kinds::LANDING, &rec).unwrap()
}

/// Writes the tag `name` on `landing` and its `tag/<name>` ref. Returns the tag's hash.
fn tag(s: &mut Store, name: &str, landing: Hash) -> Hash {
    let t = Tag {
        name: name.into(),
        landing,
        message: None,
        tagged_by: prov(4),
    };
    let h = s.put(kinds::TAG, &t).unwrap();
    s.set_refs(vec![(format!("tag/{name}"), h.0.to_vec())])
        .unwrap();
    h
}

/// A first landing `l1` creating `base`, then two landings on it that diverge: `left` and
/// `right` each update `base`.
struct Divergent {
    base: Block,
    left: Block,
    right: Block,
    c1: Hash,
    c_left: Hash,
    c_right: Hash,
    l1: Hash,
    l_left: Hash,
    l_right: Hash,
}

/// [`Divergent`] in `s`; `write_right` false leaves the right block unwritten.
fn divergent(s: &mut Store, write_right: bool) -> Divergent {
    let base = block("b-00001", "widget", "base");
    let v_base = put_block(s, &base);
    let c1 = put_change(
        s,
        &change(
            vec![Op::Create {
                block: base.clone(),
            }],
            vec![],
            "one",
        ),
    );
    let l1 = land(s, None, &[&base], &[c1], "first");
    let mut left = base.clone();
    left.body = "left".into();
    put_block(s, &left);
    let c_left = put_change(
        s,
        &change(
            vec![Op::Update {
                base: v_base,
                block: left.clone(),
            }],
            vec![c1],
            "left",
        ),
    );
    let l_left = land(s, Some(l1), &[&left], &[c_left], "left");
    let mut right = base.clone();
    right.body = "right".into();
    if write_right {
        put_block(s, &right);
    }
    let c_right = put_change(
        s,
        &change(
            vec![Op::Update {
                base: v_base,
                block: right.clone(),
            }],
            vec![c1],
            "right",
        ),
    );
    let l_right = land(s, Some(l1), &[&right], &[c_right], "right");
    Divergent {
        base,
        left,
        right,
        c1,
        c_left,
        c_right,
        l1,
        l_left,
        l_right,
    }
}

fn decode_text<T: std::fmt::Debug>(r: Result<T, StoreError>) -> String {
    match r {
        Err(StoreError::Decode(m)) => m,
        other => panic!("not a Decode refusal: {other:?}"),
    }
}

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

#[test]
fn ha7_check_holder_walks_every_tag_chain() {
    let (_d, mut s) = holder();
    let d = divergent(&mut s, true);
    tag(&mut s, "v1.0.0", d.l_left);
    tag(&mut s, "v2.0.0", d.l_right);
    tag(&mut s, "v0.1.0", d.l1);
    let c = s.check_holder().unwrap();
    assert_eq!(c.landed_changes, 3, "c1, left and right, each once");
    assert_eq!(
        (c.views, c.view_ops, c.relayed, c.transitions, c.answers),
        (0, 0, 0, 0, 0)
    );
    // each tag's chain is its own history
    let chain: Vec<Hash> = s
        .tag_chain(&d.l_right)
        .unwrap()
        .into_iter()
        .map(|(h, _)| h)
        .collect();
    assert_eq!(chain, vec![d.l_right, d.l1]);
    let chain: Vec<Hash> = s
        .tag_chain(&d.l_left)
        .unwrap()
        .into_iter()
        .map(|(h, _)| h)
        .collect();
    assert_eq!(chain, vec![d.l_left, d.l1]);
    assert_eq!(s.canon_head().unwrap(), None, "no canon is read or made");
    let _ = (&d.base, &d.left, &d.right, d.c_left, d.c_right);

    // the second history is checked too: its block is missing
    let (_d2, mut s) = holder();
    let d = divergent(&mut s, false);
    tag(&mut s, "v1.0.0", d.l_left);
    tag(&mut s, "v2.0.0", d.l_right);
    let text = decode_text(s.check_holder());
    let missing = key(kinds::BLOCK, d.right.version_hash());
    assert!(
        text.ends_with(&format!("names {missing}, which is not stored")),
        "{text}"
    );
    assert!(text.starts_with("object "), "{text}");
}

#[test]
fn ha7_check_holder_judges_landed_changes_per_chain() {
    // one change landed by two landings, each in its own chain: the global index holds only
    // one of them, and the holder never asks it
    let (_d, mut s) = holder();
    let base = block("b-00001", "widget", "base");
    let v_base = put_block(&mut s, &base);
    let c1 = put_change(
        &mut s,
        &change(
            vec![Op::Create {
                block: base.clone(),
            }],
            vec![],
            "one",
        ),
    );
    let l1 = land(&mut s, None, &[&base], &[c1], "first");
    let mut next = base.clone();
    next.body = "next".into();
    put_block(&mut s, &next);
    let c2 = put_change(
        &mut s,
        &change(
            vec![Op::Update {
                base: v_base,
                block: next.clone(),
            }],
            vec![c1],
            "two",
        ),
    );
    let la = land(&mut s, Some(l1), &[&next], &[c2], "a");
    let lb = land(&mut s, Some(l1), &[&next], &[c2], "b");
    assert_ne!(la, lb);
    tag(&mut s, "v1.0.0", la);
    tag(&mut s, "v2.0.0", lb);
    let global = s.landing_of(&c2).unwrap();
    assert!(global == Some(la) || global == Some(lb), "{global:?}");
    let c = s.check_holder().unwrap();
    assert_eq!(c.landed_changes, 2, "distinct landed changes");

    // the same change twice in one chain is damage
    let l3 = land(&mut s, Some(la), &[&next], &[c2], "again");
    tag(&mut s, "v3.0.0", l3);
    assert_eq!(
        decode_text(s.check_holder()),
        format!("tag/v3.0.0: change {c2} lands in landing {l3} and in landing {la}")
    );

    // a landed change whose dependency the chain never landed is damage
    let (_d2, mut s) = holder();
    let base = block("b-00001", "widget", "base");
    put_block(&mut s, &base);
    let c1 = put_change(
        &mut s,
        &change(
            vec![Op::Create {
                block: base.clone(),
            }],
            vec![],
            "one",
        ),
    );
    let other = block("b-00002", "widget", "other");
    put_block(&mut s, &other);
    let c_other = put_change(
        &mut s,
        &change(
            vec![Op::Create {
                block: other.clone(),
            }],
            vec![],
            "other",
        ),
    );
    let l_other = land(&mut s, None, &[&other], &[c_other], "other");
    let third = block("b-00003", "widget", "third");
    put_block(&mut s, &third);
    let c3 = put_change(
        &mut s,
        &change(
            vec![Op::Create {
                block: third.clone(),
            }],
            vec![c1],
            "three",
        ),
    );
    let l3 = land(&mut s, Some(l_other), &[&other, &third], &[c3], "three");
    tag(&mut s, "v1.0.0", l3);
    assert_eq!(
        decode_text(s.check_holder()),
        format!(
            "tag/v1.0.0: change {c3} of landing {l3} depends on {c1}, which is not landed at or before it"
        )
    );
}

/// An answer of act `act` to item `item`, targeting `target`.
fn answer(item: &str, act: AnswerAct, target: Hash) -> Answer {
    Answer {
        item: item.into(),
        act,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![target],
        person: prov(5),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    }
}

#[test]
fn ha7_check_holder_checks_answer_tag_refs() {
    let (_d, mut s) = holder();
    let d = divergent(&mut s, true);
    let t1 = tag(&mut s, "v1.0.0", d.l_left);
    let good = s
        .put(kinds::ANSWER, &answer("tag:v1.0.0", AnswerAct::Tag, t1))
        .unwrap();
    let forge = |s: &mut Store, name: &str, h: Hash| {
        s.set_refs_unchecked_for_tests(vec![(name.into(), h.0.to_vec())])
            .unwrap();
    };
    forge(&mut s, "answer/tag:v1.0.0", good);
    let c = s.check_holder().unwrap();
    assert_eq!(c.answers, 1);
    let refused = "holder ref answer/tag:v1.0.0 is not allowed";
    // an answer of another act
    let confirm = s
        .put(kinds::ANSWER, &answer("tag:v1.0.0", AnswerAct::Confirm, t1))
        .unwrap();
    forge(&mut s, "answer/tag:v1.0.0", confirm);
    assert_eq!(decode_text(s.check_holder()), refused);
    // an answer to another item
    let other = s
        .put(kinds::ANSWER, &answer("tag:v2.0.0", AnswerAct::Tag, t1))
        .unwrap();
    forge(&mut s, "answer/tag:v1.0.0", other);
    assert_eq!(decode_text(s.check_holder()), refused);
    // an object that is not an answer
    forge(&mut s, "answer/tag:v1.0.0", t1);
    assert_eq!(decode_text(s.check_holder()), refused);
    // a value that is not a hash
    s.set_refs_unchecked_for_tests(vec![("answer/tag:v1.0.0".into(), vec![1, 2, 3])])
        .unwrap();
    assert_eq!(decode_text(s.check_holder()), refused);
    // the good answer again checks clean
    forge(&mut s, "answer/tag:v1.0.0", good);
    s.check_holder().unwrap();
    // an answer ref with no tag beside it
    forge(&mut s, "answer/tag:v9.0.0", good);
    assert_eq!(
        decode_text(s.check_holder()),
        "holder ref answer/tag:v9.0.0 is not allowed"
    );
}

#[test]
fn ha7_check_holder_bounds_every_walk() {
    assert_eq!(
        (MAX_LANDING_DEPTH, MAX_OBJECTS, MAX_EDGES),
        (100_000, 1_000_000, 1_000_000)
    );
    let (_d, mut s) = holder();
    let d = divergent(&mut s, true);
    tag(&mut s, "v1.0.0", d.l_left);
    s.check_holder().unwrap();
    let limit = |r: Result<rdy_store::StoreCheck, StoreError>| {
        let e = r.unwrap_err();
        let label = holder_check_limit(&e).map(str::to_string);
        assert_eq!(
            e.to_string(),
            format!("holder check exceeds {}", label.clone().unwrap())
        );
        label.unwrap()
    };
    // the chain holds two landings
    s.check_holder_bounded_for_tests(2, 1000, 1000).unwrap();
    assert_eq!(
        limit(s.check_holder_bounded_for_tests(1, 1000, 1000)),
        "1 landing depth"
    );
    assert_eq!(
        limit(s.check_holder_bounded_for_tests(1000, 3, 1000)),
        "3 objects"
    );
    assert_eq!(
        limit(s.check_holder_bounded_for_tests(1000, 1000, 2)),
        "2 edges"
    );
    assert_eq!(
        holder_check_limit(&StoreError::Refused("other".into())),
        None
    );
}

#[test]
fn ha7_check_holder_has_no_schema_and_no_identity_links() {
    let (_d, mut s) = holder();
    // a block type no pack knows, with a pin to a version no store holds: a pin is an
    // identity, not a link
    let mut b = block("b-00001", "no-such-type", "text");
    b.fields
        .insert("anything".into(), FieldValue::Str("value".into()));
    b.pins
        .insert(BlockId::parse("b-00009").unwrap(), Hash([9; 32]));
    put_block(&mut s, &b);
    let c1 = put_change(
        &mut s,
        &change(vec![Op::Create { block: b.clone() }], vec![], "one"),
    );
    let l1 = land(&mut s, None, &[&b], &[c1], "first");
    tag(&mut s, "v1.0.0", l1);
    let c = s.check_holder().unwrap();
    assert_eq!(c.landed_changes, 1);
}

#[test]
fn ha7_check_holder_refuses_damaged_tags_and_projects() {
    let (_d, mut s) = holder();
    let d = divergent(&mut s, true);
    s.set_refs(vec![("tag/v1.0.0".into(), d.l1.0.to_vec())])
        .unwrap();
    let text = decode_text(s.check_holder());
    assert!(
        text.starts_with(&format!("tag/v1.0.0 names {}, which is not a tag", d.l1)),
        "{text}"
    );
    s.set_refs(vec![("tag/v1.0.0".into(), vec![1])]).unwrap();
    assert_eq!(
        decode_text(s.check_holder()),
        "tag/v1.0.0: the ref is not a 32-byte hash"
    );
    let t = Tag {
        name: "v2.0.0".into(),
        landing: d.l1,
        message: None,
        tagged_by: prov(4),
    };
    let h = s.put(kinds::TAG, &t).unwrap();
    s.set_refs(vec![("tag/v1.0.0".into(), h.0.to_vec())])
        .unwrap();
    assert_eq!(
        decode_text(s.check_holder()),
        "tag/v1.0.0 holds the tag \"v2.0.0\""
    );
    let _ = (d.c1, d.c_left, d.c_right, d.l_left, d.l_right);

    let dir = tempfile::tempdir().unwrap();
    let p = Store::create(&dir.path().join("p")).unwrap();
    assert_eq!(decode_text(p.check_holder()), "the store is not a holder");
}
