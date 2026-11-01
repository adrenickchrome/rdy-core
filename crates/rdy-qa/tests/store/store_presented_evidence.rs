//! ui1 (D-6M0AF, D-QVH4N; TASK-982 C7): a UI answer is local with card evidence, and an
//! elicited answer is local with elicited evidence. The one record rule that `put_answer`, fsck
//! and the sync batch check share admits both, with a card hash; local text evidence and a relay
//! that claims elicited evidence keep their refusals. A landing admits local card evidence with
//! a card hash, whatever its view and choice. Dump, load and reindex keep each valid record, and
//! a released fixture loads as before.
use rdy_engine::load::load;
use rdy_engine::{LandOptions, Rdy};
use rdy_model::*;
use rdy_qa::fixtures::{approver, seed_doc, write, writer};
use rdy_store::format::baseline_features;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{StreamHeader, StreamRecord, StreamWriter, check_batch};
use rdy_store::{ANSWER_REF, Store, StoreError, kinds};
use serde_json::json;
use std::path::PathBuf;

const ITEM: &str = "P-AAAAA@1";
const PERSON: &str = "human:nick";

/// An answer to [`ITEM`] with evidence kind `kind`, a card hash when `card`, and a relay when
/// `relayed`.
fn answer(kind: Option<EvidenceKind>, card: bool, relayed: bool) -> Answer {
    Answer {
        item: ITEM.into(),
        act: AnswerAct::RejectProposal,
        option: Some("reject".into()),
        label: Some("Reject".into()),
        options: vec!["Approve P-AAAAA@1".into(), "Reject".into(), "Later".into()],
        card_hash: card.then(|| Hash::of(b"card")),
        targets: vec![],
        person: Provenance {
            actor: PERSON.into(),
            run: Some("cli".into()),
            role: Role::Approver,
            at_ms: 7,
        },
        relayed_by: relayed.then(|| ActorRun {
            actor: "agent:relay".into(),
            run: Some("r1".into()),
        }),
        relay_evidence: relayed.then(|| RelayEvidence {
            quoted: "yes".into(),
            session_ref: "chat-7".into(),
            question: Some("Reject it?".into()),
        }),
        evidence_kind: kind,
        note: None,
    }
}

/// The record rule `a` breaks at the sync batch check, if any: a one-record batch, checked. A
/// valid record passes its rule, so its check stops only at the batch's own root rule.
fn receive_rule(a: &Answer) -> Option<String> {
    let (_, payload) = rdy_store::codec::item_new(kinds::ANSWER, a);
    let key = ObjectKey {
        kind: kinds::ANSWER,
        hash: Hash::of(&payload),
    };
    let header = StreamHeader::tag_history(
        baseline_features(),
        RepoId([3; 16]),
        Hash::of(b"tag"),
        vec![],
    );
    let mut w = StreamWriter::new(Vec::new(), &header).unwrap();
    w.record(&StreamRecord::Present {
        key,
        encoding: rdy_store::codec::latest(kinds::ANSWER),
        payload,
    })
    .unwrap();
    let (bytes, _) = w.finish().unwrap();
    match check_batch(bytes.as_slice()) {
        Ok(_) => None,
        Err(e) => {
            let text = format!("{e} {e:?}");
            let rule = format!("answer {}: ", key.hash);
            text.contains(&rule).then_some(text)
        }
    }
}

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("s")).unwrap();
    (d, s)
}

/// A project with a seed doc in canon and a proposal of one new claim in view `v`: the project,
/// its engine and the version's full hash.
fn project_with_proposal() -> (tempfile::TempDir, Rdy, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let (_, section, _) = seed_doc(&mut r, "seed");
    let p = r
        .propose(&writer(), "seed", "Seed", Some("Seeds."))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    write(
        &mut r,
        "v",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "A claim."}]),
    )
    .unwrap();
    let p = r
        .propose(&writer(), "v", "v", Some("Adds one claim."))
        .unwrap();
    (dir, r, p.id)
}

/// Dumps `r`, loads the dump into a fresh project and opens it.
fn dump_and_load(r: &Rdy) -> (tempfile::TempDir, Rdy) {
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("p.rdydump");
    r.read().dump(&file).unwrap();
    let dir = out.path().join("loaded");
    load(&file, &dir).unwrap();
    let loaded = Rdy::open(&dir).unwrap();
    (out, loaded)
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/dump")
        .join(name)
}

#[test]
fn ui1_local_evidence_survives_fsck_receive_and_reindex() {
    use EvidenceKind::{Card, Elicited, Text};
    // (case, record, the rule it breaks)
    let table: Vec<(&str, Answer, Option<&str>)> = vec![
        ("local card", answer(Some(Card), true, false), None),
        ("local elicited", answer(Some(Elicited), true, false), None),
        ("no evidence", answer(None, true, false), None),
        ("relayed card", answer(Some(Card), true, true), None),
        ("relayed text", answer(Some(Text), false, true), None),
        (
            "local card, no hash",
            answer(Some(Card), false, false),
            Some("card evidence needs card_hash"),
        ),
        (
            "local elicited, no hash",
            answer(Some(Elicited), false, false),
            Some("elicited evidence needs card_hash"),
        ),
        (
            "local text",
            answer(Some(Text), true, false),
            Some("evidence kind text needs a relay"),
        ),
        (
            "relayed elicited",
            answer(Some(Elicited), true, true),
            Some("evidence kind elicited is not card or text"),
        ),
    ];
    for (case, a, rule) in &table {
        let (_, payload) = rdy_store::codec::item_new(kinds::ANSWER, a);
        let h = Hash::of(&payload);
        let damage = rule.map(|r| format!("answer {h}: {r}"));
        // the store's write
        let (_d, mut s) = store();
        match (s.put_answer(a), &damage) {
            (Ok(got), None) => assert_eq!(got, h, "{case}"),
            (Err(StoreError::Refused(m)), Some(want)) => assert_eq!(&m, want, "{case}"),
            (other, _) => panic!("{case}: put_answer gave {other:?}"),
        }
        // fsck, over a record forged past the write
        let (_d, mut s) = store();
        s.put(kinds::ANSWER, a).unwrap();
        match (s.check_objects(), &damage) {
            (Ok(report), None) => assert_eq!(report.answers, 1, "{case}"),
            (Err(StoreError::Decode(m)), Some(want)) => assert_eq!(&m, want, "{case}"),
            (other, _) => panic!("{case}: fsck gave {other:?}"),
        }
        // the shared receive validation of a sync batch
        match (receive_rule(a), &damage) {
            (None, None) => {}
            (Some(text), Some(want)) => assert!(text.contains(want.as_str()), "{case}: {text}"),
            (got, _) => panic!("{case}: the batch check gave {got:?}"),
        }
        if damage.is_some() {
            continue;
        }
        // reindex and a reopen keep the valid record and its ref
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("s");
        let mut s = Store::create(&dir).unwrap();
        s.put_answer(a).unwrap();
        let refs = s.answers_with_prefix(ANSWER_REF).unwrap();
        assert_eq!(refs.len(), 1, "{case}");
        let check = s.check_objects().unwrap();
        s.reindex().unwrap();
        assert_eq!(s.answers_with_prefix(ANSWER_REF).unwrap(), refs, "{case}");
        assert_eq!(s.check_objects().unwrap(), check, "{case}");
        drop(s);
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.answers_with_prefix(ANSWER_REF).unwrap(), refs, "{case}");
        // a dump loads it whole
        let pd = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(pd.path(), "rdy-standard").unwrap();
        r.store_mut().put_answer(a).unwrap();
        r.close().unwrap();
        let r = Rdy::open(pd.path()).unwrap();
        let (_out, loaded) = dump_and_load(&r);
        loaded.fsck().unwrap();
        let (got, back) = loaded.store().reader().answer_of(ITEM).unwrap().unwrap();
        assert_eq!((got, &back), (h, a), "{case}");
    }

    // a landing: local card evidence with a card hash, and local elicited evidence, land and
    // survive fsck, reindex and a dump; local text evidence keeps its refusal
    let ev = |kind, card: bool, choice: &str| ApprovalEvidence {
        kind,
        options: vec!["Approve".into(), "Reject".into(), "Later".into()],
        choice: Some(choice.into()),
        card_hash: card.then(|| Hash::of(b"card")),
    };
    for (case, e) in [
        ("local card", ev(Card, true, "Approve")),
        ("local elicited", ev(Elicited, true, "Approve")),
        ("local elicited, no hash", ev(Elicited, false, "Approve")),
    ] {
        let (_d, mut r, id) = project_with_proposal();
        let info = r
            .land_with(
                &approver(),
                &id,
                LandOptions {
                    strict: false,
                    relay: None,
                    evidence: Some(e.clone()),
                },
            )
            .unwrap_or_else(|err| panic!("{case}: {err}"));
        let l: Hash = info.landing.parse().unwrap();
        let rec = r.store().landing(&l).unwrap();
        assert_eq!(rec.assurance, Assurance::Local, "{case}");
        assert_eq!(rec.evidence.as_ref(), Some(&e), "{case}");
        assert!(rec.relayed_by.is_none() && rec.relay_evidence.is_none());
        r.fsck().unwrap();
        r.reindex().unwrap();
        r.fsck().unwrap();
        let (_out, loaded) = dump_and_load(&r);
        loaded.fsck().unwrap();
        assert_eq!(
            loaded.store().landing(&l).unwrap().evidence.as_ref(),
            Some(&e),
            "{case}"
        );
    }
    let (_d, mut r, id) = project_with_proposal();
    let before = r.store().log_end();
    let err = r
        .land_with(
            &approver(),
            &id,
            LandOptions {
                strict: false,
                relay: None,
                evidence: Some(ev(Text, true, "Approve")),
            },
        )
        .unwrap_err();
    assert!(
        err.message.contains("evidence kind text needs a relay"),
        "{err}"
    );
    assert_eq!(
        r.store().log_end(),
        before,
        "a refused landing writes nothing"
    );

    // a released fixture loads and checks as before: no historical shape is tightened
    let to = tempfile::tempdir().unwrap();
    let dir = to.path().join("p");
    load(&fixture("v4-f1-e1.rdydump"), &dir).unwrap();
    let r = Rdy::open(&dir).unwrap();
    r.fsck().unwrap();
}
