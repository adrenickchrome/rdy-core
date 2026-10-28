//! cp3: the dump promise for dump format 3 and store format 17 (D-69AY3: "every later rdy reads
//! every dump format that rdy has released"; D-4JMPH: "A new store kind PLAN, code 24"; p-DC7C2).
//! The pair `(3, 17)` is released with twelve files and this file. Write them once: never
//! rewrite them, and never regenerate them to fix a later reader.
//!
//! The members follow the `(3, 16)` set (`io_dump_fixture_s16`), built by the same builders
//! (`rdy_qa::chain_fixture`) at store format 17:
//!
//! - `v3-s17-chain-full`, `-chain-1` and `-chain-2` are a full dump and two increments of one
//!   project, from the every-kind project, which holds a plan chain with its seal, an unsealed plan,
//!   every plan act and the four plan answer acts (`every_kind::add_plan_fixture_records`), and a
//!   block version with both engine metadata fields. The full dump holds a proposal with its bound
//!   plan; increment 1 holds that proposal's landing and its seal, which names the earlier file's
//!   plan. Increment 2 holds the two approved removals and the fixed stops of the `(3, 16)` set.
//! - `v3-s17` is the standalone full fixture, built as `v3-s16` is.
//! - `v3-s17-holder-chain-*` and `v3-s17-holder` are the same for a holder, filled by takes of the
//!   every-kind holder's uploads and of two sealed plan landings' histories through an export, a
//!   batch check and a take, with no `plan/` ref: increment 1 takes a seal with its chain and
//!   acceptance (`chain_fixture::PLAN_TAG`), and increment 2 takes another source's seal of the
//!   same proposal (`chain_fixture::PLAN_FORK_TAG`), which names the bound plan of increment 1.
//!
//! Every expect file pins what a load of its files must give, as the `(3, 16)` set pins it, and
//! the plan facts: every PLAN record and every row of the four plan tables. A later store format
//! adds kinds and refs, never re-encodes (D-F0904): a load then appends one upgrade batch after the
//! source log or the carried batch, and the checks allow exactly that batch.
use crate::io_dump_fixture_s15::{first_start_logs_moves, fixture, without_moves};
use rdy_engine::Rdy;
use rdy_engine::dump_stop::DumpStop;
use rdy_engine::load::{LoadSummary, load_chain};
use rdy_model::plan::PlanAct;
use rdy_model::redaction::RedactionCarried;
use rdy_model::{AnswerAct, Hash};
use rdy_qa::chain_fixture::{self, V17_KINDS};
use rdy_qa::every_kind;
use rdy_store::dump::{DUMP_FORMAT, DumpItem, DumpReader, read_envelope};
use rdy_store::format::{
    DumpStore, LAST_SCALAR_STORE_FORMAT, REF_STORE_FEATURES, baseline_features,
};
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::{Location, on_segment_write, seg_path};
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The ref families of a store-format-17 project, frozen with the v3-s17 fixtures: the families
/// of `every_kind::FAMILIES` when store format 17 was the last scalar format.
const V17_FAMILIES: [&str; 17] = [
    "meta/store_format",
    "meta/repo_id",
    "meta/origin",
    "view/",
    "tag/",
    "intent/",
    "alt/",
    "dismiss/",
    "mention/",
    "pair/",
    "semantic/",
    "inbox/",
    "canon",
    "proposal/",
    "answer/",
    "pending/",
    "plan/",
];

const FULL: &str = "v3-s17.rdydump";
const FULL_EXPECT: &str = "v3-s17.expect.json";
const CHAIN_FULL: &str = "v3-s17-chain-full.rdydump";
const CHAIN_1: &str = "v3-s17-chain-1.rdydump";
const CHAIN_2: &str = "v3-s17-chain-2.rdydump";
const CHAIN_EXPECT: &str = "v3-s17-chain.expect.json";
const HOLDER: &str = "v3-s17-holder.rdydump";
const HOLDER_EXPECT: &str = "v3-s17-holder.expect.json";
const HOLDER_CHAIN_FULL: &str = "v3-s17-holder-chain-full.rdydump";
const HOLDER_CHAIN_1: &str = "v3-s17-holder-chain-1.rdydump";
const HOLDER_CHAIN_2: &str = "v3-s17-holder-chain-2.rdydump";
const HOLDER_CHAIN_EXPECT: &str = "v3-s17-holder-chain.expect.json";

/// Each member of the pair `(3, 17)` and its blake3, set when it was written. A change here
/// needs a founder decision.
pub(crate) const MEMBERS: [(&str, &str); 12] = [
    (
        FULL,
        "94157e0d99fbd0170ea7eeca37315e141a4776485923bd27264bbc00e70442b5",
    ),
    (
        FULL_EXPECT,
        "397680b93467ef7a9dce3fdb0e16cb1a71d8bbcdbe734ce96907d657578fd44a",
    ),
    (
        CHAIN_FULL,
        "c0ee2f5236c226df0eaac875b317872d10bbc2f0f441145ea9df139317636872",
    ),
    (
        CHAIN_1,
        "1189a0c637444c8f9ec13ad4f6b1ee93dc12c7b2d912b4f877d253bc6e3e313d",
    ),
    (
        CHAIN_2,
        "1c72ece30389ba89621cf4b01702b0a640628280d0297f3ca04418fbff73b42b",
    ),
    (
        CHAIN_EXPECT,
        "9dcb2982bcae43cf79476b6ecaff2e13f528bb1a86d5f2caf0ec4e98c0f775b8",
    ),
    (
        HOLDER,
        "004dbd2f0eb39bcf227aa4a1dfc1fe58980094bd3bbb80b52bbcb70cc8124d91",
    ),
    (
        HOLDER_EXPECT,
        "8c21ae51e8a2bd0689189935d56bac40506fb77890606cf70cdf0d967f39ff69",
    ),
    (
        HOLDER_CHAIN_FULL,
        "ec0e4a4f6f3f5c6efde8229b8b65205adfa4a18de8098f4945864021f4ebf5eb",
    ),
    (
        HOLDER_CHAIN_1,
        "1ff73bc53aeb46cb50f0de29997165298bcd14e14b4876bf3909db17eecd9be2",
    ),
    (
        HOLDER_CHAIN_2,
        "bbff1ff5ce76e60d8ec2c6aacab90b9c471edf0ec4b359017dfa884c7e2bc35a",
    ),
    (
        HOLDER_CHAIN_EXPECT,
        "b38ac9d6944e139dd4e0966c5a58648d76a937c8858910f11b92446200b55e2f",
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
/// views, each tag's projection, canon's blocks, the removal facts and the plan facts.
fn store_facts(r: &Rdy, upto: Location) -> Value {
    json!({
        "facts": every_kind::facts(r),
        "identity": every_kind::identity_facts(r, Some(upto)),
        "views": r.read().view_names(),
        "projections": every_kind::projections(r),
        "canon_blocks": canon_blocks(r),
        "removals": removal_facts(r, upto),
        "plans": plan_facts(r, upto),
    })
}

/// Every PLAN record of the log of `r` before `upto`, in log order: its place, hash, view,
/// chain links, bound proposal, its seal's landing, version and decisions, and its answers' and
/// items' acts and outcomes; and every row of the four plan tables, as stored (hex).
fn plan_facts(r: &Rdy, upto: Location) -> Value {
    let mut records = vec![];
    r.store()
        .reader()
        .for_each_record_from(ORIGIN, upto, |at, f, _| {
            if let StoredFrame::Present {
                kind: kinds::PLAN,
                hash,
                payload,
            } = f
            {
                let p = rdy_store::plan::decode_plan(payload).unwrap();
                let opt = |h: Option<Hash>| h.map(|h| h.to_string());
                records.push(json!({
                    "at": loc(at),
                    "hash": hash.to_string(),
                    "view": p.view,
                    "stable": opt(p.stable),
                    "prev": opt(p.prev),
                    "proposal": opt(p.proposal),
                    "answers": p.answers.iter().map(|a| a.act.as_str()).collect::<Vec<_>>(),
                    "seal": p.seal.as_ref().map(|s| json!({
                        "landing": s.landing.to_string(),
                        "version": s.version.to_string(),
                        "decisions": s.decisions.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
                        "items": s.items.iter().map(|i| json!([i.kind.as_str(), i.outcome.as_str(), opt(i.answer)])).collect::<Vec<_>>(),
                    })),
                }));
            }
            Ok(())
        })
        .unwrap();
    let dump = r.store().index_dump_for_tests().unwrap();
    let rows = |m: &BTreeMap<Vec<u8>, Vec<u8>>| -> Vec<Value> {
        m.iter().map(|(k, v)| json!([hex(k), hex(v)])).collect()
    };
    json!({
        "records": records,
        "tables": {
            "plan_seals": rows(&dump.plan_seals),
            "plan_decisions": rows(&dump.plan_decisions),
            "plan_deferrals": rows(&dump.plan_deferrals),
            "plan_proposals": rows(&dump.plan_proposals),
        },
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

/// Writes the twelve members of the pair `(3, 17)` once. Run by hand, never by the gate:
/// `cargo test -p rdy-qa --release --test io cp3_write_format17_fixtures_once -- --ignored`.
/// It refuses a member that exists.
#[test]
#[ignore]
fn cp3_write_format17_fixtures_once() {
    for (m, _) in MEMBERS {
        assert!(
            !fixture(m).exists(),
            "{m}: the v3-s17 fixtures are written once: never rewrite them"
        );
    }
    assert_eq!(
        (DUMP_FORMAT, LAST_SCALAR_STORE_FORMAT),
        (3, 17),
        "the v3-s17 fixtures hold dump format 3 and store format 17"
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

/// One frozen input of the pair `(3, 17)`: its files in order, their names, its expect file, its
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
        "plans",
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
    assert!(!later, "a later rdy's checks are its own");
    let format = 17;
    let end = loc_of(&want["end"]);
    let got = stop_facts_of(&s, selector, to, holder, Some(end));
    (got, upgrade_after(to, end, holder, format))
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
fn cp3_format17_fixture_bytes_never_change() {
    // A change to any member needs a founder decision. The checksums are fixed here, apart from
    // any writer: a later writer that writes other bytes changes nothing in this check.
    for (name, digest) in MEMBERS {
        let bytes = std::fs::read(fixture(name)).unwrap();
        assert_eq!(Hash::of(&bytes).to_string(), digest, "{name}");
    }
}

#[test]
fn cp3_format17_project_and_holder_load_with_every_hash() {
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
                (Some(3), Some(17)),
                "{what}"
            );
            assert_eq!(f["batches"], f["trailer"]["batches"]);
            assert_eq!(f["frames"], f["trailer"]["frames"]);
            if i > 0 {
                let prev = &facts[i - 1];
                assert_eq!(f["chain"]["kind"], "increment", "{what}");
                assert_eq!(f["chain"]["base_batch"], prev["chain"]["end_batch"]);
                assert_eq!(f["chain"]["prev"], prev["trailer"]["digest"]);
            }
        }
        // the whole load: the source log byte for byte, then at most the upgrade batch
        let to = d.path().join(format!("{what}-whole"));
        let w = watch(input.canaries.to_vec());
        let s = chain_load(&files, &to, None);
        assert!(
            !w.0.get(),
            "{what}: a write held a payload removed in the chain"
        );
        drop(w);
        let got = whole_facts_of(&s, &files, &to, input.holder);
        let upgrade = upgrade_after(&to, chain_end(&files), input.holder, 17);
        check_whole(&got, &expect["whole"], upgrade, what);
        for c in input.canaries {
            assert!(!holds(&to, c), "{what}: a removed payload is in the store");
        }
        // every frame of every file under its own kind and hash
        let mut r = open(&to, input.holder);
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
        // the plan tables a reindex rebuilds are the ones the load built
        let tables = r.store().index_dump_for_tests().unwrap();
        r.reindex().unwrap();
        assert_eq!(
            r.store().index_dump_for_tests().unwrap(),
            tables,
            "{what}: a reindex rebuilds every table"
        );
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
        // a chain's full dump alone
        if files.len() > 1 {
            let to = d.path().join(format!("{what}-alone"));
            let got = whole_facts(&files[..1], &to, input.holder);
            let upgrade = upgrade_after(&to, chain_end(&files[..1]), input.holder, 17);
            check_whole(&got, &expect["full_alone"], upgrade, what);
        }
        // each fixed stop: its position, its carried batch byte for byte, and its facts
        for name in input.stops {
            let what = format!("{}: {name}", input.expect);
            let want = &expect[*name];
            let to = d.path().join(format!("{}-{name}", input.expect));
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
            let r = open(&to, input.holder);
            r.fsck().unwrap();
            r.close().unwrap();
        }
    }
}

/// The acts of every ANSWER record of the store at `dir`.
fn answer_acts(dir: &Path, holder: bool) -> BTreeSet<&'static str> {
    let r = open(dir, holder);
    let mut out = BTreeSet::new();
    r.store()
        .reader()
        .for_each_record_from(ORIGIN, r.store().log_end(), |_, f, _| {
            if let StoredFrame::Present {
                kind: kinds::ANSWER,
                payload,
                ..
            } = f
            {
                let a: rdy_model::Answer = rdy_store::bounded::decode(payload).unwrap();
                out.insert(a.act.as_str());
            }
            Ok(())
        })
        .unwrap();
    r.close().unwrap();
    out
}

/// Whether some BLOCK record of the store at `dir` holds both engine metadata fields.
fn holds_metadata(dir: &Path) -> bool {
    use rdy_model::engine_fields::{REAFFIRMED, REFLECTS_AT};
    let r = open(dir, false);
    let mut found = false;
    r.store()
        .reader()
        .for_each_record_from(ORIGIN, r.store().log_end(), |_, f, _| {
            if let StoredFrame::Present {
                kind: kinds::BLOCK,
                payload,
                ..
            } = f
            {
                let b: rdy_model::Block = rdy_store::bounded::decode(payload).unwrap();
                found |= b.fields.contains_key(REAFFIRMED) && b.fields.contains_key(REFLECTS_AT);
            }
            Ok(())
        })
        .unwrap();
    r.close().unwrap();
    found
}

#[test]
fn cp3_format17_fixtures_cover_new_kinds_refs_and_acts() {
    // the standalone project holds every kind of store format 17, the change plan among them
    let expect = read_expect(FULL_EXPECT);
    let objects = &expect["whole"]["store"]["facts"]["objects"];
    for k in V17_KINDS {
        assert!(
            objects[k.to_string()].as_u64().unwrap_or(0) >= 1,
            "kind {k}"
        );
    }
    assert!(objects.get("12").is_none(), "kind 12 is unassigned");
    // the project fixtures: the plan family, a sealed and an unsealed version, each plan act,
    // and a row in each plan table
    for name in [CHAIN_EXPECT, FULL_EXPECT] {
        let store = &read_expect(name)["whole"]["store"];
        let refs = store["facts"]["refs"].as_object().unwrap();
        assert!(refs.keys().any(|n| n.starts_with("plan/")), "{name}");
        for family in V17_FAMILIES {
            assert!(
                refs.keys().any(|n| n.starts_with(family)),
                "{name}: {family}"
            );
        }
        let records = store["plans"]["records"].as_array().unwrap();
        assert!(records.iter().any(|p| !p["seal"].is_null()), "{name}");
        assert!(records.iter().any(|p| p["seal"].is_null()), "{name}");
        let acts: BTreeSet<&str> = records
            .iter()
            .flat_map(|p| p["answers"].as_array().unwrap())
            .map(|a| a.as_str().unwrap())
            .collect();
        for act in [PlanAct::Confirm, PlanAct::Defer, PlanAct::Done] {
            assert!(acts.contains(act.as_str()), "{name}: {act:?}");
        }
        for table in [
            "plan_seals",
            "plan_decisions",
            "plan_deferrals",
            "plan_proposals",
        ] {
            assert!(
                !store["plans"]["tables"][table]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                "{name}: {table}"
            );
        }
    }
    // the four plan answer acts, and both engine metadata fields on a block version
    let d = tempfile::tempdir().unwrap();
    let to = d.path().join("full");
    chain_load(&[fixture(FULL)], &to, None);
    let acts = answer_acts(&to, false);
    for act in [
        AnswerAct::KeepSuspect,
        AnswerAct::AcceptDeferrals,
        AnswerAct::RefuseDeferrals,
        AnswerAct::RejectReaffirm,
    ] {
        assert!(acts.contains(act.as_str()), "{act:?}");
    }
    assert!(holds_metadata(&to), "both metadata field shapes");
    // the holder fixtures: plans and their tables, and no plan ref
    for name in [HOLDER_CHAIN_EXPECT, HOLDER_EXPECT] {
        let store = &read_expect(name)["whole"]["store"];
        let refs = store["facts"]["refs"].as_object().unwrap();
        assert!(!refs.keys().any(|n| n.starts_with("plan/")), "{name}");
        let records = store["plans"]["records"].as_array().unwrap();
        assert!(records.iter().any(|p| !p["seal"].is_null()), "{name}");
        assert!(
            !store["plans"]["tables"]["plan_seals"]
                .as_array()
                .unwrap()
                .is_empty(),
            "{name}"
        );
    }
}

/// The hashes of the PLAN frames of each of `files`, in order.
fn plans_by_file(files: &[PathBuf]) -> Vec<BTreeSet<Hash>> {
    files
        .iter()
        .map(|p| {
            let (mut rd, _, _) =
                DumpReader::open(BufReader::new(std::fs::File::open(p).unwrap())).unwrap();
            let mut out = BTreeSet::new();
            while let Some(item) = rd.next().unwrap() {
                if let DumpItem::Batch(b) = item {
                    out.extend(
                        b.frames
                            .iter()
                            .filter(|f| f.kind() == kinds::PLAN)
                            .map(|f| *f.hash()),
                    );
                }
            }
            out
        })
        .collect()
}

#[test]
fn cp3_format17_chain_seal_resolves_an_earlier_file() {
    let d = tempfile::tempdir().unwrap();
    for (expect, files, holder) in [
        (CHAIN_EXPECT, chain_files(), false),
        (HOLDER_CHAIN_EXPECT, holder_chain_files(), true),
    ] {
        let by_file = plans_by_file(&files);
        let file_of = |h: &Hash| by_file.iter().position(|s| s.contains(h));
        let to = d.path().join(format!("{expect}-whole"));
        chain_load(&files, &to, None);
        let mut r = open(&to, holder);
        let end = r.store().log_end();
        let mut crossing = 0;
        let mut seals = 0;
        for set in &by_file {
            for h in set {
                let p = r.store().plan(h).unwrap();
                let Some(seal) = &p.seal else { continue };
                seals += 1;
                // the seal is its landing's, in the landing's batch, and its links resolve
                assert_eq!(
                    r.store().plan_of_landing(&seal.landing, end).unwrap(),
                    Some(*h),
                    "{expect}"
                );
                assert!(r.store().has_kind(kinds::LANDING, &seal.landing).unwrap());
                assert!(r.store().has_kind(kinds::PROPOSAL, &seal.version).unwrap());
                for d in &seal.decisions {
                    assert!(
                        r.store().plans_of_decision(d, end).unwrap().contains(h),
                        "{expect}"
                    );
                }
                for link in [p.stable, p.prev].into_iter().flatten() {
                    assert!(r.store().has_kind(kinds::PLAN, &link).unwrap(), "{expect}");
                    if file_of(&link) < file_of(h) {
                        crossing += 1;
                    }
                }
                // a landing's history crosses files as well: its predecessor, held by an
                // earlier file
                if let Some(prev) = r.store().landing(&seal.landing).unwrap().prev {
                    assert!(r.store().has_kind(kinds::LANDING, &prev).unwrap());
                }
            }
        }
        assert!(seals >= 1, "{expect}: a seal");
        // the project: the full dump's bound plan, sealed by increment 1; the holder: increment
        // 1's bound plan, which a second source's seal names in increment 2
        assert!(
            crossing >= 1,
            "{expect}: a seal names an earlier file's plan"
        );
        // the indexes a reindex rebuilds are the load's
        let tables = r.store().index_dump_for_tests().unwrap();
        r.reindex().unwrap();
        assert_eq!(
            r.store().index_dump_for_tests().unwrap(),
            tables,
            "{expect}"
        );
        r.close().unwrap();
        // each fixed stop keeps the plans before it, as the expect file pins them
        let want = read_expect(expect);
        for name in ["held_stop", "missing_stop"] {
            let to = d.path().join(format!("{expect}-{name}"));
            let (got, upgrade) = stopped(&files, &want[name], &to, holder, false);
            check_stop(&got, &want[name], upgrade, &format!("{expect}: {name}"));
            assert_eq!(got["store"]["plans"], want[name]["store"]["plans"]);
        }
    }
}

/// The scalar store format of a released dump's header.
fn legacy(s: &DumpStore) -> u32 {
    match s {
        DumpStore::Legacy(n) => *n,
        DumpStore::Features(_) => panic!("a released pair holds a scalar store"),
    }
}
