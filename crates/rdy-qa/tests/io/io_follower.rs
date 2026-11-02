//! A follower follows a real source through crashes and through the QA command (D-AA4FD: "the log
//! is the truth for the end."): each increment keeps every kind at its source place, a child
//! killed at each step resumes from the log, and `qa-follower` is the engine's follower.
use rdy_engine::follower::{self, FollowerCore};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::Role;
use rdy_qa::every_kind;
use rdy_qa::fixtures::libtest_name;
use rdy_store::dump::{DumpHeader, DumpItem, DumpReader, DumpWriter};
use rdy_store::dump_chain::{BatchMarker, ChainEnd, DumpChainHeader};
use rdy_store::follower::{FollowStep, on_follow_step, read_mark};
use rdy_store::format::DumpStore;
use rdy_store::fsutil::{PrivateWriteStage, with_private_write_probe_for_tests};
use rdy_store::kinds::{self, BatchCommit};
use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint};
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const CANARY: &str = "kestrel canary text";

fn session(role: Role) -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn land(r: &mut Rdy, n: usize, body: &str) -> String {
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("d{n}"), "title": "A", "doc_type": "page"}},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": body}
    ]);
    let w = r
        .write(
            &session(Role::Writer),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(ops).unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    let p = r
        .propose(&session(Role::Writer), "work", "T", Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
    w.created["$p"].clone()
}

fn segments(root: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(root.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            (
                p.file_name().unwrap().to_string_lossy().to_string(),
                std::fs::read(&p).unwrap(),
            )
        })
        .collect()
}

/// A full dump of the first batch of the full dump `whole`, with its project files: the seed of a
/// chain whose one increment holds every later batch, at a real source's places.
fn first_batch_dump(whole: &Path, out: &Path) -> ChainEnd {
    let bytes = std::fs::read(whole).unwrap();
    let (mut reader, _, header) = DumpReader::open(std::io::Cursor::new(&bytes)).unwrap();
    let chain = header.chain.unwrap();
    let mut files = vec![];
    let batch = loop {
        match reader.next().unwrap().unwrap() {
            DumpItem::File { path, bytes } => files.push((path, bytes)),
            DumpItem::Batch(b) => break b,
        }
    };
    let mut manifest = None;
    for f in &batch.frames {
        if let rdy_store::dump::Frame::Present {
            kind: kinds::REF_UPDATE,
            payload,
            ..
        } = f
        {
            let u: kinds::RefUpdate = postcard::from_bytes(payload).unwrap();
            if u.name == rdy_store::format::REF_STORE_FEATURES {
                manifest = Some(rdy_store::format::decode_features(&u.value).unwrap());
            }
        }
    }
    let store = DumpStore::Features(manifest.unwrap());
    let marker = BatchCommit {
        start: batch.start,
        frames: batch.frames.len() as u32,
        digest: batch.digest,
    };
    let end_batch = BatchMarker::of_commit(&marker);
    let size: u64 = batch
        .frames
        .iter()
        .map(|f| 37 + u64::from(f.original_len()))
        .sum();
    let marker_len = postcard::to_allocvec(&marker).unwrap().len() as u64;
    let log_end = Location {
        segment: batch.start.segment,
        offset: batch.start.offset + size + 37 + marker_len,
    };
    let file = std::fs::File::create(out).unwrap();
    let mut w = DumpWriter::new(
        file,
        &DumpHeader {
            store: store.clone(),
            log_end,
            rdy: header.rdy.clone(),
            chain: Some(DumpChainHeader::full(chain.repo_id, end_batch)),
        },
    )
    .unwrap();
    for (path, bytes) in &files {
        w.file(path, bytes).unwrap();
    }
    w.batch(&batch).unwrap();
    let (_, t) = w
        .finish(rdy_store::dump::RemovalFacts {
            removals: 0,
            first_removed: None,
        })
        .unwrap();
    ChainEnd {
        repo_id: chain.repo_id,
        store_format: store,
        end: log_end,
        end_batch,
        digest: t.digest,
    }
}

#[test]
fn mh8_follow_preserves_every_kind_position_and_hash() {
    let src = every_kind::rolling_project(4096);
    assert!(
        segments(src.path()).len() > 1,
        "the source rolls: the follower must roll where it rolled"
    );
    let d = tempfile::tempdir().unwrap();
    let whole = d.path().join("whole.rdydump");
    let r = Rdy::open(src.path()).unwrap();
    r.read().dump(&whole).unwrap();
    let source_facts = every_kind::facts(&r);
    let full = d.path().join("first.rdydump");
    let prev = first_batch_dump(&whole, &full);
    let inc = d.path().join("rest.rdydump");
    r.read().dump_after(&prev, &inc).unwrap();
    let to = d.path().join("f");
    assert_eq!(follower::seed(&full, &to).unwrap(), prev);
    let mut core = FollowerCore::open(&to).unwrap();
    let got = core.follow(&inc).unwrap();
    assert!(!got.repeated);
    assert!(got.frames > 0);
    assert_eq!(got.end.end, r.read().seq());
    // the log byte for byte: no local batch, each frame at its source place, zeros and all
    let as_of_source = segments(src.path());
    assert_eq!(segments(&to), as_of_source);
    // the follower's reads and its own dump are the source's
    let state = core.read().unwrap();
    let again = d.path().join("again.rdydump");
    state.dump(&again).unwrap();
    assert_eq!(
        std::fs::read(&again).unwrap(),
        std::fs::read(&whole).unwrap()
    );
    core.fsck().unwrap();
    drop((state, core));
    r.close().unwrap();
    // a reopen writes nothing, and the source reads as it did
    drop(FollowerCore::open(&to).unwrap());
    assert_eq!(segments(&to), as_of_source);
    let r = Rdy::open(src.path()).unwrap();
    assert_eq!(every_kind::facts(&r), source_facts);
    r.close().unwrap();
}

/// A source whose seed holds the canary; two increments: two batches of content, then a removal
/// of the seed's block. `removal_in_one`: one increment holds both a payload and its removal.
struct Crash {
    _d: tempfile::TempDir,
    source: PathBuf,
    full: PathBuf,
    /// More than one source batch, no removal.
    plain: PathBuf,
    /// A removal of a payload the seed and `plain` hold.
    removal: PathBuf,
}

fn crash_chain(d: tempfile::TempDir) -> Crash {
    let source = d.path().join("p");
    let mut r = Rdy::init(&source, "rdy-standard").unwrap();
    let block = land(&mut r, 0, CANARY);
    let full = d.path().join("full.rdydump");
    r.read().dump(&full).unwrap();
    let version = version_of(&r, &block);
    land(&mut r, 1, "one");
    land(&mut r, 2, "two");
    std::fs::write(source.join(".rdyignore"), "*.draft.yaml\n").unwrap();
    let plain = d.path().join("plain.rdydump");
    r.read().dump_after_file(&full, &plain).unwrap();
    redact(&mut r, &format!("{block}@{version}"));
    let removal = d.path().join("removal.rdydump");
    r.read().dump_after_file(&plain, &removal).unwrap();
    r.close().unwrap();
    Crash {
        _d: d,
        source,
        full,
        plain,
        removal,
    }
}

fn version_of(r: &Rdy, block: &str) -> String {
    rdy_qa::fixtures::version_of(r, &rdy_engine::Scope::Canon, block)
}

fn redact(r: &mut Rdy, target: &str) {
    let rq = session(Role::Writer);
    r.redact_in(
        &rq,
        "rq",
        RedactInput {
            targets: vec![target.to_string()],
            reason_class: "personal".into(),
            reason: "the term names a person".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let p = r.propose(&rq, "rq", "rq", Some("Removes it.")).unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
}

/// Where the child dies.
#[derive(Clone, Copy, Debug)]
enum Stop {
    Step(FollowStep),
    Fail(Failpoint),
    Mark(PrivateWriteStage),
}

fn stop_name(s: Stop) -> String {
    match s {
        Stop::Step(FollowStep::Marker(n)) => format!("marker:{n}"),
        Stop::Step(FollowStep::Synced) => "synced".into(),
        Stop::Step(FollowStep::Indexed) => "indexed".into(),
        Stop::Step(FollowStep::FileReplaced) => "file".into(),
        Stop::Step(FollowStep::FilesSynced) => "files".into(),
        Stop::Fail(Failpoint::AfterApplySync) => "apply_sync".into(),
        Stop::Fail(Failpoint::AfterZeroWrite) => "zero".into(),
        Stop::Fail(Failpoint::AfterSegmentSync) => "segment_sync".into(),
        Stop::Fail(Failpoint::BeforeIndexPublish) => "before_index".into(),
        Stop::Fail(Failpoint::AfterIndexPublish) => "after_index".into(),
        Stop::Mark(PrivateWriteStage::Written) => "mark_written".into(),
        Stop::Mark(PrivateWriteStage::Renamed) => "mark_renamed".into(),
        Stop::Mark(_) => "mark_synced".into(),
    }
}

fn stop_parse(s: &str) -> Stop {
    let all = stops();
    if let Some(n) = s.strip_prefix("marker:") {
        return Stop::Step(FollowStep::Marker(n.parse().unwrap()));
    }
    all.into_iter().find(|x| stop_name(*x) == s).unwrap()
}

fn stops() -> Vec<Stop> {
    vec![
        Stop::Step(FollowStep::Marker(1)),
        Stop::Step(FollowStep::Marker(2)),
        Stop::Step(FollowStep::Synced),
        Stop::Fail(Failpoint::AfterApplySync),
        Stop::Fail(Failpoint::AfterZeroWrite),
        Stop::Fail(Failpoint::AfterSegmentSync),
        Stop::Fail(Failpoint::BeforeIndexPublish),
        Stop::Fail(Failpoint::AfterIndexPublish),
        Stop::Step(FollowStep::Indexed),
        Stop::Step(FollowStep::FileReplaced),
        Stop::Step(FollowStep::FilesSynced),
        Stop::Mark(PrivateWriteStage::Written),
        Stop::Mark(PrivateWriteStage::Renamed),
    ]
}

/// The child of the crash test: follows `MH8_INC` in `MH8_DIR`, and dies at `MH8_STOP`.
fn child() {
    let dir = PathBuf::from(std::env::var("MH8_DIR").unwrap());
    let inc = PathBuf::from(std::env::var("MH8_INC").unwrap());
    let stop = stop_parse(&std::env::var("MH8_STOP").unwrap());
    let mut core = FollowerCore::open(&dir).unwrap();
    let want = stop_name(stop);
    on_follow_step(Some(Box::new(move |step| {
        if stop_name(Stop::Step(step)) == want {
            std::process::abort();
        }
    })));
    if let Stop::Fail(p) = stop {
        arm_failpoint(Some((p, FailAction::Abort)));
    }
    let out = if let Stop::Mark(stage) = stop {
        with_private_write_probe_for_tests(
            move |s, path| {
                if s == stage && path.to_string_lossy().contains("FOLLOWER") {
                    std::process::abort();
                }
                Ok(())
            },
            || core.follow(&inc),
        )
    } else {
        core.follow(&inc)
    };
    println!("{out:?}");
}

fn run_child(dir: &Path, inc: &Path, stop: Stop) -> std::process::ExitStatus {
    let name = libtest_name(
        module_path!(),
        "mh8_crash_after_each_source_marker_resumes_the_suffix",
    );
    Command::new(std::env::current_exe().unwrap())
        .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
        .env("MH8_DIR", dir)
        .env("MH8_INC", inc)
        .env("MH8_STOP", stop_name(stop))
        .status()
        .unwrap()
}

#[test]
fn mh8_crash_after_each_source_marker_resumes_the_suffix() {
    if std::env::var_os("MH8_STOP").is_some() {
        return child();
    }
    let c = crash_chain(tempfile::tempdir().unwrap());
    let base = tempfile::tempdir().unwrap();
    for (case, stop) in stops().into_iter().enumerate() {
        let what = stop_name(stop);
        let dir = base.path().join(format!("f{case}"));
        follower::seed(&c.full, &dir).unwrap();
        // the plain increment holds the marker, sync, index and file steps; the removal
        // increment holds the zero, segment and index publish steps
        let zeroing = matches!(stop, Stop::Fail(_));
        if zeroing {
            FollowerCore::open(&dir).unwrap().follow(&c.plain).unwrap();
        }
        let inc = if zeroing { &c.removal } else { &c.plain };
        let store = dir.join(".rdy/store");
        let old = read_mark(&store).unwrap().unwrap();
        let status = run_child(&dir, inc, stop);
        assert!(!status.success(), "{what}: the child dies at its step");
        let moved = read_mark(&store).unwrap().unwrap() != old;
        let mut core = FollowerCore::open(&dir).unwrap_or_else(|e| panic!("{what}: {e:?}"));
        if !moved && core.end().unwrap() != old.chain.end {
            assert_eq!(
                core.read().err().map(|e| e.message),
                Some("the follower has an unfinished increment".to_string()),
                "{what}: a premature read"
            );
        }
        let got = core.follow(inc).unwrap_or_else(|e| panic!("{what}: {e:?}"));
        assert_eq!(
            got.end.digest,
            read_mark(&store).unwrap().unwrap().chain.digest
        );
        assert!(core.read().is_ok(), "{what}");
        if !zeroing {
            core.follow(&c.removal)
                .unwrap_or_else(|e| panic!("{what}: {e:?}"));
        }
        core.fsck().unwrap_or_else(|e| panic!("{what}: {e:?}"));
        drop(core);
        assert_eq!(segments(&dir), segments(&c.source), "{what}: the log");
        // no torn bytes count: any quarantine copy holds only the unacknowledged tail
        assert_eq!(
            std::fs::read(dir.join(".rdyignore")).unwrap(),
            b"*.draft.yaml\n",
            "{what}: the files"
        );
    }
}

#[test]
fn mh8_payload_and_removal_in_one_increment_cannot_reopen_inside_it() {
    if std::env::var_os("MH8_KI170").is_some() {
        let dir = PathBuf::from(std::env::var("MH8_DIR").unwrap());
        let inc = PathBuf::from(std::env::var("MH8_INC").unwrap());
        let mut core = FollowerCore::open(&dir).unwrap();
        let n: u64 = std::env::var("MH8_KI170").unwrap().parse().unwrap();
        on_follow_step(Some(Box::new(move |step| {
            if step == FollowStep::Marker(n) {
                std::process::abort();
            }
        })));
        println!("{:?}", core.follow(&inc));
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("p");
    let mut r = Rdy::init(&source, "rdy-standard").unwrap();
    land(&mut r, 0, "seed");
    let full = d.path().join("full.rdydump");
    r.read().dump(&full).unwrap();
    let block = land(&mut r, 1, CANARY);
    let v = version_of(&r, &block);
    redact(&mut r, &format!("{block}@{v}"));
    let inc = d.path().join("both.rdydump");
    r.read().dump_after_file(&full, &inc).unwrap();
    r.close().unwrap();

    // whole, it follows
    let whole = d.path().join("whole");
    follower::seed(&full, &whole).unwrap();
    FollowerCore::open(&whole).unwrap().follow(&inc).unwrap();
    assert_eq!(segments(&whole), segments(&source));

    // killed at each marker inside it: the complete batches stay. Between the batch that holds
    // the payload's zeros and the batch of its apply, the open says to seed a new follower; at
    // every other marker the retry finishes
    let mut refused = 0;
    let name = libtest_name(
        module_path!(),
        "mh8_payload_and_removal_in_one_increment_cannot_reopen_inside_it",
    );
    for n in 1u64.. {
        let dir = d.path().join(format!("f{n}"));
        follower::seed(&full, &dir).unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
            .env("MH8_KI170", n.to_string())
            .env("MH8_DIR", &dir)
            .env("MH8_INC", &inc)
            .status()
            .unwrap();
        if status.success() {
            assert!(n > 1, "the increment holds more than one batch");
            break;
        }
        let before = segments(&dir);
        match FollowerCore::open(&dir) {
            Ok(mut core) => {
                core.follow(&inc).unwrap();
                drop(core);
                let (a, b) = (segments(&dir), segments(&source));
                for (k, v) in &b {
                    let w = &a[k];
                    let at = v.iter().zip(w).position(|(x, y)| x != y);
                    assert!(
                        v == w,
                        "marker {n}: {k} source {} follower {} first diff {at:?}",
                        v.len(),
                        w.len()
                    );
                }
                // only a quarantined copy of torn bytes is extra
                for k in a.keys().filter(|k| !b.contains_key(*k)) {
                    assert!(k.contains(".torn-"), "marker {n}: {k}");
                }
            }
            Err(e) => {
                refused += 1;
                assert_eq!(e.code, "E_REDACTION_CORRUPT");
                assert_eq!(
                    e.fix,
                    "seed a new follower from a full dump of the source store"
                );
                assert_eq!(segments(&dir), before, "complete batches stay");
            }
        }
    }
    assert!(refused > 0, "KNOWN-ISSUES 170 is the stated exception");
}

fn qa_follower(args: &[&std::ffi::OsStr]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_qa-follower"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn mh8_qa_command_uses_the_same_follower() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("p");
    let mut r = Rdy::init(&source, "rdy-standard").unwrap();
    land(&mut r, 0, "seed");
    let full = d.path().join("full.rdydump");
    r.read().dump(&full).unwrap();
    land(&mut r, 1, "one");
    let i1 = d.path().join("i1.rdydump");
    let s1 = r.read().dump_after_file(&full, &i1).unwrap();
    land(&mut r, 2, "two");
    let i2 = d.path().join("i2.rdydump");
    let s2 = r.read().dump_after_file(&i1, &i2).unwrap();
    let repo = r.store().repo_id().unwrap().unwrap();
    r.close().unwrap();

    let to = d.path().join("f");
    let then = std::ffi::OsStr::new("--then");
    let (ok, out, err) = qa_follower(&[
        full.as_os_str(),
        to.as_os_str(),
        then,
        i1.as_os_str(),
        then,
        i2.as_os_str(),
    ]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(
        v.as_object().unwrap().keys().collect::<Vec<_>>().len(),
        6,
        "{v}"
    );
    assert_eq!(v["repo_id"], repo.to_string());
    assert_eq!(v["end"], serde_json::to_value(s2.end).unwrap());
    assert_eq!(v["digest"], s2.digest.to_string());
    assert_eq!(v["increments"], 2);
    assert_eq!(v["batches"], s1.batches + s2.batches);
    assert_eq!(v["frames"], s1.frames + s2.frames);
    assert_eq!(segments(&to), segments(&source));
    // the engine opens what it left, and agrees
    let core = FollowerCore::open(&to).unwrap();
    assert_eq!(core.end().unwrap(), s2.end);
    drop(core);

    // the help text and the argument refusals
    let (ok, out, _) = qa_follower(&[std::ffi::OsStr::new("--help")]);
    assert!(ok);
    assert_eq!(
        out.trim(),
        "usage: qa-follower <full> <dir> [--then <increment>]..."
    );
    let bad = std::ffi::OsStr::new("--then");
    for args in [
        vec![],
        vec![full.as_os_str()],
        vec![full.as_os_str(), to.as_os_str(), bad],
        vec![
            full.as_os_str(),
            to.as_os_str(),
            std::ffi::OsStr::new("--x"),
            i1.as_os_str(),
        ],
        vec![std::ffi::OsStr::new(""), to.as_os_str()],
    ] {
        let (ok, out, err) = qa_follower(&args);
        assert!(!ok, "{args:?}");
        assert!(out.is_empty());
        let e: Value = serde_json::from_str(err.trim()).unwrap();
        assert_eq!(e["code"], "E_BAD_INPUT");
        assert_eq!(e["message"], "invalid qa-follower arguments");
        assert_eq!(
            e["fix"],
            "pass a full dump, an empty follower directory and zero or more --then increment paths"
        );
    }
    // a refused follow is the engine's error, on stderr, with exit 1
    let other = d.path().join("g");
    let (ok, out, err) = qa_follower(&[full.as_os_str(), other.as_os_str(), then, i2.as_os_str()]);
    assert!(!ok);
    assert!(out.is_empty());
    let e: Value = serde_json::from_str(err.trim()).unwrap();
    assert_eq!(e["code"], "E_DUMP_CHAIN");

    // a holder has no second parser: the same command takes its chain
    let source = include_str!("../../src/bin/qa-follower.rs");
    assert!(!source.contains("rdy_adapter"));
    assert!(!source.contains("DumpReader"));
}
