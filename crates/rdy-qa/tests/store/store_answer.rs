//! di1 (plan Decision intake, Task 1): the ANSWER record (kind 10), the refs `answer/<item>` and
//! `pending/<person>/<item>`, their fsck rules, and store format 11. The format adds a kind and
//! two ref families and re-encodes no existing kind (D-F0904), so the v1-s10 fixture loads with
//! every hash and one upgrade batch.
use rdy_engine::{Rdy, load::load};
use rdy_model::*;
use rdy_store::dump::{DumpItem, DumpReader};
use rdy_store::format::{REF_STORE_FORMAT, STORE_FORMAT, encode_u32};
use rdy_store::kinds::{self, BlobManifest, RefUpdate};
use rdy_store::meta::REF_REPO_ID;
use rdy_store::transition::Transitions;
use rdy_store::view::ViewOp;
use rdy_store::{ANSWER_REF, PENDING_REF, Store, StoreError, answer_ref, snooze_ref};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeSet;
use std::path::PathBuf;

const ITEM: &str = "P-AAAAA@1";
const PERSON: &str = "human:nick";

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("s")).unwrap();
    (d, s)
}

fn fsck_err(s: &Store) -> String {
    match s.check_objects() {
        Err(StoreError::Decode(m)) => m,
        other => panic!("{other:?}"),
    }
}

/// A local answer of `act` to `item` with no targets.
fn answer(item: &str, act: AnswerAct) -> Answer {
    Answer {
        item: item.into(),
        act,
        option: Some("reject".into()),
        label: Some("Reject".into()),
        options: vec!["Approve P-AAAAA@1".into(), "Reject".into(), "Later".into()],
        card_hash: Some(Hash::of(b"card")),
        targets: vec![],
        person: Provenance {
            actor: PERSON.into(),
            run: None,
            role: Role::Approver,
            at_ms: 7,
        },
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    }
}

/// `a`, relayed by an agent on a card.
fn relayed(mut a: Answer) -> Answer {
    a.relayed_by = Some(ActorRun {
        actor: "agent:design".into(),
        run: Some("run-1".into()),
    });
    a.relay_evidence = Some(RelayEvidence {
        quoted: "no".into(),
        session_ref: "sess-1".into(),
        question: None,
    });
    a.evidence_kind = Some(EvidenceKind::Card);
    a
}

#[test]
fn di1_an_answer_is_stored_and_named_by_its_ref() {
    let (_d, mut s) = store();
    let target = s.put_blob(b"what the act wrote").unwrap();
    let mut a = relayed(answer(ITEM, AnswerAct::RejectProposal));
    a.targets = vec![target];
    let h = s.put_answer(&a).unwrap();
    assert_eq!(h, a.id());
    assert_eq!(answer_ref(ITEM), format!("{ANSWER_REF}{ITEM}"));
    assert_eq!(answer_ref(ITEM), "answer/P-AAAAA@1");
    assert_eq!(s.get_ref(&answer_ref(ITEM)).unwrap(), Some(h.0.to_vec()));
    assert!(s.has_kind(kinds::ANSWER, &h).unwrap());
    assert_eq!(s.answer_of(ITEM).unwrap(), Some((h, a.clone())));
    assert_eq!(s.answer_of("P-AAAAA@2").unwrap(), None);
    assert_eq!(s.snooze_of(PERSON, ITEM).unwrap(), None);
    assert_eq!(
        s.answers_with_prefix(ANSWER_REF).unwrap(),
        vec![(answer_ref(ITEM), h, a.clone())]
    );
    // a later answer to the same item rewrites the ref
    let mut b = answer(ITEM, AnswerAct::Confirm);
    b.option = Some("confirm".into());
    let hb = s.put_answer(&b).unwrap();
    assert_eq!(s.answer_of(ITEM).unwrap(), Some((hb, b)));
    let check = s.check_objects().unwrap();
    assert_eq!(check.answers, 2, "{check:?}");
}

#[test]
fn di1_a_snooze_goes_to_the_pending_family() {
    let (_d, mut s) = store();
    let a = answer(ITEM, AnswerAct::Later);
    let h = s.put_answer(&a).unwrap();
    // the ref keeps `:` and `@` as they are (O17)
    let name = snooze_ref(PERSON, ITEM);
    assert_eq!(name, "pending/human:nick/P-AAAAA@1");
    assert_eq!(name, format!("{PENDING_REF}{PERSON}/{ITEM}"));
    assert_eq!(s.get_ref(&name).unwrap(), Some(h.0.to_vec()));
    assert_eq!(
        s.get_ref(&answer_ref(ITEM)).unwrap(),
        None,
        "no answer/ ref"
    );
    assert_eq!(s.answer_of(ITEM).unwrap(), None);
    assert_eq!(s.snooze_of(PERSON, ITEM).unwrap(), Some((h, a.clone())));
    assert_eq!(s.snooze_of("human:other", ITEM).unwrap(), None);
    // a person may hold `/`: fsck rebuilds the name from the record, never splits it
    let mut b = answer("D-BBBBB@abc", AnswerAct::Later);
    b.person.actor = "human:a/b".into();
    let hb = s.put_answer(&b).unwrap();
    assert_eq!(
        s.answers_with_prefix(PENDING_REF).unwrap(),
        vec![
            (snooze_ref("human:a/b", "D-BBBBB@abc"), hb, b),
            (name, h, a)
        ]
    );
    assert_eq!(s.check_objects().unwrap().answers, 2);
}

#[test]
fn di1_only_put_answer_writes_the_answer_and_pending_families() {
    let (_d, mut s) = store();
    let h = s.put_answer(&answer(ITEM, AnswerAct::Confirm)).unwrap();
    let end = s.log_end();
    for name in [answer_ref(ITEM), snooze_ref(PERSON, ITEM)] {
        let e = s.set_refs(vec![(name.clone(), h.0.to_vec())]).unwrap_err();
        assert!(
            e.to_string().contains("only put_answer writes one"),
            "{name}: {e}"
        );
    }
    assert_eq!(s.log_end(), end, "nothing was written");
    // with other refs in the same call, nothing is written either
    let e = s
        .set_refs(vec![
            ("x/other".into(), b"1".to_vec()),
            (answer_ref("P-CCCCC@1"), h.0.to_vec()),
        ])
        .unwrap_err();
    assert!(e.to_string().contains("only put_answer writes one"), "{e}");
    assert_eq!(s.log_end(), end);
}

#[test]
fn di1_fsck_rejects_a_relay_without_evidence() {
    let (_d, mut s) = store();
    let mut a = relayed(answer(ITEM, AnswerAct::RejectProposal));
    a.relay_evidence = None;
    // put_answer keeps the rule fsck applies
    let e = s.put_answer(&a).unwrap_err();
    assert!(matches!(e, StoreError::Refused(_)), "{e:?}");
    // forged past put_answer, fsck finds it
    let h = s.put(kinds::ANSWER, &a).unwrap();
    assert_eq!(
        fsck_err(&s),
        format!("answer {h}: relayed_by and relay_evidence must both be set or both unset")
    );
    // evidence without a relayer is the same rule
    let (_d, mut s) = store();
    let mut b = relayed(answer(ITEM, AnswerAct::RejectProposal));
    b.relayed_by = None;
    let h = s.put(kinds::ANSWER, &b).unwrap();
    assert_eq!(
        fsck_err(&s),
        format!("answer {h}: relayed_by and relay_evidence must both be set or both unset")
    );
    // an evidence kind needs a relay, and is card or text
    let (_d, mut s) = store();
    let mut c = answer(ITEM, AnswerAct::RejectProposal);
    c.evidence_kind = Some(EvidenceKind::Text);
    assert!(matches!(s.put_answer(&c), Err(StoreError::Refused(_))));
    let h = s.put(kinds::ANSWER, &c).unwrap();
    assert_eq!(
        fsck_err(&s),
        format!("answer {h}: evidence kind text needs a relay")
    );
    let (_d, mut s) = store();
    let mut e = relayed(answer(ITEM, AnswerAct::RejectProposal));
    e.evidence_kind = Some(EvidenceKind::Elicited);
    assert!(matches!(s.put_answer(&e), Err(StoreError::Refused(_))));
    let h = s.put(kinds::ANSWER, &e).unwrap();
    assert_eq!(
        fsck_err(&s),
        format!("answer {h}: evidence kind elicited is not card or text")
    );
    // an answer that does not decode
    let (_d, mut s) = store();
    let h = s.write(vec![(kinds::ANSWER, vec![0xff; 3])]).unwrap()[0];
    assert_eq!(fsck_err(&s), format!("answer {h}: it does not decode"));
}

#[test]
fn di1_fsck_rejects_a_target_that_is_not_in_the_log() {
    let (_d, mut s) = store();
    let mut a = answer(ITEM, AnswerAct::Confirm);
    let nowhere = Hash::of(b"nowhere");
    a.targets = vec![nowhere];
    assert!(matches!(s.put_answer(&a), Err(StoreError::Refused(_))));
    let h = s.put(kinds::ANSWER, &a).unwrap();
    assert_eq!(
        fsck_err(&s),
        format!("answer {h}: target {nowhere} is not an object in the log before it")
    );
    // a target written after the answer is not before it
    let (_d, mut s) = store();
    let later = Store::blob_hash(b"later");
    a.targets = vec![later];
    let h = s.put(kinds::ANSWER, &a).unwrap();
    s.put_blob(b"later").unwrap();
    assert_eq!(
        fsck_err(&s),
        format!("answer {h}: target {later} is not an object in the log before it")
    );
    // a target written before it, in an earlier batch, checks clean
    let (_d, mut s) = store();
    let t = s.put_blob(b"earlier").unwrap();
    a.targets = vec![t];
    s.put_answer(&a).unwrap();
    assert_eq!(s.check_objects().unwrap().answers, 1);
}

#[test]
fn di1_fsck_rejects_a_card_without_its_hash() {
    let (_d, mut s) = store();
    let mut a = relayed(answer(ITEM, AnswerAct::RejectProposal));
    a.card_hash = None;
    assert!(matches!(s.put_answer(&a), Err(StoreError::Refused(_))));
    let h = s.put(kinds::ANSWER, &a).unwrap();
    assert_eq!(
        fsck_err(&s),
        format!("answer {h}: card evidence needs card_hash")
    );
    // text evidence needs no card hash
    let (_d, mut s) = store();
    a.evidence_kind = Some(EvidenceKind::Text);
    s.put_answer(&a).unwrap();
    s.check_objects().unwrap();
}

#[test]
fn di1_fsck_rejects_a_ref_that_names_another_item_or_act() {
    // an answer/ ref names an answer whose item is the ref's item, and never a snooze
    let (_d, mut s) = store();
    let h = s.put_answer(&answer(ITEM, AnswerAct::Confirm)).unwrap();
    s.set_refs_unchecked_for_tests(vec![(answer_ref("P-CCCCC@1"), h.0.to_vec())])
        .unwrap();
    assert_eq!(
        fsck_err(&s),
        format!("answer {h}: ref \"answer/P-CCCCC@1\" names an answer to another item")
    );
    let (_d, mut s) = store();
    let h = s.put_answer(&answer(ITEM, AnswerAct::Later)).unwrap();
    s.set_refs_unchecked_for_tests(vec![(answer_ref(ITEM), h.0.to_vec())])
        .unwrap();
    assert_eq!(
        fsck_err(&s),
        format!("answer {h}: ref \"answer/P-AAAAA@1\" names act later")
    );
    // a pending/ ref names a snooze of its person and item
    let (_d, mut s) = store();
    let h = s.put_answer(&answer(ITEM, AnswerAct::Confirm)).unwrap();
    s.set_refs_unchecked_for_tests(vec![(snooze_ref(PERSON, ITEM), h.0.to_vec())])
        .unwrap();
    assert_eq!(
        fsck_err(&s),
        format!("answer {h}: ref \"pending/human:nick/P-AAAAA@1\" names act confirm, not later")
    );
    let (_d, mut s) = store();
    let h = s.put_answer(&answer(ITEM, AnswerAct::Later)).unwrap();
    s.set_refs_unchecked_for_tests(vec![(snooze_ref("human:other", ITEM), h.0.to_vec())])
        .unwrap();
    assert_eq!(
        fsck_err(&s),
        format!(
            "answer {h}: ref \"pending/human:other/P-AAAAA@1\" names a snooze of another person or item"
        )
    );
    // a ref that names no answer
    let (_d, mut s) = store();
    let blob = s.put_blob(b"x").unwrap();
    s.set_refs_unchecked_for_tests(vec![(answer_ref(ITEM), blob.0.to_vec())])
        .unwrap();
    assert_eq!(
        fsck_err(&s),
        format!("answer {blob}: ref \"answer/P-AAAAA@1\" names no stored answer")
    );
}

#[test]
fn di1_reindex_keeps_every_answer_ref() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    let t = s.put_blob(b"t").unwrap();
    let mut a = relayed(answer(ITEM, AnswerAct::RejectDecision));
    a.targets = vec![t];
    s.put_answer(&a).unwrap();
    s.put_answer(&answer("Q-DDDDD@v1", AnswerAct::Later))
        .unwrap();
    s.put_answer(&answer("tag:v1", AnswerAct::Tag)).unwrap();
    let refs = |s: &Store| {
        let mut v = s.answers_with_prefix(ANSWER_REF).unwrap();
        v.extend(s.answers_with_prefix(PENDING_REF).unwrap());
        v
    };
    let before = refs(&s);
    assert_eq!(before.len(), 3);
    let check = s.check_objects().unwrap();
    s.reindex().unwrap();
    assert_eq!(refs(&s), before, "after the reindex");
    assert_eq!(s.check_objects().unwrap(), check);
    drop(s);
    let s = Store::open(&dir).unwrap();
    assert_eq!(refs(&s), before, "after a reopen");
    assert_eq!(s.check_objects().unwrap(), check);
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/dump")
        .join(name)
}

/// The frames of every batch of the dump at `name`, in order, and the header's log end.
fn fixture_frames(
    name: &str,
) -> (
    Vec<rdy_store::dump::Frame>,
    rdy_store::segment::Location,
    u32,
) {
    let (mut rd, _, h) = DumpReader::open(std::fs::File::open(fixture(name)).unwrap()).unwrap();
    let mut out = vec![];
    while let Some(item) = rd.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            out.extend(b.frames);
        }
    }
    (out, h.log_end, h.store_format)
}

/// Decodes `payload` as `T` and encodes it again.
fn again<T: Serialize + DeserializeOwned>(payload: &[u8]) -> Vec<u8> {
    let v: T = postcard::from_bytes(payload).unwrap();
    postcard::to_allocvec(&v).unwrap()
}

#[test]
fn di1_no_existing_kind_changes_its_bytes() {
    let (frames, _, format) = fixture_frames("v1-s10.rdydump");
    assert_eq!(format, 10);
    let mut seen = BTreeSet::new();
    for f in &frames {
        let bytes = match f.kind {
            kinds::BLOCK => again::<Block>(&f.payload),
            kinds::CHANGE => again::<Change>(&f.payload),
            kinds::PROPOSAL => again::<Proposal>(&f.payload),
            kinds::LANDING => again::<LandingRecord>(&f.payload),
            kinds::SNAPSHOT => again::<StateSnapshot>(&f.payload),
            // a chunk is raw bytes: no encoding to change
            kinds::BLOB_CHUNK => f.payload.clone(),
            kinds::BLOB_MANIFEST => again::<BlobManifest>(&f.payload),
            kinds::REF_UPDATE => again::<RefUpdate>(&f.payload),
            kinds::TAG => again::<Tag>(&f.payload),
            kinds::VIEW_OP => again::<ViewOp>(&f.payload),
            kinds::INTENT => again::<Intent>(&f.payload),
            kinds::ALT_SET => again::<AltSet>(&f.payload),
            kinds::REVIEW => again::<Review>(&f.payload),
            kinds::TENSION => again::<SemanticTension>(&f.payload),
            kinds::TRANSITION => again::<Transitions>(&f.payload),
            k => panic!("kind {k} is not a kind of store format 10"),
        };
        assert_eq!(
            bytes, f.payload,
            "kind {} {} changed its bytes",
            f.kind, f.hash
        );
        assert_eq!(Hash::of(&bytes), f.hash);
        seen.insert(f.kind);
    }
    let want: BTreeSet<u8> = rdy_qa::every_kind::V1_KINDS.into_iter().collect();
    assert_eq!(seen, want, "the fixture holds every kind of V1_KINDS");
    assert!(
        !want.contains(&kinds::ANSWER),
        "ANSWER is new in store format 11"
    );
}

/// Guard beside `dl8_the_v1_fixture_loads_with_every_hash`: a load of the v1-s10 fixture into
/// the current store format appends one `meta/store_format` batch and nothing else (FW-UP1,
/// D-WWAK7).
#[test]
fn di1_a_v1_s10_dump_loads_into_the_current_store_format() {
    let to = tempfile::tempdir().unwrap();
    let dir = to.path().join("p");
    load(&fixture("v1-s10.rdydump"), &dir).unwrap();
    let (frames, log_end, _) = fixture_frames("v1-s10.rdydump");
    let r = Rdy::open(&dir).unwrap();
    let reader = r.store().reader();
    let mut tail = vec![];
    reader
        .for_each_frame_from(log_end, r.store().log_end(), |_, kind, _, payload, _| {
            tail.push((kind, payload.to_vec()));
            Ok(())
        })
        .unwrap();
    // RULING F2 (Export and clone): a log with no repo id gets one in the upgrade batch.
    assert_eq!(tail.len(), 3, "two ref frames and their marker: {tail:?}");
    assert_eq!(tail[0].0, kinds::REF_UPDATE);
    let u: RefUpdate = postcard::from_bytes(&tail[0].1).unwrap();
    assert_eq!(
        u,
        RefUpdate {
            name: REF_STORE_FORMAT.into(),
            value: encode_u32(STORE_FORMAT),
            at: log_end,
        }
    );
    assert_eq!(tail[1].0, kinds::REF_UPDATE);
    let u: RefUpdate = postcard::from_bytes(&tail[1].1).unwrap();
    assert_eq!(u.name, REF_REPO_ID);
    assert_eq!(u.value.len(), 16, "a repo id is 16 bytes");
    assert_eq!(
        u.at,
        rdy_store::segment::Location {
            segment: log_end.segment,
            offset: log_end.offset + 1,
        }
    );
    assert_eq!(reader.get_ref(REF_REPO_ID).unwrap(), Some(u.value));
    assert_eq!(tail[2].0, kinds::BATCH_COMMIT);
    assert_eq!(
        reader.get_ref(REF_STORE_FORMAT).unwrap(),
        Some(STORE_FORMAT.to_string().into_bytes())
    );
    for f in &frames {
        assert!(
            reader.has_kind(f.kind, &f.hash).unwrap(),
            "{} {}",
            f.kind,
            f.hash
        );
    }
    assert_eq!(reader.count_refs(ANSWER_REF).unwrap(), 0);
    assert_eq!(reader.count_refs(PENDING_REF).unwrap(), 0);
    drop(reader);
    r.close().unwrap();
}
