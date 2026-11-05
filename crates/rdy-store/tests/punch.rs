//! mh1: hole punching over durable removed payloads (D-4YRQ4: "After each removal syncs its
//! zeros, rdy punches a hole over each removed span"; "Where the filesystem cannot, nothing
//! changes."). Punching is best effort: it changes no byte that rdy reads, adds no frame and
//! never fails a removal.
//!
//! The range, overflow and alignment pins are in `rdy_store::pio`'s unit tests, and the
//! attempt bound is in `rdy_store::segment`'s.
#[path = "support/carried.rs"]
mod carried_fixture;
#[path = "support/redaction.rs"]
mod redaction_fixture;

use carried_fixture::{block_residue, change_residue, find, framed_at, load, record, span};
use rdy_model::redaction::{DropSpan, RedactTarget};
use rdy_model::*;
use rdy_store::Store;
use rdy_store::dump::*;
use rdy_store::dump_chain::{BatchMarker, DumpChainHeader, batch_end};
use rdy_store::kinds;
use rdy_store::pio::{self, Punch};
use rdy_store::segment::{Location, PunchSummary, RemovalStep, SegmentLog, on_removal_step};
use redaction_fixture::{
    Fixture, FixtureOptions, Proof, RequestSpec, authorized_batch_fixture, frames, segment_bytes,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

const HEADER: u64 = 37;
const SEGMENT_MAX: u64 = 64 * 1024 * 1024;
const MAX_SPANS: u64 = 1_000_000;
const NOT_SUPPORTED: &str = "hole punching is not supported for 4 removed spans; they keep their disk space, and their content stays removed";
const FAILED: &str = "hole punching failed for 4 removed spans; they keep their disk space, and their content stays removed";

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn change(ops: Vec<Op>) -> Change {
    Change {
        view: "work".into(),
        ops,
        deps: vec![],
        provenance: Provenance {
            actor: "agent:w".into(),
            run: Some("w1".into()),
            role: Role::Writer,
            at_ms: 10,
        },
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// A closed store with a sealed removal of one block, its change and a two-chunk blob: four
/// spans, not yet erased. The next open erases them.
struct Setup {
    _dir: tempfile::TempDir,
    path: PathBuf,
    fixture: Fixture,
}

impl Setup {
    fn new() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let mut s = Store::create(&path).unwrap();
        let secret = block("b-00001", "canary alpha secret");
        let bystander = block("b-00002", "bravo stays");
        s.commit_to_view(
            "work",
            change(vec![
                Op::Create {
                    block: secret.clone(),
                },
                Op::Create { block: bystander },
            ]),
        )
        .unwrap();
        let mut bytes = b"blob canary ".repeat(100_000);
        bytes.truncate(1024 * 1024 + 4096);
        let blob = s.put_blob(&bytes).unwrap();
        drop(s);
        let spec = RequestSpec::new(
            "work",
            vec![
                RedactTarget::Version {
                    id: secret.id.clone(),
                    version: secret.version_hash(),
                },
                RedactTarget::Blob(blob),
            ],
        );
        let fixture = authorized_batch_fixture(
            &path,
            &[spec],
            &FixtureOptions::approved(Proof::local_human()),
        );
        Setup {
            _dir: dir,
            path,
            fixture,
        }
    }

    fn spans(&self) -> Vec<DropSpan> {
        let mut spans = self.fixture.requests[0]
            .draft
            .request
            .manifest
            .spans
            .clone();
        spans.sort_by_key(|s| (s.place.segment, s.place.offset));
        spans
    }

    /// The payload byte ranges of the spans, in visit order.
    fn ranges(&self) -> Vec<(u64, u64)> {
        self.spans()
            .iter()
            .map(|s| (s.place.offset + HEADER, u64::from(s.len)))
            .collect()
    }

    /// A copy of the store as it is now.
    fn copy(&self, name: &str) -> PathBuf {
        let to = self.path.with_file_name(name);
        carried_fixture::copy_store(&self.path, &to);
        to
    }
}

/// One thing the removal did, in order.
#[derive(Clone, Debug, PartialEq)]
enum Event {
    Step(RemovalStep),
    Punch(u64, u64),
}

type Events = Rc<RefCell<Vec<Event>>>;

/// Removes the hooks when a test ends, also when it panics.
struct Hooks;
impl Drop for Hooks {
    fn drop(&mut self) {
        pio::on_punch(None);
        on_removal_step(None);
    }
}

/// Records every removal step and punch on this thread. `backend` answers each punch by its
/// 0-based number; None lets the real platform call run.
fn observe(
    mut backend: impl FnMut(usize) -> Option<io::Result<Punch>> + 'static,
) -> (Events, Hooks) {
    let events: Events = Rc::default();
    let e = Rc::clone(&events);
    let mut n = 0;
    pio::on_punch(Some(Box::new(move |offset, len| {
        e.borrow_mut().push(Event::Punch(offset, len));
        n += 1;
        backend(n - 1)
    })));
    let e = Rc::clone(&events);
    on_removal_step(Some(Box::new(move |step| {
        e.borrow_mut().push(Event::Step(step));
        Ok(())
    })));
    (events, Hooks)
}

fn punches(events: &Events) -> Vec<(u64, u64)> {
    events
        .borrow()
        .iter()
        .filter_map(|e| match e {
            Event::Punch(o, l) => Some((*o, *l)),
            Event::Step(_) => None,
        })
        .collect()
}

fn diagnostics(events: &Events) -> Vec<String> {
    events
        .borrow()
        .iter()
        .filter_map(|e| match e {
            Event::Step(RemovalStep::Diagnostic(line)) => Some(line.clone()),
            _ => None,
        })
        .collect()
}

fn permission_denied() -> io::Result<Punch> {
    Err(io::Error::from(io::ErrorKind::PermissionDenied))
}

fn log_at(store: &Path) -> SegmentLog {
    SegmentLog::open_marked(&store.join("segments"), SEGMENT_MAX, None)
        .unwrap()
        .0
}

fn segment_file(store: &Path, span: &DropSpan) -> PathBuf {
    store
        .join("segments")
        .join(format!("seg-{:06}.log", span.place.segment))
}

/// The bytes of a full dump of the closed store at `path`.
fn full_dump(path: &Path) -> Vec<u8> {
    let s = Store::open(path).unwrap();
    let mut batches = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            batches.push(b);
            Ok(())
        })
        .unwrap();
    let (log_end, marker) = match batches.last() {
        Some(b) => (batch_end(b).unwrap(), BatchMarker::of(b)),
        None => (BatchMarker::ZERO.start, BatchMarker::ZERO),
    };
    let header = DumpHeader {
        store: rdy_store::format::DumpStore::Features(rdy_store::format::baseline_features()),
        log_end,
        rdy: "test".into(),
        chain: Some(DumpChainHeader::full(RepoId([7; 16]), marker)),
    };
    let mut tally = RemovalTally::default();
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    for b in &batches {
        tally.observe(b).unwrap();
        w.batch(b).unwrap();
    }
    w.finish(tally.facts().unwrap()).unwrap().0
}

/// Every frame's place, kind, hash and length, read through the store.
fn places(path: &Path) -> Vec<(Location, u8, Hash, u32)> {
    let s = Store::open(path).unwrap();
    frames(&s.reader(), s.log_end())
        .into_iter()
        .map(|f| (f.loc, f.kind, f.hash, f.len))
        .collect()
}

#[test]
fn mh1_punch_preserves_bytes_headers_length_and_hashes() {
    // sentinels around a zero payload, through the real platform call
    let f = tempfile::NamedTempFile::new().unwrap();
    let mut content = vec![0xA5u8; 64 * 1024];
    content.extend(vec![0u8; 1024 * 1024]);
    content.extend(vec![0x5Au8; 64 * 1024]);
    std::fs::write(f.path(), &content).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.path())
        .unwrap();
    let outcome = pio::punch_hole(&file, 64 * 1024, 1024 * 1024);
    assert!(outcome.is_ok(), "a valid range: {outcome:?}");
    assert_eq!(std::fs::read(f.path()).unwrap(), content);

    // a real removal through the real call, then one where the file system cannot punch
    let setup = Setup::new();
    let unsupported = setup.copy("unsupported");
    let original = segment_bytes(&setup.path);
    let spans = setup.spans();
    let mut want = original.clone();
    for s in &spans {
        let bytes = want
            .get_mut(&format!("seg-{:06}.log", s.place.segment))
            .unwrap();
        let start = (s.place.offset + HEADER) as usize;
        bytes[start..start + s.len as usize].fill(0);
    }
    let (events, hooks) = observe(|_| None);
    drop(Store::open(&setup.path).unwrap());
    assert_eq!(
        punches(&events),
        setup.ranges(),
        "exactly the payload ranges, each once, in order"
    );
    drop(hooks);
    assert_eq!(
        segment_bytes(&setup.path),
        want,
        "only the removal's zeros differ"
    );
    let at_real = places(&setup.path);

    let (events, hooks) = observe(|_| Some(Ok(Punch::Unsupported)));
    drop(Store::open(&unsupported).unwrap());
    assert_eq!(punches(&events), setup.ranges());
    drop(hooks);
    assert_eq!(segment_bytes(&unsupported), want);
    assert_eq!(places(&unsupported), at_real);
    assert_eq!(full_dump(&unsupported), full_dump(&setup.path));
}

#[test]
fn mh1_only_durable_authorized_zeros_reach_pio() {
    // every file sync and the directory sync come before the first punch
    let setup = Setup::new();
    let (events, hooks) = observe(|_| Some(Ok(Punch::Supported)));
    drop(Store::open(&setup.path).unwrap());
    let seen = events.borrow().clone();
    drop(hooks);
    let at = |want: &dyn Fn(&Event) -> bool| seen.iter().position(want).unwrap();
    let dir = at(&|e| *e == Event::Step(RemovalStep::DirSynced));
    let last_file = seen
        .iter()
        .rposition(|e| matches!(e, Event::Step(RemovalStep::FileSynced(_))))
        .unwrap();
    let before = at(&|e| *e == Event::Step(RemovalStep::BeforePunch));
    let first = at(&|e| matches!(e, Event::Punch(..)));
    let after = at(&|e| *e == Event::Step(RemovalStep::AfterPunch));
    assert!(last_file < dir && dir < before && before < first && first < after);

    // a failed file sync or directory sync makes no call, and the next open finishes the work
    for failing in ["file", "dir"] {
        let setup = Setup::new();
        let calls: Events = Rc::default();
        let c = Rc::clone(&calls);
        pio::on_punch(Some(Box::new(move |o, l| {
            c.borrow_mut().push(Event::Punch(o, l));
            Some(Ok(Punch::Supported))
        })));
        on_removal_step(Some(Box::new(move |step| match (&step, failing) {
            (RemovalStep::FileSynced(_), "file") | (RemovalStep::DirSynced, "dir") => {
                Err(io::Error::other("injected sync failure"))
            }
            _ => Ok(()),
        })));
        let hooks = Hooks;
        assert!(
            Store::open(&setup.path).is_err(),
            "{failing}: the sync fails"
        );
        assert!(punches(&calls).is_empty(), "{failing}: no punch");
        drop(hooks);
        drop(Store::open(&setup.path).unwrap());
    }

    // after the removal: a payload that is not zero and a forged header make no call
    let setup = Setup::new();
    drop(Store::open(&setup.path).unwrap());
    let spans = setup.spans();
    let log = log_at(&setup.path);
    let write_at = |span: &DropSpan, at: u64, byte: u8| {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(segment_file(&setup.path, span))
            .unwrap();
        pio::write_all_at(&f, &[byte], at).unwrap();
    };
    write_at(&spans[0], spans[0].place.offset + HEADER + 1, 0x77); // a retained payload byte
    write_at(&spans[1], spans[1].place.offset + 5, !spans[1].hash.0[0]); // a forged hash
    let (events, hooks) = observe(|_| Some(Ok(Punch::Supported)));
    let summary = log.punch_removed(MAX_SPANS);
    assert_eq!(punches(&events), setup.ranges()[2..].to_vec());
    drop(hooks);
    assert_eq!(summary.attempted, 4);
    assert_eq!(summary.failed, 2);
    assert_eq!(summary.unsupported, 0);
    assert_eq!(summary.not_tried, 0);
    assert!(diagnostics(&events).is_empty(), "the method prints nothing");

    // a held carried span is not a removable hole: its intact payload never reaches pio
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let mut s = Store::create(&src).unwrap();
    let held = block("b-00001", "held text");
    let id = s
        .commit_to_view(
            "work",
            change(vec![Op::Create {
                block: held.clone(),
            }]),
        )
        .unwrap();
    let stored = s.change(&id).unwrap();
    let writer = stored.provenance.clone();
    let human = Provenance {
        actor: "human:alice".into(),
        run: None,
        role: Role::Approver,
        at_ms: 11,
    };
    let p = s.propose("work", "held", writer).unwrap();
    s.land(&p, human).unwrap();
    drop(s);
    let framed = framed_at(&src);
    let residues = vec![block_residue(&held), change_residue(&stored)];
    let mut held_spans = vec![
        span(
            &find(&framed, kinds::BLOCK, held.version_hash()),
            Some(&residues[0]),
            true,
        ),
        span(&find(&framed, kinds::CHANGE, id), Some(&residues[1]), true),
    ];
    held_spans.sort_by_key(|s| s.place);
    let rec = record(
        vec![RedactTarget::Version {
            id: held.id.clone(),
            version: held.version_hash(),
        }],
        held_spans,
    );
    let to = dir.path().join("to");
    load(&src, &to, &residues, std::slice::from_ref(&rec)).unwrap();
    let log = log_at(&to);
    let (events, hooks) = observe(|_| Some(Ok(Punch::Supported)));
    let summary = log.punch_removed(MAX_SPANS);
    assert!(punches(&events).is_empty());
    drop(hooks);
    assert_eq!(summary.attempted, 0);
}

#[test]
fn mh1_punch_failures_do_not_fail_removal() {
    type Answer = fn() -> io::Result<Punch>;
    let cases: [(&str, Answer, &[&str]); 3] = [
        ("unsupported", || Ok(Punch::Unsupported), &[NOT_SUPPORTED]),
        ("permission", permission_denied, &[FAILED]),
        ("io", || Err(io::Error::other("injected")), &[FAILED]),
    ];
    for (name, answer, want) in cases {
        let setup = Setup::new();
        let (events, hooks) = observe(move |_| Some(answer()));
        let s = Store::open(&setup.path).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(diagnostics(&events), want, "{name}");
        drop(hooks);
        let report = s.verify_report().unwrap();
        assert_eq!(report.redaction.unwrap().frames, 4, "{name}");
        let removed = frames(&s.reader(), s.log_end())
            .into_iter()
            .filter(|f| f.payload.is_none())
            .count();
        assert_eq!(removed, 4, "{name}: the content stays removed");
    }

    // both kinds at once: each text once, in the stated order, whichever came first
    let setup = Setup::new();
    let (events, hooks) = observe(|n| {
        Some(match n {
            0 => permission_denied(),
            1 => Ok(Punch::Unsupported),
            _ => Ok(Punch::Supported),
        })
    });
    let mut s = Store::open(&setup.path).unwrap();
    assert_eq!(
        diagnostics(&events),
        [
            "hole punching is not supported for 1 removed spans; they keep their disk space, and their content stays removed",
            "hole punching failed for 1 removed spans; they keep their disk space, and their content stays removed",
        ]
    );
    drop(hooks);
    // a live write after the failure still works: nothing was poisoned
    assert!(matches!(s.finish_redactions(), Ok(c) if c.frames == 4));
    s.put_blob(b"after").unwrap();
}

#[test]
fn mh1_punch_repeats_and_survives_each_boundary() {
    // the child of a barrier case: it dies at the barrier, with no destructor run
    if let Ok(barrier) = std::env::var("RDY_MH1_BARRIER") {
        let path = std::env::var("RDY_MH1_STORE").unwrap();
        on_removal_step(Some(Box::new(move |step| {
            if matches!(
                (barrier.as_str(), &step),
                ("before", RemovalStep::BeforePunch) | ("after", RemovalStep::AfterPunch)
            ) {
                std::process::abort();
            }
            Ok(())
        })));
        let _ = Store::open(Path::new(&path));
        panic!("the child did not reach its barrier");
    }

    let setup = Setup::new();
    let barriers = ["before", "after"].map(|b| (b, setup.copy(b)));
    let (_, hooks) = observe(|_| Some(Ok(Punch::Unsupported)));
    drop(Store::open(&setup.path).unwrap());
    drop(hooks);
    let dump = full_dump(&setup.path);
    let at = places(&setup.path);
    let bytes = segment_bytes(&setup.path);

    for (barrier, path) in &barriers {
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "mh1_punch_repeats_and_survives_each_boundary",
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env("RDY_MH1_BARRIER", barrier)
            .env("RDY_MH1_STORE", path)
            .output()
            .unwrap();
        assert!(
            !out.status.success()
                && !String::from_utf8_lossy(&out.stderr).contains("did not reach"),
            "{barrier}: {out:?}"
        );
        assert_eq!(full_dump(path), dump, "{barrier}: a restart keeps the dump");
        assert_eq!(places(path), at, "{barrier}: a restart keeps the places");
        assert_eq!(
            segment_bytes(path),
            bytes,
            "{barrier}: a restart keeps the bytes"
        );

        // repeating, real platform call included, adds no frame and erases no new byte
        let end = Store::open(path).unwrap().log_end();
        let log = log_at(path);
        let once: PunchSummary = log.punch_removed(MAX_SPANS);
        let twice = log.punch_removed(MAX_SPANS);
        drop(log);
        assert_eq!(once.attempted, 4);
        assert_eq!(twice.attempted, 4);
        assert_eq!(once.failed + twice.failed, 0, "{barrier}");
        assert_eq!(
            segment_bytes(path),
            bytes,
            "{barrier}: the repeat keeps the bytes"
        );
        assert_eq!(Store::open(path).unwrap().log_end(), end);
        assert_eq!(
            full_dump(path),
            dump,
            "{barrier}: the repeat keeps the dump"
        );
    }
}

/// A file of `len` bytes: `0xA5` sentinels with zeros at `zeros`, opened to read and write.
fn sentinel_file(len: usize, zeros: std::ops::Range<usize>) -> (tempfile::NamedTempFile, Vec<u8>) {
    let f = tempfile::NamedTempFile::new().unwrap();
    let mut content = vec![0xA5u8; len];
    content[zeros].fill(0);
    std::fs::write(f.path(), &content).unwrap();
    (f, content)
}

fn open_rw(path: &Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

#[test]
fn wx3_sparse_punch_changes_only_authorized_zeros() {
    // the real platform call over zero runs that touch sentinels on both sides, unaligned and
    // aligned, twice (a file marked sparse once stays so): every byte, and the length, as before
    let (f, content) = sentinel_file(3 * 1024 * 1024, 4097..(2 * 1024 * 1024 + 13));
    let file = open_rw(f.path());
    for (offset, len) in [
        (4097u64, 1u64),
        (4097, 2 * 1024 * 1024 + 13 - 4097),
        (8192, 1024 * 1024),
        (2 * 1024 * 1024 + 12, 1),
    ] {
        for round in 0..2 {
            let outcome = pio::punch_hole(&file, offset, len);
            assert!(outcome.is_ok(), "{offset} {len} round {round}: {outcome:?}");
            assert_eq!(
                std::fs::read(f.path()).unwrap(),
                content,
                "{offset} {len} round {round}"
            );
            assert_eq!(file.metadata().unwrap().len(), content.len() as u64);
        }
    }
    // the punch reads the bytes through the handle it punched: positioned reads agree
    let mut back = vec![1u8; content.len()];
    pio::read_exact_at(&file, &mut back, 0).unwrap();
    assert_eq!(back, content);

    // a removal through the real call: only the removal's zeros differ from the original
    let setup = Setup::new();
    let original = segment_bytes(&setup.path);
    let mut want = original.clone();
    for s in &setup.spans() {
        let bytes = want
            .get_mut(&format!("seg-{:06}.log", s.place.segment))
            .unwrap();
        let start = (s.place.offset + HEADER) as usize;
        bytes[start..start + s.len as usize].fill(0);
    }
    let at_start = places(&setup.path);
    drop(Store::open(&setup.path).unwrap());
    assert_eq!(segment_bytes(&setup.path), want);
    let at_real = places(&setup.path);
    assert_eq!(at_real.len(), at_start.len(), "no frame added or lost");

    // a removed payload that no longer reads as zeros, and a forged header, are refused before
    // the real call: their bytes stay as they are, and the rest punch again unchanged
    let spans = setup.spans();
    let log = log_at(&setup.path);
    let write_at = |span: &DropSpan, at: u64, byte: u8| {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(segment_file(&setup.path, span))
            .unwrap();
        pio::write_all_at(&f, &[byte], at).unwrap();
    };
    let kept = spans[0].place.offset + HEADER + 1;
    let forged = spans[1].place.offset + 5;
    write_at(&spans[0], kept, 0x77);
    write_at(&spans[1], forged, !spans[1].hash.0[0]);
    let before = segment_bytes(&setup.path);
    let summary = log.punch_removed(MAX_SPANS);
    drop(log);
    assert_eq!(summary.attempted, 4);
    assert_eq!(summary.failed, 2);
    assert_eq!(segment_bytes(&setup.path), before);
    let seg0 = &before[&format!("seg-{:06}.log", spans[0].place.segment)];
    assert_eq!(seg0[kept as usize], 0x77, "a byte the punch was not given");
}

#[test]
fn wx3_unsupported_punch_keeps_the_existing_note() {
    // a removal on open, then a compaction: each outcome gives exactly its existing note, and no
    // byte changes after the removal's own zeros
    type Answer = fn() -> io::Result<Punch>;
    let cases: [(&str, Answer, &[&str]); 3] = [
        ("unsupported", || Ok(Punch::Unsupported), &[NOT_SUPPORTED]),
        ("permission", permission_denied, &[FAILED]),
        ("io", || Err(io::Error::other("injected")), &[FAILED]),
    ];
    for (name, answer, want) in cases {
        let setup = Setup::new();
        let (events, hooks) = observe(move |_| Some(answer()));
        let s = Store::open(&setup.path).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(diagnostics(&events), want, "{name}: on open");
        let after_open = segment_bytes(&setup.path);
        let report = s.compact(false).unwrap();
        assert_eq!(report.notes, want, "{name}: compact");
        assert_eq!(
            report.removed_bytes,
            setup.ranges().iter().map(|r| r.1).sum::<u64>()
        );
        assert_eq!(segment_bytes(&setup.path), after_open, "{name}");
        drop(hooks);
        let removed = frames(&s.reader(), s.log_end())
            .into_iter()
            .filter(|f| f.payload.is_none())
            .count();
        assert_eq!(removed, 4, "{name}: the content stays removed");
    }

    // a census that cannot read the allocation after punching keeps its existing note
    let setup = Setup::new();
    let s = Store::open(&setup.path).unwrap();
    let (_, hooks) = observe(|_| Some(Ok(Punch::Unsupported)));
    let reads = Rc::new(RefCell::new(0usize));
    let r = Rc::clone(&reads);
    let segments = segment_bytes(&setup.path).len();
    rdy_store::compact::on_allocation(Some(Box::new(move |_| {
        *r.borrow_mut() += 1;
        (*r.borrow() > segments).then(|| Err(io::Error::other("injected census failure")))
    })));
    let report = s.compact(false);
    rdy_store::compact::on_allocation(None);
    drop(hooks);
    let report = report.unwrap();
    assert_eq!(
        report.notes,
        [
            NOT_SUPPORTED,
            "disk allocation could not be read after punching; on_disk_bytes is the count before this call",
        ]
    );
    assert_eq!(report.punched_bytes, 0);
}

#[test]
fn wx3_allocated_bytes_are_not_log_length() {
    // a real punch of a zero interior frees allocation; the length stays
    let len = 8 * 1024 * 1024;
    let (f, content) = sentinel_file(len, 1024 * 1024..7 * 1024 * 1024);
    let file = open_rw(f.path());
    file.sync_all().unwrap();
    let before = pio::allocated_bytes(&file).unwrap();
    assert!(
        before >= len as u64 / 2,
        "a written file is allocated: {before}"
    );
    let outcome = pio::punch_hole(&file, 1024 * 1024, 6 * 1024 * 1024).unwrap();
    assert_eq!(outcome, Punch::Supported, "this file system punches");
    file.sync_all().unwrap();
    let after = pio::allocated_bytes(&file).unwrap();
    assert_eq!(
        file.metadata().unwrap().len(),
        len as u64,
        "the length stays"
    );
    assert!(
        after < len as u64 && after < before,
        "allocation fell below the length: {before} -> {after} of {len}"
    );
    assert_eq!(std::fs::read(f.path()).unwrap(), content);

    // compaction's on_disk_bytes is the segment files' allocation, never the log's length
    let setup = Setup::new();
    let s = Store::open(&setup.path).unwrap();
    let report = s.compact(true).unwrap();
    let allocated: u64 = std::fs::read_dir(setup.path.join("segments"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            name.starts_with("seg-") && name.ends_with(".log")
        })
        .map(|p| pio::allocated_bytes(&std::fs::File::open(p).unwrap()).unwrap())
        .sum();
    assert_eq!(report.on_disk_bytes, allocated);
    let lengths: u64 = segment_bytes(&setup.path)
        .values()
        .map(|b| b.len() as u64)
        .sum();
    assert_eq!(
        report.log_bytes, lengths,
        "log bytes are the segment lengths"
    );
    // the removed megabyte blob was punched on open, so its allocation is gone
    assert!(
        report.on_disk_bytes < report.log_bytes,
        "{} on disk for {} log bytes",
        report.on_disk_bytes,
        report.log_bytes
    );
}
