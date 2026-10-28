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
    land_state(s, prev, &state, changes, title, |_| {})
}

/// [`land`] with the state `state`, and the landing record as `edit` leaves it.
fn land_state(
    s: &mut Store,
    prev: Option<Hash>,
    state: &StateSnapshot,
    changes: &[Hash],
    title: &str,
    edit: impl FnOnce(&mut LandingRecord),
) -> Hash {
    let state = s.put(kinds::SNAPSHOT, state).unwrap();
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
        assessment: None,
        policy: None,
        read_identity: None,
    };
    let mut rec = rec;
    edit(&mut rec);
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

/// A holder with a tag `v1.0.0` and its relay answer, not yet named by any answer ref: the
/// store, the tag and the answer.
fn relayed_holder() -> (tempfile::TempDir, Store, Hash, Hash, Hash) {
    let (d, mut s) = holder();
    let div = divergent(&mut s, true);
    let t1 = tag(&mut s, "v1.0.0", div.l_left);
    let good = s
        .put(kinds::ANSWER, &answer("tag:v1.0.0", AnswerAct::Tag, t1))
        .unwrap();
    (d, s, t1, good, div.l_left)
}

fn forge(s: &mut Store, name: &str, h: Hash) {
    s.set_refs_unchecked_for_tests(vec![(name.into(), h.0.to_vec())])
        .unwrap();
}

#[test]
fn ha7_check_holder_checks_answer_tag_object_refs() {
    let (_d, mut s, t1, good, l) = relayed_holder();
    // the relay answer of a tag is found by the tag object (O11, ruling task-13)
    let name = format!("answer/tag-object:{t1}");
    forge(&mut s, &name, good);
    let c = s.check_holder().unwrap();
    assert_eq!(c.answers, 1);
    let refused = format!("holder ref {name} is not allowed");
    // an answer of another act
    let confirm = s
        .put(kinds::ANSWER, &answer("tag:v1.0.0", AnswerAct::Confirm, t1))
        .unwrap();
    forge(&mut s, &name, confirm);
    assert_eq!(decode_text(s.check_holder()), refused);
    // an answer to another item
    let other = s
        .put(kinds::ANSWER, &answer("tag:v2.0.0", AnswerAct::Tag, t1))
        .unwrap();
    forge(&mut s, &name, other);
    assert_eq!(decode_text(s.check_holder()), refused);
    // an object that is not an answer
    forge(&mut s, &name, t1);
    assert_eq!(decode_text(s.check_holder()), refused);
    // a value that is not a hash
    s.set_refs_unchecked_for_tests(vec![(name.clone(), vec![1, 2, 3])])
        .unwrap();
    assert_eq!(decode_text(s.check_holder()), refused);
    // the good answer again checks clean
    forge(&mut s, &name, good);
    s.check_holder().unwrap();

    // a twin's tag object of the same version kept beside it, with its own relay answer
    let twin = s
        .put(
            kinds::TAG,
            &Tag {
                name: "1.0.0".into(),
                landing: l,
                message: Some("twin".into()),
                tagged_by: prov(9),
            },
        )
        .unwrap();
    let twin_answer = s
        .put(kinds::ANSWER, &answer("tag:1.0.0", AnswerAct::Tag, twin))
        .unwrap();
    let twin_ref = format!("answer/tag-object:{twin}");
    forge(&mut s, &twin_ref, twin_answer);
    s.check_holder().unwrap();
    // its relay answer is one of its own name
    forge(&mut s, &twin_ref, good);
    assert_eq!(
        decode_text(s.check_holder()),
        format!("holder ref {twin_ref} is not allowed")
    );

    // names that name no stored TAG: an object of another kind, nothing, a name that is not
    // 64 lowercase hex digits, and a local project's ref by tag name
    for bad in [
        format!("answer/tag-object:{good}"),
        format!("answer/tag-object:{}", Hash([9; 32])),
        format!("answer/tag-object:{}", t1.to_string().to_uppercase()),
        format!("answer/tag-object:{}", &t1.to_string()[..63]),
        "answer/tag:v1.0.0".to_string(),
    ] {
        let (_d, mut s, _, good, _) = relayed_holder();
        forge(&mut s, &bad, good);
        assert_eq!(
            decode_text(s.check_holder()),
            format!("holder ref {bad} is not allowed")
        );
    }

    // the TAG an answer ref names is read with the tags' objects: one whose landing is not
    // stored is damage
    let (_d, mut s, _, _, _) = relayed_holder();
    let lost = Hash([8; 32]);
    let stray = s
        .put(
            kinds::TAG,
            &Tag {
                name: "1.0.0".into(),
                landing: lost,
                message: None,
                tagged_by: prov(9),
            },
        )
        .unwrap();
    let stray_answer = s
        .put(kinds::ANSWER, &answer("tag:1.0.0", AnswerAct::Tag, stray))
        .unwrap();
    forge(&mut s, &format!("answer/tag-object:{stray}"), stray_answer);
    assert_eq!(
        decode_text(s.check_holder()),
        format!(
            "object {} names {}, which is not stored",
            key(kinds::TAG, stray),
            key(kinds::LANDING, lost)
        )
    );
}

/// Fix round 2: an answer ref names the relay answer of exactly its TAG object, so two tag
/// objects of one name never share an answer, and an answer that targets anything else (no
/// object, the tag and more) is not a tag object's relay answer.
#[test]
fn ha13_answer_tag_object_refs_name_the_answer_of_their_own_tag() {
    let (_d, mut s, _t1, _good, l) = relayed_holder();
    let twin = |s: &mut Store, message: &str| {
        s.put(
            kinds::TAG,
            &Tag {
                name: "1.0.0".into(),
                landing: l,
                message: Some(message.into()),
                tagged_by: prov(9),
            },
        )
        .unwrap()
    };
    let a = twin(&mut s, "A");
    let b = twin(&mut s, "B");
    assert_ne!(a, b);
    let of_a = s
        .put(kinds::ANSWER, &answer("tag:1.0.0", AnswerAct::Tag, a))
        .unwrap();
    let of_b = s
        .put(kinds::ANSWER, &answer("tag:1.0.0", AnswerAct::Tag, b))
        .unwrap();
    let (ref_a, ref_b) = (
        format!("answer/tag-object:{a}"),
        format!("answer/tag-object:{b}"),
    );
    // each tag object names its own relay answer
    forge(&mut s, &ref_a, of_a);
    forge(&mut s, &ref_b, of_b);
    s.check_holder().unwrap();
    // crossed: B's ref names A's answer, then A's ref names B's answer
    forge(&mut s, &ref_b, of_a);
    assert_eq!(
        decode_text(s.check_holder()),
        format!("holder ref {ref_b} is not allowed")
    );
    forge(&mut s, &ref_b, of_b);
    forge(&mut s, &ref_a, of_b);
    assert_eq!(
        decode_text(s.check_holder()),
        format!("holder ref {ref_a} is not allowed")
    );
    forge(&mut s, &ref_a, of_a);
    s.check_holder().unwrap();
    // an answer of the right act and name that targets no object, or the tag and another
    for targets in [vec![], vec![b, a], vec![b, b]] {
        let loose = s
            .put(
                kinds::ANSWER,
                &Answer {
                    targets: targets.clone(),
                    ..answer("tag:1.0.0", AnswerAct::Tag, b)
                },
            )
            .unwrap();
        forge(&mut s, &ref_b, loose);
        assert_eq!(
            decode_text(s.check_holder()),
            format!("holder ref {ref_b} is not allowed"),
            "{targets:?}"
        );
    }
    forge(&mut s, &ref_b, of_b);
    s.check_holder().unwrap();
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

/// The state with `blocks` at their versions.
fn snapshot(blocks: &[&Block]) -> StateSnapshot {
    StateSnapshot {
        blocks: blocks
            .iter()
            .map(|b| (b.id.clone(), b.version_hash()))
            .collect(),
    }
}

#[test]
fn ha7_check_holder_refuses_a_snapshot_version_of_another_block() {
    let (_d, mut s) = holder();
    // a block type no pack knows: the check is structural, never a schema verdict
    let b = block("b-00001", "no-such-type", "text");
    let v = put_block(&mut s, &b);
    let c1 = put_change(
        &mut s,
        &change(vec![Op::Create { block: b.clone() }], vec![], "one"),
    );
    let l1 = land(&mut s, None, &[&b], &[c1], "first");
    tag(&mut s, "v1.0.0", l1);
    s.check_holder().unwrap();
    // the next snapshot holds the version again, under another block's id
    let other = BlockId::parse("b-00002").unwrap();
    let state = StateSnapshot {
        blocks: BTreeMap::from([(b.id.clone(), v), (other.clone(), v)]),
    };
    let l2 = land_state(&mut s, Some(l1), &state, &[], "second", |_| {});
    tag(&mut s, "v2.0.0", l2);
    let snap = s.landing(&l2).unwrap().state;
    assert_eq!(
        decode_text(s.check_holder()),
        format!(
            "snapshot {snap}: block {other} names version {v}, which is a version of {}",
            b.id
        )
    );
    // the same, where the walk reads the version before the snapshot that misnames it
    let (_d, mut s) = holder();
    let v = put_block(&mut s, &b);
    let bad = StateSnapshot {
        blocks: BTreeMap::from([(other.clone(), v)]),
    };
    let l1 = land_state(&mut s, None, &bad, &[], "first", |_| {});
    let l2 = land(&mut s, Some(l1), &[&b], &[], "second");
    tag(&mut s, "v1.0.0", l2);
    let snap = s.landing(&l1).unwrap().state;
    assert_eq!(
        decode_text(s.check_holder()),
        format!(
            "snapshot {snap}: block {other} names version {v}, which is a version of {}",
            b.id
        )
    );
}

/// A holder whose tag v1.0.0 lands a figure naming the blob manifest that `put` writes.
fn figure_holder(put: impl FnOnce(&mut Store) -> Hash) -> (tempfile::TempDir, Store, Hash) {
    let (d, mut s) = holder();
    let m = put(&mut s);
    let mut b = block("b-00001", "figure", "a figure");
    b.fields
        .insert("blob".into(), FieldValue::Str(m.to_string()));
    put_block(&mut s, &b);
    let c1 = put_change(
        &mut s,
        &change(vec![Op::Create { block: b.clone() }], vec![], "one"),
    );
    let l1 = land(&mut s, None, &[&b], &[c1], "first");
    tag(&mut s, "v1.0.0", l1);
    (d, s, m)
}

/// Writes one raw chunk of `bytes`.
fn put_chunk(s: &mut Store, bytes: &[u8]) -> Hash {
    s.write(vec![(kinds::BLOB_CHUNK, bytes.to_vec())]).unwrap()[0]
}

#[test]
fn ha7_check_holder_checks_blob_manifests_and_chunks() {
    // what put_blob writes checks clean
    let (_d, s, m) = figure_holder(|s| s.put_blob(b"a figure's bytes").unwrap());
    s.check_holder().unwrap();
    assert_eq!(s.get_blob(&m).unwrap(), b"a figure's bytes");
    // a manifest of two bytes whose one chunk holds one: a read refuses it, and so does fsck
    let (_d, s, m) = figure_holder(|s| {
        let c = put_chunk(s, &[1]);
        s.put(
            kinds::BLOB_MANIFEST,
            &kinds::BlobManifest {
                len: 2,
                chunks: vec![c],
            },
        )
        .unwrap()
    });
    let text = format!("blob {m} has a chunk of 1 bytes where 2 are due");
    assert_eq!(decode_text(s.get_blob(&m)), text);
    assert_eq!(decode_text(s.check_holder()), text);
    // a manifest listing two chunks for one byte
    let (_d, s, m) = figure_holder(|s| {
        let c = put_chunk(s, &[1]);
        s.put(
            kinds::BLOB_MANIFEST,
            &kinds::BlobManifest {
                len: 1,
                chunks: vec![c, c],
            },
        )
        .unwrap()
    });
    assert_eq!(
        decode_text(s.check_holder()),
        format!("blob {m} lists 2 chunks for 1 bytes")
    );
}

#[test]
fn ha7_check_holder_checks_every_reached_landing() {
    // a relayed landing with no relay fields, reached by another link than a tag's prev chain
    for link in ["rebased_from", "approved_version"] {
        let (_d, mut s) = holder();
        let b = block("b-00001", "widget", "base");
        put_block(&mut s, &b);
        let c1 = put_change(
            &mut s,
            &change(vec![Op::Create { block: b.clone() }], vec![], "one"),
        );
        let bad = land_state(&mut s, None, &snapshot(&[&b]), &[c1], "bad", |r| {
            r.assurance = Assurance::Relayed;
        });
        // a proposal version whose base is that landing
        let approved = s
            .put(
                kinds::PROPOSAL,
                &Proposal {
                    stable: None,
                    prev: None,
                    title: "approved".into(),
                    view: "work".into(),
                    changes: vec![c1],
                    alternative: None,
                    provenance: prov(2),
                    canon_at: Some(bad),
                    summary: None,
                },
            )
            .unwrap();
        let good = land_state(&mut s, None, &snapshot(&[&b]), &[c1], "good", |r| {
            if link == "rebased_from" {
                r.rebased_from = vec![bad];
            } else {
                r.approved_version = Some(approved);
            }
        });
        tag(&mut s, "v1.0.0", good);
        assert_eq!(
            decode_text(s.check_holder()),
            format!("landing {bad}: assurance is relayed but the relay fields are unset"),
            "{link}"
        );
    }
}

#[test]
fn ha7_tag_histories_read_each_landing_once() {
    for n in [64usize, 128] {
        let (_d, mut s) = holder();
        let state = StateSnapshot::default();
        let mut prev = None;
        let mut heads = vec![];
        for i in 0..n {
            let l = land_state(&mut s, prev, &state, &[], &format!("l{i}"), |_| {});
            tag(&mut s, &format!("v{i}.0.0"), l);
            heads.push(l);
            prev = Some(l);
        }
        // oldest first: each newer head's history holds every older one
        let before = s.frames_read();
        let all = s.tag_histories(&heads).unwrap();
        assert_eq!(
            s.frames_read() - before,
            n as u64,
            "each landing is read once"
        );
        assert_eq!(all.len(), n);
        assert!(all.iter().all(|h| heads.contains(h)));
        let c = s.check_holder().unwrap();
        assert_eq!(c.landed_changes, 0);
    }
    // the depth bound is each history's own, also where it joins one already walked
    let (_d, mut s) = holder();
    let state = StateSnapshot::default();
    let l1 = land_state(&mut s, None, &state, &[], "l1", |_| {});
    let l2 = land_state(&mut s, Some(l1), &state, &[], "l2", |_| {});
    let l3 = land_state(&mut s, Some(l2), &state, &[], "l3", |_| {});
    s.tag_histories_bounded_for_tests(&[l1, l3], 3).unwrap();
    let e = s.tag_histories_bounded_for_tests(&[l1, l3], 2).unwrap_err();
    assert_eq!(holder_check_limit(&e), Some("2 landing depth"), "{e}");
    let e = s.tag_histories_bounded_for_tests(&[l3], 2).unwrap_err();
    assert_eq!(holder_check_limit(&e), Some("2 landing depth"), "{e}");
}

/// The chunk size `put_blob` cuts.
const CHUNK: usize = 1024 * 1024;

/// A holder with one figure block per manifest that `put` writes, all in one tagged landing.
fn figures_holder(
    put: impl FnOnce(&mut Store) -> Vec<Hash>,
) -> (tempfile::TempDir, Store, Vec<Hash>) {
    let (d, mut s) = holder();
    let ms = put(&mut s);
    let mut blocks = vec![];
    for (i, m) in ms.iter().enumerate() {
        let mut b = block(&format!("b-{:05}", i + 1), "figure", "a figure");
        b.fields
            .insert("blob".into(), FieldValue::Str(m.to_string()));
        put_block(&mut s, &b);
        blocks.push(b);
    }
    let ops = blocks
        .iter()
        .map(|b| Op::Create { block: b.clone() })
        .collect();
    let c1 = put_change(&mut s, &change(ops, vec![], "one"));
    let refs: Vec<&Block> = blocks.iter().collect();
    let l1 = land(&mut s, None, &refs, &[c1], "first");
    tag(&mut s, "v1.0.0", l1);
    (d, s, ms)
}

/// Writes the manifest of `len` bytes over `chunks`.
fn put_manifest(s: &mut Store, len: u64, chunks: Vec<Hash>) -> Hash {
    s.put(kinds::BLOB_MANIFEST, &kinds::BlobManifest { len, chunks })
        .unwrap()
}

/// The frames one holder check reads, which must pass.
fn check_frames(s: &Store) -> u64 {
    let before = s.frames_read();
    s.check_holder().unwrap();
    s.frames_read() - before
}

#[test]
fn ha7_check_holder_reads_a_repeated_chunk_once() {
    // one full chunk listed k times: the check reads it once, and still checks every position
    let mut frames = vec![];
    for k in [1usize, 8] {
        let (_d, s, ms) = figures_holder(|s| {
            let c = put_chunk(s, &vec![7; CHUNK]);
            vec![put_manifest(s, (k * CHUNK) as u64, vec![c; k])]
        });
        assert_eq!(s.get_blob(&ms[0]).unwrap().len(), k * CHUNK);
        frames.push(check_frames(&s));
    }
    assert_eq!(frames[1], frames[0], "a repeated chunk is read once");
}

#[test]
fn ha7_check_holder_reads_a_chunk_shared_across_manifests_once() {
    // two manifests over one chunk read one chunk; over two chunks, two
    let shared = figures_holder(|s| {
        let c = put_chunk(s, &vec![7; CHUNK]);
        vec![
            put_manifest(s, CHUNK as u64, vec![c]),
            put_manifest(s, 2 * CHUNK as u64, vec![c, c]),
        ]
    });
    let distinct = figures_holder(|s| {
        let c = put_chunk(s, &vec![7; CHUNK]);
        let d = put_chunk(s, &vec![8; CHUNK]);
        vec![
            put_manifest(s, CHUNK as u64, vec![c]),
            put_manifest(s, 2 * CHUNK as u64, vec![d, d]),
        ]
    });
    assert_eq!(
        check_frames(&distinct.1) - check_frames(&shared.1),
        1,
        "a chunk two manifests share is read once"
    );
}

#[test]
fn ha7_check_holder_checks_each_position_of_a_cached_chunk() {
    // a full chunk at the last position of a blob of CHUNK + 1 bytes: its length is known from
    // the first position, and the last position is still checked against it
    let (_d, s, ms) = figures_holder(|s| {
        let f = put_chunk(s, &vec![7; CHUNK]);
        vec![put_manifest(s, CHUNK as u64 + 1, vec![f, f])]
    });
    let text = format!(
        "blob {} has a chunk of {CHUNK} bytes where 1 are due",
        ms[0]
    );
    assert_eq!(decode_text(s.get_blob(&ms[0])), text);
    assert_eq!(decode_text(s.check_holder()), text);
    // the same across manifests: a good blob over the chunk and a bad one, in either order
    for bad_first in [false, true] {
        let (_d, s, ms) = figures_holder(|s| {
            let f = put_chunk(s, &vec![7; CHUNK]);
            let good = put_manifest(s, CHUNK as u64, vec![f]);
            let bad = put_manifest(s, CHUNK as u64 + 1, vec![f, f]);
            if bad_first {
                vec![bad, good]
            } else {
                vec![good, bad]
            }
        });
        let bad = ms[usize::from(!bad_first)];
        assert_eq!(
            decode_text(s.check_holder()),
            format!("blob {bad} has a chunk of {CHUNK} bytes where 1 are due"),
            "bad_first {bad_first}"
        );
    }
}
