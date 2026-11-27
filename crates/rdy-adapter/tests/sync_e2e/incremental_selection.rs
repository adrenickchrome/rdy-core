//! r59015 (D-3PNRK, D-H0BZN, D-KE83X): one incremental selection per pull operation. A pull
//! above a base the receiver holds prepares its selection once
//! ([`rdy_adapter::Snapshot::prepare_pull_with_base`]): one `history_closure_before` call with
//! that base. Its known pages and every get of the operation read that selection; a get checks
//! its keys against it and walks nothing again. A request with no operation keeps its own walks.
//!
//! The references are the existing walker (`write_history` with the base) and the hub's
//! request-local steps (a known pull and gets outside any scope). Each batch, page, refusal and
//! client result must equal theirs.
use super::mock::{Called, Fault, Hub, Served};
use super::{Machine, copy_of, doc, held_at, land, land_sealed_plan, remove, session, tag_at};
use rdy_adapter::stream::{ObjectKey, PreparedPull, StreamPurpose};
use rdy_adapter::{Hash, StoreHandle};
use rdy_engine::hub_sync_wire::{GetRequest, KnownPull, decode_known_page};
use rdy_engine::hub_types::Secret;
use rdy_engine::stream_export;
use rdy_engine::{Rdy, RdyError, Session, WriteRequest};
use rdy_model::check_request::{CheckMode, CheckRequest, CheckSubject};
use rdy_model::dispute::Dispute;
use rdy_model::signature::Signature;
use rdy_model::{Provenance, Role};
use rdy_store::kinds;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const CANON: StreamPurpose = StreamPurpose::CanonHistory;
const CANARY: &str = "ZEBRA-7731";

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

fn landing(hash: Hash) -> ObjectKey {
    key(kinds::LANDING, hash)
}

fn person(at_ms: i64) -> Provenance {
    Provenance {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        at_ms,
    }
}

// ---------------------------------------------------------------------------------------------
// the worlds

/// A project whose base landing has records below it that were attached after it was landed.
struct World {
    dir: tempfile::TempDir,
    /// The landing the receiver holds.
    base: Hash,
    /// The landing with the secret block, below the base.
    first: Hash,
    /// The removal's landing, above the base.
    removal: Hash,
    /// Hub canon: the last landing.
    root: Hash,
    /// The late attachments below the base, by name.
    late: BTreeMap<&'static str, ObjectKey>,
    /// A prose block of the base that a later removal may take.
    spare: String,
}

impl World {
    fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Lands `n` more docs; returns the last landing.
fn land_docs(r: &mut Rdy, prefix: &str, n: usize) -> Hash {
    let mut last = None;
    for i in 0..n {
        let view = format!("w-{prefix}{i}");
        doc(
            r,
            &view,
            &format!("{prefix} {i}"),
            &format!("{prefix} {i} is landed."),
        );
        last = Some(land(r, &view));
    }
    last.expect("one landing at least")
}

/// A sealed plan, a landing that holds a secret block (`first`) and the base. After the base: a
/// signature, a tag, a relayed tag (its answer), a check request and a dispute of `first`, the
/// approved removal of its secret, then `above` more landings.
fn world(above: usize) -> World {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    land_sealed_plan(&mut r);
    let made = doc(
        &mut r,
        "w-secret",
        "Secret",
        &format!("The launch code is {CANARY}."),
    );
    let secret = made["$p"].as_str().unwrap().to_string();
    let first = land(&mut r, "w-secret");
    let made = doc(&mut r, "w-base", "Base", "The base is landed.");
    let spare = made["$p"].as_str().unwrap().to_string();
    let base = land(&mut r, "w-base");

    let mut late = BTreeMap::new();
    let signature = r
        .store_mut()
        .put_signature(&Signature {
            subject: first,
            namespace: "rdy-approval-v1".into(),
            statement: vec![1],
            sig: b"not checked here".to_vec(),
            signed_by: Provenance {
                actor: "agent:s".into(),
                run: None,
                role: Role::Approver,
                at_ms: 7,
            },
        })
        .unwrap();
    late.insert("signature", key(kinds::SIGNATURE, signature));
    let at = first.to_string();
    r.tag(&session(Role::Approver), "v0.1.0", Some(&at), None)
        .unwrap();
    let agent = Session::agent("agent:a".into(), Some("run-1".into()), Role::Writer).unwrap();
    let relay = rdy_engine::answer::relay::RelayInput {
        evidence: Some("Yes, tag it.".into()),
        ..Default::default()
    };
    let info = r
        .tag_as(
            &agent,
            "human:founder",
            &relay,
            true,
            "v0.2.0",
            Some(&at),
            None,
        )
        .unwrap();
    assert!(info.relayed_by.is_some());
    let tags = r.store().tags().unwrap();
    for (name, tag) in [("tag", "v0.1.0"), ("relayed tag", "v0.2.0")] {
        let (_, hash, _) = tags.iter().find(|t| t.0 == tag).unwrap();
        late.insert(name, key(kinds::TAG, *hash));
    }
    let changes = r.store().landing(&first).unwrap().changes;
    let request = r
        .store_mut()
        .put_check_request(&CheckRequest {
            check: "consistency".into(),
            subject: CheckSubject {
                blocks: vec![],
                change: Some(changes[0]),
                version: None,
                code: None,
            },
            scope: "docs".into(),
            case_hash: Hash::of(b"r59015 check case"),
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
        })
        .unwrap();
    late.insert("check request", key(kinds::CHECK_REQUEST, request));
    let (dispute, _) = r
        .store_mut()
        .put_dispute(
            &Dispute {
                landing: first,
                opened_by: person(5),
                reason: "an old dispute".into(),
                at_ms: 5,
            },
            None,
        )
        .unwrap();
    late.insert("dispute", key(kinds::DISPUTE, dispute));
    r.seal_open().unwrap();
    let removal = remove(&mut r, &secret);
    let root = land_docs(&mut r, "top", above);
    r.seal_open().unwrap();
    // the answer of the relayed tag, and the removal's apply
    let read = r.read();
    let reader = read.reader();
    let mut answers = vec![];
    reader
        .for_each_object_of(kinds::ANSWER, |h| {
            answers.push(h);
            Ok(())
        })
        .unwrap();
    let mut applies = vec![];
    reader
        .for_each_object_of(kinds::REDACTION_APPLY, |h| {
            applies.push(h);
            Ok(())
        })
        .unwrap();
    assert_eq!(applies.len(), 1, "one removal applied");
    late.insert("removal apply", key(kinds::REDACTION_APPLY, applies[0]));
    drop(read);
    r.close().unwrap();
    let mut w = World {
        dir,
        base,
        first,
        removal,
        root,
        late,
        spare,
    };
    // the relayed tag's answer: an answer the walker of the base finds above the base
    let h = StoreHandle::open(w.path()).unwrap();
    let report = h
        .snapshot()
        .unwrap()
        .write_history(
            CANON,
            w.root,
            Some(w.base),
            &BTreeSet::new(),
            std::io::sink(),
        )
        .unwrap();
    h.close().unwrap();
    let carried: Vec<Hash> = answers
        .into_iter()
        .filter(|a| report.keys.contains(&key(kinds::ANSWER, *a)))
        .collect();
    assert!(
        !carried.is_empty(),
        "an answer attached after the base travels"
    );
    w.late.insert("answer", key(kinds::ANSWER, carried[0]));
    w
}

/// A world of `old` landings (the base is the last of them), then `new` landings above it.
fn long_world(old: usize, new: usize) -> (tempfile::TempDir, Hash, Hash) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = r
        .write(
            &session(Role::Writer),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Long", "doc_type": "page"}},
                    {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "Version 0."}
                ]))
                .unwrap(),
                justification: None,
                message: "r59015".into(),
            },
        )
        .unwrap();
    let prose = w.created["$p"].clone();
    let mut tip = land(&mut r, "work");
    let mut base = tip;
    for n in 1..old + new {
        r.write(
            &session(Role::Writer),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(
                    json!([{"op": "update", "id": prose, "body": format!("Version {n}.")}]),
                )
                .unwrap(),
                justification: None,
                message: "r59015".into(),
            },
        )
        .unwrap();
        tip = land(&mut r, "work");
        if n == old - 1 {
            base = tip;
        }
    }
    r.seal_open().unwrap();
    r.close().unwrap();
    (dir, base, tip)
}

/// A hub whose canon is the whole canon history of the project at `dir`, seeded in process.
fn seeded(dir: &Path) -> Hub {
    let h = StoreHandle::open(dir).unwrap();
    let snap = h.snapshot().unwrap();
    let tip = snap.canon().unwrap();
    let file: PathBuf = dir.join("seed.rdysync");
    let mut out = std::fs::File::create(&file).unwrap();
    snap.write_history(CANON, tip, None, &BTreeSet::new(), &mut out)
        .unwrap();
    drop(snap);
    h.close().unwrap();
    let hub = Hub::start();
    hub.seed(&[file]);
    assert_eq!(hub.canon(), Some(tip));
    hub
}

// ---------------------------------------------------------------------------------------------
// the steps, as a client sends them

fn known(hub: &Hub, root: Hash, base: Option<Hash>, after: Option<ObjectKey>) -> Called {
    let body = serde_json::to_value(KnownPull {
        after,
        base: base.map(landing),
        purpose: CANON,
        root: landing(root),
    })
    .unwrap();
    hub.call("/known", &body, false)
}

fn page(c: &Called) -> (Vec<ObjectKey>, Option<ObjectKey>) {
    assert_eq!(c.status, 200, "{}", String::from_utf8_lossy(&c.body));
    let p = decode_known_page(&c.body, &Secret::new("<token>")).unwrap();
    (p.keys, p.next)
}

fn all_known(hub: &Hub, root: Hash, base: Option<Hash>) -> Vec<ObjectKey> {
    let mut out = vec![];
    let mut after = None;
    loop {
        let (keys, next) = page(&known(hub, root, base, after));
        out.extend(keys);
        match next {
            Some(n) => after = Some(n),
            None => return out,
        }
    }
}

fn get(hub: &Hub, root: Hash, keys: &[ObjectKey], zstd: bool) -> Called {
    let body = serde_json::to_value(GetRequest {
        keys: keys.to_vec(),
        purpose: CANON,
        root: landing(root),
    })
    .unwrap();
    hub.call("/objects/get", &body, zstd)
}

/// A get reply's batch, decoded from its zstd frame when it is one.
fn batch(c: &Called) -> Vec<u8> {
    assert_eq!(c.status, 200, "{}", String::from_utf8_lossy(&c.body));
    match c.header("content-encoding") {
        Some("zstd") => zstd::decode_all(&c.body[..]).unwrap(),
        _ => c.body.clone(),
    }
}

/// The walks the steps of `operation` made, and how many steps they were.
fn walks_of(hub: &Hub, operation: &str) -> (u64, usize) {
    let served: Vec<Served> = hub
        .served()
        .into_iter()
        .filter(|s| s.operation == operation)
        .collect();
    (served.iter().map(|s| s.walks).sum(), served.len())
}

fn all_pages(p: &PreparedPull, limit: usize) -> Vec<ObjectKey> {
    let mut out = vec![];
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

fn same_error(a: &RdyError, b: &RdyError) {
    assert_eq!(
        (a.code, a.message.as_str(), a.fix.as_str()),
        (b.code, b.message.as_str(), b.fix.as_str())
    );
    assert_eq!(
        serde_json::to_string(&a.issues).unwrap(),
        serde_json::to_string(&b.issues).unwrap()
    );
}

// ---------------------------------------------------------------------------------------------
// the pins

/// The late attachments of records below the base travel in the one selection, with every
/// key, prerequisite, feature, order and byte of the existing walker. Known attachment keys may
/// leave those records out; a known landing never cuts the walk.
#[test]
fn r59015_late_attachments_below_base_survive() {
    let w = world(2);
    let h = StoreHandle::open(w.path()).unwrap();
    let snap = h.snapshot().unwrap();
    let none = BTreeSet::new();
    let mut walker = Vec::new();
    let report = snap
        .write_history(CANON, w.root, Some(w.base), &none, &mut walker)
        .unwrap();
    let (w0, _) = stream_export::walks_for_tests();
    let prepared = snap
        .prepare_pull_with_base(CANON, w.root, Some(w.base))
        .unwrap();
    assert_eq!(
        stream_export::walks_for_tests().0 - w0,
        1,
        "one history_closure_before call"
    );
    // the pages are the walker's keys in its order, whatever their size
    for limit in [1, 3, report.keys.len(), 100_000] {
        assert_eq!(all_pages(&prepared, limit), report.keys, "pages of {limit}");
    }
    // the base's link closure stays home; the late attachments below it travel
    let held: BTreeSet<ObjectKey> = report.keys.iter().copied().collect();
    for l in [w.first, w.base] {
        assert!(!held.contains(&landing(l)));
    }
    assert!(held.contains(&landing(w.removal)));
    assert!(held.contains(&landing(w.root)));
    for (name, k) in &w.late {
        assert!(held.contains(k), "the late {name} travels");
    }
    // what they need below the base is named with its kind, not sent and not kind 0
    assert!(report.header.prerequisites.contains(&landing(w.first)));
    assert!(report.header.prerequisites.contains(&landing(w.base)));
    assert!(report.header.prerequisites.iter().all(|k| k.kind != 0));
    // one get of every key: the walker's bytes, header and report
    let mut out = Vec::new();
    let got = prepared.write_keys(&report.keys, &mut out).unwrap();
    assert_eq!(out, walker, "the walker's bytes");
    assert_eq!(got, report);
    assert_eq!(got.header.features, report.header.features);
    assert_eq!(
        rdy_store::sync_stream::check_batch(&out[..]).unwrap(),
        report
    );
    // each late attachment alone: the prerequisites a request-local selection names
    for (name, k) in &w.late {
        let mut one = Vec::new();
        let r = prepared.write_keys(&[*k], &mut one).unwrap();
        let mut whole = Vec::new();
        let old = snap
            .write_history_keys(CANON, w.root, &[*k], &mut whole)
            .unwrap();
        assert_eq!(r.keys, old.keys, "{name}");
        assert_eq!(r.header.prerequisites, old.header.prerequisites, "{name}");
        assert_eq!(r.records, old.records, "{name}");
    }

    // explicit known attachment keys leave those records out, and nothing else
    let known: BTreeSet<ObjectKey> = [w.late["signature"], w.late["dispute"]].into();
    let less = snap
        .write_history(CANON, w.root, Some(w.base), &known, std::io::sink())
        .unwrap();
    let less_keys: BTreeSet<ObjectKey> = less.keys.iter().copied().collect();
    assert_eq!(less_keys, &held - &known);
    // a known landing above the base never cuts the walk: what lies below it still travels
    let known: BTreeSet<ObjectKey> = [landing(w.removal)].into();
    let less = snap
        .write_history(CANON, w.root, Some(w.base), &known, std::io::sink())
        .unwrap();
    let less_keys: BTreeSet<ObjectKey> = less.keys.iter().copied().collect();
    assert_eq!(less_keys, &held - &known);
    for (name, k) in &w.late {
        assert!(less_keys.contains(k), "the late {name} still travels");
    }

    // through the hub: one scope's pages and get are the walker's
    drop(prepared);
    drop(snap);
    h.close().unwrap();
    let hub = seeded(w.path());
    let _scope = hub.scope("pull");
    let keys = all_known(&hub, w.root, Some(w.base));
    assert_eq!(keys, report.keys);
    assert_eq!(batch(&get(&hub, w.root, &keys, false)), walker);
    assert_eq!(batch(&get(&hub, w.root, &keys, true)), walker);
    assert_eq!(walks_of(&hub, "pull").0, 1);
    hub.finish();
}

/// One operation pages its known keys and makes three disjoint gets from one selection. Their
/// union and each encoded batch equal the request-local steps'. Outside keys, a stale epoch and
/// a bad cursor keep their refusals, with no output.
#[test]
fn r59015_known_and_get_share_incremental_selection() {
    let w = world(3);
    let hub = seeded(w.path());
    // the request-local reference: no operation owns these steps
    let reference = all_known(&hub, w.root, Some(w.base));
    let n = reference.len();
    assert!(n >= 9, "a pull of some size: {n}");
    let parts: Vec<Vec<ObjectKey>> = vec![
        reference[..n / 3].to_vec(),
        reference[n / 3..2 * n / 3].to_vec(),
        reference[2 * n / 3..].to_vec(),
    ];
    let local: Vec<(Called, Called)> = parts
        .iter()
        .map(|p| (get(&hub, w.root, p, false), get(&hub, w.root, p, true)))
        .collect();
    // a key below the base: the whole history holds it, this pull does not
    let below = landing(w.first);
    let bad_cursor_local = known(&hub, w.root, Some(w.base), Some(below));
    let outside_local = get(&hub, w.root, &[parts[0][0], below], false);
    assert_eq!(
        outside_local.status, 200,
        "a request-local get reads the whole history"
    );

    let prepared_before = hub.prepared();
    {
        let _scope = hub.scope("pull");
        let (first, next) = page(&known(&hub, w.root, Some(w.base), None));
        assert_eq!(first, reference);
        assert_eq!(next, None);
        // a page after a key: the rest of the same order
        let (rest, _) = page(&known(&hub, w.root, Some(w.base), Some(reference[2])));
        assert_eq!(rest, reference[3..]);
        let mut union = BTreeSet::new();
        for (p, (identity, zstd)) in parts.iter().zip(&local) {
            let a = get(&hub, w.root, p, false);
            assert_eq!(a.status, identity.status);
            assert_eq!(a.body, identity.body, "the identity batch");
            assert_eq!(a.headers, identity.headers);
            // the encoded batch: one zstd frame of the same batch bytes (a request-local get
            // sends identity bytes, whatever it is offered)
            let z = get(&hub, w.root, p, true);
            assert_eq!(z.header("content-encoding"), Some("zstd"));
            assert_eq!(zstd.header("content-encoding"), None);
            assert_eq!(batch(&z), batch(zstd), "the zstd frame's batch");
            assert_eq!(batch(&z), identity.body);
            union.extend(p.iter().copied());
        }
        assert_eq!(union, reference.iter().copied().collect::<BTreeSet<_>>());
        // a key outside the operation's selection refuses before any batch byte
        let outside = get(&hub, w.root, &[parts[0][0], below], false);
        assert_eq!(
            outside.status,
            404,
            "{}",
            String::from_utf8_lossy(&outside.body)
        );
        assert_eq!(outside.header("content-type"), Some("application/json"));
        assert!(!outside.body.starts_with(b"RDYSYNC"));
        // a cursor outside the selection: the request-local refusal
        let cursor = known(&hub, w.root, Some(w.base), Some(below));
        assert_eq!(
            (cursor.status, &cursor.body),
            (bad_cursor_local.status, &bad_cursor_local.body)
        );
        assert_eq!(
            walks_of(&hub, "pull").0,
            1,
            "one selection for the operation"
        );
    }
    assert_eq!(hub.prepared(), prepared_before + 1, "one preparation");
    hub.finish();

    // a stale epoch and a bad cursor refuse as the request-local calls do, with no output
    let mut r = Rdy::open(w.path()).unwrap();
    let read = r.read();
    let prepared = PreparedPull::with_base(Arc::clone(&read), CANON, w.root, Some(w.base)).unwrap();
    let keys = all_pages(&prepared, 100_000);
    assert_eq!(keys, reference);
    let cursor = prepared.page(Some(below), 10).unwrap_err();
    let want = stream_export::history_page(&read, CANON, w.root, Some(w.base), Some(below), 10)
        .unwrap_err();
    same_error(&want, &cursor);
    assert_eq!(cursor.code, "E_UNRESOLVED_REF");
    let mut out = Vec::new();
    let e = prepared
        .write_keys(&[keys[0], below], &mut out)
        .unwrap_err();
    assert_eq!(e.code, "E_UNRESOLVED_REF");
    assert!(out.is_empty());
    remove(&mut r, &w.spare);
    let e = prepared.page(None, 5).unwrap_err();
    let want =
        stream_export::history_page(&read, CANON, w.root, Some(w.base), None, 5).unwrap_err();
    same_error(&want, &e);
    assert_eq!(e.code, "E_REDACTED_STATE", "{e}");
    let e = prepared.write_keys(&keys, &mut out).unwrap_err();
    let want =
        stream_export::write_history_keys(&read, CANON, w.root, &keys, Vec::new()).unwrap_err();
    same_error(&want, &e);
    assert!(out.is_empty(), "nothing is written for a removed state");
    drop(prepared);
    drop(read);
    r.close().unwrap();
}

/// A pull of 10 landings above a base of 200 makes one `history_closure_before` call across its
/// known pages and all its gets; a second operation makes one more. No timing is asserted.
#[test]
fn r59015_one_history_closure_before_per_operation() {
    let (dir, base, root) = long_world(200, 10);
    let hub = seeded(dir.path());
    for operation in ["pull", "pull again"] {
        let before = hub.prepared();
        let _scope = hub.scope(operation);
        let keys = all_known(&hub, root, Some(base));
        assert!(keys.contains(&landing(root)));
        assert!(!keys.contains(&landing(base)));
        assert_eq!(
            keys.iter().filter(|k| k.kind == kinds::LANDING).count(),
            10,
            "ten landings above the base"
        );
        // four disjoint gets, and one repeated
        let n = keys.len();
        for i in 0..4 {
            let part = &keys[i * n / 4..(i + 1) * n / 4];
            assert_eq!(get(&hub, root, part, i % 2 == 1).status, 200);
        }
        assert_eq!(get(&hub, root, &keys[..1], false).status, 200);
        let (walks, steps) = walks_of(&hub, operation);
        assert_eq!(steps, 6, "{operation}: one known page and five gets");
        assert_eq!(
            walks, 1,
            "{operation}: one history_closure_before call across known and every get"
        );
        assert_eq!(hub.prepared(), before + 1, "{operation}: one preparation");
    }
    // the same steps with no operation: each walks for itself
    let keys = all_known(&hub, root, Some(base));
    assert_eq!(get(&hub, root, &keys, false).status, 200);
    assert_eq!(
        walks_of(&hub, "").0,
        2,
        "request-local steps keep their own walks"
    );
    hub.finish();
}

/// The client results of a pull served from one selection equal those of a pull served by
/// request-local steps: the pull reply and a refused pull's envelope, the stores' records and
/// TRANSITION bytes, fsck, status, suspects, explicit tensions, coverage, the push capture
/// (render and disagreement inputs) and a land, with the same tension build counts.
#[test]
fn r59015_required_consumers_and_transitions_stay_exact() {
    let hub = Hub::start();
    let a = Machine::new("a", &hub);
    a.init();
    // a project name, so a capture's title is not the name of each copy's directory
    let cfg = a.path().join(".rdy.yaml");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("name: Atlas\n");
    std::fs::write(&cfg, text).unwrap();
    a.with(land_sealed_plan);
    let secret = a.with(|r| {
        let made = doc(
            r,
            "w-secret",
            "Secret",
            &format!("The launch code is {CANARY}."),
        );
        land(r, "w-secret");
        made["$p"].as_str().unwrap().to_string()
    });
    let first = a.canon().unwrap();
    a.land_doc("base", "The base is landed.");
    a.remote(&hub.url());
    let v = a.cli_ok(&["push"]);
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    // B is in step with the hub at the base
    let b = copy_of("b", &a, &hub);
    // late attachments below the base (a tag, a dispute), its removal, then two landings above
    // it; the fixture signature of the first pin is left out, since fsck checks signatures
    tag_at(&a, "v0.1.0");
    a.with(|r| {
        r.store_mut()
            .put_dispute(
                &Dispute {
                    landing: first,
                    opened_by: person(5),
                    reason: "an old dispute".into(),
                    at_ms: 5,
                },
                None,
            )
            .unwrap();
        r.seal_open().unwrap();
        remove(r, &secret);
    });
    a.land_doc("top1", "Top one is landed.");
    a.land_doc("top2", "Top two is landed.");
    let v = a.cli_ok(&["push"]);
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    let b2 = copy_of("b2", &b, &hub);

    // a refused pull: the cut get's envelope, from one selection and from request-local steps
    hub.fault(Fault::CutGet(64));
    let scoped_refusal = {
        let _scope = hub.scope("refused pull");
        b.cli_err(&["pull"])
    };
    hub.fault(Fault::CutGet(64));
    let local_refusal = b2.cli_err(&["pull"]);
    assert_eq!(
        normal(&scoped_refusal, &b),
        normal(&local_refusal, &b2),
        "the same refusal envelope"
    );

    // the pulls
    let scoped = {
        let _scope = hub.scope("pull");
        b.cli_ok(&["pull"])
    };
    let (walks, _) = walks_of(&hub, "pull");
    assert_eq!(walks, 1, "the scoped pull read one selection");
    let local = b2.cli_ok(&["pull"]);
    assert_eq!(scoped["canon"]["status"], "pulled", "{scoped}");
    assert_eq!(
        normal(&scoped, &b),
        normal(&local, &b2),
        "the same pull reply"
    );
    assert_eq!(b.canon(), a.canon());

    // the stores: every record of the canon history, and every TRANSITION's bytes
    b.stop();
    b2.stop();
    assert_eq!(held_at(b.path()), held_at(b2.path()));
    assert_eq!(transitions(&b), transitions(&b2));
    assert!(!transitions(&b).is_empty(), "the pull records transitions");
    assert_eq!(history_bytes(b.path()), history_bytes(b2.path()));
    // status, as the person reads it
    assert_eq!(
        normal(&b.cli_ok(&["status"]), &b),
        normal(&b2.cli_ok(&["status"]), &b2)
    );
    // the required consumers, read in process with their build counts
    let (one, builds_one) = consumers(&b);
    let (two, builds_two) = consumers(&b2);
    for (name, value) in one.as_object().unwrap() {
        match (value, &two[name]) {
            (Value::Object(a), Value::Object(b)) => {
                for (field, v) in a {
                    assert_eq!(Some(v), b.get(field), "{name}.{field}");
                }
                assert_eq!(a.len(), b.len(), "{name}");
            }
            (a, b) => assert_eq!(a, b, "{name}"),
        }
    }
    assert_eq!(
        one, two,
        "suspects, tensions, coverage and the push capture"
    );
    assert_eq!(builds_one, builds_two, "the same required builds");
    hub.finish();
}

/// `v` with the project path of `m` replaced, and its times left out.
fn normal(v: &Value, m: &Machine) -> Value {
    let text = v
        .to_string()
        .replace(m.path().to_str().unwrap(), "<project>");
    let mut v: Value = serde_json::from_str(&text).unwrap();
    fn strip(v: &mut Value) {
        match v {
            Value::Object(o) => {
                // a time or the process's live memory figure differs from run to run
                o.retain(|k, _| !k.ends_with("_ms") && k != "elapsed" && k != "memory");
                o.values_mut().for_each(strip);
            }
            Value::Array(a) => a.iter_mut().for_each(strip),
            _ => {}
        }
    }
    strip(&mut v);
    v
}

/// Every TRANSITION record of the project, by hash, with its bytes.
fn transitions(m: &Machine) -> BTreeMap<Hash, (u8, Vec<u8>)> {
    m.with(|r| {
        let reader = r.store().reader();
        let mut hashes = vec![];
        reader
            .for_each_object_of(kinds::TRANSITION, |h| {
                hashes.push(h);
                Ok(())
            })
            .unwrap();
        hashes
            .into_iter()
            .map(|h| (h, reader.get_raw(&h).unwrap()))
            .collect()
    })
}

/// The bytes of the whole canon history the project at `dir` (its daemon stopped) exports.
fn history_bytes(dir: &Path) -> Vec<u8> {
    let h = StoreHandle::open(dir).unwrap();
    let snap = h.snapshot().unwrap();
    let mut out = Vec::new();
    snap.write_history(
        CANON,
        snap.canon().unwrap(),
        None,
        &BTreeSet::new(),
        &mut out,
    )
    .unwrap();
    drop(snap);
    h.close().unwrap();
    out
}

/// What the required consumers read of the project after the pull, and the tension builds they
/// made: suspects, explicit tensions, coverage and the push capture's render inputs, then a land
/// of one more doc and its tensions.
fn consumers(m: &Machine) -> (Value, (u64, u64, u64)) {
    use rdy_engine::tensions::set::{canon_part_runs, full_builds, landing_detections_for_tests};
    m.with(|r| {
        let counts = || {
            (
                full_builds(),
                canon_part_runs(),
                landing_detections_for_tests(),
            )
        };
        let before = counts();
        let read = r.read();
        let scope = rdy_engine::Scope::Canon;
        let suspects = format!("{:?}", read.suspects(&scope).unwrap());
        let tensions = format!("{:?}", read.canon_tensions().unwrap());
        let coverage = serde_json::to_value(&*read.coverage_summary(&scope).unwrap()).unwrap();
        let head = read.canon_head().unwrap();
        let inputs =
            rdy_render::render_input::capture_inputs(&read, &rdy_engine::Scope::At(head)).unwrap();
        let inputs = serde_json::to_value(&inputs).unwrap();
        drop(read);
        doc(r, "w-after", "After", "After the pull.");
        land(r, "w-after");
        let after = format!("{}", r.read().canon_tensions().unwrap().len());
        let now = counts();
        (
            json!({"suspects": suspects, "tensions": tensions, "coverage": coverage,
                "capture": inputs, "after_land": after}),
            (now.0 - before.0, now.1 - before.1, now.2 - before.2),
        )
    })
}
