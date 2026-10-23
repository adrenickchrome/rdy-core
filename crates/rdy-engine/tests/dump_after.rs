//! hc4 (D-2FMRR, D-82K89): an increment holds the sealed batches after the end of an earlier dump
//! or increment of the same store, each at its source position. The predecessor is checked
//! against this store with the export rows of `E_DUMP_CHAIN`; a predecessor file is read by its
//! envelope alone. An empty suffix writes nothing. Every export belongs to one redaction epoch: a
//! stale state, or a removal that lands during the export, is `E_REDACTED_STATE`, and no file or
//! partial file is left.
use rdy_engine::dump::{
    chain_end_of, on_dump_batch, on_project_file, project_file_bytes, project_file_reads,
};
use rdy_engine::formats::inspect_dump;
use rdy_engine::redaction::RedactInput;
use rdy_engine::*;
use rdy_model::{Hash, RepoId, Role};
use rdy_store::dump::{
    DumpBatch, DumpHeader, DumpItem, DumpReader, DumpTrailer, on_batch_walk_record,
};
use rdy_store::dump_chain::{DumpChainHeader, batch_end};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::channel;

const SECRET: &str = "The launch code is ZEBRA-7731.";
const EXPORT_FIX: &str =
    "follow the newest file of this store's chain, or start a new chain with rdy dump";

fn writer() -> Session {
    Session::human("human:t".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:t".into(), Role::Approver).unwrap()
}

/// Writes one doc with a prose block to view `work`: one ordinary batch (or more).
fn add(r: &mut Rdy, n: usize, body: &str) -> String {
    let w = r
        .write(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("d{n}"), "title": format!("D{n}"), "doc_type": "page"}},
                    {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": body}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    w.created["$p"].clone()
}

/// Lands view `work`.
fn land(r: &mut Rdy) {
    let p = r.propose(&writer(), "work", "Work", None).unwrap();
    r.land(&approver(), &p.id).unwrap();
}

/// A project with a few landed writes.
fn project(dir: &Path) -> Rdy {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    for n in 0..3 {
        add(&mut r, n, &format!("body {n}"));
    }
    land(&mut r);
    r
}

/// Every sealed batch of `st`'s log, in order.
fn source_batches(st: &ReadState) -> Vec<DumpBatch> {
    let mut all = Vec::new();
    st.reader()
        .for_each_batch(st.seq(), |b| {
            all.push(b);
            Ok(())
        })
        .unwrap();
    all
}

/// A whole, checked read of the dump `path`: its format, header, items and trailer.
fn read_all(path: &Path) -> (u32, DumpHeader, Vec<DumpItem>, DumpTrailer) {
    let (mut rd, format, header) =
        DumpReader::open(BufReader::new(File::open(path).unwrap())).unwrap();
    let mut items = Vec::new();
    while let Some(item) = rd.next().unwrap() {
        items.push(item);
    }
    let trailer = *rd.trailer().unwrap();
    (format, header, items, trailer)
}

fn batches_of(items: &[DumpItem]) -> Vec<DumpBatch> {
    items
        .iter()
        .filter_map(|i| match i {
            DumpItem::Batch(b) => Some(b.clone()),
            DumpItem::File { .. } => None,
        })
        .collect()
}

fn files_of(items: &[DumpItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            DumpItem::File { path, .. } => Some(path.clone()),
            DumpItem::Batch(_) => None,
        })
        .collect()
}

/// No file of `dir` is a dump's partial file.
fn no_partial(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".partial-")
    })
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

/// The bytes of each segment file of the project at `root`, by name.
fn segments(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut all: Vec<(String, Vec<u8>)> = std::fs::read_dir(root.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                bytes(&e.path()),
            )
        })
        .collect();
    all.sort();
    assert!(!all.is_empty());
    all
}

/// A frozen fixture of an older dump format (read only).
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rdy-qa/fixtures/dump")
        .join(name)
}

fn expect_err<T: std::fmt::Debug>(r: Result<T, RdyError>) -> RdyError {
    match r {
        Ok(v) => panic!("expected an error, got {v:?}"),
        Err(e) => e,
    }
}

fn is_hex(v: &Value, digits: usize) -> bool {
    v.as_str().is_some_and(|s| {
        s.len() == digits
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

#[test]
fn hc4_increment_is_only_the_sealed_suffix() {
    let d = tempfile::tempdir().unwrap();
    let mut r = project(&d.path().join("p"));
    let first = r.read();
    let full = first.dump(&d.path().join("full.rdydump")).unwrap();
    assert_eq!(full.kind, DumpKind::Full);
    assert_eq!(
        full.base,
        Location {
            segment: 0,
            offset: 0
        }
    );
    assert_eq!(full.end, first.seq());
    for n in 10..13 {
        add(&mut r, n, &format!("later {n}"));
    }
    land(&mut r);
    let st = r.read();
    // ordinary writes after the captured state are not in its increment
    add(&mut r, 20, "after the capture");
    assert!(r.read().seq() > st.seq());

    let out = d.path().join("inc-1.rdydump");
    let inc = st.dump_after(&full.chain_end(), &out).unwrap();
    assert_eq!(inc.out.as_deref(), Some(out.display().to_string().as_str()));
    assert_eq!(inc.kind, DumpKind::Increment);
    assert_eq!(inc.dump_format, 3);
    assert_eq!(inc.store_format, rdy_store::format::STORE_FORMAT);
    assert_eq!(inc.repo_id, full.repo_id);
    assert_eq!(inc.base, full.end);
    assert_eq!(inc.end, st.seq());
    assert_eq!(inc.files, full.files);
    assert_eq!(inc.bytes, std::fs::metadata(&out).unwrap().len());

    let (format, header, items, trailer) = read_all(&out);
    assert_eq!(format, 3);
    assert_eq!(header.log_end, st.seq());
    assert_eq!(
        header.chain,
        Some(DumpChainHeader {
            kind: DumpKind::Increment,
            repo_id: full.repo_id,
            base: full.end,
            base_batch: full.end_batch,
            prev: full.digest,
            end_batch: inc.end_batch,
        })
    );
    assert_eq!(files_of(&items), inc.files);
    // exactly the source batches after the base, through the captured end, byte for byte
    let suffix: Vec<DumpBatch> = source_batches(&st)
        .into_iter()
        .filter(|b| b.start >= full.end)
        .collect();
    assert!(suffix.len() >= 2, "{}", suffix.len());
    let got = batches_of(&items);
    assert_eq!(got, suffix);
    assert_eq!(got[0].start, full.end);
    assert_eq!(batch_end(got.last().unwrap()), Some(st.seq()));
    assert_eq!(inc.end_batch, BatchMarker::of(got.last().unwrap()));
    assert_eq!(inc.batches, got.len() as u64);
    assert_eq!(
        inc.frames,
        got.iter().map(|b| b.frames.len() as u64).sum::<u64>()
    );
    assert_eq!(trailer.digest, inc.digest);
    assert_eq!(
        (trailer.removals, trailer.first_removed),
        (inc.removals, inc.first_removed)
    );
    assert_eq!((inc.removals, inc.first_removed), (0, None));

    // the next increment follows this one
    let now = r.read();
    let next = now
        .dump_after(&inc.chain_end(), &d.path().join("inc-2.rdydump"))
        .unwrap();
    assert_eq!(next.base, inc.end);
    let (_, header, items, _) = read_all(&d.path().join("inc-2.rdydump"));
    let chain = header.chain.unwrap();
    assert_eq!(
        (chain.base, chain.base_batch, chain.prev),
        (inc.end, inc.end_batch, inc.digest)
    );
    assert_eq!(batches_of(&items)[0].start, inc.end);
    assert_eq!(next.end, now.seq());
}

#[test]
fn hc4_rollover_keeps_source_positions() {
    let d = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(&d.path().join("p"), "rdy-standard").unwrap();
    r.store_mut().set_segment_max(2_000);
    let mut states: Vec<Arc<ReadState>> = vec![r.read()];
    for n in 0..30 {
        add(&mut r, n, &"x".repeat(300));
        states.push(r.read());
    }
    let last = r.read();
    let all = source_batches(&last);
    // a batch that ends its segment, and a state that ends there
    let (i, st) = all
        .windows(2)
        .enumerate()
        .find_map(|(i, w)| {
            let rolled = w[1].start.offset == 0 && w[1].start.segment == w[0].start.segment + 1;
            let end = batch_end(&w[0]).unwrap();
            let st = states.iter().find(|s| s.seq() == end)?;
            rolled.then(|| (i, st.clone()))
        })
        .expect("a state that ends at the end of a segment");
    let full = st.dump(&d.path().join("full.rdydump")).unwrap();
    assert_eq!(full.end, batch_end(&all[i]).unwrap());
    let out = d.path().join("inc.rdydump");
    let inc = last.dump_after(&full.chain_end(), &out).unwrap();
    let (_, header, items, _) = read_all(&out);
    let got = batches_of(&items);
    // the first batch is at offset zero of the next segment, not at the base
    assert_eq!(
        got[0].start,
        Location {
            segment: full.end.segment + 1,
            offset: 0
        }
    );
    assert_eq!(header.chain.unwrap().base, full.end);
    // every batch keeps its source position, frames and marker
    assert_eq!(got, all[i + 1..].to_vec());
    assert!(
        got.iter().map(|b| b.start.segment).max() > Some(got[0].start.segment),
        "the suffix rolls again"
    );
    assert_eq!(inc.end, last.seq());
    assert_eq!(batch_end(got.last().unwrap()), Some(last.seq()));
}

#[test]
fn hc4_no_suffix_returns_prior_end_without_any_file() {
    let d = tempfile::tempdir().unwrap();
    let mut r = project(&d.path().join("p"));
    let st = r.read();
    let full_path = d.path().join("full.rdydump");
    let full = st.dump(&full_path).unwrap();
    let before = bytes(&full_path);
    // later writes do not reach the captured state
    add(&mut r, 9, "later");
    let out = d.path().join("inc.rdydump");
    let kept = d.path().join("kept.rdydump");
    std::fs::write(&kept, b"someone else's file").unwrap();
    let reads = project_file_reads();
    // an output that exists is refused even when nothing would be written
    let e = st.dump_after(&full.chain_end(), &kept).unwrap_err();
    assert_eq!(e.code, "E_ALREADY_EXISTS", "{e:?}");
    assert_eq!(e.message, format!("{} exists", kept.display()));
    for (got, target) in [
        (st.dump_after(&full.chain_end(), &out).unwrap(), &out),
        (st.dump_after_file(&full_path, &out).unwrap(), &out),
    ] {
        assert_eq!(
            got,
            DumpSummary {
                out: None,
                dump_format: 3,
                kind: DumpKind::Increment,
                store_format: full.store_format,
                repo_id: full.repo_id,
                base: full.end,
                end: full.end,
                end_batch: full.end_batch,
                digest: full.digest,
                removals: 0,
                first_removed: None,
                batches: 0,
                frames: 0,
                files: vec![],
                bytes: 0,
            }
        );
        assert_eq!(got.chain_end(), full.chain_end());
        assert!(!target.exists());
    }
    // no project file was read, no output was written or replaced, no partial was left
    assert_eq!(project_file_reads(), reads);
    assert_eq!(bytes(&kept), b"someone else's file");
    assert!(!out.exists());
    assert!(no_partial(d.path()));
    assert_eq!(bytes(&full_path), before);
}

#[test]
fn hc4_predecessor_checks_use_exact_errors() {
    let d = tempfile::tempdir().unwrap();
    let mut r = project(&d.path().join("p"));
    let full_path = d.path().join("full.rdydump");
    let full = r.read().dump(&full_path).unwrap();
    for n in 5..7 {
        add(&mut r, n, "more");
    }
    land(&mut r);
    let st = r.read();
    let prev = full.chain_end();
    let out = d.path().join("inc.rdydump");
    let check = |e: RdyError, message: String| {
        assert_eq!(e.code, E_DUMP_CHAIN, "{e:?}");
        assert_eq!(e.message, message);
        assert_eq!(e.fix, EXPORT_FIX);
        assert!(!out.exists());
        assert!(no_partial(d.path()));
    };
    let repo = full.repo_id;
    let seq = st.seq();
    let other_repo = RepoId([9; 16]);

    // the repo, first of all
    let bad = ChainEnd {
        repo_id: other_repo,
        store_format: 15,
        end: Location {
            segment: seq.segment + 1,
            offset: 1,
        },
        ..prev
    };
    check(
        expect_err(st.dump_after(&bad, &out)),
        format!("previous dump holds repo {other_repo}, but this project is repo {repo}"),
    );
    // then the store format
    let bad = ChainEnd {
        store_format: 15,
        end: Location {
            segment: seq.segment + 1,
            offset: 1,
        },
        ..prev
    };
    check(
        expect_err(st.dump_after(&bad, &out)),
        "previous dump is of store format 15, but this store is of store format 18".into(),
    );
    // then an end past this state
    for past in [
        Location {
            segment: seq.segment,
            offset: seq.offset + 1,
        },
        Location {
            segment: seq.segment + 1,
            offset: 0,
        },
    ] {
        let bad = ChainEnd { end: past, ..prev };
        check(
            expect_err(st.dump_after(&bad, &out)),
            format!(
                "previous dump ends at {}.{}, after the end of this store",
                past.segment, past.offset
            ),
        );
    }
    // then an end that closes no batch: inside a frame, at a frame inside a batch, past a
    // marker by a byte
    let batches = source_batches(&st);
    let in_batch = batches.iter().find(|b| b.frames.len() >= 2).unwrap();
    let second = Location {
        segment: in_batch.start.segment,
        offset: in_batch.start.offset + 37 + u64::from(in_batch.frames[0].original_len()),
    };
    for inside in [
        Location {
            segment: 0,
            offset: 0,
        },
        Location {
            segment: 0,
            offset: 1,
        },
        second,
        Location {
            segment: full.end.segment,
            offset: full.end.offset - 1,
        },
    ] {
        let bad = ChainEnd {
            end: inside,
            ..prev
        };
        check(
            expect_err(st.dump_after(&bad, &out)),
            format!(
                "previous dump ends at {}.{}, which is not the end of a batch in this store",
                inside.segment, inside.offset
            ),
        );
    }
    // then the marker of the batch that ends there
    let mut marker = prev.end_batch;
    marker.digest = Hash([7; 32]);
    let bad = ChainEnd {
        end_batch: marker,
        ..prev
    };
    check(
        expect_err(st.dump_after(&bad, &out)),
        format!(
            "the batch of this store at {}.{} differs from the last batch of previous dump",
            prev.end.segment, prev.end.offset
        ),
    );
    // an end at another real batch end with the old marker
    let earlier = batch_end(&batches[0]).unwrap();
    let bad = ChainEnd {
        end: earlier,
        ..prev
    };
    check(
        expect_err(st.dump_after(&bad, &out)),
        format!(
            "the batch of this store at {}.{} differs from the last batch of previous dump",
            earlier.segment, earlier.offset
        ),
    );

    // a file names itself; a dump of another project is another repo
    let other_path = d.path().join("other.rdydump");
    let other = Rdy::init(&d.path().join("q"), "rdy-standard")
        .unwrap()
        .read()
        .dump(&other_path)
        .unwrap();
    check(
        expect_err(st.dump_after_file(&other_path, &out)),
        format!(
            "{} holds repo {}, but this project is repo {repo}",
            other_path.display(),
            other.repo_id
        ),
    );
    // a predecessor of dump format 1 or 2 holds no chain facts
    for (name, format) in [("v1-s12.rdydump", 1), ("v2-s15.rdydump", 2)] {
        let old = fixture(name);
        let was = bytes(&old);
        check(
            expect_err(st.dump_after_file(&old, &out)),
            format!(
                "{}: dump format {format} holds no chain facts; dump the project again",
                old.display()
            ),
        );
        assert_eq!(bytes(&old), was, "{name}");
    }
    // a malformed envelope is the load's damaged-dump error, not a chain error
    let cut = d.path().join("cut.rdydump");
    let whole = bytes(&full_path);
    std::fs::write(&cut, &whole[..whole.len() - 10]).unwrap();
    let e = expect_err(st.dump_after_file(&cut, &out));
    assert_eq!(e.code, E_DUMP, "{e:?}");
    assert!(
        e.message
            .starts_with(&format!("{} is damaged at byte ", cut.display())),
        "{e:?}"
    );
    let junk = d.path().join("junk");
    std::fs::write(&junk, b"not a dump at all").unwrap();
    assert_eq!(expect_err(st.dump_after_file(&junk, &out)).code, E_DUMP);
    let missing = d.path().join("nope.rdydump");
    let e = expect_err(st.dump_after_file(&missing, &out));
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(!out.exists());
    // the good predecessor works, and was never changed
    assert_eq!(bytes(&full_path), whole);
    st.dump_after_file(&full_path, &out).unwrap();
    assert_eq!(bytes(&full_path), whole);
}

/// A file reader that records the byte ranges it reads.
struct Counting {
    f: File,
    at: u64,
    reads: Vec<(u64, u64)>,
}

impl Read for Counting {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.f.read(buf)?;
        if n > 0 {
            self.reads.push((self.at, self.at + n as u64));
        }
        self.at += n as u64;
        Ok(n)
    }
}

impl Seek for Counting {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.at = self.f.seek(to)?;
        Ok(self.at)
    }
}

#[test]
fn hc4_after_file_reads_no_body() {
    let d = tempfile::tempdir().unwrap();
    let mut r = project(&d.path().join("p"));
    let full_path = d.path().join("full.rdydump");
    let full = r.read().dump(&full_path).unwrap();
    let whole = bytes(&full_path);
    // the body lies between the header record and the trailer record
    let header_len = u64::from(u32::from_le_bytes(whole[12..16].try_into().unwrap()));
    let body = 12 + 37 + header_len;
    let trailer = whole.len() as u64 - 106;
    assert!(body < trailer);
    let mut counting = Counting {
        f: File::open(&full_path).unwrap(),
        at: 0,
        reads: vec![],
    };
    assert_eq!(
        chain_end_of(&full_path, &mut counting).unwrap(),
        full.chain_end()
    );
    assert!(!counting.reads.is_empty());
    for (from, to) in &counting.reads {
        assert!(
            *to <= body || *from >= trailer,
            "read {from}..{to} of the body {body}..{trailer}"
        );
    }

    // a predecessor whose body is destroyed still names its end: the body is never read
    let mut broken = whole.clone();
    for b in &mut broken[body as usize..trailer as usize] {
        *b = 0xff;
    }
    let broken_path = d.path().join("broken.rdydump");
    std::fs::write(&broken_path, &broken).unwrap();
    assert!(rdy_engine::load::load(&broken_path, &d.path().join("t")).is_err());
    add(&mut r, 8, "more");
    let st = r.read();
    let out = d.path().join("inc.rdydump");
    let inc = st.dump_after_file(&broken_path, &out).unwrap();
    assert_eq!(inc.base, full.end);
    assert_eq!(read_all(&out).1.chain.unwrap().prev, full.digest);
    assert_eq!(bytes(&broken_path), broken);
}

#[test]
fn hc4_same_state_and_files_make_identical_increments() {
    let d = tempfile::tempdir().unwrap();
    let mut r = project(&d.path().join("p"));
    let full_path = d.path().join("full.rdydump");
    let full = r.read().dump(&full_path).unwrap();
    let pred = bytes(&full_path);
    for n in 5..8 {
        add(&mut r, n, "more");
    }
    land(&mut r);
    let st = r.read();
    let log = segments(&d.path().join("p"));
    let outs: Vec<PathBuf> = (0..3)
        .map(|i| d.path().join(format!("inc-{i}.rdydump")))
        .collect();
    let a = st.dump_after(&full.chain_end(), &outs[0]).unwrap();
    let b = st.dump_after(&full.chain_end(), &outs[1]).unwrap();
    let c = st.dump_after_file(&full_path, &outs[2]).unwrap();
    assert_eq!(bytes(&outs[0]), bytes(&outs[1]));
    assert_eq!(bytes(&outs[0]), bytes(&outs[2]));
    assert_eq!(a.chain_end(), b.chain_end());
    assert_eq!(a.chain_end(), c.chain_end());
    // the source log and the predecessor are only read
    assert_eq!(segments(&d.path().join("p")), log);
    assert_eq!(bytes(&full_path), pred);
    // a later state of the same project with the same files: the same bytes again
    let seq = st.seq();
    drop(st);
    r.close().unwrap();
    let reopened = Rdy::open(&d.path().join("p")).unwrap();
    let again = reopened.read();
    assert_eq!(again.seq(), seq);
    let later = d.path().join("inc-later.rdydump");
    again.dump_after(&full.chain_end(), &later).unwrap();
    assert_eq!(bytes(&later), bytes(&outs[0]));
}

#[test]
fn hc4_public_facts_use_hex_ids() {
    let _clock = pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let (mut r, prop, early) = removable(&d.path().join("p"));
    let full_path = d.path().join("full.rdydump");
    let full = early.dump(&full_path).unwrap();
    r.land(&approver(), &prop.id).unwrap();
    let inc_path = d.path().join("inc.rdydump");
    let inc = r.read().dump_after(&full.chain_end(), &inc_path).unwrap();
    assert!(inc.removals >= 1, "{inc:?}");
    let repo = full.repo_id.to_string();
    assert_eq!(repo.len(), 32);

    let marker = |v: &Value| {
        assert!(is_hex(&v["digest"], 64), "{v}");
        assert!(
            v["frames"].is_u64() && v["start"]["segment"].is_u64(),
            "{v}"
        );
        assert!(v["start"]["offset"].is_u64(), "{v}");
    };
    for (s, kind) in [(&full, "full"), (&inc, "increment")] {
        let v = serde_json::to_value(s).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "base",
                "batches",
                "bytes",
                "digest",
                "dump_format",
                "end",
                "end_batch",
                "files",
                "first_removed",
                "frames",
                "kind",
                "out",
                "removals",
                "repo_id",
                "store_format"
            ]
        );
        assert_eq!(v["kind"], kind);
        assert_eq!(v["repo_id"], repo.as_str());
        assert!(is_hex(&v["repo_id"], 32));
        assert!(is_hex(&v["digest"], 64));
        assert_eq!(v["digest"], s.digest.to_string());
        marker(&v["end_batch"]);
        assert!(v["out"].is_string());
    }
    assert_eq!(
        serde_json::to_value(&full).unwrap()["first_removed"],
        Value::Null
    );
    let first = &serde_json::to_value(&inc).unwrap()["first_removed"];
    assert!(
        first["segment"].is_u64() && first["offset"].is_u64(),
        "{first}"
    );

    let end = serde_json::to_value(inc.chain_end()).unwrap();
    assert_eq!(end["repo_id"], repo.as_str());
    assert!(is_hex(&end["digest"], 64));
    marker(&end["end_batch"]);
    let m = serde_json::to_value(inc.end_batch).unwrap();
    marker(&m);
    // the chain facts round-trip through JSON, as a consumer passes them back
    let back: ChainEnd = serde_json::from_value(end).unwrap();
    assert_eq!(back, inc.chain_end());

    // the format facts of a full dump, an increment and an old dump
    let f = serde_json::to_value(inspect_dump(&full_path).unwrap()).unwrap();
    assert_eq!(f["kind"], "full");
    assert_eq!(f["repo_id"], repo.as_str());
    assert_eq!(f["prev"], "0".repeat(64));
    assert_eq!(f["base"], json!({"segment": 0, "offset": 0}));
    marker(&f["base_batch"]);
    marker(&f["end_batch"]);
    assert_eq!(f["end"], serde_json::to_value(full.end).unwrap());
    let i = serde_json::to_value(inspect_dump(&inc_path).unwrap()).unwrap();
    assert_eq!(i["kind"], "increment");
    assert_eq!(i["repo_id"], repo.as_str());
    assert_eq!(i["prev"], full.digest.to_string());
    assert_eq!(i["base"], serde_json::to_value(full.end).unwrap());
    assert_eq!(
        i["base_batch"],
        serde_json::to_value(full.end_batch).unwrap()
    );
    assert_eq!(i["end_batch"], serde_json::to_value(inc.end_batch).unwrap());
    for name in ["v1-s12.rdydump", "v2-s15.rdydump"] {
        let old = inspect_dump(&fixture(name)).unwrap();
        let v = serde_json::to_value(&old).unwrap();
        for key in ["kind", "repo_id", "base", "base_batch", "prev", "end_batch"] {
            assert_eq!(v[key], Value::Null, "{name} {key}");
        }
        assert!(
            v["end"]["segment"].is_u64() && v["end"]["offset"].is_u64(),
            "{v}"
        );
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "base",
                "base_batch",
                "dump",
                "end",
                "end_batch",
                "kind",
                "prev",
                "rdy",
                "repo_id",
                "store"
            ]
        );
    }
}

/// A project holding the secret in a landed prose block, tagged `v1.0.0`, and an unlanded
/// proposal that removes it; with the state read right after the project was made, before the
/// secret was written.
fn removable(dir: &Path) -> (Rdy, ProposalInfo, Arc<ReadState>) {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    add(&mut r, 0, "a first block");
    land(&mut r);
    let early = r.read();
    let target = add(&mut r, 1, SECRET);
    for n in 2..5 {
        add(&mut r, n, "later block");
    }
    land(&mut r);
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    r.redact_in(
        &writer(),
        "redact",
        RedactInput {
            targets: vec![target],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let prop = r
        .propose(&writer(), "redact", "Remove the code", None)
        .unwrap();
    (r, prop, early)
}

#[test]
fn hc4_stale_full_and_increment_states_leave_no_file() {
    let _clock = pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let (mut r, prop, early) = removable(&d.path().join("p"));
    let full_path = d.path().join("full.rdydump");
    let full = early.dump(&full_path).unwrap();
    let was = bytes(&full_path);
    let old = r.read();
    // the old state's own end: an empty suffix after it
    let own = old.dump(&d.path().join("own.rdydump")).unwrap();
    r.land(&approver(), &prop.id).unwrap();
    let out = d.path().join("stale.rdydump");
    let stale = |e: RdyError| {
        assert_eq!(e.code, E_REDACTED_STATE, "{e:?}");
        assert_eq!(e.fix, "Read the project again, or run the dump again.");
        assert!(!out.exists());
        assert!(no_partial(d.path()));
    };
    stale(expect_err(old.dump(&out)));
    stale(expect_err(old.dump_after(&full.chain_end(), &out)));
    stale(expect_err(old.dump_after_file(&full_path, &out)));
    stale(expect_err(old.dump_after(&own.chain_end(), &out)));
    stale(expect_err(early.dump(&out)));
    stale(expect_err(early.dump_after(&full.chain_end(), &out)));
    assert_eq!(bytes(&full_path), was);
}

#[test]
fn hc4_removal_during_export_refuses_and_cleans_partial() {
    let _clock = pin_clock(1_000_000);
    for increment in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let (mut r, prop, early) = removable(&d.path().join("p"));
        let full = early.dump(&d.path().join("full.rdydump")).unwrap();
        let st = r.read();
        let out = d.path().join("out.rdydump");
        let (paused, wait) = channel::<()>();
        let (go, released) = channel::<()>();
        let prev = full.chain_end();
        let target = out.clone();
        let export = std::thread::spawn(move || {
            on_dump_batch(Some(Box::new(move |n| {
                if n == 1 {
                    paused.send(()).unwrap();
                    released.recv().unwrap();
                }
            })));
            let got = if increment {
                st.dump_after(&prev, &target)
            } else {
                st.dump(&target)
            };
            on_dump_batch(None);
            got
        });
        // the export has written its first batch and waits; the removal lands
        wait.recv().unwrap();
        r.land(&approver(), &prop.id).unwrap();
        go.send(()).unwrap();
        let e = expect_err(export.join().unwrap());
        assert_eq!(e.code, E_REDACTED_STATE, "increment {increment}: {e:?}");
        assert!(!out.exists(), "increment {increment}");
        assert!(no_partial(d.path()), "increment {increment}");
    }
}

/// The three exports whose first store read is a batch-marker walk: a full dump (the marker at
/// its own end), an increment with batches and an empty increment (the predecessor's marker).
#[derive(Clone, Copy, Debug)]
enum Lookup {
    Full,
    Increment,
    Empty,
}

#[test]
fn hc4_removal_during_a_marker_lookup_is_redacted_state() {
    let _clock = pin_clock(1_000_000);
    let mut got = Vec::new();
    for case in [Lookup::Full, Lookup::Increment, Lookup::Empty] {
        let d = tempfile::tempdir().unwrap();
        let (mut r, prop, _) = removable(&d.path().join("p"));
        // a predecessor whose end lies past the secret's frame, so its marker walk reads it
        let held_state = r.read();
        let held = held_state.dump(&d.path().join("held.rdydump")).unwrap();
        let st = match case {
            Lookup::Empty => held_state,
            Lookup::Full | Lookup::Increment => {
                add(&mut r, 9, "after the predecessor");
                r.read()
            }
        };
        let out = d.path().join("out.rdydump");
        let (paused, wait) = channel::<()>();
        let (go, released) = channel::<()>();
        let prev = held.chain_end();
        let target = out.clone();
        let export = std::thread::spawn(move || {
            // the first marker walk has read the removal spans and its first record, and no
            // later frame: it waits there
            let mut first = true;
            on_batch_walk_record(Some(Box::new(move |_| {
                if std::mem::take(&mut first) {
                    paused.send(()).unwrap();
                    released.recv().unwrap();
                }
            })));
            let got = match case {
                Lookup::Full => st.dump(&target),
                Lookup::Increment | Lookup::Empty => st.dump_after(&prev, &target),
            };
            on_batch_walk_record(None);
            got
        });
        wait.recv().unwrap();
        r.land(&approver(), &prop.id).unwrap();
        go.send(()).unwrap();
        let e = expect_err(export.join().unwrap());
        got.push((
            format!("{case:?}"),
            e.code.to_string(),
            e.fix.clone(),
            out.exists(),
            no_partial(d.path()),
        ));
    }
    let stale = |case: &str| {
        (
            case.to_string(),
            E_REDACTED_STATE.to_string(),
            "Read the project again, or run the dump again.".to_string(),
            false,
            true,
        )
    };
    assert_eq!(
        got,
        vec![stale("Full"), stale("Increment"), stale("Empty")],
        "(case, code, fix, out exists, no partial)"
    );
}

#[test]
fn hc4_fresh_dump_after_removal_loads() {
    let _clock = pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let (mut r, prop, early) = removable(&d.path().join("p"));
    let full = early.dump(&d.path().join("full.rdydump")).unwrap();
    // a full dump that holds the secret's frame, made before the removal
    let held = r.read().dump(&d.path().join("held.rdydump")).unwrap();
    r.land(&approver(), &prop.id).unwrap();
    let fresh = r.read();
    let whole = d.path().join("fresh.rdydump");
    fresh.dump(&whole).unwrap();
    rdy_engine::load::load(&whole, &d.path().join("loaded")).unwrap();
    Rdy::open(&d.path().join("loaded")).unwrap().fsck().unwrap();
    // increments of the fresh state read back whole, with their removal facts
    for (prev, name) in [(&full, "inc.rdydump"), (&held, "inc-held.rdydump")] {
        let out = d.path().join(name);
        let inc = fresh.dump_after(&prev.chain_end(), &out).unwrap();
        let (_, _, items, trailer) = read_all(&out);
        assert_eq!(batches_of(&items).len() as u64, inc.batches);
        assert_eq!(inc.removals, 1, "{inc:?}");
        assert_eq!(
            (trailer.removals, trailer.first_removed),
            (inc.removals, inc.first_removed)
        );
        assert!(
            !bytes(&out)
                .windows(SECRET.len())
                .any(|w| w == SECRET.as_bytes()),
            "{name}"
        );
    }
    let after_early = fresh
        .dump_after(&full.chain_end(), &d.path().join("again.rdydump"))
        .unwrap();
    let after_held = fresh
        .dump_after(&held.chain_end(), &d.path().join("again-held.rdydump"))
        .unwrap();
    // the removed frame lies inside the first increment, and before the second one's base
    let first = after_early.first_removed.unwrap();
    assert_eq!(after_held.first_removed, Some(first));
    assert!(first >= after_early.base, "{after_early:?}");
    assert!(first < after_held.base, "{after_held:?}");
}

/// The project files a dump on this thread reached through its reader (after their length
/// check, before their first byte), shared with the test that set the hook.
fn watch_files() -> std::rc::Rc<std::cell::RefCell<Vec<String>>> {
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let log = seen.clone();
    on_project_file(Some(Box::new(move |path: &Path| {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        log.borrow_mut().push(name);
    })));
    seen
}

#[test]
fn hc4_a_project_file_over_16_mib_is_refused_before_it_is_read_whole() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    let mut r = project(&root);
    let full = r.read().dump(&d.path().join("full.rdydump")).unwrap();
    add(&mut r, 7, "more");
    let ignore = root.join(".rdyignore");
    let config_len = std::fs::metadata(root.join(".rdy.yaml")).unwrap().len();
    let st = r.read();
    let out = d.path().join("inc.rdydump");
    let seen = watch_files();
    for len in [16 << 20, (16 << 20) + 1] {
        std::fs::write(&ignore, vec![b'#'; len]).unwrap();
        for full_dump in [false, true] {
            seen.borrow_mut().clear();
            let before = project_file_bytes();
            let got = if full_dump {
                st.dump(&out)
            } else {
                st.dump_after(&full.chain_end(), &out)
            };
            let read = project_file_bytes() - before;
            if len == 16 << 20 {
                // the reader is the path the bytes take: every carried byte, once
                let files = got.unwrap().files;
                assert!(files.contains(&".rdyignore".to_string()));
                let carried: u64 = files
                    .iter()
                    .map(|f| std::fs::metadata(root.join(f)).unwrap().len())
                    .sum();
                assert_eq!(read, carried, "full {full_dump}");
                assert!(seen.borrow().contains(&".rdyignore".to_string()));
                std::fs::remove_file(&out).unwrap();
            } else {
                let e = expect_err(got);
                assert_eq!(e.code, E_BAD_INPUT);
                assert_eq!(e.message, ".rdyignore is larger than 16 MiB");
                assert_eq!(e.fix, "make the file 16 MiB or smaller");
                assert!(!out.exists());
                assert!(no_partial(d.path()));
                // refused by its length: its reader never started, and no byte of it was read
                assert_eq!(
                    *seen.borrow(),
                    vec![".rdy.yaml".to_string()],
                    "full {full_dump}"
                );
                assert_eq!(read, config_len, "full {full_dump}");
            }
        }
    }
    on_project_file(None);
}

#[test]
fn hc4_a_project_file_that_grows_is_read_to_one_byte_past_the_limit() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    let mut r = project(&root);
    let full = r.read().dump(&d.path().join("full.rdydump")).unwrap();
    add(&mut r, 7, "more");
    let ignore = root.join(".rdyignore");
    let config_len = std::fs::metadata(root.join(".rdy.yaml")).unwrap().len();
    let st = r.read();
    let out = d.path().join("inc.rdydump");
    for full_dump in [false, true] {
        // small when its length is checked; it grows past the limit before its first byte is read
        std::fs::write(&ignore, b"# small\n").unwrap();
        let grown = (16 << 20) + (1 << 20);
        let path = ignore.clone();
        on_project_file(Some(Box::new(move |p: &Path| {
            if p == path {
                let f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
                f.set_len(grown).unwrap();
            }
        })));
        let before = project_file_bytes();
        let got = if full_dump {
            st.dump(&out)
        } else {
            st.dump_after(&full.chain_end(), &out)
        };
        let read = project_file_bytes() - before;
        on_project_file(None);
        assert_eq!(std::fs::metadata(&ignore).unwrap().len(), grown);
        let e = expect_err(got);
        assert_eq!(e.code, E_BAD_INPUT);
        assert_eq!(e.message, ".rdyignore is larger than 16 MiB");
        assert!(!out.exists());
        assert!(no_partial(d.path()));
        // the config file whole, then the grown file up to one byte past the limit
        assert_eq!(read, config_len + 16_777_217, "full {full_dump}");
    }
}
