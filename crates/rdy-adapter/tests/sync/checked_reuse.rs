//! r59011 (D-D2NPN, D-DZDB7, D-H0BZN, D-47HVF): a push quarantine made of the checks its puts
//! already ran. Each put checks its file once ([`check_batch_file`]); the quarantine reads each
//! file's exact bytes again and uses that check only for the same bytes. Changed bytes refuse
//! with no move, and every check over the holder, its tip, its epoch and a removal's proof runs
//! again under the writer, as for a quarantine of plain files.
//!
//! The public API takes only a [`CheckedBatchFile`], which only [`check_batch_file`] makes (the
//! `compile_fail` example on its type), so no report a caller builds can enter it.

use rdy_adapter::stream::{ObjectKey, StreamPurpose};
use rdy_adapter::sync::{CanonMove, CheckedBatchFile, CheckedQuarantine, Moved, check_batch_file};
use rdy_adapter::{Hash, RdyError, RepoId, Snapshot, StoreHandle};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::{Provenance, Role};
use rdy_store::LandRequest;
use rdy_store::kinds;
use rdy_store::sync_stream::{
    StreamHeader, StreamReader, StreamRecord, StreamWriter, batch_checks_for_tests,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

/// The writer's clock of every move (pinned on the calling thread).
const AT: i64 = 1_763_164_800_000;

// ---------------------------------------------------------------------------------------------
// source projects (core)

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn write_in(r: &mut Rdy, view: &str, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "r59011".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy, view: &str) -> Hash {
    let p = r
        .propose(&session(Role::Writer), view, view, Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id)
        .unwrap()
        .parse()
        .unwrap()
}

fn doc(r: &mut Rdy, view: &str, title: &str, body: &str) -> rdy_engine::WriteResult {
    write_in(
        r,
        view,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": title, "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": body}
        ]),
    )
}

fn tag(r: &mut Rdy, name: &str) -> Hash {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

fn project(dir: &Path) -> Rdy {
    std::fs::create_dir_all(dir).unwrap();
    drop(Rdy::init(dir, "rdy-standard").unwrap());
    Rdy::open(dir).unwrap()
}

fn repo_of(r: &Rdy) -> RepoId {
    r.store().reader().repo_id().unwrap().unwrap()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let dest = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_dir(&p, &dest);
        } else {
            std::fs::copy(&p, &dest).unwrap();
        }
    }
}

/// Removes every version of block `target` through the store's own landing path.
fn remove(r: &mut Rdy, target: &str) {
    let s = session(Role::Writer);
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
            assessment: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
}

// ---------------------------------------------------------------------------------------------
// the hub's side

/// The canon history at `root` (less `base`'s closure) of the project at `dir`, written to `out`.
fn export(dir: &Path, root: Hash, base: Option<Hash>, out: &Path) {
    let h = StoreHandle::open(dir).unwrap();
    let snap = h.snapshot().unwrap();
    let file = File::create(out).unwrap();
    snap.write_history(
        StreamPurpose::CanonHistory,
        root,
        base,
        &BTreeSet::<ObjectKey>::new(),
        file,
    )
    .unwrap();
    drop(snap);
    h.close().unwrap();
}

/// One put: the file at `p` checked once, kept open with its check.
fn put(p: &Path, repo: RepoId) -> CheckedBatchFile {
    check_batch_file(File::open(p).unwrap(), Some(repo), |_, _, _| {}).unwrap()
}

/// The plain quarantine of the files at `paths`.
fn plain(snap: &Snapshot, paths: &[&Path]) -> Result<CheckedQuarantine, RdyError> {
    let files: Vec<File> = paths.iter().map(|p| File::open(p).unwrap()).collect();
    snap.check_quarantine(&files)
}

/// The batch checks the quarantine `f` runs on this thread, with its result.
fn counted<T>(f: impl FnOnce() -> T) -> (u64, T) {
    let before = batch_checks_for_tests();
    let out = f();
    (batch_checks_for_tests() - before, out)
}

/// `bytes` written again with its header changed by `head` and its records by `edit`.
fn rewrite(
    bytes: &[u8],
    head: impl FnOnce(&mut StreamHeader),
    edit: impl FnOnce(&mut Vec<StreamRecord>),
) -> Vec<u8> {
    let (mut r, mut header) = StreamReader::open_history(bytes).unwrap();
    let mut records = Vec::new();
    while let Some(rec) = r.next().unwrap() {
        records.push(rec);
    }
    head(&mut header);
    edit(&mut records);
    let mut w = StreamWriter::new(Vec::new(), &header).unwrap();
    for rec in &records {
        w.record(rec).unwrap();
    }
    w.finish().unwrap().0
}

/// The framing error of `bytes` as the stream reader reads them whole, if any: framing, each
/// payload's hash, the header, the trailer and the end of the file.
fn framing(bytes: &[u8]) -> Option<rdy_store::stream_error::StreamError> {
    let (mut r, _) = match StreamReader::open_history(bytes) {
        Ok(r) => r,
        Err(e) => return Some(e),
    };
    loop {
        match r.next() {
            Ok(Some(_)) => {}
            Ok(None) => return None,
            Err(e) => return Some(e),
        }
    }
}

/// A canon world: P lands `l1`, then `l2`; Q, a copy of P at `l1`, lands `q2`. The histories
/// are P's at `l1`, P's at `l2` less `l1` and Q's at `q2` less `l1`.
struct World {
    dir: tempfile::TempDir,
    repo: RepoId,
    l1: Hash,
    l2: Hash,
    q2: Hash,
    e1: PathBuf,
    e2: PathBuf,
    eq: PathBuf,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let pd = dir.path().join("p");
        let qd = dir.path().join("q");
        let mut p = project(&pd);
        let repo = repo_of(&p);
        doc(&mut p, "w0", "Canon", "The hub keeps one canon.");
        let l1 = land(&mut p, "w0");
        p.close().unwrap();
        copy_dir(&pd, &qd);
        let mut p = Rdy::open(&pd).unwrap();
        doc(&mut p, "w1", "Extend", "The canon moves forward.");
        let l2 = land(&mut p, "w1");
        p.close().unwrap();
        let mut q = Rdy::open(&qd).unwrap();
        doc(&mut q, "w2", "Other", "A sibling of the next landing.");
        let q2 = land(&mut q, "w2");
        q.close().unwrap();
        let at = |n: &str| dir.path().join(format!("{n}.rdysync"));
        export(&pd, l1, None, &at("e1"));
        export(&pd, l2, Some(l1), &at("e2"));
        export(&qd, q2, Some(l1), &at("eq"));
        World {
            repo,
            l1,
            l2,
            q2,
            e1: at("e1"),
            e2: at("e2"),
            eq: at("eq"),
            dir,
        }
    }

    fn holder(&self, name: &str) -> StoreHandle {
        StoreHandle::create_holder(&self.dir.path().join(name), self.repo).unwrap()
    }

    /// A copy of the file at `p`, as a new put's own file.
    fn copy(&self, p: &Path, name: &str) -> PathBuf {
        let to = self.dir.path().join(name);
        std::fs::copy(p, &to).unwrap();
        to
    }
}

/// Seeds `h` with `l1` from the put of `e1`.
fn seed(h: &StoreHandle, w: &World) {
    let snap = h.snapshot().unwrap();
    let c = snap
        .check_quarantine_checked(&[put(&w.e1, w.repo)])
        .unwrap();
    assert_eq!(
        h.move_canon(&c, CanonMove::Seed, None).unwrap(),
        Moved::Moved { tip: w.l1 }
    );
}

const CHANGED: &str = "input changed during take";

// ---------------------------------------------------------------------------------------------
// the pins

/// Two puts keep one batch check each, and their quarantine runs none again. A file whose bytes
/// change after its put, in a payload, the header, a raw encoding, a dropped record's proof or
/// the trailer, refuses with no move: a valid stream of other bytes is `input changed during
/// take`, and a malformed one keeps the framing or hash error its own check gives.
#[test]
fn r59011_put_receipt_reuses_checks_without_trusting_changed_bytes() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = World::new();

    // two puts of one push (the same history twice): one check each, none at the quarantine
    let h = w.holder("hub");
    let a = w.copy(&w.e1, "a.rdysync");
    let (n, puts) = counted(|| vec![put(&w.e1, w.repo), put(&a, w.repo)]);
    assert_eq!(n, 2, "one check per put");
    let snap = h.snapshot().unwrap();
    let (n, c) = counted(|| snap.check_quarantine_checked(&puts).unwrap());
    assert_eq!(n, 0, "the quarantine checks no file again");
    // the quarantine of plain files checks each once and agrees
    let (n, p) = counted(|| plain(&snap, &[&w.e1, &a]).unwrap());
    assert_eq!(n, 2);
    assert_eq!(c.facts(), p.facts());
    assert_eq!(
        h.move_canon(&c, CanonMove::Seed, None).unwrap(),
        Moved::Moved { tip: w.l1 }
    );
    drop((snap, c, p, puts));

    let good = std::fs::read(&w.e2).unwrap();
    let other = std::fs::read(&w.eq).unwrap();
    let payload = {
        let (mut r, _) = StreamReader::open_history(&good[..]).unwrap();
        let mut first = None;
        while let Some(rec) = r.next().unwrap() {
            if let StreamRecord::Present { payload, .. } = rec
                && payload.len() > 8
            {
                first = Some(payload);
                break;
            }
        }
        first.unwrap()
    };
    let at = good
        .windows(payload.len())
        .position(|win| win == &payload[..])
        .unwrap();
    let flip = |i: usize| {
        let mut b = good.clone();
        b[i] ^= 0x55;
        b
    };
    let dropped = |apply: Hash| StreamRecord::Dropped {
        key: ObjectKey {
            kind: kinds::BLOB_CHUNK,
            hash: Hash::of(b"r59011 chunk"),
        },
        encoding: 0,
        original_len: 12,
        apply,
        residue: None,
    };
    // a batch with a dropped record whose proof is a prerequisite
    let with_drop = rewrite(
        &good,
        |h| {
            h.prerequisites.push(ObjectKey {
                kind: kinds::REDACTION_APPLY,
                hash: Hash::of(b"proof one"),
            });
            h.prerequisites.sort();
        },
        |rs| rs.push(dropped(Hash::of(b"proof one"))),
    );
    let cases: Vec<(&str, Vec<u8>, Vec<u8>)> = vec![
        ("payload", good.clone(), flip(at + payload.len() / 2)),
        (
            "header",
            good.clone(),
            rewrite(
                &good,
                |h| {
                    h.prerequisites.push(ObjectKey {
                        kind: kinds::LANDING,
                        hash: w.q2,
                    });
                    h.prerequisites.sort();
                    h.prerequisites.dedup();
                },
                |_| {},
            ),
        ),
        (
            "raw encoding",
            good.clone(),
            // the frame's raw kind (`len: u32, kind: u8, hash: [u8; 32], payload`) with its
            // versioned bit turned
            {
                let mut b = good.clone();
                b[at - 33] ^= 0x80;
                b
            },
        ),
        (
            "dropped proof",
            with_drop.clone(),
            rewrite(
                &with_drop,
                |_| {},
                |rs| {
                    rs.pop();
                    rs.push(dropped(Hash::of(b"proof two")));
                },
            ),
        ),
        ("trailer", good.clone(), flip(good.len() - 1)),
        ("other batch", good.clone(), other.clone()),
        ("cut", good.clone(), good[..good.len() - 9].to_vec()),
    ];
    let (mut changed, mut framed) = (0, 0);
    for (name, before, after) in cases {
        let p = w
            .dir
            .path()
            .join(format!("put-{}.rdysync", name.replace(' ', "-")));
        std::fs::write(&p, &before).unwrap();
        // the put's check of the bytes it received
        let Ok(b) = check_batch_file(File::open(&p).unwrap(), Some(w.repo), |_, _, _| {}) else {
            panic!("{name}: the put's bytes pass its check");
        };
        // the same file changes in place before the write
        std::fs::write(&p, &after).unwrap();
        let snap = h.snapshot().unwrap();
        let (n, got) = counted(|| snap.check_quarantine_checked(std::slice::from_ref(&b)));
        assert_eq!(n, 0, "{name}: no check runs again");
        let e = got.expect_err(name);
        // the changed bytes alone: a stream whose framing, hashes, trailer and end read keeps
        // that error; a stream that reads is other bytes than the check's
        match framing(&after) {
            None => {
                changed += 1;
                assert!(e.message.contains(CHANGED), "{name}: {e:#?}");
            }
            Some(own) => {
                framed += 1;
                assert!(!e.message.contains(CHANGED), "{name}: {e:#?}");
                assert_eq!((e.code, &e.message), (own.code, &own.message), "{name}");
            }
        }
        // nothing moved
        assert_eq!(h.snapshot().unwrap().canon(), Some(w.l1), "{name}");
    }
    assert!(
        changed >= 3 && framed >= 3,
        "{changed} changed, {framed} malformed"
    );
    // the unchanged history still moves the canon
    let snap = h.snapshot().unwrap();
    let c = snap
        .check_quarantine_checked(&[put(&w.e2, w.repo)])
        .unwrap();
    assert_eq!(
        h.move_canon(&c, CanonMove::Extend, Some(w.l1)).unwrap(),
        Moved::Moved { tip: w.l2 }
    );
    h.fsck().unwrap();
    drop((snap, c));
    h.close().unwrap();
}

/// After a put's check, the holder's tip or redaction epoch changes: the compare and the epoch
/// check refuse as for a quarantine of plain files. A removal's proof is checked again under the
/// writer: a removal history whose file changes after a good put gains nothing, and a put of a
/// bad proof refuses as the plain quarantine does.
#[test]
fn r59011_context_and_removal_are_rechecked() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = World::new();

    // the tip moves after the check: behind, nothing written
    let h = w.holder("hub");
    seed(&h, &w);
    let snap = h.snapshot().unwrap();
    let c2 = snap
        .check_quarantine_checked(&[put(&w.e2, w.repo)])
        .unwrap();
    let cq = snap
        .check_quarantine_checked(&[put(&w.eq, w.repo)])
        .unwrap();
    assert_eq!(
        h.move_canon(&cq, CanonMove::Extend, Some(w.l1)).unwrap(),
        Moved::Moved { tip: w.q2 }
    );
    assert_eq!(
        h.move_canon(&c2, CanonMove::Extend, Some(w.l1)).unwrap(),
        Moved::Behind { tip: Some(w.q2) }
    );
    assert_eq!(h.snapshot().unwrap().canon(), Some(w.q2));
    drop((snap, c2, cq));
    h.close().unwrap();

    // the epoch moves after the check: the move and the snapshot's reads refuse
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().join("p");
    let mut r = project(&pd);
    let repo = repo_of(&r);
    let d = doc(&mut r, "work", "Intro", "Public words.");
    let secret = write_in(
        &mut r,
        "work",
        json!([{"op": "create", "type": "prose", "id": "$x", "parent": d.created["$s"], "body": "The launch code is ZEBRA-7731."}]),
    );
    let l1 = land(&mut r, "work");
    let v1 = tag(&mut r, "v1.0.0");
    let mut out = Vec::new();
    rdy_engine::stream_export::write(&r.read(), v1, &BTreeSet::new(), &mut out).unwrap();
    let t1 = dir.path().join("t1.rdysync");
    std::fs::write(&t1, out).unwrap();
    let e1 = dir.path().join("e1.rdysync");
    let mut out = Vec::new();
    rdy_engine::stream_export::write_history(
        &r.read(),
        StreamPurpose::CanonHistory,
        l1,
        None,
        &BTreeSet::new(),
        &mut out,
    )
    .unwrap();
    std::fs::write(&e1, out).unwrap();
    let target = secret.created["$x"].clone();
    remove(&mut r, &target);
    let v2 = tag(&mut r, "v2.0.0");
    let l2t = r.store().read_tag(&v2).unwrap().landing;
    let mut out = Vec::new();
    rdy_engine::stream_export::write(&r.read(), v2, &BTreeSet::new(), &mut out).unwrap();
    let t2 = dir.path().join("t2.rdysync");
    std::fs::write(&t2, out).unwrap();
    r.close().unwrap();

    let h = StoreHandle::create_holder(&dir.path().join("hub"), repo).unwrap();
    let up = |p: &Path, tag: &str, landing: Hash| {
        h.take(
            &[File::open(p).unwrap()],
            rdy_adapter::take::UploadTarget {
                tag: tag.into(),
                landing,
            },
        )
        .unwrap();
    };
    up(&t1, "v1.0.0", l1);
    let snap = h.snapshot().unwrap();
    let b1 = put(&e1, repo);
    let checked = snap
        .check_quarantine_checked(std::slice::from_ref(&b1))
        .unwrap();
    up(&t2, "v2.0.0", l2t);
    let e = h.move_canon(&checked, CanonMove::Seed, None).unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e:#?}");
    assert_eq!(h.snapshot().unwrap().canon(), None);
    // a new quarantine of the same put on the old snapshot refuses as well
    let e = snap.check_quarantine_checked(&[b1]).unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e:#?}");
    drop((snap, checked));
    h.close().unwrap();

    // a removal history: a good put, then other bytes; and a put of a bad proof
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().join("p");
    let mut r = project(&pd);
    let repo = repo_of(&r);
    let d = doc(&mut r, "work", "Intro", "Public words.");
    let secret = write_in(
        &mut r,
        "work",
        json!([{"op": "create", "type": "prose", "id": "$x", "parent": d.created["$s"], "body": "The launch code is ZEBRA-7731."}]),
    );
    let l1 = land(&mut r, "work");
    r.close().unwrap();
    let e1 = dir.path().join("e1.rdysync");
    export(&pd, l1, None, &e1);
    let h = StoreHandle::create_holder(&dir.path().join("hub"), repo).unwrap();
    let s0 = h.snapshot().unwrap();
    let c1 = s0.check_quarantine_checked(&[put(&e1, repo)]).unwrap();
    assert_eq!(
        h.move_canon(&c1, CanonMove::Seed, None).unwrap(),
        Moved::Moved { tip: l1 }
    );
    let mut r = Rdy::open(&pd).unwrap();
    remove(&mut r, &secret.created["$x"]);
    let l2 = r.store().reader().canon_head().unwrap().unwrap();
    r.close().unwrap();
    let e2 = dir.path().join("e2.rdysync");
    export(&pd, l2, Some(l1), &e2);
    let good = std::fs::read(&e2).unwrap();
    // the proof of each dropped record, replaced
    let bad = rewrite(
        &good,
        |_| {},
        |rs| {
            for rec in rs.iter_mut() {
                if let StreamRecord::Dropped { apply, .. } = rec {
                    *apply = Hash::of(b"r59011 no such removal");
                }
            }
        },
    );
    assert_ne!(bad, good, "the history carries dropped records");
    let held = |h: &StoreHandle| {
        let s = h.snapshot().unwrap();
        (s.canon(), s.view_heads().is_ok())
    };

    // the good put is quarantined; its file then holds the bad proof: the move gains nothing
    let s1 = h.snapshot().unwrap();
    let b2 = put(&e2, repo);
    let c2 = s1
        .check_quarantine_checked(std::slice::from_ref(&b2))
        .unwrap();
    std::fs::write(&e2, &bad).unwrap();
    let e = h.move_canon(&c2, CanonMove::Extend, Some(l1)).unwrap_err();
    assert_eq!(held(&h), (Some(l1), true), "{e:#?}");
    drop((c2, b2));

    // a put of the bad proof: the checked quarantine refuses as the plain one does
    let bp = dir.path().join("bad.rdysync");
    std::fs::write(&bp, &bad).unwrap();
    let plain_out = plain(&s1, &[&bp]).map(|c| c.facts().clone());
    let checked_out = check_batch_file(File::open(&bp).unwrap(), Some(repo), |_, _, _| {})
        .and_then(|b| s1.check_quarantine_checked(&[b]))
        .map(|c| c.facts().clone());
    match (&plain_out, &checked_out) {
        (Err(a), Err(b)) => assert_eq!((a.code, &a.message), (b.code, &b.message)),
        (Ok(a), Ok(b)) => {
            assert_eq!(a, b);
            // the move checks the proof under the writer
            let c = s1.check_quarantine_checked(&[put(&bp, repo)]).unwrap();
            assert!(h.move_canon(&c, CanonMove::Extend, Some(l1)).is_err());
        }
        _ => panic!("plain {plain_out:#?} checked {checked_out:#?}"),
    }
    assert_eq!(held(&h), (Some(l1), true));

    // the good bytes still move, with their removal
    std::fs::write(&e2, &good).unwrap();
    let c2 = s1.check_quarantine_checked(&[put(&e2, repo)]).unwrap();
    assert_eq!(
        h.move_canon(&c2, CanonMove::Extend, Some(l1)).unwrap(),
        Moved::Moved { tip: l2 }
    );
    assert_eq!(s1.view_heads().unwrap_err().code, "E_REDACTED_STATE");
    h.fsck().unwrap();
    drop((s0, c1, s1, c2));
    h.close().unwrap();
}
