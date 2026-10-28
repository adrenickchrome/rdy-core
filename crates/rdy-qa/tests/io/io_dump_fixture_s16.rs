//! hc9: the dump promise for dump format 3 and store format 16 (D-69AY3: "every later rdy reads
//! every dump format that rdy has released"; D-82K89; D-QF2HQ; quality gate `p-KFV8S`,
//! `p-83RHT`, `n-EP5QM`, `tb-A4DAV`). The pair `(3, 16)` is released with twelve files and this
//! file. Write them once: never rewrite them, and never regenerate them to fix a later reader.
//!
//! - `v3-s16-chain-full`, `-chain-1` and `-chain-2` are a full dump and two increments of one
//!   project (`rdy_qa::chain_fixture::project_chain`, from the every-kind project). The second
//!   increment holds two approved removals: one of bytes that the earlier files hold, one of a
//!   block first written in that increment. Two fixed stops before the first removal's apply
//!   (`held_stop`, a landing; `missing_stop`, a batch end) load with a carried record of held
//!   spans and one of missing spans.
//! - `v3-s16` is the standalone full fixture: a dump of the full dump written after those
//!   removals, loaded at `missing_stop` (every carried span is zero), then written to again: a new
//!   block, a tagged landing and a person's approved removal of that block. It holds every kind of
//!   store format 16, the carried record (kind 23) among them, and its stop (`missing_stop`, a tag)
//!   before its own apply appends a new carried record.
//! - `v3-s16-holder-chain-*` and `v3-s16-holder` are the same for a holder, filled by takes of the
//!   every-kind holder's uploads: no canon, no project file, divergent tags, the relay answer's
//!   `answer/tag-object:<hash>` ref, and a removal approved in one source and taken at the
//!   holder's local places. The standalone holder takes the matching apply again, which ends its
//!   warning.
//!
//! Every expect file pins what a load of its files must give: per file its chain and trailer
//! facts, the source log's segment digests, its batch and frame counts, every original key, the
//! identity, the tags, views and landings, each tag's projection, the removal and carried facts,
//! and each fixed stop's position, warnings and appended carried batch, byte for byte. A later
//! store format adds kinds and refs, never re-encodes (D-F0904): a load then appends one upgrade
//! batch after the source log or the carried batch, and the tests allow exactly that batch.
use crate::io_dump_fixture_s15::{first_start_logs_moves, fixture, without_moves};
use rdy_engine::dump_stop::DumpStop;
use rdy_engine::load::{LoadSummary, load_chain};
use rdy_engine::{Rdy, RdyError, WriteRequest};
use rdy_model::Hash;
use rdy_model::redaction::RedactionCarried;
use rdy_qa::chain_fixture::{self, V16_KINDS};
use rdy_qa::every_kind;
use rdy_store::dump::{DUMP_FORMAT, DumpItem, DumpReader, read_envelope};
use rdy_store::format::{
    DumpStore, LAST_SCALAR_STORE_FORMAT, REF_STORE_FEATURES, baseline_features,
};
use rdy_store::holder::{ANSWER_TAG_OBJECT_REF, REF_STORE_MODE};
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::redaction_carried::{HOLDER_FIX, PROJECT_FIX};
use rdy_store::segment::{Location, on_segment_write, seg_path};
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const FULL: &str = "v3-s16.rdydump";
const FULL_EXPECT: &str = "v3-s16.expect.json";
const CHAIN_FULL: &str = "v3-s16-chain-full.rdydump";
const CHAIN_1: &str = "v3-s16-chain-1.rdydump";
const CHAIN_2: &str = "v3-s16-chain-2.rdydump";
const CHAIN_EXPECT: &str = "v3-s16-chain.expect.json";
const HOLDER: &str = "v3-s16-holder.rdydump";
const HOLDER_EXPECT: &str = "v3-s16-holder.expect.json";
const HOLDER_CHAIN_FULL: &str = "v3-s16-holder-chain-full.rdydump";
const HOLDER_CHAIN_1: &str = "v3-s16-holder-chain-1.rdydump";
const HOLDER_CHAIN_2: &str = "v3-s16-holder-chain-2.rdydump";
const HOLDER_CHAIN_EXPECT: &str = "v3-s16-holder-chain.expect.json";

/// Each member of the pair `(3, 16)` and its blake3, set when it was written. A change here
/// needs a founder decision.
pub(crate) const MEMBERS: [(&str, &str); 12] = [
    (
        FULL,
        "5de4badbd1e5692e12b8465f06fce5f6cea1f42fd619a18cde756584c792c2b5",
    ),
    (
        FULL_EXPECT,
        "0df206615c985815d676a5ca91efd865899209464f06f575ea235976425e6232",
    ),
    (
        CHAIN_FULL,
        "7deafdc1a6b2d42a5f71165ca2961d7f32c9c8f4f2a0012a1798d230d1935a62",
    ),
    (
        CHAIN_1,
        "23f64d4625407f257e5a0ceb55a133c7472188f21a2ea78e291e49fd02d9069c",
    ),
    (
        CHAIN_2,
        "d8735bd462d05edf2d371bed9cf56ce3ca04080fd06882b5e1a02d3e2e2b8111",
    ),
    (
        CHAIN_EXPECT,
        "2b00f08d44c55b74e6e97375f52acd9e6ed9458ab15790bb31fd6d391a05b615",
    ),
    (
        HOLDER,
        "dc4ca5b0fcce7e097db9ecf64196ce00a26ad6c0233df11341a578f8d9e7f26e",
    ),
    (
        HOLDER_EXPECT,
        "c188d8ff41afdb9ecd2e6a172a20e285aef89826dcf3442bab9730ce446b1372",
    ),
    (
        HOLDER_CHAIN_FULL,
        "bf1a88b6d553393f34ad812131bdcb4497347c5a1ec16273a8a8e86c2565f300",
    ),
    (
        HOLDER_CHAIN_1,
        "f4069d58ca2dc9705f8b95cf1ae1d5795edf8af5a87ba8a30cc01289debf4262",
    ),
    (
        HOLDER_CHAIN_2,
        "3aa9e3303fe228c02b79e3e219494992a7f4b6250e602bf0bcaea6f869150057",
    ),
    (
        HOLDER_CHAIN_EXPECT,
        "369da6d39efc8aeb46369ed36aff7b52e5f5b20f6ba22d0c73531639159bef80",
    ),
];

/// The blake3 of member `name`, as [`MEMBERS`] pins it.
pub(crate) fn digest_of(name: &str) -> &'static str {
    MEMBERS
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("{name} is no member"))
        .1
}

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};
const SEGMENT_MAX: u64 = chain_fixture::SEGMENT_MAX;

fn chain_files() -> Vec<PathBuf> {
    [CHAIN_FULL, CHAIN_1, CHAIN_2].map(fixture).to_vec()
}

fn holder_chain_files() -> Vec<PathBuf> {
    [HOLDER_CHAIN_FULL, HOLDER_CHAIN_1, HOLDER_CHAIN_2]
        .map(fixture)
        .to_vec()
}

fn read_expect(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture(name)).unwrap()).unwrap()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn loc(l: Location) -> Value {
    json!([l.segment, l.offset])
}

fn loc_of(v: &Value) -> Location {
    Location {
        segment: v[0].as_u64().unwrap().try_into().unwrap(),
        offset: v[1].as_u64().unwrap(),
    }
}

fn open(dir: &Path, holder: bool) -> Rdy {
    if holder {
        Rdy::open_holder(dir).unwrap()
    } else {
        Rdy::open(dir).unwrap()
    }
}

fn chain_load(files: &[PathBuf], to: &Path, stop: Option<&str>) -> LoadSummary {
    let stop = stop.map(|s| DumpStop::parse(s).unwrap());
    load_chain(files, to, stop.as_ref()).unwrap_or_else(|e| panic!("the load failed: {e:?}"))
}

fn chain_err(files: &[PathBuf], to: &Path, stop: Option<&str>) -> RdyError {
    let stop = stop.map(|s| DumpStop::parse(s).unwrap());
    match load_chain(files, to, stop.as_ref()) {
        Ok(s) => panic!("the load passed: {s:?}"),
        Err(e) => e,
    }
}

// ---------------------------------------------------------------------------------------------
// facts

/// What the dump file `p` holds, by its envelope and a whole read: its formats, chain facts,
/// trailer, length, batch and frame counts, project file names, each batch's start and its
/// dropped frames' count. Its own name is `name`.
fn file_facts(name: &str, p: &Path) -> Value {
    let mut f = std::fs::File::open(p).unwrap();
    let (format, header, trailer, len) = read_envelope(&mut f).unwrap();
    let (mut rd, _, _) = DumpReader::open(BufReader::new(std::fs::File::open(p).unwrap())).unwrap();
    let (mut batches, mut frames, mut dropped) = (0u64, 0u64, 0u64);
    let (mut files, mut starts) = (vec![], vec![]);
    while let Some(item) = rd.next().unwrap() {
        match item {
            DumpItem::File { path, .. } => files.push(path),
            DumpItem::Batch(b) => {
                batches += 1;
                frames += b.frames.len() as u64;
                dropped += b
                    .frames
                    .iter()
                    .filter(|f| f.present_payload().is_none())
                    .count() as u64;
                starts.push(loc(b.start));
            }
        }
    }
    json!({
        "name": name,
        "dump_format": format,
        "store_format": legacy(&header.store),
        "rdy": header.rdy,
        "log_end": loc(header.log_end),
        "chain": header.chain,
        "trailer": {
            "batches": trailer.batches,
            "frames": trailer.frames,
            "files": trailer.files,
            "removals": trailer.removals,
            "first_removed": trailer.first_removed.map(loc),
            "digest": trailer.digest.to_string(),
        },
        "len": len,
        "batches": batches,
        "frames": frames,
        "dropped": dropped,
        "files": files,
        "batch_starts": starts,
    })
}

/// The blake3 of each segment file of the store under `project`, up to `end`: whole for the
/// segments before `end`'s, the prefix before `end` for its own.
fn segment_digests(project: &Path, end: Location) -> Value {
    let dir = project.join(".rdy/store/segments");
    let mut out = serde_json::Map::new();
    for seg in 0..=end.segment {
        let p = seg_path(&dir, seg);
        let bytes = std::fs::read(&p).unwrap();
        let bytes = if seg == end.segment {
            &bytes[..usize::try_from(end.offset).unwrap()]
        } else {
            &bytes[..]
        };
        out.insert(
            p.file_name().unwrap().to_string_lossy().into_owned(),
            json!(Hash::of(bytes).to_string()),
        );
    }
    Value::Object(out)
}

/// The bytes of the log of the store under `project` from `from` to `to`, in `from`'s segment;
/// with no `to`, to the log's end, which must be in that segment.
fn tail_bytes(project: &Path, from: Location, to: Option<Location>) -> Vec<u8> {
    let dir = project.join(".rdy/store/segments");
    let bytes = std::fs::read(seg_path(&dir, from.segment)).unwrap();
    let start = usize::try_from(from.offset).unwrap();
    match to {
        Some(to) => {
            assert_eq!(
                to.segment, from.segment,
                "the tail stays in the stop's segment"
            );
            let end = usize::try_from(to.offset).unwrap();
            assert!(end <= bytes.len(), "the log ends before {to:?}");
            bytes[start..end].to_vec()
        }
        None => {
            assert!(
                !seg_path(&dir, from.segment + 1).exists(),
                "the tail stays in the stop's segment"
            );
            bytes[start..].to_vec()
        }
    }
}

/// Every removal record and dropped frame of the log of `r` before `upto`: each dropped frame
/// with its place, original kind, hash and length and the record that authorizes its absence,
/// each request and apply with its place, and each carried record with its place and record.
fn removal_facts(r: &Rdy, upto: Location) -> Value {
    let (mut dropped, mut requests, mut applies, mut carried) = (vec![], vec![], vec![], vec![]);
    r.store()
        .reader()
        .for_each_record_from(ORIGIN, upto, |at, f, _| {
            match f {
                StoredFrame::Dropped {
                    kind,
                    hash,
                    len,
                    apply,
                    residue,
                } => dropped.push(json!({
                    "at": loc(at),
                    "kind": kind,
                    "hash": hash.to_string(),
                    "len": len,
                    "authority": apply.to_string(),
                    "residue": residue.map(|h| h.to_string()),
                })),
                StoredFrame::Present {
                    kind: kinds::REDACTION,
                    hash,
                    ..
                } => requests.push(json!({"at": loc(at), "hash": hash.to_string()})),
                StoredFrame::Present {
                    kind: kinds::REDACTION_APPLY,
                    hash,
                    ..
                } => applies.push(json!({"at": loc(at), "hash": hash.to_string()})),
                StoredFrame::Present {
                    kind: kinds::REDACTION_CARRIED,
                    hash,
                    payload,
                } => carried.push(carried_facts(at, hash, payload)),
                StoredFrame::Present { .. } => {}
            }
            Ok(())
        })
        .unwrap();
    json!({
        "dropped": dropped,
        "requests": requests,
        "applies": applies,
        "carried": carried,
    })
}

/// A carried record at `at`: its hash, its record, and its zero spans' bytes.
fn carried_facts(at: Location, hash: Hash, payload: &[u8]) -> Value {
    let record: RedactionCarried = rdy_store::bounded::decode(payload).unwrap();
    let zero: u64 = record
        .spans
        .iter()
        .filter(|s| !s.held)
        .map(|s| u64::from(s.len))
        .sum();
    json!({
        "at": loc(at),
        "hash": hash.to_string(),
        "record": record,
        "zero_bytes": zero,
    })
}

/// Canon's blocks of a project: each with its version and whether it reads as removed.
fn canon_blocks(r: &Rdy) -> Value {
    let st = r.read();
    let Some(head) = st.canon_head() else {
        return Value::Null;
    };
    let m = st.materialized(&rdy_engine::Scope::At(head)).unwrap();
    let mut out = serde_json::Map::new();
    for (id, v) in m.versions() {
        let b = st.reader().get_block(v).unwrap();
        out.insert(
            id.to_string(),
            json!({"version": v.to_string(), "redacted": b.is_redacted()}),
        );
    }
    Value::Object(out)
}

/// What a later rdy must read the same of the store `r`, up to `upto`, the end of the log it
/// loaded (before any upgrade batch): its refs, kinds, landings, fsck, signatures, identity,
/// views, each tag's projection, canon's blocks and the removal facts.
fn store_facts(r: &Rdy, upto: Location) -> Value {
    json!({
        "facts": every_kind::facts(r),
        "identity": every_kind::identity_facts(r, Some(upto)),
        "views": r.read().view_names(),
        "projections": every_kind::projections(r),
        "canon_blocks": canon_blocks(r),
        "removals": removal_facts(r, upto),
    })
}

/// A load's summary, without its target path and its file paths.
fn summary_facts(s: &LoadSummary) -> Value {
    json!({
        "dump_format": s.dump_format,
        "store_format": legacy(&s.store_format),
        "batches": s.batches,
        "frames": s.frames,
        "files_written": s.files_written,
        "files_kept": s.files_kept,
        "landings": s.landings,
        "tags": s.tags,
        "views": s.views,
        "chain": s.chain.iter().map(|c| json!({"kind": c.kind, "base": loc(c.base), "end": loc(c.end)})).collect::<Vec<_>>(),
        "stopped_at": s.stopped_at,
        "warnings": s.warnings,
    })
}

/// The end of the log `files` hold: the last file's header end.
fn chain_end(files: &[PathBuf]) -> Location {
    let mut f = std::fs::File::open(files.last().unwrap()).unwrap();
    read_envelope(&mut f).unwrap().1.log_end
}

/// A whole load of `files` into `to`: its summary, the loaded log's segment digests up to the
/// source end, and the store's facts.
fn whole_facts(files: &[PathBuf], to: &Path, holder: bool) -> Value {
    let s = chain_load(files, to, None);
    whole_facts_of(&s, files, to, holder)
}

/// The facts of the whole load of `files` into `to` that gave `s`.
fn whole_facts_of(s: &LoadSummary, files: &[PathBuf], to: &Path, holder: bool) -> Value {
    let end = chain_end(files);
    let r = open(to, holder);
    let v = json!({
        "end": loc(end),
        "summary": summary_facts(s),
        "segments": segment_digests(to, end),
        "store": store_facts(&r, end),
    });
    r.close().unwrap();
    v
}

/// A load of `files` stopped at `selector` into `to`: the selector, the summary, the bytes this rdy
/// appended after the stop (the carried batch), the fsck warnings and the store's facts up to the
/// carried batch's end.
fn stop_facts(files: &[PathBuf], selector: &str, to: &Path, holder: bool) -> Value {
    let s = chain_load(files, to, Some(selector));
    stop_facts_of(&s, selector, to, holder, None)
}

/// The facts of the load into `to`, stopped at `selector`, that gave `s`. `end` is the end of
/// the carried batch as the expect file pins it: the tail and the store's facts stop there, and
/// any upgrade batch after it is checked apart (`upgrade_after`). With no `end` (the writer, of
/// this rdy's own store format), the carried batch ends at the log's end.
fn stop_facts_of(
    s: &LoadSummary,
    selector: &str,
    to: &Path,
    holder: bool,
    end: Option<Location>,
) -> Value {
    let at = s.stopped_at.as_ref().unwrap().position;
    let tail = tail_bytes(to, at, end);
    let end = Location {
        segment: at.segment,
        offset: at.offset + tail.len() as u64,
    };
    let r = open(to, holder);
    let v = json!({
        "selector": selector,
        "position": loc(at),
        "end": loc(end),
        "summary": summary_facts(s),
        "tail": hex(&tail),
        "fsck_warnings": r.fsck().unwrap().warnings,
        "store": store_facts(&r, end),
    });
    r.close().unwrap();
    v
}

/// The expect file of a chain: its files' facts, the whole load, the full dump alone, and the two
/// fixed stops.
fn chain_expect(
    files: &[PathBuf],
    names: [&str; 3],
    c: &chain_fixture::Chain,
    scratch: &Path,
    holder: bool,
) -> Value {
    json!({
        "segment_max": SEGMENT_MAX,
        "files": files.iter().zip(names).map(|(p, n)| file_facts(n, p)).collect::<Vec<_>>(),
        "whole": whole_facts(files, &scratch.join("whole"), holder),
        "full_alone": whole_facts(&files[..1], &scratch.join("full-alone"), holder),
        "held_stop": stop_facts(files, &c.held_stop, &scratch.join("held"), holder),
        "missing_stop": stop_facts(files, &c.missing_stop, &scratch.join("missing"), holder),
    })
}

/// The expect file of a standalone full fixture: its facts, the whole load, the stop the source
/// chain was loaded at to build it, and its own stop before its later apply.
fn standalone_expect(
    file: &Path,
    name: &str,
    built_from: &str,
    stop: &str,
    scratch: &Path,
    holder: bool,
) -> Value {
    let files = vec![file.to_path_buf()];
    json!({
        "segment_max": SEGMENT_MAX,
        "file": file_facts(name, file),
        "built_from": built_from,
        "whole": whole_facts(&files, &scratch.join("standalone-whole"), holder),
        "missing_stop": stop_facts(&files, stop, &scratch.join("standalone-stop"), holder),
    })
}

/// Creates `p` and writes `bytes` to it; refuses when `p` exists.
fn write_new(p: &Path, bytes: &[u8]) {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)
        .unwrap_or_else(|e| panic!("{}: never rewrite a fixture member: {e}", p.display()));
    f.write_all(bytes).unwrap();
    f.sync_all().unwrap();
}

fn pretty(v: &Value) -> Vec<u8> {
    (serde_json::to_string_pretty(v).unwrap() + "\n").into_bytes()
}

/// Writes the twelve members of the pair `(3, 16)` once. Run by hand, never by the gate:
/// `cargo test -p rdy-qa --release --test io hc9_write_format_three_fixtures_once -- --ignored`.
#[test]
#[ignore]
fn hc9_write_format_three_fixtures_once() {
    for (m, _) in MEMBERS {
        assert!(
            !fixture(m).exists(),
            "{m}: the v3-s16 fixtures are written once: never rewrite them"
        );
    }
    assert_eq!(
        (DUMP_FORMAT, LAST_SCALAR_STORE_FORMAT),
        (3, 16),
        "the v3-s16 fixtures hold dump format 3 and store format 16"
    );
    let scratch = tempfile::tempdir().unwrap();
    let out = build_members(scratch.path());
    assert_eq!(out.len(), MEMBERS.len());
    for (name, bytes) in &out {
        write_new(&fixture(name), bytes);
        eprintln!("{name} blake3 {}", Hash::of(bytes));
    }
}

/// Builds the twelve members in memory, from the builders of `rdy_qa::chain_fixture`; scratch
/// stores go under `scratch`.
fn build_members(scratch: &Path) -> Vec<(&'static str, Vec<u8>)> {
    let mut out: Vec<(&str, Vec<u8>)> = vec![];

    let c = chain_fixture::project_chain();
    let s = chain_fixture::project_standalone(&c);
    let d = scratch.join("project");
    std::fs::create_dir_all(&d).unwrap();
    let expect = chain_expect(&c.files(), [CHAIN_FULL, CHAIN_1, CHAIN_2], &c, &d, false);
    for (name, p) in [
        (CHAIN_FULL, &c.full),
        (CHAIN_1, &c.inc1),
        (CHAIN_2, &c.inc2),
    ] {
        out.push((name, std::fs::read(p).unwrap()));
    }
    out.push((CHAIN_EXPECT, pretty(&expect)));
    let expect = standalone_expect(&s.dump, FULL, &c.missing_stop, &s.stop, &d, false);
    out.push((FULL, std::fs::read(&s.dump).unwrap()));
    out.push((FULL_EXPECT, pretty(&expect)));

    let (hc, ups) = chain_fixture::holder_chain();
    let hs = chain_fixture::holder_standalone(&hc, &ups);
    let d = scratch.join("holder");
    std::fs::create_dir_all(&d).unwrap();
    let names = [HOLDER_CHAIN_FULL, HOLDER_CHAIN_1, HOLDER_CHAIN_2];
    let expect = chain_expect(&hc.files(), names, &hc, &d, true);
    for (name, p) in names.into_iter().zip(hc.files()) {
        out.push((name, std::fs::read(p).unwrap()));
    }
    out.push((HOLDER_CHAIN_EXPECT, pretty(&expect)));
    let expect = standalone_expect(&hs.dump, HOLDER, &hc.missing_stop, &hs.stop, &d, true);
    out.push((HOLDER, std::fs::read(&hs.dump).unwrap()));
    out.push((HOLDER_EXPECT, pretty(&expect)));

    out
}

// ---------------------------------------------------------------------------------------------
// the checks

/// One frozen input of the pair `(3, 16)`: its files in order, their names, its expect file, its
/// mode, its fixed stops, and the canaries that only removed payloads hold.
struct Input {
    names: &'static [&'static str],
    expect: &'static str,
    holder: bool,
    stops: &'static [&'static str],
    canaries: &'static [&'static [u8]],
}

impl Input {
    fn files(&self) -> Vec<PathBuf> {
        self.names.iter().map(|n| fixture(n)).collect()
    }
}

const PROJECT_CANARIES: &[&[u8]] = &[
    chain_fixture::EARLY_CANARY.as_bytes(),
    chain_fixture::LATE_CANARY.as_bytes(),
    chain_fixture::BLOB_CANARY,
    chain_fixture::STANDALONE_CANARY.as_bytes(),
    every_kind::CANARY_BODY.as_bytes(),
    every_kind::CANARY_FIELD.as_bytes(),
];
const HOLDER_CANARIES: &[&[u8]] = &[
    every_kind::HOLDER_CANARY.as_bytes(),
    every_kind::HOLDER_BLOB,
];

const PROJECT_CHAIN: Input = Input {
    names: &[CHAIN_FULL, CHAIN_1, CHAIN_2],
    expect: CHAIN_EXPECT,
    holder: false,
    stops: &["held_stop", "missing_stop"],
    canaries: PROJECT_CANARIES,
};
const HOLDER_CHAIN: Input = Input {
    names: &[HOLDER_CHAIN_FULL, HOLDER_CHAIN_1, HOLDER_CHAIN_2],
    expect: HOLDER_CHAIN_EXPECT,
    holder: true,
    stops: &["held_stop", "missing_stop"],
    canaries: HOLDER_CANARIES,
};
const PROJECT_FULL: Input = Input {
    names: &[FULL],
    expect: FULL_EXPECT,
    holder: false,
    stops: &["missing_stop"],
    canaries: PROJECT_CANARIES,
};
const HOLDER_FULL: Input = Input {
    names: &[HOLDER],
    expect: HOLDER_EXPECT,
    holder: true,
    stops: &["missing_stop"],
    canaries: HOLDER_CANARIES,
};
const INPUTS: [Input; 4] = [PROJECT_CHAIN, HOLDER_CHAIN, PROJECT_FULL, HOLDER_FULL];

/// Each segment file of the store under `project`, by name.
fn segs(project: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(project.join(".rdy/store/segments")).unwrap() {
        let e = e.unwrap();
        out.insert(
            e.file_name().to_string_lossy().into_owned(),
            std::fs::read(e.path()).unwrap(),
        );
    }
    out
}

fn holds(project: &Path, needle: &[u8]) -> bool {
    segs(project)
        .values()
        .any(|b| b.windows(needle.len()).any(|w| w == needle))
}

/// `want`, the facts of a store as the expect file pins them, as this rdy must read them: with
/// this rdy's store format, and with the frames of `upgrade` upgrade batch data frames counted.
fn as_read_now(want: &Value, upgrade: u64) -> Value {
    let mut want = want.clone();
    want["facts"]["refs"][REF_STORE_FEATURES] = json!(hex(&baseline_features().to_json()));
    if upgrade > 0 {
        let n = &mut want["facts"]["objects"][kinds::REF_UPDATE.to_string()];
        *n = json!(n.as_u64().unwrap() + upgrade);
        let fsck = want["facts"]["fsck"].as_object_mut().unwrap();
        let n = &mut fsck["frames"];
        *n = json!(n.as_u64().unwrap() + upgrade);
        // a redaction count: in the report only while a redaction is active, and kept absent
        // when it is not
        if let Some(n) = fsck.get_mut("checked_payloads") {
            *n = json!(n.as_u64().unwrap() + upgrade);
        }
    }
    want
}

/// Checks the loaded store facts `got` against the expect file's `want`, member by member.
fn check_store(got: &Value, want: &Value, upgrade: u64, what: &str) {
    let want = as_read_now(want, upgrade);
    for member in [
        "identity",
        "views",
        "projections",
        "canon_blocks",
        "removals",
    ] {
        assert_eq!(got[member], want[member], "{what}: {member}");
    }
    for member in ["refs", "objects", "landings", "signatures", "fsck"] {
        // the fsck's unlogged move count is analysis, checked by the first writable start
        // (`first_start_logs_moves`)
        assert_eq!(
            without_moves(&got["facts"][member]),
            without_moves(&want["facts"][member]),
            "{what}: {member}"
        );
    }
}

/// The data frames of the upgrade batch after `end`, for fixtures of store format `format`: none
/// while this rdy writes that format.
fn upgrade_after(dir: &Path, end: Location, holder: bool, format: u32) -> u64 {
    let r = open(dir, holder);
    let n = crate::io_dump_fixture_s15::upgrade_tail(&r, end, format);
    r.close().unwrap();
    n
}

/// Checks the facts of a whole load (`got`) against the expect file's `want`.
fn check_whole(got: &Value, want: &Value, upgrade: u64, what: &str) {
    for member in ["end", "summary", "segments"] {
        assert_eq!(got[member], want[member], "{what}: {member}");
    }
    check_store(&got["store"], &want["store"], upgrade, what);
}

/// Checks the facts of a stopped load (`got`, read up to the pinned end of its carried batch)
/// against the expect file's `want`. The bytes from the stop to that end are the pinned carried
/// batch; `upgrade_after` checks the at most one upgrade batch after it.
fn check_stop(got: &Value, want: &Value, upgrade: u64, what: &str) {
    for member in ["selector", "position", "end", "summary", "fsck_warnings"] {
        assert_eq!(got[member], want[member], "{what}: {member}");
    }
    assert_eq!(
        got["tail"], want["tail"],
        "{what}: the carried batch, byte for byte"
    );
    check_store(&got["store"], &want["store"], upgrade, what);
}

/// A load of `files` into `to` stopped at the expect file's stop `want`, checked as this rdy or,
/// when `later`, as a later one (`as_a_later_rdy`): its facts up to the pinned end of its carried
/// batch, and the data frames of the upgrade batch after that end.
fn stopped(files: &[PathBuf], want: &Value, to: &Path, holder: bool, later: bool) -> (Value, u64) {
    let selector = want["selector"].as_str().unwrap();
    let s = chain_load(files, to, Some(selector));
    let format = if later {
        as_a_later_rdy(to, holder)
    } else {
        16
    };
    let end = loc_of(&want["end"]);
    let got = stop_facts_of(&s, selector, to, holder, Some(end));
    (got, upgrade_after(to, end, holder, format))
}

#[test]
fn hc9_fixtures_are_never_rewritten() {
    // A change to any member needs a founder decision. The checksums are fixed here, apart from
    // any writer: a later writer that writes other bytes changes nothing in this check.
    for (name, digest) in MEMBERS {
        let bytes = std::fs::read(fixture(name)).unwrap();
        assert_eq!(Hash::of(&bytes).to_string(), digest, "{name}");
    }
}

#[test]
fn hc9_full_and_three_file_chains_load_whole() {
    let d = tempfile::tempdir().unwrap();
    for input in &INPUTS {
        let what = input.expect;
        let expect = read_expect(input.expect);
        let files = input.files();
        // each file's envelope and body, read without a load
        let facts: Vec<Value> = files
            .iter()
            .zip(input.names)
            .map(|(p, n)| file_facts(n, p))
            .collect();
        if files.len() == 1 {
            assert_eq!(facts[0], expect["file"], "{what}");
        } else {
            assert_eq!(json!(facts), expect["files"], "{what}");
        }
        for (i, f) in facts.iter().enumerate() {
            assert_eq!(
                (f["dump_format"].as_u64(), f["store_format"].as_u64()),
                (Some(3), Some(16))
            );
            let chain = &f["chain"];
            if i == 0 {
                assert_eq!(chain["kind"], "full", "{what}");
                assert_eq!(chain["prev"], json!(Hash([0; 32]).to_string()));
            } else {
                // each increment follows the file before it: its end, its last batch and its
                // trailer digest
                let prev = &facts[i - 1];
                assert_eq!(chain["kind"], "increment", "{what}");
                assert_eq!(chain["repo_id"], prev["chain"]["repo_id"]);
                assert_eq!(
                    json!([chain["base"]["segment"], chain["base"]["offset"]]),
                    prev["log_end"]
                );
                assert_eq!(chain["base_batch"], prev["chain"]["end_batch"]);
                assert_eq!(chain["prev"], prev["trailer"]["digest"]);
            }
            assert_eq!(f["batches"], f["trailer"]["batches"]);
            assert_eq!(f["frames"], f["trailer"]["frames"]);
        }
        // the whole load: the source log, byte for byte up to its end, then at most an upgrade.
        // A payload an earlier file holds intact and a later file's apply removes is written as
        // zeros from the start: no segment write ever holds a removed payload, even one that the
        // load would erase before it ends
        let to = d.path().join(format!("{what}-whole"));
        let w = watch(input.canaries.to_vec());
        let s = chain_load(&files, &to, None);
        assert!(
            !w.0.get(),
            "{what}: a write held a payload removed in the chain"
        );
        drop(w);
        let got = whole_facts_of(&s, &files, &to, input.holder);
        let end = chain_end(&files);
        let upgrade = upgrade_after(&to, end, input.holder, 16);
        check_whole(&got, &expect["whole"], upgrade, what);
        assert!(
            got["summary"]["warnings"] == json!([]),
            "{what}: no new carried record"
        );
        for c in input.canaries {
            assert!(!holds(&to, c), "{what}: a removed payload is in the store");
        }
        // every frame of every file under its own kind and hash
        let r = open(&to, input.holder);
        let mut frames = 0;
        for p in &files {
            let (mut rd, _, _) =
                DumpReader::open(BufReader::new(std::fs::File::open(p).unwrap())).unwrap();
            while let Some(item) = rd.next().unwrap() {
                if let DumpItem::Batch(b) = item {
                    for f in &b.frames {
                        frames += 1;
                        assert!(r.store().has_kind(f.kind(), f.hash()).unwrap(), "{what}");
                    }
                }
            }
        }
        assert_eq!(json!(frames), expect_frames(&expect), "{what}");
        // the first writable start logs the moves content-2 leaves, then the logged invariant
        // holds
        r.close().unwrap();
        first_start_logs_moves(&to, input.holder, what);
        let r = open(&to, input.holder);
        if input.holder {
            r.fsck().unwrap();
            r.close().unwrap();
        } else {
            rdy_qa::invariants::check_all(&r).unwrap_or_else(|v| panic!("{what}: {v:#?}"));
            r.close().unwrap();
            rdy_qa::reindex_check::reindex_reproduces(&to).unwrap_or_else(|v| panic!("{v:#?}"));
        }
        // a chain's full dump alone: the prefix, which still holds the payloads removed later
        if files.len() > 1 {
            let to = d.path().join(format!("{what}-alone"));
            let got = whole_facts(&files[..1], &to, input.holder);
            let upgrade = upgrade_after(&to, chain_end(&files[..1]), input.holder, 16);
            check_whole(&got, &expect["full_alone"], upgrade, what);
            assert!(
                holds(&to, input.canaries[0]),
                "{what}: the full dump predates the removal"
            );
            assert_eq!(got["store"]["removals"]["carried"], json!([]));
        }
    }
    // the standalone fixtures hold every kind of store format 16, the carried record among
    // them, and their removals are counted in their trailers
    let expect = read_expect(FULL_EXPECT);
    let objects = &expect["whole"]["store"]["facts"]["objects"];
    for k in V16_KINDS {
        assert!(
            objects[k.to_string()].as_u64().unwrap_or(0) >= 1,
            "kind {k}"
        );
    }
    assert!(objects.get("12").is_none(), "kind 12 is unassigned");
    for name in [FULL_EXPECT, HOLDER_EXPECT] {
        let expect = read_expect(name);
        let carried = expect["whole"]["store"]["removals"]["carried"]
            .as_array()
            .unwrap();
        assert!(!carried.is_empty(), "{name}: the carried input");
        for c in carried {
            for s in c["record"]["spans"].as_array().unwrap() {
                assert_eq!(s["held"], false, "{name}: the carried input is all zero");
            }
        }
        assert_eq!(expect["whole"]["summary"]["warnings"], json!([]));
        assert!(expect["file"]["trailer"]["removals"].as_u64().unwrap() >= 1);
    }
}

/// The frames of the expect file's files: their sum.
fn expect_frames(expect: &Value) -> Value {
    match expect.get("files") {
        Some(fs) => json!(
            fs.as_array()
                .unwrap()
                .iter()
                .map(|f| f["frames"].as_u64().unwrap())
                .sum::<u64>()
        ),
        None => expect["file"]["frames"].clone(),
    }
}

/// A write observer for this thread that records whether any segment write held one of
/// `needles`. Dropping the guard removes it.
struct Watch(Rc<Cell<bool>>);

impl Drop for Watch {
    fn drop(&mut self) {
        on_segment_write(None);
    }
}

fn watch(needles: Vec<&'static [u8]>) -> Watch {
    let seen = Rc::new(Cell::new(false));
    let s = seen.clone();
    on_segment_write(Some(Box::new(move |b: &[u8]| {
        if needles.iter().any(|n| b.windows(n.len()).any(|w| w == *n)) {
            s.set(true);
        }
        Ok(())
    })));
    Watch(seen)
}

/// The carried records of a stop's appended batch, as the expect file pins them.
fn tail_carried(stop: &Value) -> Vec<Value> {
    let end = loc_of(&stop["position"]);
    stop["store"]["removals"]["carried"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| loc_of(&c["at"]) >= end)
        .cloned()
        .collect()
}

fn helds(c: &Value) -> Vec<bool> {
    c["record"]["spans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["held"].as_bool().unwrap())
        .collect()
}

#[test]
fn hc9_fixed_stops_before_removal_load_and_warn() {
    let d = tempfile::tempdir().unwrap();
    for input in &INPUTS {
        let expect = read_expect(input.expect);
        let files = input.files();
        for name in input.stops {
            let what = format!("{}: {name}", input.expect);
            let want = &expect[*name];
            let to = d.path().join(format!("{}-{name}", input.expect));
            // the payloads that stay removed at the stop are never written
            let missing: Vec<&'static [u8]> = input
                .canaries
                .iter()
                .copied()
                .filter(|c| !stop_holds(want, input, c))
                .collect();
            let w = watch(missing.clone());
            let (got, upgrade) = stopped(&files, want, &to, input.holder, false);
            assert!(
                !w.0.get(),
                "{what}: a write held a payload removed at the stop"
            );
            drop(w);
            for c in &missing {
                assert!(!holds(&to, c), "{what}");
            }
            check_stop(&got, want, upgrade, &what);
            // one warning for each appended carried record, with the fix of the store's mode
            let carried = tail_carried(want);
            let warnings = want["summary"]["warnings"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert_eq!(warnings.len(), carried.len(), "{what}");
            for (w, c) in warnings.iter().zip(&carried) {
                assert_eq!(w["code"], "W_STOP_BEFORE_REDACTION");
                assert_eq!(w["carried"], c["hash"], "{what}");
                assert_eq!(w["apply"], c["record"]["apply"]);
                let fix = if input.holder {
                    HOLDER_FIX
                } else {
                    PROJECT_FIX
                };
                assert_eq!(w["fix"], fix, "{what}");
                assert!(!w["issues"].as_array().unwrap().is_empty());
            }
            let r = open(&to, input.holder);
            r.fsck().unwrap();
            r.close().unwrap();
        }
    }
    // the fixed stops, as the expect files pin them
    let chain = read_expect(CHAIN_EXPECT);
    let held = &chain["held_stop"];
    let landing = held["selector"]
        .as_str()
        .unwrap()
        .strip_prefix("landing:")
        .unwrap();
    assert_eq!(held["summary"]["stopped_at"]["landing"], landing);
    assert_eq!(held["summary"]["stopped_at"]["tag"], Value::Null);
    let missing = &chain["missing_stop"];
    assert!(missing["selector"].as_str().unwrap().starts_with("pos:"));
    assert_eq!(missing["summary"]["stopped_at"]["landing"], Value::Null);
    assert!(loc_of(&held["position"]) < loc_of(&missing["position"]));
    for stop in [held, missing] {
        // one load with mixed held states: the early block's and the blob's payloads, which the
        // full dump holds, are held; the late block's, which the apply's own file holds only as
        // dropped frames, stay zero
        let carried = tail_carried(stop);
        assert_eq!(carried.len(), 2, "{stop}");
        assert!(helds(&carried[0]).iter().all(|h| *h));
        assert!(helds(&carried[1]).iter().all(|h| !*h));
        assert_eq!(carried[0]["zero_bytes"], 0);
        assert!(carried[1]["zero_bytes"].as_u64().unwrap() > 0);
        // before the first apply of the last increment: no apply of increment 2 is held
        let applies = &stop["store"]["removals"]["applies"];
        assert_eq!(
            applies.as_array().unwrap().len(),
            1,
            "the every-kind removal only"
        );
        assert_eq!(
            stop["fsck_warnings"].as_array().unwrap().len(),
            1,
            "held spans only"
        );
    }
    // the standalone full fixture: a new carried record of zero spans after the carried input
    let full = read_expect(FULL_EXPECT);
    let stop = &full["missing_stop"];
    assert!(stop["selector"].as_str().unwrap().starts_with("tag:"));
    let carried = tail_carried(stop);
    assert_eq!(carried.len(), 1);
    assert!(helds(&carried[0]).iter().all(|h| !*h));
    let input = stop["store"]["removals"]["carried"]
        .as_array()
        .unwrap()
        .len()
        - 1;
    assert_eq!(
        input, 2,
        "the carried input of the two applies after the source's stop"
    );
    // the holder chain's stops: held spans at the holder's own places, with the holder fix
    let holder = read_expect(HOLDER_CHAIN_EXPECT);
    for name in ["held_stop", "missing_stop"] {
        let stop = &holder[name];
        assert!(stop["selector"].as_str().unwrap().starts_with("tag:"));
        let carried = tail_carried(stop);
        assert_eq!(carried.len(), 1, "{name}");
        assert!(helds(&carried[0]).iter().all(|h| *h));
    }
    // the standalone holder: its later take's apply covers every carried span, so its stop
    // before that take appends nothing
    let full = read_expect(HOLDER_EXPECT);
    assert_eq!(full["missing_stop"]["tail"], "");
    assert_eq!(full["missing_stop"]["summary"]["warnings"], json!([]));
}

/// Whether the store at `stop` holds the payload with `canary`, as the expect file pins it: a
/// held span of a carried record, or a payload no removal selected before the stop.
fn stop_holds(stop: &Value, input: &Input, canary: &[u8]) -> bool {
    // the early block and the blob are held at a chain's stops; the late block, the standalone
    // block and every earlier removal stay removed
    let held_in_chain: [&[u8]; 4] = [
        chain_fixture::EARLY_CANARY.as_bytes(),
        chain_fixture::BLOB_CANARY,
        every_kind::HOLDER_CANARY.as_bytes(),
        every_kind::HOLDER_BLOB,
    ];
    let chain = input.names.len() > 1;
    let all_zero = tail_carried(stop)
        .iter()
        .all(|c| helds(c).iter().all(|h| !*h));
    chain && !all_zero && held_in_chain.contains(&canary)
}

#[test]
fn hc9_stopped_fixtures_redump_reload_and_reindex() {
    let d = tempfile::tempdir().unwrap();
    for input in &INPUTS {
        let expect = read_expect(input.expect);
        let files = input.files();
        // the source of a holder take: the whole load's last tag, which holds the removal
        let source = d.path().join(format!("{}-source", input.expect));
        chain_load(&files, &source, None);
        for name in input.stops {
            let what = format!("{}: {name}", input.expect);
            let want = &expect[*name];
            let a = d.path().join(format!("{}-{name}-a", input.expect));
            chain_load(&files, &a, Some(want["selector"].as_str().unwrap()));
            let mut r = open(&a, input.holder);
            let before = r.fsck().unwrap();
            assert_eq!(json!(before.warnings), want["fsck_warnings"], "{what}");
            r.reindex().unwrap();
            assert_eq!(
                serde_json::to_value(r.fsck().unwrap()).unwrap(),
                serde_json::to_value(&before).unwrap(),
                "{what}: a reindex derives the same"
            );
            // its dump loads whole into the same log: the carried records are retained
            let again = d.path().join(format!("{}-{name}.rdydump", input.expect));
            r.read().dump(&again).unwrap();
            r.close().unwrap();
            let b = d.path().join(format!("{}-{name}-b", input.expect));
            let s = chain_load(std::slice::from_ref(&again), &b, None);
            assert!(s.warnings.is_empty(), "{what}: {:?}", s.warnings);
            assert_eq!(segs(&b), segs(&a), "{what}: the same log");
            // the first writable start of each logs the moves content-2 leaves, then the logged
            // invariant holds
            let n = first_start_logs_moves(&a, input.holder, &what);
            assert_eq!(first_start_logs_moves(&b, input.holder, &what), n, "{what}");
            if !input.holder {
                rdy_qa::reindex_check::reindex_reproduces(&a).unwrap_or_else(|v| panic!("{v:#?}"));
            }
            // a later write of the store's own mode
            let mut r = open(&b, input.holder);
            if input.holder {
                let tag = every_kind::HOLDER_TAGS[2];
                take_tag(&mut r, &source, tag, d.path());
                // the take of the matching apply ends the warning and erases the held copies
                assert!(r.fsck().unwrap().warnings.is_empty(), "{what}");
                r.close().unwrap();
                for c in input.canaries {
                    assert!(!holds(&b, c), "{what}: the take erased the held copy");
                }
            } else {
                later_write(&mut r);
                rdy_qa::invariants::check_all(&r).unwrap_or_else(|v| panic!("{what}: {v:#?}"));
                r.close().unwrap();
            }
        }
    }
}

/// Appends to the store under `dir` the batch a later rdy's load appends after the source log
/// and the carried batch: one ref update that names this rdy's store format, and its marker.
/// Returns the store format the checks then take the fixtures to hold: 15, as if this rdy were a
/// later one. When this rdy writes a later store format than 16, its load appended that batch
/// already, and nothing is appended.
fn as_a_later_rdy(dir: &Path, holder: bool) -> u32 {
    // this rdy writes feature stores: its load appended that batch already
    let _ = (dir, holder);
    16
}

#[test]
fn hc9_a_later_store_format_reads_the_fixtures_after_one_upgrade_batch() {
    // the checks of a later rdy, which appends one upgrade batch after each load: the whole
    // loads, a chain's full dump alone, and each fixed stop, of a project (whose report holds the
    // redaction counts) and of a holder (whose chain's full dump and stops hold none)
    let d = tempfile::tempdir().unwrap();
    let (mut with_counts, mut without) = (0, 0);
    let mut count = |want: &Value| {
        if want["store"]["facts"]["fsck"]
            .get("checked_payloads")
            .is_some()
        {
            with_counts += 1;
        } else {
            without += 1;
        }
    };
    for input in &INPUTS {
        let expect = read_expect(input.expect);
        let files = input.files();
        let mut wholes = vec![(files.clone(), "whole")];
        if files.len() > 1 {
            wholes.push((files[..1].to_vec(), "full_alone"));
        }
        for (files, name) in wholes {
            let what = format!("{}: {name}, upgraded", input.expect);
            let to = d.path().join(format!("{}-{name}", input.expect));
            let s = chain_load(&files, &to, None);
            let format = as_a_later_rdy(&to, input.holder);
            let got = whole_facts_of(&s, &files, &to, input.holder);
            let upgrade = upgrade_after(&to, chain_end(&files), input.holder, format);
            assert_eq!(upgrade, 1, "{what}");
            check_whole(&got, &expect[name], upgrade, &what);
            count(&expect[name]);
        }
        for name in input.stops {
            let what = format!("{}: {name}, upgraded", input.expect);
            let want = &expect[*name];
            let to = d.path().join(format!("{}-{name}", input.expect));
            let (got, upgrade) = stopped(&files, want, &to, input.holder, true);
            assert_eq!(upgrade, 1, "{what}");
            check_stop(&got, want, upgrade, &what);
            count(want);
        }
    }
    assert!(with_counts > 0 && without > 0, "{with_counts} {without}");
}

/// An ordinary write to a loaded project: one block, landed.
fn later_write(r: &mut Rdy) {
    let w = r
        .write(
            &rdy_qa::fixtures::writer(),
            WriteRequest {
                view: "later".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "later", "title": "Later", "doc_type": "page"}},
                    {"op": "create", "type": "prose", "parent": "$d", "body": "A write after the load."}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    assert!(!w.created.is_empty());
    rdy_qa::fixtures::land(r, "later").unwrap();
    r.fsck().unwrap();
}

/// Takes the history of tag `name` of the holder at `source` into `h`, as an upload of one
/// batch file under `dir`.
fn take_tag(h: &mut Rdy, source: &Path, name: &str, dir: &Path) {
    let src = Rdy::open_holder(source).unwrap();
    let (_, hash, t) = src
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, ..)| n == name)
        .unwrap();
    let bytes = every_kind::export_tag(&src, hash);
    src.close().unwrap();
    let p = dir.join(format!("take-{}.rdysync", h.store().log_end().offset));
    std::fs::write(&p, &bytes).unwrap();
    rdy_engine::stream_take::take(
        h,
        &[std::fs::File::open(&p).unwrap()],
        rdy_store::take_batch::UploadTarget {
            tag: t.name,
            landing: t.landing,
        },
    )
    .unwrap();
}

#[test]
fn hc9_holder_chain_preserves_local_removal_without_canon() {
    let d = tempfile::tempdir().unwrap();
    let expect = read_expect(HOLDER_CHAIN_EXPECT);
    // no file of the holder's chain holds a project file
    for f in expect["files"].as_array().unwrap() {
        assert_eq!(f["files"], json!([]), "{}", f["name"]);
        assert_eq!(f["trailer"]["files"], 0);
    }
    let files = holder_chain_files();
    let whole = d.path().join("whole");
    let s = chain_load(&files, &whole, None);
    assert!(s.files_written.is_empty() && s.files_kept.is_empty());
    assert!(!whole.join(".rdy.yaml").exists(), "no project config");
    let h = Rdy::open_holder(&whole).unwrap();
    let id = every_kind::identity_facts(&h, None);
    assert_eq!(
        (&id["mode"], &id["canon"], &id["origin"]),
        (&json!("holder"), &Value::Null, &Value::Null)
    );
    assert_eq!(h.read().canon_head(), None);
    // three tags; v2.0.0 leaves the history of v1.0.0 and v3.0.0 after the shared landing
    let tags = id["tags"].as_object().unwrap();
    assert_eq!(
        tags.keys().map(String::as_str).collect::<Vec<_>>(),
        every_kind::HOLDER_TAGS
    );
    let landing = |name: &str| -> Hash { tags[name]["landing"].as_str().unwrap().parse().unwrap() };
    {
        let st = h.read();
        let chain = |l: Hash| -> BTreeSet<Hash> {
            let mut out = BTreeSet::new();
            let mut cur = Some(l);
            while let Some(c) = cur {
                out.insert(c);
                cur = st.reader().landing(&c).unwrap().prev;
            }
            out
        };
        let (a, b) = (chain(landing("v3.0.0")), chain(landing("v2.0.0")));
        assert!(a.contains(&landing("v1.0.0")) && b.contains(&landing("v1.0.0")));
        assert!(!a.contains(&landing("v2.0.0")) && !b.contains(&landing("v3.0.0")));
    }
    // the relay answer's ref by tag object, the mode ref, and the five signatures, each checked
    let refs = h.store().refs_with_prefix("").unwrap();
    assert!(
        refs.iter()
            .any(|(n, v)| n == REF_STORE_MODE && v == b"holder")
    );
    let relay: Vec<_> = refs
        .iter()
        .filter(|(n, _)| n.starts_with(ANSWER_TAG_OBJECT_REF))
        .collect();
    assert_eq!(relay.len(), 1);
    assert_eq!(
        relay[0].0.strip_prefix(ANSWER_TAG_OBJECT_REF).unwrap(),
        tags["v3.0.0"]["tag"]
    );
    let signed = every_kind::facts(&h)["signatures"].clone();
    assert_eq!(signed.as_object().unwrap().len(), 5);
    // the removal approved in one source, at the holder's own places: every dropped frame names
    // the one apply the holder took, and the places are the ones the expect file pins
    let removals = removal_facts(&h, h.store().log_end());
    assert_eq!(removals, expect["whole"]["store"]["removals"]);
    let applies = removals["applies"].as_array().unwrap();
    assert_eq!(applies.len(), 1);
    let full_end = loc_of(&expect["files"][0]["log_end"]);
    let mut early = BTreeSet::new();
    for dr in removals["dropped"].as_array().unwrap() {
        assert_eq!(dr["authority"], applies[0]["hash"]);
        if loc_of(&dr["at"]) < full_end {
            early.insert(dr["at"].to_string());
        }
    }
    assert!(
        !early.is_empty(),
        "the take erased copies the full dump holds"
    );
    h.fsck().unwrap();
    h.close().unwrap();

    // stopped before the take of the removal: the carried spans are exactly those places, held,
    // and the holder's warning persists across opens
    let stop = &expect["held_stop"];
    let stopped = d.path().join("stopped");
    let s = chain_load(&files, &stopped, Some(stop["selector"].as_str().unwrap()));
    assert_eq!(s.warnings.len(), 1);
    assert_eq!(s.warnings[0].fix, HOLDER_FIX);
    let carried = tail_carried(stop);
    let places: BTreeSet<String> = carried[0]["record"]["spans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|sp| json!([sp["place"]["segment"], sp["place"]["offset"]]).to_string())
        .collect();
    assert_eq!(places, early);
    for _ in 0..2 {
        let h = Rdy::open_holder(&stopped).unwrap();
        let ws = h.fsck().unwrap().warnings;
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].fix, HOLDER_FIX);
        h.close().unwrap();
    }

    // the standalone holder: the carried record of zero spans, then the take of the matching
    // apply; the whole load names the apply as each span's proof, its stop the carried record
    let expect = read_expect(HOLDER_EXPECT);
    assert_eq!(expect["file"]["files"], json!([]));
    let whole = &expect["whole"]["store"]["removals"];
    let stop = &expect["missing_stop"]["store"]["removals"];
    let carried = whole["carried"].as_array().unwrap();
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0]["record"]["apply"], whole["applies"][0]["hash"]);
    for (at, by_whole, by_stop) in proofs(whole, stop) {
        assert_eq!(by_whole, whole["applies"][0]["hash"], "{at}");
        assert_eq!(by_stop, carried[0]["hash"], "{at}");
    }
    assert_eq!(expect["whole"]["store"]["identity"]["canon"], Value::Null);
}

/// The dropped frames at a place both `whole` and `stop` hold: the place and each one's proof.
fn proofs(whole: &Value, stop: &Value) -> Vec<(Value, Value, Value)> {
    let of = |v: &Value| -> BTreeMap<String, Value> {
        v["dropped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| (d["at"].to_string(), d["authority"].clone()))
            .collect()
    };
    let (w, s) = (of(whole), of(stop));
    assert!(!s.is_empty());
    s.into_iter()
        .map(|(at, by_stop)| (json!(at), w[&at].clone(), by_stop))
        .collect()
}

/// A copy of fixture `name` under `dir`, changed by `f`.
fn changed(dir: &Path, name: &str, f: impl FnOnce(&mut Vec<u8>)) -> PathBuf {
    let mut bytes = std::fs::read(fixture(name)).unwrap();
    f(&mut bytes);
    let p = dir.join(name);
    std::fs::write(&p, &bytes).unwrap();
    p
}

/// The offset of the first record of `kind` in the dump `bytes` whose payload is not empty.
fn record_of(bytes: &[u8], kind: u8) -> (usize, usize) {
    let mut at = 12usize;
    while at < bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        if bytes[at + 4] == kind && len > 0 {
            return (at, len);
        }
        at += 37 + len;
    }
    panic!("no record of kind {kind}")
}

#[test]
fn hc9_truncated_reordered_and_mutated_chain_fixtures_refuse() {
    let d = tempfile::tempdir().unwrap();
    let to = d.path().join("to");
    let [full, one, two] = [CHAIN_FULL, CHAIN_1, CHAIN_2].map(fixture);
    let refuse = |files: &[PathBuf], stop: Option<&str>| -> RdyError {
        let e = chain_err(files, &to, stop);
        assert!(!to.exists(), "a refused load leaves no target: {e:?}");
        e
    };
    let chain_fix = "pass the full dump, then each increment in chain order";
    let base = |p: &Path| -> String {
        let f = file_facts("", p);
        format!(
            "{}.{}",
            f["chain"]["base"]["segment"], f["chain"]["base"]["offset"]
        )
    };
    let end = |p: &Path| -> String {
        let f = file_facts("", p);
        format!("{}.{}", f["log_end"][0], f["log_end"][1])
    };
    let rows: Vec<(Vec<PathBuf>, String)> = vec![
        // reordered: the second increment before the first
        (
            vec![full.clone(), two.clone(), one.clone()],
            format!(
                "{} starts at {}, but the file before it ends at {}",
                two.display(),
                base(&two),
                end(&full)
            ),
        ),
        // the middle file left out
        (
            vec![full.clone(), two.clone()],
            format!(
                "{} starts at {}, but the file before it ends at {}",
                two.display(),
                base(&two),
                end(&full)
            ),
        ),
        // an increment first
        (
            vec![one.clone(), two.clone()],
            format!(
                "{}: it is an increment; a dump chain starts with a full dump",
                one.display()
            ),
        ),
        // a second full dump
        (
            vec![full.clone(), fixture(FULL)],
            format!(
                "{}: it is a full dump; only the first file of a dump chain is one",
                fixture(FULL).display()
            ),
        ),
        // a released dump of format 2 in a chain
        (
            vec![fixture("v2-s15.rdydump"), one.clone()],
            format!(
                "{}: dump format 2 holds no chain facts; a dump chain starts with a full dump of format 3",
                fixture("v2-s15.rdydump").display()
            ),
        ),
    ];
    for (files, message) in rows {
        let e = refuse(&files, None);
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            ("E_DUMP_CHAIN", message.as_str(), chain_fix),
            "{files:?}"
        );
    }
    // another repo's increment
    let holder_one = fixture(HOLDER_CHAIN_1);
    let e = refuse(&[full.clone(), holder_one.clone()], None);
    assert_eq!(e.code, "E_DUMP_CHAIN");
    assert!(
        e.message
            .starts_with(&format!("{} holds repo ", holder_one.display())),
        "{e:?}"
    );

    // truncated: the last file without the end of its trailer, with and without a stop before
    // it; the file of the stop is read through its end
    let damaged_fix = "dump the project again; rdy never repairs a dump";
    let damaged = |e: &RdyError, p: &Path, at: usize, what: &str| {
        assert_eq!(
            (e.code, e.message.clone(), e.fix.as_str()),
            (
                "E_DUMP",
                format!("{} is damaged at byte {at}: {what}", p.display()),
                damaged_fix
            )
        );
    };
    let m = d.path().join("m");
    std::fs::create_dir_all(&m).unwrap();
    let mut cut_len = 0;
    let cut = changed(&m, CHAIN_2, |b| {
        b.truncate(b.len() - 10);
        cut_len = b.len();
    });
    let held = read_expect(CHAIN_EXPECT)["held_stop"]["selector"]
        .as_str()
        .unwrap()
        .to_string();
    for stop in [None, Some(held.as_str())] {
        let e = refuse(&[full.clone(), one.clone(), cut.clone()], stop);
        let trailer = cut_len - 37 - rdy_store::dump::TRAILER_LEN;
        damaged(&e, &cut, trailer, "the trailer does not parse");
    }
    // a changed payload byte: in the stop's file, in a file wholly after the stop, and in the
    // standalone fixture's carried record
    let flip = |name: &str, kind: u8| -> (PathBuf, usize) {
        let mut offset = 0;
        let p = changed(&m, name, |b| {
            let (at, len) = record_of(b, kind);
            b[at + 37 + len - 1] ^= 1;
            offset = at;
        });
        (p, offset)
    };
    let mismatch = "a record's hash does not match its bytes";
    let (bad_two, at) = flip(CHAIN_2, kinds::LANDING);
    let e = refuse(&[full.clone(), one.clone(), bad_two.clone()], Some(&held));
    damaged(&e, &bad_two, at, mismatch);
    let holder_files = holder_chain_files();
    let (bad_h2, at) = flip(HOLDER_CHAIN_2, kinds::LANDING);
    let held = read_expect(HOLDER_CHAIN_EXPECT)["held_stop"]["selector"]
        .as_str()
        .unwrap()
        .to_string();
    let e = refuse(
        &[
            holder_files[0].clone(),
            holder_files[1].clone(),
            bad_h2.clone(),
        ],
        Some(&held),
    );
    damaged(&e, &bad_h2, at, mismatch);
    let (bad_full, at) = flip(FULL, kinds::REDACTION_CARRIED);
    let e = refuse(std::slice::from_ref(&bad_full), None);
    damaged(&e, &bad_full, at, mismatch);
    // the frozen files themselves are untouched, and load after all this
    for (name, digest) in MEMBERS {
        assert_eq!(
            Hash::of(&std::fs::read(fixture(name)).unwrap()).to_string(),
            digest
        );
    }
    chain_load(&chain_files(), &to, None);
}

#[test]
fn hc9_invariants_accept_a_carried_removal_whose_request_is_not_held() {
    // a load stopped before a source's apply holds that apply's request only in its carried
    // record, and a version it keeps zero reads as a stub that names that request: the QA
    // invariants take the carried span with held: false as the version's authorization
    let d = tempfile::tempdir().unwrap();
    let to = d.path().join("p");
    let want = read_expect(CHAIN_EXPECT)["held_stop"].clone();
    chain_load(
        &chain_files(),
        &to,
        Some(want["selector"].as_str().unwrap()),
    );
    // the first writable start logs the moves content-2 leaves
    first_start_logs_moves(&to, false, "held_stop");
    let carried = tail_carried(&want);
    let request: Hash = carried[1]["record"]["request"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let r = Rdy::open(&to).unwrap();
    assert!(
        r.store().reader().redaction_request(&request).is_err(),
        "the source's request is not held"
    );
    {
        let st = r.read();
        let m = st.materialized(&rdy_engine::Scope::Canon).unwrap();
        let stubs = m
            .iter()
            .filter(|b| {
                b.read_identity
                    .as_ref()
                    .is_some_and(|i| i.request == request)
            })
            .count();
        assert_eq!(stubs, 1, "the late block reads as its stub");
    }
    rdy_qa::invariants::check_all(&r).unwrap_or_else(|v| panic!("{v:#?}"));
    r.close().unwrap();
}

/// The scalar store format of a released dump's header.
fn legacy(s: &DumpStore) -> u32 {
    match s {
        DumpStore::Legacy(n) => *n,
        DumpStore::Features(_) => panic!("a released pair holds a scalar store"),
    }
}
