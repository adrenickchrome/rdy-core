//! The closure of a tag (plan HA1 Task 5; D-Q0AVZ, D-D2NPN; D-3M81W: rdy "drops their bytes and
//! keeps their hashes"; D-CC90E; O11): every portable kind a history holds, in dependency order;
//! known keys as prerequisites; removed payloads as placeholders with their later proofs, checked
//! by object links alone; reverse attachments to a fixed point; signatures and the relay-tag
//! answer.
//!
//! The source holds one change that wrote a secret block and a figure, landed and tagged `v1`,
//! with coordination, review, answer and signature records around it; then the shared raw
//! authorized batch (`redaction_fixture`) removes the secret block's version. The tag predates
//! the removal, so its closure carries the later proof.
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::redaction::{RedactTarget, Redaction, RedactionApply};
use rdy_model::signature::Signature;
use rdy_model::*;
use rdy_store::closure::{
    END_OF_LOG, StoreSource, closure, closure_before, dependency_order, write_batch,
};
use rdy_store::kinds::{self, BlobManifest};
use rdy_store::object_refs::{ObjectKey, refs_of};
use rdy_store::stream_error::StreamError;
use rdy_store::sync_proof::{ObjectSource, PortableRemoval, RemovalObject, check};
use rdy_store::sync_stream::{SYNC_KINDS, StreamHeader, StreamReader, StreamRecord, check_batch};
use rdy_store::{Store, StoreReader};
use redaction_fixture::{
    Fixture, FixtureOptions, Proof, RequestSpec, authorized_batch_fixture, enc, hash_of,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// The removed block's body; no batch of the redacted source may hold it.
const CANARY: &[u8] = b"canary alpha secret";

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

fn prov(actor: &str, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: None,
        role,
        at_ms: 1,
    }
}

fn block(id: &str, ty: &str, body: &str, fields: BTreeMap<String, FieldValue>) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: ty.into(),
        parent: None,
        order: "V".into(),
        fields,
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn change(view: &str, ops: Vec<Op>, links: ChangeLinks) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: prov("agent:w", Role::Writer),
        justification: None,
        message: String::new(),
        links,
        read_identity: None,
    }
}

fn answer(item: &str, act: AnswerAct, targets: Vec<Hash>) -> Answer {
    Answer {
        item: item.into(),
        act,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets,
        person: prov("human:alice", Role::Approver),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    }
}

fn sig(subject: Hash, namespace: &str, n: u8) -> Signature {
    Signature {
        subject,
        namespace: namespace.into(),
        statement: format!("statement {n}").into_bytes(),
        sig: vec![n],
        signed_by: prov("human:alice", Role::Approver),
    }
}

/// The redacted source and the hashes of what it holds.
struct Src {
    _dir: tempfile::TempDir,
    path: PathBuf,
    fixture: Fixture,
    secret: Block,
    figure: Block,
    manifest: Hash,
    intent: Hash,
    alt: Hash,
    change: Hash,
    proposal: Hash,
    landing: Hash,
    review: Hash,
    tension: Hash,
    view_ops: Vec<Hash>,
    answer: Hash,
    tag: Hash,
    landing_sig: Hash,
    tag_sig: Hash,
    relay: Hash,
    side_change: Hash,
    side_block: Hash,
}

fn source() -> Src {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src");
    let mut s = Store::create(&path).unwrap();
    let w = prov("agent:w", Role::Writer);
    let human = prov("human:alice", Role::Approver);
    let intent = s
        .put(
            kinds::INTENT,
            &Intent {
                stable: None,
                prev: None,
                actor: "agent:w".into(),
                run: None,
                view: "work".into(),
                area: None,
                blocks: vec![],
                angle: "write the claim".into(),
                expires_at_ms: 100,
                status: IntentStatus::Open,
                at_ms: 1,
            },
        )
        .unwrap();
    let alt = s
        .put(
            kinds::ALT_SET,
            &AltSet {
                stable: None,
                prev: None,
                title: "which claim".into(),
                question: None,
                members: vec![AltMember {
                    label: "this one".into(),
                    view: "work".into(),
                }],
                status: AltStatus::Open,
                provenance: w.clone(),
            },
        )
        .unwrap();
    let manifest = s.put_blob(&b"figure bytes ".repeat(20)).unwrap();
    let secret = block("b-00001", "claim", "canary alpha secret", BTreeMap::new());
    let figure = block(
        "b-00002",
        "figure",
        "a figure",
        BTreeMap::from([("blob".to_string(), FieldValue::Str(manifest.to_string()))]),
    );
    let change_id = s
        .commit_to_view(
            "work",
            change(
                "work",
                vec![
                    Op::Create {
                        block: secret.clone(),
                    },
                    Op::Create {
                        block: figure.clone(),
                    },
                ],
                ChangeLinks {
                    intent: Some(intent),
                    alternative: Some(AltChoice {
                        set: alt,
                        member: "this one".into(),
                    }),
                    client_nonce: None,
                    imported_by: None,
                },
            ),
        )
        .unwrap();
    let proposal = s.propose("work", "first", w.clone()).unwrap();
    let landing = s.land(&proposal, human.clone()).unwrap();
    let pair = vec![
        PairItem {
            block: secret.id.clone(),
            content: Hash([1; 32]),
        },
        PairItem {
            block: figure.id.clone(),
            content: Hash([2; 32]),
        },
    ];
    let review = s
        .put(
            kinds::REVIEW,
            &Review {
                subject: ReviewSubject::Pair {
                    key: Hash([3; 32]),
                    items: pair.clone(),
                },
                case_hash: None,
                verdict: Verdict::Tension,
                reason: "they disagree".into(),
                findings: vec![],
                provenance: w.clone(),
            },
        )
        .unwrap();
    let tension = s
        .put(
            kinds::TENSION,
            &SemanticTension {
                key: Hash([3; 32]),
                items: pair,
                review,
                advice: None,
                blocking: false,
                relay: None,
                relayed_by: None,
                prev: None,
                provenance: w.clone(),
            },
        )
        .unwrap();
    let head = s.view_head("work").unwrap().unwrap().head;
    let mut view_ops = vec![];
    let mut at = Some(head);
    while let Some(h) = at {
        view_ops.push(h);
        at = s.view_op(&h).unwrap().prev;
    }
    let answer_id = s
        .put_answer(&answer(
            "P-x@1",
            AnswerAct::Confirm,
            vec![landing, tension, head],
        ))
        .unwrap();
    let tag = s
        .put_tag(&Tag {
            name: "v1".into(),
            landing,
            message: None,
            tagged_by: human.clone(),
        })
        .unwrap();
    let landing_sig = s
        .put_signature(&sig(landing, "rdy-approval-v1", 1))
        .unwrap();
    let tag_sig = s.put_signature(&sig(tag, "rdy-tag-v1", 2)).unwrap();
    // a relayed tag's answer: no target names it, only the ref answer/tag:v1 (O11)
    let relay = s
        .put_answer(&answer("tag:v1", AnswerAct::Tag, vec![]))
        .unwrap();
    // another view's work, never landed
    let side = block("b-00003", "claim", "side work", BTreeMap::new());
    let side_change = s
        .commit_to_view(
            "side",
            change(
                "side",
                vec![Op::Create {
                    block: side.clone(),
                }],
                ChangeLinks::default(),
            ),
        )
        .unwrap();
    drop(s);
    let fixture = authorized_batch_fixture(
        &path,
        &[RequestSpec::new(
            "work",
            vec![RedactTarget::Version {
                id: secret.id.clone(),
                version: secret.version_hash(),
            }],
        )
        .at(60)],
        &FixtureOptions::approved(Proof::local_human()),
    );
    // the open finishes the removal
    drop(Store::open(&path).unwrap());
    Src {
        _dir: dir,
        path,
        fixture,
        secret,
        figure,
        manifest,
        intent,
        alt,
        change: change_id,
        proposal,
        landing,
        review,
        tension,
        view_ops,
        answer: answer_id,
        tag,
        landing_sig,
        tag_sig,
        relay,
        side_change,
        side_block: side.version_hash(),
    }
}

impl Src {
    fn residues(&self) -> Vec<Hash> {
        self.fixture.requests[0]
            .draft
            .residues
            .iter()
            .map(hash_of)
            .collect()
    }

    fn residue_of(&self, kind: u8, hash: Hash) -> Hash {
        self.fixture.requests[0]
            .draft
            .request
            .manifest
            .spans
            .iter()
            .find(|s| s.kind == kind && s.hash == hash)
            .and_then(|s| s.residue)
            .unwrap()
    }

    fn request(&self) -> Hash {
        self.fixture.requests[0].request
    }

    fn apply(&self) -> Hash {
        self.fixture.applies[0].0
    }

    fn later_landing(&self) -> Hash {
        self.fixture.landing.unwrap()
    }
}

/// The closure of `tag` written as one batch.
fn write(reader: &StoreReader, tag: Hash, known: &BTreeSet<ObjectKey>) -> Vec<u8> {
    let c = closure(reader, tag, known).unwrap();
    let header = StreamHeader::tag_history(
        reader.repo_id().unwrap().unwrap(),
        tag,
        c.prerequisites.clone(),
    );
    let mut source = StoreSource {
        reader,
        end: END_OF_LOG,
    };
    write_batch(&mut source, &header, &c.records, Vec::new())
        .unwrap()
        .0
}

/// The records of a batch, by key.
fn records_of(bytes: &[u8]) -> BTreeMap<ObjectKey, StreamRecord> {
    let (mut r, _) = StreamReader::open(bytes).unwrap();
    let mut out = BTreeMap::new();
    while let Some(rec) = r.next().unwrap() {
        out.insert(rec.key(), rec);
    }
    r.finish().unwrap();
    out
}

fn position(records: &[ObjectKey], k: ObjectKey) -> usize {
    records
        .iter()
        .position(|r| *r == k)
        .unwrap_or_else(|| panic!("{k} is in the closure"))
}

#[test]
fn ha5_every_kind_closure_is_ordered_and_complete() {
    let src = source();
    let s = Store::open(&src.path).unwrap();
    let reader = s.reader();
    let c = closure(&reader, src.tag, &BTreeSet::new()).unwrap();

    // the inventory, key by key
    let landing: LandingRecord = reader.landing(&src.landing).unwrap();
    let chunks = reader
        .get_typed::<BlobManifest>(kinds::BLOB_MANIFEST, &src.manifest)
        .unwrap()
        .chunks;
    let later = src.fixture.landing_record.clone().unwrap();
    let mut want: BTreeSet<ObjectKey> = BTreeSet::from([
        key(kinds::TAG, src.tag),
        key(kinds::LANDING, src.landing),
        key(kinds::PROPOSAL, src.proposal),
        key(kinds::SNAPSHOT, landing.state),
        key(kinds::CHANGE, src.change),
        key(kinds::BLOCK, src.secret.version_hash()),
        key(kinds::BLOCK, src.figure.version_hash()),
        key(kinds::BLOB_MANIFEST, src.manifest),
        key(kinds::INTENT, src.intent),
        key(kinds::ALT_SET, src.alt),
        key(kinds::ANSWER, src.answer),
        key(kinds::ANSWER, src.relay),
        key(kinds::REVIEW, src.review),
        key(kinds::TENSION, src.tension),
        key(kinds::SIGNATURE, src.landing_sig),
        key(kinds::SIGNATURE, src.tag_sig),
        // the later removal proof and its human landing's history
        key(kinds::REDACTION, src.request()),
        key(kinds::REDACTION_APPLY, src.apply()),
        key(kinds::CHANGE, src.fixture.requests[0].carrier),
        key(kinds::LANDING, src.later_landing()),
        key(kinds::PROPOSAL, src.fixture.proposal.unwrap()),
        key(kinds::SNAPSHOT, later.state),
    ]);
    want.extend(chunks.iter().map(|h| key(kinds::BLOB_CHUNK, *h)));
    want.extend(src.view_ops.iter().map(|h| key(kinds::VIEW_OP, *h)));
    want.extend(
        src.residues()
            .iter()
            .map(|h| key(kinds::REDACTION_RESIDUE, *h)),
    );
    let got: BTreeSet<ObjectKey> = c.records.iter().copied().collect();
    assert_eq!(got.len(), c.records.len(), "each record once");
    assert_eq!(got, want, "the complete inventory");
    let got_kinds: BTreeSet<u8> = got.iter().map(|k| k.kind).collect();
    assert_eq!(
        got_kinds,
        SYNC_KINDS.into_iter().collect(),
        "every sync kind"
    );
    assert!(c.prerequisites.is_empty(), "a whole history needs nothing");
    // not walked: another view's work, the fixture's own view ops
    assert!(!got.contains(&key(kinds::CHANGE, src.side_change)));
    assert!(!got.contains(&key(kinds::BLOCK, src.side_block)));

    // the order: everything a record needs comes before it, and the batch checks
    let bytes = write(&reader, src.tag, &BTreeSet::new());
    let report = check_batch(&bytes[..]).unwrap();
    assert_eq!(
        report.keys, c.records,
        "the batch keeps the closure's order"
    );
    assert!(report.header.prerequisites.is_empty());
    let order = &c.records;
    let records = records_of(&bytes);
    for (i, k) in order.iter().enumerate() {
        if let StreamRecord::Present { payload, .. } = &records[k] {
            for e in refs_of(k.kind, payload).unwrap() {
                if let Some(kind) = e.kind {
                    assert!(
                        position(order, key(kind, e.hash)) < i,
                        "{k} needs {kind}:{}",
                        e.hash
                    );
                }
            }
        }
    }
    // the walk starts at the root: the tag's own history first, ending at the tag
    let tag_at = position(order, key(kinds::TAG, src.tag));
    assert!(position(order, key(kinds::LANDING, src.landing)) < tag_at);
    assert!(tag_at < position(order, key(kinds::SIGNATURE, src.tag_sig)));
    assert!(
        position(order, key(kinds::LANDING, src.landing))
            < position(order, key(kinds::SIGNATURE, src.landing_sig))
    );
    // the same set always gives the same order
    assert_eq!(closure(&reader, src.tag, &BTreeSet::new()).unwrap(), c);
}

#[test]
fn ha5_known_keys_become_prerequisites() {
    let src = source();
    let s = Store::open(&src.path).unwrap();
    let reader = s.reader();
    let root = key(kinds::TAG, src.tag);
    // the receiver holds the landing: the tag and what attaches to it stay
    let held = BTreeSet::from([key(kinds::LANDING, src.landing)]);
    let c = closure(&reader, src.tag, &held).unwrap();
    assert_eq!(
        c.records,
        vec![
            root,
            key(kinds::ANSWER, src.relay),
            key(kinds::SIGNATURE, src.tag_sig),
        ]
    );
    assert_eq!(c.prerequisites, vec![key(kinds::LANDING, src.landing)]);
    let bytes = write(&reader, src.tag, &held);
    let report = check_batch(&bytes[..]).unwrap();
    assert_eq!(report.keys, c.records);
    assert_eq!(report.header.prerequisites, c.prerequisites);
    // a key is its kind and its hash: the landing's hash under another kind is not the landing
    let other_kind = BTreeSet::from([key(kinds::SNAPSHOT, src.landing)]);
    assert_eq!(
        closure(&reader, src.tag, &other_kind).unwrap(),
        closure(&reader, src.tag, &BTreeSet::new()).unwrap()
    );
    // a key the walk never meets changes nothing
    let unmet = BTreeSet::from([key(kinds::BLOCK, src.side_block)]);
    assert_eq!(
        closure(&reader, src.tag, &unmet).unwrap(),
        closure(&reader, src.tag, &BTreeSet::new()).unwrap()
    );
    // the receiver holds the root: a zero-object batch that names it
    let all = BTreeSet::from([root]);
    let c = closure(&reader, src.tag, &all).unwrap();
    assert!(c.records.is_empty());
    assert_eq!(c.prerequisites, vec![root]);
    let report = check_batch(&write(&reader, src.tag, &all)[..]).unwrap();
    assert!(report.keys.is_empty());
    // a tag the store does not hold
    let e = closure(&reader, Hash([9; 32]), &BTreeSet::new()).unwrap_err();
    assert_eq!(e.code, "E_UNRESOLVED_REF");
    assert_eq!(
        e.message,
        format!("stream record 0 needs 9:{}", Hash([9; 32]))
    );
}

#[test]
fn ha5_removed_payloads_use_placeholders_and_later_proofs() {
    let src = source();
    let s = Store::open(&src.path).unwrap();
    let reader = s.reader();
    let bytes = write(&reader, src.tag, &BTreeSet::new());
    assert!(
        !bytes.windows(CANARY.len()).any(|w| w == CANARY),
        "no removed byte travels"
    );
    let mut records = records_of(&bytes);
    let block = key(kinds::BLOCK, src.secret.version_hash());
    let change = key(kinds::CHANGE, src.change);
    let block_residue = src.residue_of(kinds::BLOCK, block.hash);
    let change_residue = src.residue_of(kinds::CHANGE, change.hash);
    let block_len = enc(&src.secret).len() as u32;
    let StreamRecord::Dropped {
        original_len,
        apply,
        residue,
        ..
    } = records[&block]
    else {
        panic!("the removed block is a placeholder")
    };
    assert_eq!(
        (original_len, apply, residue),
        (block_len, src.apply(), Some(block_residue))
    );
    assert!(
        matches!(records[&change], StreamRecord::Dropped { residue: Some(r), .. } if r == change_residue)
    );
    let order = closure(&reader, src.tag, &BTreeSet::new()).unwrap().records;
    // a placeholder follows its residue; the proof follows what it removes
    assert!(
        position(&order, key(kinds::REDACTION_RESIDUE, block_residue)) < position(&order, block)
    );
    assert!(
        position(&order, key(kinds::REDACTION_RESIDUE, change_residue)) < position(&order, change)
    );
    assert!(position(&order, block) < position(&order, key(kinds::REDACTION_APPLY, src.apply())));

    // the logical authorization, from the batch's records alone and from the store
    let want = PortableRemoval {
        request: src.request(),
        apply: src.apply(),
        landing: src.later_landing(),
        objects: BTreeMap::from([
            (
                block,
                RemovalObject {
                    len: block_len,
                    residue: Some(block_residue),
                },
            ),
            (
                change,
                RemovalObject {
                    len: match records[&change] {
                        StreamRecord::Dropped { original_len, .. } => original_len,
                        _ => unreachable!(),
                    },
                    residue: Some(change_residue),
                },
            ),
        ]),
    };
    assert_eq!(check(&mut records, src.apply()).unwrap(), want);
    let mut store_source = StoreSource {
        reader: &reader,
        end: END_OF_LOG,
    };
    assert_eq!(check(&mut store_source, src.apply()).unwrap(), want);
    // the original bytes, where a receiver still holds them, match their residue
    let mut with_original = records.clone();
    with_original.insert(
        block,
        StreamRecord::Present {
            key: block,
            payload: enc(&src.secret),
        },
    );
    assert_eq!(check(&mut with_original, src.apply()).unwrap(), want);
    // a placeholder of another length is no copy of the approved tuple
    let mut other_len = records.clone();
    if let Some(StreamRecord::Dropped { original_len, .. }) = other_len.get_mut(&block) {
        *original_len += 1;
    }
    assert_eq!(invalid_at(check(&mut other_len, src.apply())), Some(block));

    // forged proofs: each is refused at the record that fails
    let request: Redaction = reader.redaction_request(&src.request()).unwrap();
    let real: RedactionApply = src.fixture.applies[0].1.clone();
    let forge = |records: &mut BTreeMap<ObjectKey, StreamRecord>, kind: u8, bytes: Vec<u8>| {
        let k = key(kind, Hash::of(&bytes));
        records.insert(
            k,
            StreamRecord::Present {
                key: k,
                payload: bytes,
            },
        );
        k.hash
    };
    // a landing approved by policy
    let mut policy = src.fixture.landing_record.clone().unwrap();
    policy.assurance = Assurance::Policy;
    let mut forged = records.clone();
    let policy_hash = forge(&mut forged, kinds::LANDING, enc(&policy));
    let a = forge(
        &mut forged,
        kinds::REDACTION_APPLY,
        enc(&RedactionApply {
            landing: policy_hash,
            ..real.clone()
        }),
    );
    assert_eq!(
        invalid_at(check(&mut forged, a)),
        Some(key(kinds::LANDING, policy_hash))
    );
    // another report hash
    let a = forge(
        &mut forged,
        kinds::REDACTION_APPLY,
        enc(&RedactionApply {
            report_hash: Hash([1; 32]),
            ..real.clone()
        }),
    );
    assert_eq!(
        invalid_at(check(&mut forged, a)),
        Some(key(kinds::REDACTION, src.request()))
    );
    // a request that names its spans twice (two physical copies): equal tuples collapse
    let reforge = |records: &mut BTreeMap<ObjectKey, StreamRecord>, r: &Redaction| {
        let mut buf = vec![0u8; rdy_model::redaction::MAX_RECORD_BYTES];
        let report_hash = r.manifest.report_hash(&mut buf).unwrap();
        let rh = forge(records, kinds::REDACTION, enc(r));
        forge(
            records,
            kinds::REDACTION_APPLY,
            enc(&RedactionApply {
                request: rh,
                landing: real.landing,
                report_hash,
            }),
        )
    };
    let mut twice = request.clone();
    let mut copy = twice.manifest.spans[0].clone();
    copy.place.offset += 1_000_000;
    twice.manifest.spans.push(copy.clone());
    let a = reforge(&mut forged, &twice);
    let mut collapsed = want.clone();
    collapsed.apply = a;
    collapsed.request = Hash::of(&enc(&twice));
    assert_eq!(check(&mut forged, a).unwrap(), collapsed);
    // unequal tuples of one key refuse
    let mut unequal = twice.clone();
    unequal.manifest.spans.last_mut().unwrap().len += 1;
    let a = reforge(&mut forged, &unequal);
    assert_eq!(
        invalid_at(check(&mut forged, a)),
        Some(key(copy.kind, copy.hash))
    );
    // all versions of an id never adds a version the approved manifest does not list
    let mut every = request.clone();
    every.targets = vec![RedactTarget::AllVersions(src.secret.id.clone())];
    let a = reforge(&mut forged, &every);
    let other = block_version("b-00001", "another version of the secret block");
    forge(&mut forged, kinds::BLOCK, enc(&other));
    let got = check(&mut forged, a).unwrap();
    assert_eq!(got.objects, want.objects);
    // a proof record the batches do not hold
    let mut missing = records.clone();
    missing.remove(&key(kinds::REDACTION, src.request()));
    let e = check(&mut missing, src.apply()).unwrap_err();
    assert_eq!(e.code, "E_UNRESOLVED_REF");
}

fn block_version(id: &str, body: &str) -> Block {
    block(id, "claim", body, BTreeMap::new())
}

/// The object an `invalid removal proof` refusal names.
fn invalid_at(r: Result<PortableRemoval, StreamError>) -> Option<ObjectKey> {
    let e = r.unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            "E_BAD_RECORD",
            "stream record 0 at byte 0: invalid removal proof"
        )
    );
    e.object
}

#[test]
fn ha5_attachments_reach_a_fixed_point_without_identity_cycles() {
    let src = source();
    // attachments of a record that only an attachment reached: the proof's later landing
    let (late_sig, late_answer) = {
        let mut s = Store::open(&src.path).unwrap();
        let g = s
            .put_signature(&sig(src.later_landing(), "rdy-approval-v1", 3))
            .unwrap();
        let a = s
            .put_answer(&answer(
                "P-y@1",
                AnswerAct::Confirm,
                vec![src.later_landing()],
            ))
            .unwrap();
        (g, a)
    };
    let s = Store::open(&src.path).unwrap();
    let reader = s.reader();
    let c = closure(&reader, src.tag, &BTreeSet::new()).unwrap();
    let set: BTreeSet<ObjectKey> = c.records.iter().copied().collect();
    assert_eq!(
        set.len(),
        c.records.len(),
        "each key once: the walk ends on visited keys"
    );
    for k in [
        key(kinds::REDACTION_APPLY, src.apply()),
        key(kinds::LANDING, src.later_landing()),
        key(kinds::SIGNATURE, late_sig),
        key(kinds::ANSWER, late_answer),
    ] {
        assert!(set.contains(&k), "{k}");
    }
    // a residue names its removed object by identity, never by link: no cycle through it
    let bytes = write(&reader, src.tag, &BTreeSet::new());
    let records = records_of(&bytes);
    for r in src.residues() {
        let StreamRecord::Present { payload, .. } = &records[&key(kinds::REDACTION_RESIDUE, r)]
        else {
            panic!("a residue travels whole")
        };
        let refs = refs_of(kinds::REDACTION_RESIDUE, payload).unwrap();
        assert!(
            !refs
                .iter()
                .any(|e| e.hash == src.secret.version_hash() && e.kind == Some(kinds::BLOCK))
                || payload_is_change_residue(payload),
            "a block residue has no link to its block"
        );
    }
    assert!(check_batch(&bytes[..]).is_ok());

    // the order itself refuses a real cycle, and stops at the depth bound
    let (a, b) = (
        key(kinds::INTENT, Hash([1; 32])),
        key(kinds::INTENT, Hash([2; 32])),
    );
    let cycle = BTreeMap::from([(a, vec![b]), (b, vec![a])]);
    let e = dependency_order(a, &cycle).unwrap_err();
    assert_eq!(e.message, "stream record 0 at byte 0: dependency cycle");
    let chain = |n: u32| -> BTreeMap<ObjectKey, Vec<ObjectKey>> {
        let k = |i: u32| {
            let mut h = [0u8; 32];
            h[..4].copy_from_slice(&i.to_be_bytes());
            key(kinds::INTENT, Hash(h))
        };
        (0..n)
            .map(|i| (k(i), if i == 0 { vec![] } else { vec![k(i - 1)] }))
            .collect()
    };
    let top = |n: u32| {
        let mut h = [0u8; 32];
        h[..4].copy_from_slice(&(n - 1).to_be_bytes());
        key(kinds::INTENT, Hash(h))
    };
    assert_eq!(
        dependency_order(top(100_000), &chain(100_000))
            .unwrap()
            .len(),
        100_000
    );
    let e = dependency_order(top(100_001), &chain(100_001)).unwrap_err();
    assert_eq!(e.message, "stream exceeds 100000 history depth");
    // a key outside the records is a prerequisite: never placed
    let outside = BTreeMap::from([(a, vec![b])]);
    assert_eq!(dependency_order(a, &outside).unwrap(), vec![a]);
}

/// Whether a residue payload is a change residue (which links to the blocks it kept).
fn payload_is_change_residue(payload: &[u8]) -> bool {
    matches!(
        postcard::from_bytes::<rdy_model::redaction::RedactionResidue>(payload),
        Ok(rdy_model::redaction::RedactionResidue::Change(_))
    )
}

#[test]
fn ha5_signatures_and_the_relay_tag_answer_are_attached() {
    let src = source();
    let (end, late, outside_sig, outside_tag) = {
        let mut s = Store::open(&src.path).unwrap();
        let end = s.log_end();
        // after the captured end: a signature of the tag
        let late = s.put_signature(&sig(src.tag, "rdy-tag-v1", 4)).unwrap();
        // outside the closure: a landing of other work, its signature and its own relayed tag
        let p = s
            .propose("side", "side", prov("agent:w", Role::Writer))
            .unwrap();
        let l = s.land(&p, prov("human:alice", Role::Approver)).unwrap();
        let g = s.put_signature(&sig(l, "rdy-approval-v1", 5)).unwrap();
        let t = s
            .put_tag(&Tag {
                name: "v2".into(),
                landing: l,
                message: None,
                tagged_by: prov("human:alice", Role::Approver),
            })
            .unwrap();
        s.put_answer(&answer("tag:v2", AnswerAct::Tag, vec![]))
            .unwrap();
        (end, late, g, t)
    };
    let s = Store::open(&src.path).unwrap();
    let reader = s.reader();
    let set = |c: Vec<ObjectKey>| -> BTreeSet<ObjectKey> { c.into_iter().collect() };
    let now = set(closure(&reader, src.tag, &BTreeSet::new()).unwrap().records);
    for k in [
        key(kinds::SIGNATURE, src.landing_sig),
        key(kinds::SIGNATURE, src.tag_sig),
        key(kinds::SIGNATURE, late),
        key(kinds::ANSWER, src.relay),
    ] {
        assert!(now.contains(&k), "{k}");
    }
    assert!(!now.contains(&key(kinds::SIGNATURE, outside_sig)));
    assert!(!now.contains(&key(kinds::TAG, outside_tag)));
    // a signature is ordered after its subject (its subject link resolves to the TAG)
    let order = closure(&reader, src.tag, &BTreeSet::new()).unwrap().records;
    assert!(
        position(&order, key(kinds::TAG, src.tag)) < position(&order, key(kinds::SIGNATURE, late))
    );
    // as the log stood at a captured end: no signature written after it
    let before = set(closure_before(&reader, src.tag, &BTreeSet::new(), end)
        .unwrap()
        .records);
    assert!(!before.contains(&key(kinds::SIGNATURE, late)));
    assert!(before.contains(&key(kinds::SIGNATURE, src.tag_sig)));
    assert!(before.contains(&key(kinds::ANSWER, src.relay)));
    // the other tag's closure holds its own relay answer and signature, not v1's
    let v2 = set(closure(&reader, outside_tag, &BTreeSet::new())
        .unwrap()
        .records);
    assert!(v2.contains(&key(kinds::SIGNATURE, outside_sig)));
    assert!(!v2.contains(&key(kinds::ANSWER, src.relay)));
    assert!(!v2.contains(&key(kinds::SIGNATURE, src.tag_sig)));
    drop(reader);
    drop(s);

    // the ref answer/tag:v1 names an answer of another act: nothing is attached by it
    {
        let mut s = Store::open(&src.path).unwrap();
        s.put_answer(&answer("tag:v1", AnswerAct::Confirm, vec![]))
            .unwrap();
    }
    let s = Store::open(&src.path).unwrap();
    let reader = s.reader();
    let after = set(closure(&reader, src.tag, &BTreeSet::new()).unwrap().records);
    assert!(!after.contains(&key(kinds::ANSWER, src.relay)));
    assert!(
        !after
            .iter()
            .any(|k| k.kind == kinds::ANSWER && k.hash != src.answer)
    );
    let mut store_source = StoreSource {
        reader: &reader,
        end: END_OF_LOG,
    };
    // every record the closure names reads back from the store
    for k in &after {
        assert!(store_source.record(*k).unwrap().is_some(), "{k}");
    }
}
