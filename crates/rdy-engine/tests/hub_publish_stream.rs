//! hpb11 (D-Q0AVZ, D-D2NPN, D-48NYM, D-VP42Y, D-KDFZ3; hub i-SG442, i-QD955, tb-5TB84, r-79KGK):
//! the upload of one tag's full history under one push id. The history is HA1's export, checked
//! once; the hub's known step removes the ordinary records it holds; every removal placeholder and
//! its proofs travel anyway; the rest goes in HA1's dependency order, in the longest batches the
//! negotiated limits admit. The tests use a fake wire and HA1's take as the receiver.
mod common;

use rdy_engine::history_facts::presence;
use rdy_engine::hub_publish_stream::{
    BatchLimits, HistorySent, PreparedHistory, PushId, missing_keys, send_history,
};
use rdy_engine::hub_publish_wire::{
    Candidate, Discovery, FileIdentity, ObjectPut, PUSH_ID_CHARS, PublicationInputs, PublishState,
    PublishWire, StateRequest, UploadContext,
};
use rdy_engine::hub_types::{Origin, RequestClaim, Secret};
use rdy_engine::publication_manifest::Manifest;
use rdy_engine::redaction::RedactInput;
use rdy_engine::stream_export;
use rdy_engine::stream_take::take;
use rdy_engine::*;
use rdy_model::*;
use rdy_store::LandRequest;
use rdy_store::closure;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{BatchReport, StreamReader, StreamRecord, check_batch};
use rdy_store::take_batch::UploadTarget;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const CANARY: &str = "ZEBRA-7731";
const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
/// The bytes of a batch with no object record and one prerequisite: prelude 12, header frame
/// 37 + 57 + 36 (the baseline manifest) + 33, trailer frame 37 + 48.
const EMPTY_BATCH: u64 = 260;
const WIDE: BatchLimits = BatchLimits {
    bytes: 2_147_483_648,
    records: 1_000_000,
};

// ---------------------------------------------------------------------------------------------
// source projects

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "hpb11".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) -> Hash {
    let p = r
        .propose(&writer(), "work", "batch", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap().parse().unwrap()
}

fn tag(r: &mut Rdy, name: &str) -> Hash {
    r.tag(&founder(), name, None, None).unwrap();
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

fn landing_of(r: &Rdy, tag: Hash) -> Hash {
    r.store().read_tag(&tag).unwrap().landing
}

/// A project with a doc and a prose block that holds the canary, landed and tagged v1.0.0.
/// Returns the project, the doc's id, the prose block's id and the tag.
fn project(dir: &Path) -> (Rdy, String, String, Hash) {
    std::fs::create_dir_all(dir).unwrap();
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": format!("The launch code is {CANARY}.")},
            {"op": "create", "type": "prose", "id": "$q", "parent": "$d", "body": "A second paragraph."}
        ]),
    );
    land(&mut r);
    let v1 = tag(&mut r, "v1.0.0");
    (r, w.created["$d"].clone(), w.created["$p"].clone(), v1)
}

/// Retitles the doc `doc` and lands it.
fn retitle(r: &mut Rdy, doc: &str, title: &str) {
    write(
        r,
        json!([{"op": "update", "id": doc, "fields": {"title": title}}]),
    );
    land(r);
}

/// Removes every version of block `target` through the store's own landing path.
fn remove(r: &mut Rdy, target: &str) {
    let s = writer();
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: vec![target.to_string()],
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
    r.store_mut()
        .land_atomic(LandRequest {
            proposal: version,
            accept: None,
            approver: Provenance {
                actor: "human:t".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
}

/// The whole history of `tag` as HA1 exports it: its bytes and report.
fn export(r: &Rdy, tag: Hash) -> (Vec<u8>, BatchReport) {
    let mut out = Vec::new();
    let report = stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut out).unwrap();
    (out, report)
}

fn closure_keys(r: &Rdy, tag: Hash) -> Vec<ObjectKey> {
    closure::closure(&r.store().reader(), tag, &BTreeSet::new())
        .unwrap()
        .records
}

fn holds(bytes: &[u8], needle: &str) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
}

// ---------------------------------------------------------------------------------------------
// holders

fn holder(dir: &Path, repo: RepoId) -> (PathBuf, Rdy) {
    let root = dir.join("holder");
    (root.clone(), Rdy::create_holder(&root, repo).unwrap())
}

fn holder_holds_canary(root: &Path) -> bool {
    std::fs::read_dir(root.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_name().to_string_lossy().starts_with("seg-"))
        .any(|e| holds(&std::fs::read(e.path()).unwrap(), CANARY))
}

/// The keys of `keys` the holder `h` holds, as the hub's known step would see them.
fn held_by(h: &Rdy, keys: &[ObjectKey]) -> BTreeSet<ObjectKey> {
    let mut sorted = keys.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = BTreeSet::new();
    for chunk in sorted.chunks(100_000) {
        for p in presence(&h.read(), chunk).unwrap() {
            if p.present {
                out.insert(p.key);
            }
        }
    }
    out
}

/// Takes the batch bodies `bodies` into the holder `h` as one upload of `name`.
fn take_into(
    h: &mut Rdy,
    dir: &Path,
    label: &str,
    bodies: &[Vec<u8>],
    name: &str,
    landing: Hash,
) -> Result<stream_take::TakeOutcome, RdyError> {
    let files: Vec<File> = bodies
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let p = dir.join(format!("{label}-{i}.rdysync"));
            std::fs::write(&p, b).unwrap();
            File::open(&p).unwrap()
        })
        .collect();
    take(
        h,
        &files,
        UploadTarget {
            tag: name.into(),
            landing,
        },
    )
}

// ---------------------------------------------------------------------------------------------
// the fake wire

#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    Known { push: String, keys: Vec<ObjectKey> },
    Put { push: String, body: Vec<u8> },
    Other(&'static str),
}

/// How the fake hub answers a known step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KnownReply {
    /// The keys it does not hold, sorted.
    Honest,
    /// The missing keys in reverse order.
    Unsorted,
    /// The missing keys with the first one twice.
    Repeated,
    /// The missing keys and one key no request named.
    Foreign,
}

/// How the fake hub answers an objects step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PutReply {
    /// The push quarantine's distinct records and bytes so far, cumulative.
    Cumulative,
    /// From the second batch on, fewer records than the previous reply.
    Shrinking,
    /// More records than any upload may hold.
    OverCap,
}

struct Hub {
    held: BTreeSet<ObjectKey>,
    known: KnownReply,
    put: PutReply,
    /// The 0-based call that fails, and its error.
    fail: Option<(usize, RdyError)>,
    calls: Vec<Call>,
    quarantine: BTreeSet<ObjectKey>,
    bytes: u64,
}

struct FakeHub(Mutex<Hub>);

impl FakeHub {
    fn new(held: BTreeSet<ObjectKey>) -> FakeHub {
        FakeHub(Mutex::new(Hub {
            held,
            known: KnownReply::Honest,
            put: PutReply::Cumulative,
            fail: None,
            calls: Vec::new(),
            quarantine: BTreeSet::new(),
            bytes: 0,
        }))
    }

    fn calls(&self) -> Vec<Call> {
        self.0.lock().unwrap().calls.clone()
    }

    fn bodies(&self) -> Vec<Vec<u8>> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Put { body, .. } => Some(body),
                _ => None,
            })
            .collect()
    }

    fn known_calls(&self) -> Vec<Vec<ObjectKey>> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Known { keys, .. } => Some(keys),
                _ => None,
            })
            .collect()
    }

    /// Records `call`; the scripted failure of its number, if any.
    fn enter(&self, call: Call) -> Result<(), RdyError> {
        let mut hub = self.0.lock().unwrap();
        let n = hub.calls.len();
        hub.calls.push(call);
        match &hub.fail {
            Some((at, e)) if *at == n => Err(e.clone()),
            _ => Ok(()),
        }
    }

    fn other(&self, step: &'static str) -> RdyError {
        self.enter(Call::Other(step)).ok();
        RdyError::new(
            "E_TEST",
            format!("{step} is not an upload history step"),
            "",
        )
    }
}

impl PublishWire for FakeHub {
    fn discovery(&self, _: &UploadContext, _: &str) -> Result<Discovery, RdyError> {
        Err(self.other("discovery"))
    }

    fn state(&self, _: &UploadContext, _: &StateRequest) -> Result<PublishState, RdyError> {
        Err(self.other("state"))
    }

    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        assert_eq!(cx.bearer.expose(), ACCESS);
        self.enter(Call::Known {
            push: push.into(),
            keys: keys.to_vec(),
        })?;
        let hub = self.0.lock().unwrap();
        let mut missing: Vec<ObjectKey> = keys
            .iter()
            .filter(|k| !hub.held.contains(k))
            .copied()
            .collect();
        match hub.known {
            KnownReply::Honest => {}
            KnownReply::Unsorted => missing.reverse(),
            KnownReply::Repeated => missing.insert(0, missing[0]),
            KnownReply::Foreign => {
                missing.push(ObjectKey {
                    kind: kinds::TAG,
                    hash: Hash([0xee; 32]),
                });
                missing.sort_unstable();
            }
        }
        Ok(missing)
    }

    fn put_objects(
        &self,
        _: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError> {
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len() as u64, len, "the stated length is the body's");
        self.enter(Call::Put {
            push: push.into(),
            body: bytes.clone(),
        })?;
        let report = check_batch(&bytes[..]).expect("every batch is valid alone");
        let mut hub = self.0.lock().unwrap();
        hub.quarantine.extend(report.keys);
        hub.bytes += len;
        let records = hub.quarantine.len() as u64;
        let puts = hub
            .calls
            .iter()
            .filter(|c| matches!(c, Call::Put { .. }))
            .count();
        Ok(match hub.put {
            PutReply::Cumulative => ObjectPut {
                records,
                bytes: hub.bytes,
            },
            PutReply::Shrinking => ObjectPut {
                records: if puts > 1 { 0 } else { records },
                bytes: hub.bytes,
            },
            PutReply::OverCap => ObjectPut {
                records: 1_000_001,
                bytes: hub.bytes,
            },
        })
    }

    fn known_files(
        &self,
        _: &UploadContext,
        _: &str,
        _: &[FileIdentity],
    ) -> Result<Vec<Hash>, RdyError> {
        Err(self.other("files/known"))
    }

    fn put_file(
        &self,
        _: &UploadContext,
        _: &str,
        _: &FileIdentity,
        _: &str,
        _: &mut dyn Read,
    ) -> Result<FileIdentity, RdyError> {
        Err(self.other("files/put"))
    }

    fn publish(
        &self,
        _: &UploadContext,
        _: &str,
        _: &str,
        _: &PublicationInputs,
        _: &Manifest,
        _: Option<bool>,
    ) -> Result<Candidate, RdyError> {
        Err(self.other("publish"))
    }
}

fn cx() -> UploadContext {
    UploadContext {
        install: Origin::parse("https://hub.test").unwrap(),
        sync_base: Some("https://hub.test/sync/v1/acme/designs".into()),
        ca_file: None,
        claim: RequestClaim::from_session(&Session {
            actor: "agent:claude".into(),
            run: Some("r7".into()),
            role: Role::Writer,
            kind: SessionKind::Agent,
        })
        .unwrap(),
        bearer: Secret::new(ACCESS),
    }
}

/// Sends `history` to `hub` under `push`; returns the result and how many times the context
/// was asked for.
fn send(
    history: &mut PreparedHistory,
    hub: &FakeHub,
    push: &PushId,
    limits: BatchLimits,
) -> (Result<HistorySent, RdyError>, usize) {
    let mut asked = 0;
    let mut context = || {
        asked += 1;
        Ok(cx())
    };
    let got = send_history(history, push, limits, hub, &mut context);
    (got, asked)
}

/// Every record of `bodies`, in wire order across the bodies.
fn records_of(bodies: &[Vec<u8>]) -> Vec<StreamRecord> {
    let mut out = Vec::new();
    for b in bodies {
        let (mut reader, _) = StreamReader::open(&b[..]).unwrap();
        while let Some(r) = reader.next().unwrap() {
            out.push(r);
        }
    }
    out
}

fn reports(bodies: &[Vec<u8>]) -> Vec<BatchReport> {
    bodies
        .iter()
        .map(|b| check_batch(&b[..]).unwrap())
        .collect()
}

fn issue_text(e: &RdyError) -> Vec<String> {
    e.issues.iter().map(|i| i.message.clone()).collect()
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn hpb11_push_id_has_128_random_bits_and_keeps_block_ids_stable() {
    // the encoding: the 16 bytes as one unsigned big-endian 128-bit integer, 26 digits
    assert_eq!(PushId::from_bytes([0; 16]).as_str(), "0".repeat(26));
    assert_eq!(
        PushId::from_bytes([0xff; 16]).as_str(),
        format!("7{}", "z".repeat(25))
    );
    let mut one = [0u8; 16];
    one[15] = 1;
    assert_eq!(
        PushId::from_bytes(one).as_str(),
        format!("{}1", "0".repeat(25))
    );
    let mut top = [0u8; 16];
    top[0] = 0x80;
    assert_eq!(
        PushId::from_bytes(top).as_str(),
        format!("4{}", "0".repeat(25)),
        "the top bit is the third bit of the first digit"
    );
    let mixed: [u8; 16] = *b"\x01\x23\x45\x67\x89\xab\xcd\xef\xfe\xdc\xba\x98\x76\x54\x32\x10";
    let id = PushId::from_bytes(mixed);
    // decoded through the lower-case alphabet, the digits give the bytes back
    let alphabet: Vec<u8> = CROCKFORD.iter().map(|c| c.to_ascii_lowercase()).collect();
    let value = id.as_str().bytes().fold(0u128, |v, c| {
        (v << 5) | alphabet.iter().position(|a| *a == c).unwrap() as u128
    });
    assert_eq!(value.to_be_bytes(), mixed);
    assert_eq!(id.as_str(), "014d2pf2dbsqqzxq5tk1v58cgg");

    // random ids: 26 characters of the alphabet, first digit 0 to 7, all distinct
    let ids: BTreeSet<String> = (0..256)
        .map(|_| PushId::new().as_str().to_string())
        .collect();
    assert_eq!(ids.len(), 256);
    for id in &ids {
        assert_eq!(id.len(), PUSH_ID_CHARS);
        assert!(id.bytes().all(|b| alphabet.contains(&b)), "{id}");
        assert!(('0'..='7').contains(&id.chars().next().unwrap()), "{id}");
    }

    // a seeded thread: the push id is the repo-id byte stream's 16 bytes, and drawing it never
    // moves the BlockId stream
    seed_ids_for_tests(11);
    let plain: Vec<BlockId> = (0..4).map(|_| BlockId::new("p")).collect();
    let bytes = RepoId::new().0;
    seed_ids_for_tests(11);
    let first = PushId::new();
    let second = PushId::new();
    let with_push: Vec<BlockId> = (0..4).map(|_| BlockId::new("p")).collect();
    seed_ids_for_tests(11);
    let again = PushId::new();
    unseed_ids_for_tests();
    assert_eq!(first, PushId::from_bytes(bytes));
    assert_eq!(first, again, "one seed, one push id");
    assert_ne!(first, second);
    assert_eq!(
        with_push, plain,
        "a push id leaves the BlockId stream alone"
    );
}

#[test]
fn hpb11_batches_reassemble_one_full_tag_history() {
    let _clock = pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let (mut r, doc, _, v1) = project(&dir.path().join("src"));
    retitle(&mut r, &doc, "Guide two");
    let v2 = tag(&mut r, "v2.0.0");
    let repo = r.read().repo_id().unwrap();
    let (_, full) = export(&r, v1);
    assert!(full.keys.len() > 6, "{}", full.keys.len());

    // nothing held: three records a batch, HA1's order, every record once
    let hub = FakeHub::new(BTreeSet::new());
    let push = PushId::new();
    let mut h1 = PreparedHistory::capture(&r.read(), v1).unwrap();
    let limits = BatchLimits {
        bytes: WIDE.bytes,
        records: 3,
    };
    let (sent, asked) = send(&mut h1, &hub, &push, limits);
    let sent = sent.unwrap();
    let bodies = hub.bodies();
    let batches = reports(&bodies);
    let wire_order: Vec<ObjectKey> = batches.iter().flat_map(|b| b.keys.clone()).collect();
    assert_eq!(wire_order, full.keys, "HA1's dependency order, split");
    assert_eq!(batches.len(), full.keys.len().div_ceil(3));
    for b in &batches[..batches.len() - 1] {
        assert_eq!(b.keys.len(), 3, "the longest prefix that fits");
    }
    for b in &batches {
        assert_eq!(b.header.tag, v1);
        assert_eq!(b.header.repo_id, repo);
        assert!(b.header.prerequisites.windows(2).all(|w| w[0] < w[1]));
    }
    assert_eq!(
        sent,
        HistorySent {
            batches: batches.len() as u32,
            records: full.keys.len() as u64,
            bytes: bodies.iter().map(|b| b.len() as u64).sum(),
        }
    );
    // one known step and one context per request
    assert_eq!(hub.known_calls().len(), 1);
    assert_eq!(asked, 1 + batches.len());
    assert!(hub.calls().iter().all(|c| match c {
        Call::Known { push: p, .. } | Call::Put { push: p, .. } => p == push.as_str(),
        Call::Other(_) => false,
    }));

    // the union takes into an empty holder as one upload
    let (root, mut h) = holder(dir.path(), repo);
    take_into(
        &mut h,
        dir.path(),
        "v1",
        &bodies,
        "v1.0.0",
        landing_of(&r, v1),
    )
    .unwrap();
    assert!(holder_holds_canary(&root));

    // an already-held tag: the hub holds v1's history, so v2 sends only what v1 lacks
    let keys2 = closure_keys(&r, v2);
    let held = held_by(&h, &keys2);
    let shared: BTreeSet<ObjectKey> = full
        .keys
        .iter()
        .filter(|k| keys2.contains(k))
        .copied()
        .collect();
    assert!(shared.len() > 3);
    assert_eq!(held, shared, "v1's history is held");
    let hub2 = FakeHub::new(held.clone());
    let mut h2 = PreparedHistory::capture(&r.read(), v2).unwrap();
    let limits = BatchLimits {
        bytes: WIDE.bytes,
        records: 2,
    };
    let sent2 = send(&mut h2, &hub2, &PushId::new(), limits).0.unwrap();
    let bodies2 = hub2.bodies();
    let batches2 = reports(&bodies2);
    let sent_keys: BTreeSet<ObjectKey> = batches2.iter().flat_map(|b| b.keys.clone()).collect();
    assert!(
        sent_keys.is_disjoint(&held),
        "no held ordinary record is sent"
    );
    let expected: BTreeSet<ObjectKey> = keys2
        .iter()
        .filter(|k| !held.contains(k))
        .copied()
        .collect();
    assert_eq!(sent_keys, expected);
    assert_eq!(sent2.records, expected.len() as u64);
    // a held record that a sent one needs is a prerequisite
    assert!(
        batches2
            .iter()
            .any(|b| b.header.prerequisites.iter().any(|p| held.contains(p)))
    );
    let got = take_into(
        &mut h,
        dir.path(),
        "v2",
        &bodies2,
        "v2.0.0",
        landing_of(&r, v2),
    )
    .unwrap();
    assert_eq!(got.records_taken, expected.len() as u64);
    h.fsck().unwrap();
}

#[test]
fn hpb11_known_queries_are_bounded_and_kind_sensitive() {
    // 250,002 keys: each hash under two kinds; the hub holds the BLOCK of every even hash
    let key = |kind: u8, i: u32| {
        let mut h = [0u8; 32];
        h[..4].copy_from_slice(&i.to_be_bytes());
        ObjectKey {
            kind,
            hash: Hash(h),
        }
    };
    let mut keys: Vec<ObjectKey> = (0..125_001u32)
        .flat_map(|i| [key(kinds::BLOCK, i), key(kinds::CHANGE, i)])
        .collect();
    keys.sort_unstable();
    let held: BTreeSet<ObjectKey> = (0..125_001u32)
        .filter(|i| i % 2 == 0)
        .map(|i| key(kinds::BLOCK, i))
        .collect();
    let hub = FakeHub::new(held.clone());
    let push = PushId::new();
    let mut asked = 0;
    let mut context = || {
        asked += 1;
        Ok(cx())
    };
    let missing = missing_keys(&keys, &push, &hub, &mut context).unwrap();
    let expected: Vec<ObjectKey> = keys.iter().filter(|k| !held.contains(k)).copied().collect();
    assert_eq!(
        missing, expected,
        "a held BLOCK does not hold the CHANGE of its hash"
    );
    let chunks = hub.known_calls();
    assert_eq!(
        chunks.iter().map(Vec::len).collect::<Vec<_>>(),
        [100_000, 100_000, 50_002]
    );
    assert_eq!(chunks.concat(), keys, "sorted chunks of the sorted keys");
    assert_eq!(asked, 3);

    // a reply outside its chunk's subset contract stops at that chunk
    for bad in [
        KnownReply::Unsorted,
        KnownReply::Repeated,
        KnownReply::Foreign,
    ] {
        let hub = FakeHub::new(held.clone());
        hub.0.lock().unwrap().known = bad;
        let e = missing_keys(&keys, &push, &hub, &mut || Ok(cx())).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                upstream_protocol().code,
                upstream_protocol().message.as_str(),
                upstream_protocol().fix.as_str()
            ),
            "{bad:?}"
        );
        assert!(e.issues.is_empty(), "no warning: {:?}", e.issues);
        assert_eq!(hub.calls().len(), 1, "{bad:?}");
    }

    // more keys than one upload holds are refused before any request
    let many: Vec<ObjectKey> = (0..500_001u32)
        .flat_map(|i| [key(kinds::BLOCK, i), key(kinds::CHANGE, i)])
        .collect();
    let mut sorted = many;
    sorted.sort_unstable();
    let hub = FakeHub::new(BTreeSet::new());
    let e = missing_keys(&sorted, &push, &hub, &mut || Ok(cx())).unwrap_err();
    assert_eq!(e.code, E_LIMIT, "{e:?}");
    assert_eq!(e.message, "stream exceeds 1000000 object records");
    assert!(hub.calls().is_empty());

    // through the sender: a hub that holds the root's hash as a LANDING, and the root landing's
    // hash as a TAG, still gets both records
    let _clock = pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let (r, _, _, v1) = project(&dir.path().join("src"));
    let landing = landing_of(&r, v1);
    let swapped = BTreeSet::from([
        ObjectKey {
            kind: kinds::LANDING,
            hash: v1,
        },
        ObjectKey {
            kind: kinds::TAG,
            hash: landing,
        },
    ]);
    let hub = FakeHub::new(swapped);
    let mut history = PreparedHistory::capture(&r.read(), v1).unwrap();
    send(&mut history, &hub, &push, WIDE).0.unwrap();
    let (bytes, _) = export(&r, v1);
    assert_eq!(hub.bodies(), [bytes], "every record, as HA1 exports it");
}

#[test]
fn hpb11_all_known_still_sends_one_valid_empty_batch() {
    let _clock = pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let (r, _, _, v1) = project(&dir.path().join("src"));
    let repo = r.read().repo_id().unwrap();
    let (root, mut h) = holder(dir.path(), repo);
    let first = FakeHub::new(BTreeSet::new());
    let mut history = PreparedHistory::capture(&r.read(), v1).unwrap();
    send(&mut history, &first, &PushId::new(), WIDE).0.unwrap();
    take_into(
        &mut h,
        dir.path(),
        "v1",
        &first.bodies(),
        "v1.0.0",
        landing_of(&r, v1),
    )
    .unwrap();
    assert!(holder_holds_canary(&root));

    // the hub holds every record and no drop needs sending: one batch of no record, rooted at
    // the tag, which it names as its one prerequisite
    let keys = closure_keys(&r, v1);
    let held = held_by(&h, &keys);
    assert_eq!(held.len(), keys.len());
    let hub = FakeHub::new(held);
    let push = PushId::new();
    let (sent, asked) = send(&mut history, &hub, &push, WIDE);
    assert_eq!(
        sent.unwrap(),
        HistorySent {
            batches: 1,
            records: 0,
            bytes: EMPTY_BATCH,
        }
    );
    assert_eq!(asked, 2, "one known step and one objects step");
    let bodies = hub.bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0].len() as u64, EMPTY_BATCH);
    let report = check_batch(&bodies[0][..]).unwrap();
    assert!(report.keys.is_empty());
    assert_eq!(report.records, 0);
    assert_eq!(report.header.tag, v1);
    assert_eq!(
        report.header.prerequisites,
        [ObjectKey {
            kind: kinds::TAG,
            hash: v1
        }]
    );
    // the quarantine opens with it, and the take of it changes nothing
    let got = take_into(
        &mut h,
        dir.path(),
        "empty",
        &bodies,
        "v1.0.0",
        landing_of(&r, v1),
    )
    .unwrap();
    assert!(got.unchanged, "{got:?}");

    // a hub batch limit below the empty batch is refused before it is sent
    let hub = FakeHub::new(held_by(&h, &keys));
    let e = send(
        &mut history,
        &hub,
        &push,
        BatchLimits {
            bytes: EMPTY_BATCH - 1,
            records: 1,
        },
    )
    .0
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_LIMIT,
            "a stream record does not fit the hub batch limit",
            "ask the operator to raise the batch limit"
        )
    );
    assert!(hub.bodies().is_empty());
}

#[test]
fn hpb11_old_tag_upload_carries_later_removal_proofs() {
    let _clock = pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _, secret, v1) = project(&dir.path().join("src"));
    let repo = r.read().repo_id().unwrap();
    let (root, mut h) = holder(dir.path(), repo);
    let first = FakeHub::new(BTreeSet::new());
    let mut before = PreparedHistory::capture(&r.read(), v1).unwrap();
    send(&mut before, &first, &PushId::new(), WIDE).0.unwrap();
    take_into(
        &mut h,
        dir.path(),
        "before",
        &first.bodies(),
        "v1.0.0",
        landing_of(&r, v1),
    )
    .unwrap();
    assert!(holder_holds_canary(&root));

    // the source removes the secret after the tag; the hub holds every original hash
    remove(&mut r, &secret);
    let keys = closure_keys(&r, v1);
    let held = held_by(&h, &keys);
    let hub = FakeHub::new(held.clone());
    let mut history = PreparedHistory::capture(&r.read(), v1).unwrap();
    send(
        &mut history,
        &hub,
        &PushId::new(),
        BatchLimits {
            bytes: WIDE.bytes,
            records: 4,
        },
    )
    .0
    .unwrap();
    let bodies = hub.bodies();
    assert!(
        bodies.iter().all(|b| !holds(b, CANARY)),
        "no removed byte travels"
    );
    let records = records_of(&bodies);
    let sent: BTreeSet<ObjectKey> = records.iter().map(StreamRecord::key).collect();
    let dropped: Vec<&StreamRecord> = records
        .iter()
        .filter(|r| matches!(r, StreamRecord::Dropped { .. }))
        .collect();
    assert!(!dropped.is_empty());
    for d in &dropped {
        let StreamRecord::Dropped {
            key,
            apply,
            residue,
            ..
        } = d
        else {
            unreachable!()
        };
        assert!(
            held.contains(key),
            "the hub holds {key}, and the placeholder travels anyway"
        );
        let apply = ObjectKey {
            kind: kinds::REDACTION_APPLY,
            hash: *apply,
        };
        assert!(sent.contains(&apply), "its apply travels");
        if let Some(residue) = residue {
            assert!(sent.contains(&ObjectKey {
                kind: kinds::REDACTION_RESIDUE,
                hash: *residue
            }));
        }
    }
    for kind in [
        kinds::REDACTION,
        kinds::REDACTION_APPLY,
        kinds::REDACTION_RESIDUE,
    ] {
        assert!(sent.iter().any(|k| k.kind == kind), "kind {kind} travels");
    }
    // the held removed objects' sent copies are placeholders only
    for k in sent.intersection(&held) {
        let r = records.iter().find(|r| r.key() == *k).unwrap();
        if let StreamRecord::Present { payload, .. } = r {
            assert!(!holds(payload, CANARY));
        }
    }
    // the hub's take applies the removal to its own copy
    take_into(
        &mut h,
        dir.path(),
        "after",
        &bodies,
        "v1.0.0",
        landing_of(&r, v1),
    )
    .unwrap();
    assert!(!holder_holds_canary(&root), "the hub's copy is erased");
    h.fsck().unwrap();

    // the same old tag again: the proofs are mandatory even when the hub holds every hash
    let held = held_by(&h, &keys);
    assert_eq!(held.len(), keys.len());
    let hub = FakeHub::new(held);
    send(&mut history, &hub, &PushId::new(), WIDE).0.unwrap();
    let again: BTreeSet<ObjectKey> = records_of(&hub.bodies())
        .iter()
        .map(StreamRecord::key)
        .collect();
    for d in &dropped {
        assert!(again.contains(&d.key()));
    }
    assert!(again.iter().any(|k| k.kind == kinds::REDACTION_APPLY));
}

#[test]
fn hpb11_batch_limits_count_framing_and_fail_without_publish() {
    let _clock = pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let (r, _, _, v1) = project(&dir.path().join("src"));
    let (bytes, full) = export(&r, v1);
    let n = full.keys.len() as u64;
    let total = bytes.len() as u64;
    let mut history = PreparedHistory::capture(&r.read(), v1).unwrap();
    let push = PushId::new();
    let run = |history: &mut PreparedHistory, limits: BatchLimits| {
        let hub = FakeHub::new(BTreeSet::new());
        let (got, asked) = send(history, &hub, &push, limits);
        (got, asked, hub)
    };

    // a limit of exactly the whole history's bytes, framing included, is one batch: HA1's
    let (got, _, hub) = run(
        &mut history,
        BatchLimits {
            bytes: total,
            records: n,
        },
    );
    assert_eq!(
        got.unwrap(),
        HistorySent {
            batches: 1,
            records: n,
            bytes: total
        }
    );
    assert_eq!(hub.bodies(), std::slice::from_ref(&bytes));
    // one byte or one record less is two batches, each within the limit
    for limits in [
        BatchLimits {
            bytes: total - 1,
            records: n,
        },
        BatchLimits {
            bytes: total,
            records: n - 1,
        },
    ] {
        let (got, _, hub) = run(&mut history, limits);
        let got = got.unwrap();
        let bodies = hub.bodies();
        assert_eq!(got.batches, 2, "{limits:?}");
        assert_eq!(got.records, n);
        assert_eq!(
            got.bytes,
            bodies.iter().map(|b| b.len() as u64).sum::<u64>()
        );
        assert!(got.bytes > total, "a second header and trailer count");
        for (b, report) in bodies.iter().zip(reports(&bodies)) {
            assert!(b.len() as u64 <= limits.bytes);
            assert!(report.records <= limits.records);
        }
    }

    // the smallest byte limit that sends: one record alone fills its batch exactly
    let (mut low, mut high) = (EMPTY_BATCH, total);
    while low < high {
        let mid = (low + high) / 2;
        let limits = BatchLimits {
            bytes: mid,
            records: n,
        };
        if run(&mut history, limits).0.is_ok() {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    let least = low;
    let (got, _, hub) = run(
        &mut history,
        BatchLimits {
            bytes: least,
            records: n,
        },
    );
    got.unwrap();
    let bodies = hub.bodies();
    let fitted: Vec<&Vec<u8>> = bodies.iter().filter(|b| b.len() as u64 == least).collect();
    assert!(!fitted.is_empty(), "the batch at the limit is exactly it");
    assert!(
        fitted
            .iter()
            .all(|b| check_batch(&b[..]).unwrap().records == 1)
    );

    // one byte less: E_LIMIT for that record, before any batch is sent or published
    let (got, asked, hub) = run(
        &mut history,
        BatchLimits {
            bytes: least - 1,
            records: n,
        },
    );
    let e = got.unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_LIMIT,
            "a stream record does not fit the hub batch limit",
            "ask the operator to raise the batch limit"
        )
    );
    let issues = issue_text(&e);
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(issues[0].starts_with("record "), "{issues:?}");
    let big = check_batch(&fitted[0][..]).unwrap().keys[0];
    assert!(
        issues[0].ends_with(&format!("; object {big}")),
        "{issues:?}"
    );
    assert!(hub.bodies().is_empty());
    assert!(!hub.calls().contains(&Call::Other("publish")));
    assert_eq!(hub.calls().len(), 1, "the known step only");
    assert_eq!(asked, 1);
    // a record limit of zero fits nothing
    let (got, _, hub) = run(
        &mut history,
        BatchLimits {
            bytes: total,
            records: 0,
        },
    );
    assert_eq!(got.unwrap_err().code, E_LIMIT);
    assert!(hub.bodies().is_empty());
}

#[test]
fn hpb11_failure_never_retries_or_changes_push() {
    let _clock = pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let (r, _, _, v1) = project(&dir.path().join("src"));
    let n = export(&r, v1).1.keys.len();
    assert!(n > 4);
    let mut history = PreparedHistory::capture(&r.read(), v1).unwrap();
    let push = PushId::new();
    let limits = BatchLimits {
        bytes: WIDE.bytes,
        records: 2,
    };
    let pushes = |hub: &FakeHub| -> BTreeSet<String> {
        hub.calls()
            .into_iter()
            .map(|c| match c {
                Call::Known { push, .. } | Call::Put { push, .. } => push,
                Call::Other(s) => s.to_string(),
            })
            .collect()
    };

    // an uncertain objects reply (call 2: the second batch) ends the upload, unchanged
    let hub = FakeHub::new(BTreeSet::new());
    hub.0.lock().unwrap().fail = Some((2, upstream_uncertain()));
    let (got, asked) = send(&mut history, &hub, &push, limits);
    let e = got.unwrap_err();
    let want = upstream_uncertain();
    assert_eq!(
        (e.code, &e.message, &e.fix, e.issues.len()),
        (want.code, &want.message, &want.fix, 0)
    );
    assert_eq!(
        hub.calls().len(),
        3,
        "one known step, two batches, no retry"
    );
    assert_eq!(asked, 3);
    assert_eq!(pushes(&hub), BTreeSet::from([push.as_str().to_string()]));

    // a failed known step sends no batch
    let hub = FakeHub::new(BTreeSet::new());
    hub.0.lock().unwrap().fail = Some((0, upstream_transport()));
    let (got, asked) = send(&mut history, &hub, &push, limits);
    assert_eq!(got.unwrap_err().code, upstream_transport().code);
    assert_eq!((hub.calls().len(), asked), (1, 1));

    // a context that fails before the second batch stops before that request
    let hub = FakeHub::new(BTreeSet::new());
    let mut asked = 0;
    let mut context = || {
        asked += 1;
        if asked == 3 {
            Err(RdyError::new(
                "E_REDACTED_STATE",
                "content was removed",
                "read again",
            ))
        } else {
            Ok(cx())
        }
    };
    let e = send_history(&mut history, &push, limits, &hub, &mut context).unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE");
    assert_eq!(hub.calls().len(), 2, "the known step and the first batch");

    // a quarantine reply that shrinks, or passes the upload caps, is a protocol error that
    // stops the upload
    for (reply, calls) in [(PutReply::Shrinking, 3), (PutReply::OverCap, 2)] {
        let hub = FakeHub::new(BTreeSet::new());
        hub.0.lock().unwrap().put = reply;
        let e = send(&mut history, &hub, &push, limits).0.unwrap_err();
        assert_eq!(e.code, upstream_protocol().code, "{reply:?}");
        assert_eq!(hub.calls().len(), calls, "{reply:?}");
        assert_eq!(pushes(&hub), BTreeSet::from([push.as_str().to_string()]));
    }

    // the same history sends again under a new push id; the call counts are exact
    let hub = FakeHub::new(BTreeSet::new());
    let other = PushId::new();
    assert_ne!(other, push);
    let (got, asked) = send(&mut history, &hub, &other, limits);
    let got = got.unwrap();
    assert_eq!(got.batches as usize, n.div_ceil(2));
    assert_eq!(hub.calls().len(), 1 + n.div_ceil(2));
    assert_eq!(asked, hub.calls().len());
    assert_eq!(pushes(&hub), BTreeSet::from([other.as_str().to_string()]));
    let all: BTreeMap<ObjectKey, ()> = reports(&hub.bodies())
        .into_iter()
        .flat_map(|b| b.keys)
        .map(|k| (k, ()))
        .collect();
    assert_eq!(all.len(), n);
}
