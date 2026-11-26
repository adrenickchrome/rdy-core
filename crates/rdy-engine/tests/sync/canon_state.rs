//! r59017 (D-GGTYA, D-4PV25): the export of one canon tip's state. A `PreparedCanonState` reads
//! the root's map through the one map read and selects the tip's records and attachments by the
//! closed state link table; it never walks the history closure. Its known pages and every get read
//! that one selection; a get is one complete format-5 batch, and the gets of a whole state check
//! together (`check_state_batches`).
use crate::common;
use rdy_engine::canon_state_export::PreparedCanonState;
use rdy_engine::history_facts::MAX_KEYS;
use rdy_engine::hub_http::BodyEncoding;
use rdy_engine::redaction::RedactInput;
use rdy_engine::stream_check::check_state_batches;
use rdy_engine::stream_export;
use rdy_engine::*;
use rdy_model::check_request::{CheckMode, CheckRequest, CheckSubject};
use rdy_model::dispute::{Dispute, DisputeOutcome, DisputeResolution};
use rdy_model::review::{Review, ReviewSubject, Verdict};
use rdy_model::signature::Signature;
use rdy_model::*;
use rdy_store::LandRequest;
use rdy_store::canon_state::{MAX_SNAPSHOT_BYTES, lower_snapshot_bytes_for_tests};
use rdy_store::kinds;
use rdy_store::object_refs::{ObjectKey, portable_refs_of};
use rdy_store::sync_stream::{
    BatchReport, CheckCaps, StreamPurpose, StreamReader, StreamRecord, check_canon_state_batch,
    lower_caps_for_tests,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::Arc;

fn writer() -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn person(at_ms: i64) -> Provenance {
    Provenance {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        at_ms,
    }
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "r59017".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy, view: &str) -> Hash {
    let p = r
        .propose(&writer(), view, view, Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap().parse().unwrap()
}

fn tag_hash(r: &Rdy, name: &str) -> Hash {
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

fn sign(r: &mut Rdy, subject: Hash, n: u8) -> Hash {
    r.store_mut()
        .put_signature(&Signature {
            subject,
            namespace: "rdy-approval-v1".into(),
            statement: vec![n],
            sig: b"not checked here".to_vec(),
            signed_by: Provenance {
                actor: "agent:s".into(),
                run: None,
                role: Role::Approver,
                at_ms: 7,
            },
        })
        .unwrap()
}

/// Removes every version of block `target` through the store's own landing path; returns the
/// landing.
fn remove(r: &mut Rdy, target: String) -> Hash {
    let s = writer();
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: vec![target],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r.propose(&s, "work", "Remove the code", None).unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    let l = r
        .store_mut()
        .land_atomic(LandRequest {
            proposal: version,
            accept: None,
            approver: person(1_000_000),
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap()
        .landing;
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
    l
}

/// An edit of `claim` in view `w-plan` that opens plan items, each answered twice, landed: the
/// landing's sealed plan is a delta on a whole plan version.
fn land_sealed_plan(r: &mut Rdy, claim: &str) -> Hash {
    write(
        r,
        "w-plan",
        json!([{"op": "update", "id": claim, "body": "A holds 40 days."}]),
    );
    let items: Vec<String> = r
        .read()
        .view_plan("w-plan")
        .unwrap()
        .items
        .iter()
        .filter_map(|row| match row {
            rdy_engine::plan::api::PlanRow::Live(i) => Some(i.id.clone()),
            _ => None,
        })
        .collect();
    assert!(!items.is_empty(), "the edit opens a plan item");
    for reason in ["it still holds", "it still holds, checked again"] {
        let answers: Vec<rdy_engine::plan::PlanAnswerInput> = items
            .iter()
            .map(|i| rdy_engine::plan::PlanAnswerInput {
                item: i.clone(),
                act: rdy_model::plan::PlanAct::Confirm,
                reason: Some(reason.into()),
            })
            .collect();
        r.answer_plan(&writer(), "w-plan", &answers).unwrap();
    }
    land(r, "w-plan")
}

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

fn get(p: &PreparedCanonState, keys: &[ObjectKey]) -> (Vec<u8>, BatchReport) {
    let mut out = Vec::new();
    let report = p.write_keys(keys, &mut out).unwrap();
    (out, report)
}

fn all_pages(p: &PreparedCanonState, limit: usize) -> Vec<ObjectKey> {
    let mut out = Vec::new();
    let mut after = None;
    loop {
        let page = p.page(after, limit).unwrap();
        out.extend(page.keys.iter().copied());
        match page.next {
            Some(n) => after = Some(n),
            None => return out,
        }
    }
}

/// The present records of a batch, by key: their encoding and payload.
fn records_of(bytes: &[u8]) -> BTreeMap<ObjectKey, StreamRecord> {
    let (mut reader, _) = StreamReader::open_for(bytes, StreamPurpose::CanonState).unwrap();
    let mut out = BTreeMap::new();
    while let Some(rec) = reader.next().unwrap() {
        out.insert(rec.key(), rec);
    }
    out
}

fn code(e: &RdyError) -> (&str, &str) {
    (e.code, e.message.as_str())
}

// ---------------------------------------------------------------------------------------------
// r59017_state_export_never_walks_history_closure

#[test]
fn r59017_state_export_never_walks_history_closure() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "Version 0."}
        ]),
    );
    let doc = w.created["$d"].clone();
    let prose = w.created["$p"].clone();
    let mut root = land(&mut r, "work");
    // 200 old landings, each a new version of the prose
    for n in 1..=200 {
        write(
            &mut r,
            "work",
            json!([{"op": "update", "id": prose, "body": format!("Version {n}.")}]),
        );
        root = land(&mut r, "work");
    }
    r.seal_open().unwrap();
    let read = r.read();
    let reader = read.reader();
    let landing = reader.landing(&root).unwrap();
    let map = reader.snapshot_at(&root).unwrap();
    assert_eq!(map.blocks.len(), 2);
    assert_eq!(map.id(), landing.state);
    // the settled selection: the root, its SNAPSHOT, the two current versions and the root's
    // delta; the 200 earlier landings, their changes and versions are frontier or absent
    let expected = |p: &PreparedCanonState| {
        let keys: BTreeSet<ObjectKey> = p.keys().iter().copied().collect();
        let mut want: BTreeSet<ObjectKey> = [
            key(kinds::LANDING, root),
            key(kinds::SNAPSHOT, landing.state),
        ]
        .into_iter()
        .chain(map.blocks.values().map(|v| key(kinds::BLOCK, *v)))
        .collect();
        let deltas: Vec<ObjectKey> = keys
            .iter()
            .filter(|k| k.kind == kinds::SNAPSHOT_DELTA)
            .copied()
            .collect();
        assert_eq!(deltas.len(), 1, "the root's one delta");
        want.extend(deltas);
        // the approval answer of the root, when the landing path writes one
        want.extend(keys.iter().filter(|k| k.kind == kinds::ANSWER).copied());
        assert_eq!(keys, want);
        assert!(
            keys.iter().all(|k| k.kind != kinds::CHANGE),
            "no change is part of a state"
        );
        let prerequisites: BTreeSet<ObjectKey> = p.prerequisites().iter().copied().collect();
        assert!(prerequisites.contains(&key(kinds::LANDING, landing.prev.unwrap())));
        assert!(prerequisites.contains(&key(kinds::PROPOSAL, landing.proposal)));
        for c in &landing.changes {
            assert!(prerequisites.contains(&key(kinds::CHANGE, *c)));
        }
        keys
    };

    // a warm map: no history walk, no fold, a few decodes whatever the history's length
    assert_eq!(
        read.reader().snapshot_at(&root).unwrap(),
        map,
        "warm the map"
    );
    let walks = stream_export::walks_for_tests();
    let folds = rdy_store::snapshot::folded_deltas_for_tests();
    let decodes = rdy_store::bounded::decodes_for_tests();
    let warm = PreparedCanonState::new(Arc::clone(&read), root).unwrap();
    let warm_decodes = rdy_store::bounded::decodes_for_tests() - decodes;
    assert_eq!(stream_export::walks_for_tests(), walks, "no history walk");
    assert_eq!(
        rdy_store::snapshot::folded_deltas_for_tests(),
        folds,
        "a warm map folds none"
    );
    assert!(
        warm_decodes < 40,
        "the selection decodes its own records, not 201 landings: {warm_decodes}"
    );
    let keys = expected(&warm);
    assert_eq!(warm.snapshot(), landing.state);
    assert_eq!(warm.root(), root);
    // the whole state as one get: a checked format-5 batch, and still no walk
    let (bytes, report) = get(&warm, warm.keys());
    assert_eq!(
        check_state_batches(vec![&bytes[..]], None).unwrap(),
        std::slice::from_ref(&report)
    );
    assert_eq!(report.keys, warm.keys());
    assert_eq!(
        bytes.len() as u64 - frame_overhead(&report),
        warm.frame_bytes(),
        "known bytes are the object frames"
    );
    assert_eq!(stream_export::walks_for_tests(), walks);

    // a cold map folds the deltas through the map read, counted apart; the same selection, and
    // the only extra decodes are those of the fold (no change payload is read)
    read.reader().clear_map_cache_for_tests();
    let folds = rdy_store::snapshot::folded_deltas_for_tests();
    let decodes = rdy_store::bounded::decodes_for_tests();
    let cold = PreparedCanonState::new(Arc::clone(&read), root).unwrap();
    let folded = rdy_store::snapshot::folded_deltas_for_tests() - folds;
    let cold_decodes = rdy_store::bounded::decodes_for_tests() - decodes;
    assert!(folded > 0, "a cold map folds");
    assert!(
        cold_decodes <= warm_decodes + 2 * folded,
        "a fold reads deltas and landings only: {cold_decodes} decodes, {folded} folds"
    );
    assert_eq!(stream_export::walks_for_tests(), walks, "no history walk");
    assert_eq!(expected(&cold), keys);
    assert_eq!(cold.keys(), warm.keys());
    assert_eq!(
        get(&cold, cold.keys()).0,
        bytes,
        "the same bytes, cold or warm"
    );
    let _ = doc;
}

// ---------------------------------------------------------------------------------------------
// r59017_attachments_and_frontier_are_exact

struct Rich {
    _dir: tempfile::TempDir,
    r: Rdy,
    /// The first landing (a tag, a signature, a check request and a dispute of its own, none
    /// of them part of the root's state), the removal's landing, the root, and a later landing.
    p1: Hash,
    p2: Hash,
    root: Hash,
    p4: Hash,
    blob: Hash,
    tags: [Hash; 2],
    signatures: [Hash; 2],
    request: Hash,
    review: Hash,
    dispute: Hash,
    resolution: Hash,
    secret: BlockId,
    /// A plain block that a later removal may take.
    spare: BlockId,
}

/// A doc with a secret prose block, a figure with a two-chunk blob and a claim, landed (`p1`,
/// with attachments of its own); the removal of the secret (`p2`); an edit with a sealed plan
/// (the root, `root`), relay-tagged `v1.0.0` and tagged `v1.0.1`, the root and the second tag
/// signed, a check request of the root's change with its review, and a dispute of the root
/// resolved by a later landing `p4`.
fn rich() -> Rich {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let blob = r.store_mut().put_blob(&vec![7u8; (1 << 20) + 5]).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$s", "parent": "$d", "body": "The launch code is ZEBRA-7731."},
            {"op": "create", "type": "prose", "id": "$q", "parent": "$d", "body": "A plain second block."},
            {"op": "create", "type": "figure", "id": "$f", "parent": "$d",
             "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "fig"}},
            {"op": "create", "type": "section", "id": "$x", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$x", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$x", "body": "B builds on {{$a}}."}
        ]),
    );
    let secret: BlockId = w.created["$s"].parse().unwrap();
    let spare: BlockId = w.created["$q"].parse().unwrap();
    let claim = w.created["$a"].clone();
    let p1 = land(&mut r, "work");
    r.tag(&founder(), "v0.1.0", None, None).unwrap();
    let old_tag = tag_hash(&r, "v0.1.0");
    sign(&mut r, p1, 9);
    sign(&mut r, old_tag, 8);
    r.store_mut()
        .put_dispute(
            &Dispute {
                landing: p1,
                opened_by: person(5),
                reason: "an old dispute".into(),
                at_ms: 5,
            },
            None,
        )
        .unwrap();
    let p2 = remove(&mut r, secret.to_string());
    let root = land_sealed_plan(&mut r, &claim);
    // a relayed tag (its answer is relayed) and a plain one
    let agent = Session::agent("agent:a".into(), Some("run-1".into()), Role::Writer).unwrap();
    let relay = rdy_engine::answer::relay::RelayInput {
        evidence: Some("Yes, tag it.".into()),
        ..Default::default()
    };
    let info = r
        .tag_as(&agent, "human:founder", &relay, true, "v1.0.0", None, None)
        .unwrap();
    assert!(info.relayed_by.is_some());
    r.tag(&founder(), "v1.0.1", None, None).unwrap();
    let tags = [tag_hash(&r, "v1.0.0"), tag_hash(&r, "v1.0.1")];
    let signatures = [sign(&mut r, root, 1), sign(&mut r, tags[1], 2)];
    let landing = r.store().landing(&root).unwrap();
    let q = CheckRequest {
        check: "consistency".into(),
        subject: CheckSubject {
            blocks: vec![],
            change: Some(landing.changes[0]),
            version: None,
            code: None,
        },
        scope: "docs".into(),
        case_hash: Hash::of(b"r59017 check case"),
        mode: CheckMode::Independent,
        reason: Some("a second look".into()),
        requested_by: Provenance {
            actor: "agent:c".into(),
            run: None,
            role: Role::Writer,
            at_ms: 11,
        },
        relayed_by: None,
        relay: None,
    };
    let request = r.store_mut().put_check_request(&q).unwrap();
    let review = r
        .store_mut()
        .put_check_review(&Review {
            subject: ReviewSubject::Check { request },
            case_hash: Some(q.case_hash),
            verdict: Verdict::Pass,
            reason: String::new(),
            citations: vec![],
            provenance: Provenance {
                actor: "agent:checker".into(),
                run: None,
                role: Role::Reviewer,
                at_ms: 12,
            },
        })
        .unwrap();
    r.seal_open().unwrap();
    write(
        &mut r,
        "w4",
        json!([{"op": "update", "id": claim, "body": "A holds 50 days."}]),
    );
    let p4 = land(&mut r, "w4");
    let (dispute, _) = r
        .store_mut()
        .put_dispute(
            &Dispute {
                landing: root,
                opened_by: person(10),
                reason: "the rule changed".into(),
                at_ms: 10,
            },
            None,
        )
        .unwrap();
    let (resolution, _) = r
        .store_mut()
        .put_dispute_resolution(
            &DisputeResolution {
                dispute,
                outcome: DisputeOutcome::Upheld,
                by: person(20),
                correction: Some(p4),
            },
            None,
        )
        .unwrap();
    r.seal_open().unwrap();
    r.invalidate();
    Rich {
        _dir: dir,
        r,
        p1,
        p2,
        root,
        p4,
        blob,
        tags,
        signatures,
        request,
        review,
        dispute,
        resolution,
        secret,
        spare,
    }
}

/// The links of each present record of `bytes` under the stream's features, with every
/// any-kind link resolved to the one key of that hash among `known`.
fn links_of(bytes: &[u8], known: &BTreeSet<ObjectKey>) -> BTreeMap<ObjectKey, BTreeSet<ObjectKey>> {
    let (mut reader, header) = StreamReader::open_for(bytes, StreamPurpose::CanonState).unwrap();
    let mut out = BTreeMap::new();
    while let Some(rec) = reader.next().unwrap() {
        if let StreamRecord::Present {
            key: from,
            encoding,
            payload,
        } = rec
        {
            let raw = rdy_store::codec::raw_kind(from.kind, encoding);
            let refs = portable_refs_of(&header.features, raw, &payload).unwrap();
            let resolved = refs
                .into_iter()
                .map(|r| match r.kind {
                    Some(kind) => key(kind, r.hash),
                    None => {
                        let found: Vec<ObjectKey> =
                            known.iter().filter(|k| k.hash == r.hash).copied().collect();
                        assert_eq!(found.len(), 1, "{from:?} names {} once", r.hash);
                        found[0]
                    }
                })
                .collect();
            out.insert(from, resolved);
        }
    }
    out
}

#[test]
fn r59017_attachments_and_frontier_are_exact() {
    let w = rich();
    let read = w.r.read();
    let reader = read.reader();
    let p = PreparedCanonState::new(Arc::clone(&read), w.root).unwrap();
    let keys: BTreeSet<ObjectKey> = p.keys().iter().copied().collect();
    let landing = reader.landing(&w.root).unwrap();
    let map = reader.snapshot_at(&w.root).unwrap();
    let of = |kind: u8| -> BTreeSet<Hash> {
        keys.iter()
            .filter(|k| k.kind == kind)
            .map(|k| k.hash)
            .collect()
    };

    // the carried rows, family by family
    assert_eq!(of(kinds::LANDING), BTreeSet::from([w.root]));
    assert_eq!(of(kinds::SNAPSHOT), BTreeSet::from([landing.state]));
    assert_eq!(
        of(kinds::BLOCK),
        map.blocks.values().copied().collect::<BTreeSet<_>>()
    );
    assert!(
        map.blocks.contains_key(&w.secret),
        "the removed block stays in the map"
    );
    assert_eq!(of(kinds::BLOB_MANIFEST), BTreeSet::from([w.blob]));
    assert_eq!(of(kinds::BLOB_CHUNK).len(), 2, "the blob's two chunks");
    assert_eq!(of(kinds::SNAPSHOT_DELTA).len(), 1);
    assert_eq!(of(kinds::TAG), BTreeSet::from(w.tags));
    assert_eq!(of(kinds::SIGNATURE), BTreeSet::from(w.signatures));
    assert_eq!(of(kinds::CHECK_REQUEST), BTreeSet::from([w.request]));
    assert_eq!(of(kinds::REVIEW), BTreeSet::from([w.review]));
    assert_eq!(of(kinds::DISPUTE), BTreeSet::from([w.dispute]));
    assert_eq!(
        of(kinds::DISPUTE_RESOLUTION),
        BTreeSet::from([w.resolution])
    );
    assert_eq!(of(kinds::REDACTION).len(), 1, "the removal's request");
    assert_eq!(of(kinds::REDACTION_APPLY).len(), 1, "its active apply");
    assert!(
        !of(kinds::REDACTION_RESIDUE).is_empty(),
        "its residue proofs"
    );
    // the sealed plan: its delta chain back to and including the nearest whole version
    let plans = of(kinds::PLAN);
    let chain = plan_chain(&p, &plans);
    assert_eq!(chain.iter().copied().collect::<BTreeSet<_>>(), plans);
    assert!(chain.len() >= 2 && chain.len() <= 129, "{chain:?}");
    // the answers: the two tags' answers (one relayed) and the seal's answers
    let answers = of(kinds::ANSWER);
    let (relay_answer, relayed) = reader.answer_of("tag:v1.0.0").unwrap().unwrap();
    assert!(relayed.relayed_by.is_some(), "a relayed answer");
    assert!(relayed.targets.contains(&w.tags[0]));
    assert!(answers.contains(&relay_answer), "the relayed tag's answer");
    // nothing of the first landing, the later landing or another tag
    for absent in [
        key(kinds::LANDING, w.p1),
        key(kinds::LANDING, w.p2),
        key(kinds::LANDING, w.p4),
        key(kinds::TAG, tag_hash(&w.r, "v0.1.0")),
    ] {
        assert!(
            !keys.contains(&absent),
            "{absent:?} is not part of the state"
        );
    }
    assert!(
        keys.iter()
            .all(|k| k.kind != kinds::CHANGE && k.kind != kinds::PROPOSAL)
    );
    // only the kinds of the table
    let table: BTreeSet<u8> = [
        kinds::LANDING,
        kinds::SNAPSHOT,
        kinds::BLOCK,
        kinds::BLOB_MANIFEST,
        kinds::BLOB_CHUNK,
        kinds::SNAPSHOT_DELTA,
        kinds::TAG,
        kinds::ANSWER,
        kinds::SIGNATURE,
        kinds::PLAN,
        kinds::CHECK_REQUEST,
        kinds::REVIEW,
        kinds::DISPUTE,
        kinds::DISPUTE_RESOLUTION,
        kinds::REDACTION_APPLY,
        kinds::REDACTION,
        kinds::REDACTION_RESIDUE,
    ]
    .into();
    assert!(keys.iter().all(|k| table.contains(&k.kind)), "{keys:?}");

    // the edges: every link of a carried record is carried or a listed frontier edge, and the
    // prerequisites are exactly the links the state omits
    let (bytes, report) = get(&p, p.keys());
    let prerequisites: BTreeSet<ObjectKey> = p.prerequisites().iter().copied().collect();
    assert_eq!(report.header.prerequisites, p.prerequisites());
    let known: BTreeSet<ObjectKey> = keys.union(&prerequisites).copied().collect();
    let links = links_of(&bytes, &known);
    let omitted: BTreeSet<ObjectKey> = links
        .values()
        .flatten()
        .filter(|k| !keys.contains(k))
        .copied()
        .collect();
    assert_eq!(omitted, prerequisites);
    // the never-absent rows resolve within the state
    for (from, targets) in &links {
        for t in targets {
            let required = matches!(
                (from.kind, t.kind),
                (kinds::SNAPSHOT, kinds::BLOCK)
                    | (kinds::SNAPSHOT_DELTA, kinds::BLOCK)
                    | (kinds::BLOCK, kinds::BLOB_MANIFEST)
                    | (kinds::BLOB_MANIFEST, kinds::BLOB_CHUNK)
                    | (kinds::REDACTION_APPLY, kinds::REDACTION)
                    | (kinds::REDACTION, kinds::REDACTION_RESIDUE)
                    | (kinds::PLAN, kinds::ANSWER)
            ) || *t == key(kinds::LANDING, w.root);
            if required {
                assert!(keys.contains(t), "{from:?} -> {t:?} is never absent");
            }
        }
    }
    // each carried answer is the seal's or targets the root or one of its tags
    let named: BTreeSet<Hash> = links
        .iter()
        .filter(|(from, _)| from.kind == kinds::PLAN)
        .flat_map(|(_, targets)| targets.iter())
        .filter(|k| k.kind == kinds::ANSWER)
        .map(|k| k.hash)
        .collect();
    for a in &answers {
        let targets = &links[&key(kinds::ANSWER, *a)];
        let attached = targets.contains(&key(kinds::LANDING, w.root))
            || w.tags
                .iter()
                .any(|t| targets.contains(&key(kinds::TAG, *t)));
        assert!(named.contains(a) || attached, "answer {a} is attached");
    }
    // the named frontier edges
    let delta_key = keys
        .iter()
        .find(|k| k.kind == kinds::SNAPSHOT_DELTA)
        .copied()
        .unwrap();
    let delta: SnapshotDelta = reader
        .get_typed(kinds::SNAPSHOT_DELTA, &delta_key.hash)
        .unwrap();
    assert_eq!(delta.landing, w.root);
    let mut frontier = vec![
        key(kinds::LANDING, landing.prev.unwrap()),
        key(kinds::PROPOSAL, landing.proposal),
        key(kinds::LANDING, w.p4),
    ];
    frontier.extend(landing.changes.iter().map(|c| key(kinds::CHANGE, *c)));
    frontier.extend(delta.base.map(|b| key(kinds::SNAPSHOT_DELTA, b)));
    for f in &frontier {
        assert!(prerequisites.contains(f), "{f:?} is a frontier edge");
    }
    assert!(!prerequisites.contains(&key(kinds::LANDING, w.root)));

    // the gets of the whole state check together; leaving out a carried record refuses
    assert!(check_state_batches(vec![&bytes[..]], None).is_ok());
    let block = keys
        .iter()
        .find(|k| k.kind == kinds::BLOCK)
        .copied()
        .unwrap();
    let rest: Vec<ObjectKey> = p.keys().iter().filter(|k| **k != block).copied().collect();
    let (partial, _) = get(&p, &rest);
    let e = check_state_batches(vec![&partial[..]], None).unwrap_err();
    assert_eq!(e.code, "E_BAD_RECORD", "{e:?}");
    assert!(e.message.contains("dependency"), "{}", e.message);
    // with that record in a second file, the union passes
    let (one, _) = get(&p, &rest);
    let (two, _) = get(&p, &[block]);
    assert_eq!(
        check_state_batches(vec![&one[..], &two[..]], None)
            .unwrap()
            .len(),
        2
    );
    let _ = w.p2;
}

// ---------------------------------------------------------------------------------------------
// r59017_pages_gets_and_compression_match (the state wire pin)

#[test]
fn r59017_pages_gets_and_compression_match() {
    let mut w = rich();
    let read = w.r.read();
    let p = PreparedCanonState::new(Arc::clone(&read), w.root).unwrap();
    let keys = p.keys().to_vec();
    assert!(keys.len() > 20, "a state of some size: {}", keys.len());
    for limit in [1, 3, 7, keys.len(), MAX_KEYS] {
        assert_eq!(all_pages(&p, limit), keys, "pages of {limit}");
    }
    let page = p.page(None, 5).unwrap();
    assert_eq!(
        (page.keys.as_slice(), page.next),
        (&keys[..5], Some(keys[4]))
    );
    let last = p.page(Some(keys[keys.len() - 2]), 5).unwrap();
    assert_eq!(
        (last.keys.as_slice(), last.next),
        (&keys[keys.len() - 1..], None)
    );

    // the whole state as one get, and as split gets: the same records, raw bytes and features
    let (whole, whole_report) = get(&p, &keys);
    let whole_records = records_of(&whole);
    assert_eq!(whole_records.len(), keys.len());
    assert_eq!(whole_report.header.features, *p.features());
    assert_eq!(
        whole.len() as u64 - frame_overhead(&whole_report),
        p.frame_bytes(),
        "known bytes are the object frames"
    );
    let mut files = Vec::new();
    let mut seen = BTreeMap::new();
    for chunk in keys.chunks(4) {
        let (bytes, report) = get(&p, chunk);
        assert_eq!(report.header.features, *p.features());
        assert_eq!(report.header.purpose, StreamPurpose::CanonState);
        assert_eq!(check_canon_state_batch(&bytes[..]).unwrap(), report);
        seen.extend(records_of(&bytes));
        files.push(bytes);
    }
    assert_eq!(seen, whole_records, "split gets carry the same raw records");
    let reports = check_state_batches(files.iter().map(|f| &f[..]).collect(), None).unwrap();
    assert_eq!(reports.len(), files.len());
    // reversed order of the same keys is the same batch
    let reversed: Vec<ObjectKey> = keys.iter().rev().copied().collect();
    assert_eq!(get(&p, &reversed).0, whole);

    // zstd and identity decode to the same checked batch
    let plain = p.write_keys_encoded(&keys, false).unwrap();
    assert_eq!(plain.encoding, BodyEncoding::Identity);
    let packed = p.write_keys_encoded(&keys, true).unwrap();
    assert_eq!(packed.encoding, BodyEncoding::Zstd);
    assert!(packed.wire_bytes < plain.wire_bytes);
    let mut a = Vec::new();
    let mut plain_body = plain.body;
    plain_body.read_to_end(&mut a).unwrap();
    assert_eq!(a, whole);
    let mut z = Vec::new();
    let mut packed_body = packed.body;
    packed_body.read_to_end(&mut z).unwrap();
    assert_eq!(z.len() as u64, packed.wire_bytes);
    assert_eq!(zstd::decode_all(&z[..]).unwrap(), whole);
    assert_eq!(packed.report, plain.report);
    assert_eq!(plain.report, whole_report);

    // repeats count once; an outside key, a bad cursor and a bad limit refuse
    let (bytes, report) = get(&p, &[keys[3], keys[3], keys[3]]);
    assert_eq!(report.keys, [keys[3]]);
    assert_eq!(check_canon_state_batch(&bytes[..]).unwrap(), report);
    let outside = key(kinds::LANDING, w.p1);
    let mut out = Vec::new();
    let e = p.write_keys(&[keys[0], outside], &mut out).unwrap_err();
    assert_eq!(e.code, "E_UNRESOLVED_REF");
    assert!(out.is_empty(), "nothing written");
    assert_eq!(
        p.page(Some(outside), 5).unwrap_err().code,
        "E_UNRESOLVED_REF"
    );
    for limit in [0, MAX_KEYS + 1] {
        assert_eq!(p.page(None, limit).unwrap_err().code, "E_LIMIT");
    }

    // a removal applied since refuses every later call
    remove(&mut w.r, w.spare.to_string());
    for e in [
        p.page(None, 5).unwrap_err(),
        p.write_keys(&keys[..2], Vec::new()).unwrap_err(),
        p.write_keys_encoded(&keys[..2], true).unwrap_err(),
        PreparedCanonState::new(Arc::clone(&read), w.root).unwrap_err(),
    ] {
        assert_eq!(e.code, "E_REDACTED_STATE", "{e:?}");
    }
}

/// The PLAN records `plans` of the state as one chain: from the one that no other names as its
/// `prev` (the sealed plan) back along each delta's `prev` to one whole version, which ends it.
fn plan_chain(p: &PreparedCanonState, plans: &BTreeSet<Hash>) -> Vec<Hash> {
    use rdy_store::decoded_record::DecodedRecord;
    use rdy_store::plan_record::PlanRecord;
    let (bytes, _) = get(p, p.keys());
    let records = records_of(&bytes);
    let decoded: BTreeMap<Hash, PlanRecord> = plans
        .iter()
        .map(
            |h| match DecodedRecord::decode(&records[&key(kinds::PLAN, *h)]).unwrap() {
                DecodedRecord::Plan(plan) => (*h, *plan),
                _ => panic!("a plan record"),
            },
        )
        .collect();
    let named: BTreeSet<Hash> = decoded
        .values()
        .filter(|r| matches!(r, PlanRecord::Delta(_)))
        .filter_map(PlanRecord::prev)
        .collect();
    let heads: Vec<Hash> = plans.difference(&named).copied().collect();
    assert_eq!(heads.len(), 1, "one sealed plan heads the chain");
    let mut chain = heads;
    loop {
        match &decoded[chain.last().unwrap()] {
            PlanRecord::Delta(d) => chain.push(d.prev),
            PlanRecord::Full(_) => return chain,
        }
    }
}

/// The bytes of a batch outside its object frames: its prelude, header frame and trailer.
fn frame_overhead(report: &BatchReport) -> u64 {
    let mut header = Vec::new();
    rdy_store::sync_stream::StreamWriter::new(&mut header, &report.header)
        .unwrap()
        .finish()
        .unwrap();
    header.len() as u64
}

// ---------------------------------------------------------------------------------------------
// r59017_limits_fail_before_allocation

#[test]
fn r59017_limits_fail_before_allocation() {
    let w = rich();
    let read = w.r.read();
    let p = PreparedCanonState::new(Arc::clone(&read), w.root).unwrap();
    let records = p.keys().len() as u64;
    let prerequisites = p.prerequisites().len();
    let caps = |c: CheckCaps| lower_caps_for_tests(c);
    let released = CheckCaps::RELEASED;

    // records: the selection's own count passes, one fewer refuses
    {
        let _g = caps(CheckCaps {
            records,
            ..released
        });
        PreparedCanonState::new(Arc::clone(&read), w.root).unwrap();
    }
    {
        let _g = caps(CheckCaps {
            records: records - 1,
            ..released
        });
        let e = PreparedCanonState::new(Arc::clone(&read), w.root).unwrap_err();
        assert_eq!(
            code(&e),
            ("E_LIMIT", "stream exceeds 1000000 object records")
        );
    }
    // prerequisites
    {
        let _g = caps(CheckCaps {
            prerequisites,
            ..released
        });
        PreparedCanonState::new(Arc::clone(&read), w.root).unwrap();
    }
    {
        let _g = caps(CheckCaps {
            prerequisites: prerequisites - 1,
            ..released
        });
        let e = PreparedCanonState::new(Arc::clone(&read), w.root).unwrap_err();
        assert_eq!(code(&e), ("E_LIMIT", "stream exceeds 100000 prerequisites"));
    }
    // the SNAPSHOT payload: its own size passes, one byte less refuses with the record bound
    let snapshot_len = {
        let (bytes, _) = get(&p, &[key(kinds::SNAPSHOT, p.snapshot())]);
        match records_of(&bytes).remove(&key(kinds::SNAPSHOT, p.snapshot())) {
            Some(StreamRecord::Present { payload, .. }) => payload.len() as u64,
            other => panic!("the SNAPSHOT is present: {other:?}"),
        }
    };
    {
        let _g = lower_snapshot_bytes_for_tests(snapshot_len);
        PreparedCanonState::new(Arc::clone(&read), w.root).unwrap();
    }
    {
        let _g = lower_snapshot_bytes_for_tests(snapshot_len - 1);
        let e = PreparedCanonState::new(Arc::clone(&read), w.root).unwrap_err();
        assert_eq!(e.code, "E_LIMIT");
        assert!(
            e.message.contains(&format!("{MAX_SNAPSHOT_BYTES}")),
            "{}",
            e.message
        );
    }
    // keys: a page of MAX_KEYS passes, one more refuses; a get of MAX_KEYS + 1 distinct keys
    // refuses before it resolves any
    assert!(p.page(None, MAX_KEYS).is_ok());
    let e = p.page(None, MAX_KEYS + 1).unwrap_err();
    assert_eq!(
        code(&e),
        ("E_LIMIT", "object presence query exceeds 100000 keys")
    );
    let many: Vec<ObjectKey> = (0..=MAX_KEYS as u32)
        .map(|n| key(kinds::BLOCK, Hash::of(&n.to_le_bytes())))
        .collect();
    let mut out = Vec::new();
    let e = p.write_keys(&many, &mut out).unwrap_err();
    assert_eq!(
        code(&e),
        ("E_LIMIT", "object presence query exceeds 100000 keys")
    );
    assert!(out.is_empty());
    let e = p.write_keys(&many[..MAX_KEYS], &mut out).unwrap_err();
    assert_eq!(e.code, "E_UNRESOLVED_REF", "MAX_KEYS keys pass the bound");

    // a malformed purpose and a truncated frame keep the stream's own errors
    let (bytes, _) = get(&p, p.keys());
    let mut bad = bytes.clone();
    let purpose_at = purpose_offset(&bytes);
    assert_eq!(bad[purpose_at], 4, "the canon state's purpose byte");
    bad[purpose_at] = 9;
    rehash_header(&mut bad);
    let e = check_state_batches(vec![&bad[..]], None).unwrap_err();
    assert_eq!(e.code, "E_STREAM_PURPOSE", "{e:?}");
    let cut = &bytes[..bytes.len() - 40];
    let e = check_state_batches(vec![cut], None).unwrap_err();
    assert_eq!(e.code, "E_STREAM_TRUNCATED", "{e:?}");
    assert_eq!(
        check_state_batches(Vec::<&[u8]>::new(), None)
            .unwrap_err()
            .code,
        "E_BAD_RECORD"
    );
}

const PRELUDE: usize = 12;
const FRAME: usize = 4 + 1 + 32;

/// Where a batch's purpose byte is: after the prelude, the header frame's own header, the
/// header's manifest (its length first) and the repo id.
fn purpose_offset(bytes: &[u8]) -> usize {
    let at = PRELUDE + FRAME;
    let manifest = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    at + manifest + 4 + 16
}

/// Sets the header frame's hash to its (edited) payload's, so the edit reaches the header check.
fn rehash_header(bytes: &mut [u8]) {
    let len = u32::from_le_bytes(bytes[PRELUDE..PRELUDE + 4].try_into().unwrap()) as usize;
    let start = PRELUDE + FRAME;
    let hash = Hash::of(&bytes[start..start + len]);
    bytes[PRELUDE + 5..PRELUDE + FRAME].copy_from_slice(&hash.0);
}
