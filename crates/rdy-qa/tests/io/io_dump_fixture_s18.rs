//! rf4: the dump promise for dump format 3 and store format 18 (D-69AY3: "every later rdy reads
//! every dump format that rdy has released"; D-4WC6R: "Store format 18 adds this kind. No other
//! record encoding changes."; D-F0904). The pair `(3, 18)` is released with twelve files and this
//! file. Write them once: never rewrite them, and never regenerate them to fix a later reader.
//!
//! The members follow the `(3, 17)` set (`io_dump_fixture_s17`), built by the same builders
//! (`rdy_qa::chain_fixture`) with their store format 18 stages (`project_chain_v18`,
//! `holder_chain_v18`; the standalone builders are shared):
//!
//! - `v3-s18-chain-full`, `-chain-1` and `-chain-2` are a full dump and two increments of one
//!   project. The full dump holds the reflects fixture's records
//!   (`every_kind::add_reflects_fixture_records`): a source, a finding whose evidence quotes and
//!   cites it (and pins it), a decision of authority basis `delegated` with its attestation that
//!   depends on the finding, and a claim that reflects the source; increment 1 holds a
//!   source-only and a code-only Reflects change of the claim (`rewrite_reflects_fixture`), and
//!   increment 2 the source's change and a sealed plan that names the earlier decision and claim
//!   (`finish_reflects_fixture`). The every-kind project's pair review keeps its citations.
//! - `v3-s18` is the standalone full fixture, built as `v3-s17` is: it keeps both forms.
//! - `v3-s18-holder-chain-*` and `v3-s18-holder` are the same for a holder, which takes the three
//!   stages' tag histories (`chain_fixture::REFLECTS_TAGS`) through an export, a batch check and
//!   a take at the same boundaries.
//!
//! Every expect file pins what a load of its files must give, as the `(3, 17)` set pins it, and
//! the reflects facts: every Reflects change, every version of a source, a finding, a delegated
//! decision and a block that reflects a source, and the seals that name a delegated decision.
//! A later store format adds kinds and refs, never re-encodes (D-F0904): a load then appends one
//! upgrade batch after the source log or the carried batch, and the checks allow exactly that
//! batch.
use crate::io_dump_fixture_s15::{first_start_logs_moves, fixture, without_moves};
use rdy_engine::Rdy;
use rdy_engine::dump_stop::DumpStop;
use rdy_engine::load::{LoadSummary, load_chain};
use rdy_model::Hash;
use rdy_model::redaction::RedactionCarried;
use rdy_qa::chain_fixture::{self, V18_KINDS};
use rdy_qa::every_kind;
use rdy_store::dump::{DUMP_FORMAT, DumpItem, DumpReader, read_envelope};
use rdy_store::format::{DumpStore, REF_STORE_FEATURES, baseline_features};
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::{Location, on_segment_write, seg_path};
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const FULL: &str = "v3-s18.rdydump";
const FULL_EXPECT: &str = "v3-s18.expect.json";
const CHAIN_FULL: &str = "v3-s18-chain-full.rdydump";
const CHAIN_1: &str = "v3-s18-chain-1.rdydump";
const CHAIN_2: &str = "v3-s18-chain-2.rdydump";
const CHAIN_EXPECT: &str = "v3-s18-chain.expect.json";
const HOLDER: &str = "v3-s18-holder.rdydump";
const HOLDER_EXPECT: &str = "v3-s18-holder.expect.json";
const HOLDER_CHAIN_FULL: &str = "v3-s18-holder-chain-full.rdydump";
const HOLDER_CHAIN_1: &str = "v3-s18-holder-chain-1.rdydump";
const HOLDER_CHAIN_2: &str = "v3-s18-holder-chain-2.rdydump";
const HOLDER_CHAIN_EXPECT: &str = "v3-s18-holder-chain.expect.json";

/// Each member of the pair `(3, 18)` and its blake3, set when it was written. A change here
/// needs a founder decision.
pub(crate) const MEMBERS: [(&str, &str); 12] = [
    (
        FULL,
        "b69978453eaf0bc06b5e8040cd152836c1a290a913603cd53a5a2e5ece53c615",
    ),
    (
        FULL_EXPECT,
        "c2bf21e87c2c4babf30750ec8d619f204367ea8d94eeae600ff394f1429603c0",
    ),
    (
        CHAIN_FULL,
        "7c734b3043f0b5e3873621eb97799ae7c0aab1638f1f130c9a268da9ef6e0002",
    ),
    (
        CHAIN_1,
        "b1203e3fdc5bd5a8f15622d23b4cc8951bfe193ac59b23372900aecd81ceb947",
    ),
    (
        CHAIN_2,
        "5837691a6d83e7537fb4e62211aaaf54f33e9948e41be0343fc23bc6f85c28e9",
    ),
    (
        CHAIN_EXPECT,
        "1d93b0abc3570711b46075d5405b4df54af86fd9eeb45512ab8d511e930c7785",
    ),
    (
        HOLDER,
        "0ba16663f3cc01684e1a01e36d2941cb7ae4f92d6940be4cc3a4b833ad3afbe7",
    ),
    (
        HOLDER_EXPECT,
        "c6cad8a5b549df4c840ae698c8d5e6cd67830525370e76fc4f372ba638a7dccf",
    ),
    (
        HOLDER_CHAIN_FULL,
        "2ac0183140a5c494bcd6d63e41eb6cc3437d684096b904d856a453be9bcfd907",
    ),
    (
        HOLDER_CHAIN_1,
        "06a8b3bfa187101dc1179d0b9b7708e86a826068bf767bf79cf03919fd116b9e",
    ),
    (
        HOLDER_CHAIN_2,
        "d160d815ecde5f20f4eda78cc1b0710199b56add6e5063916b61a7f25b74e974",
    ),
    (
        HOLDER_CHAIN_EXPECT,
        "07ae3d4064f82907bbb9e4be41a062bad173d7a6dbe86265302db67607410b88",
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
pub(crate) const SEGMENT_MAX: u64 = chain_fixture::SEGMENT_MAX;

pub(crate) fn chain_files() -> Vec<PathBuf> {
    [CHAIN_FULL, CHAIN_1, CHAIN_2].map(fixture).to_vec()
}

pub(crate) fn holder_chain_files() -> Vec<PathBuf> {
    [HOLDER_CHAIN_FULL, HOLDER_CHAIN_1, HOLDER_CHAIN_2]
        .map(fixture)
        .to_vec()
}

pub(crate) fn read_expect(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture(name)).unwrap()).unwrap()
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn loc(l: Location) -> Value {
    json!([l.segment, l.offset])
}

pub(crate) fn loc_of(v: &Value) -> Location {
    Location {
        segment: v[0].as_u64().unwrap().try_into().unwrap(),
        offset: v[1].as_u64().unwrap(),
    }
}

pub(crate) fn open(dir: &Path, holder: bool) -> Rdy {
    if holder {
        Rdy::open_holder(dir).unwrap()
    } else {
        Rdy::open(dir).unwrap()
    }
}

pub(crate) fn chain_load(files: &[PathBuf], to: &Path, stop: Option<&str>) -> LoadSummary {
    let stop = stop.map(|s| DumpStop::parse(s).unwrap());
    load_chain(files, to, stop.as_ref()).unwrap_or_else(|e| panic!("the load failed: {e:?}"))
}

// ---------------------------------------------------------------------------------------------
// facts

/// A dump's store as the expect files write it: a scalar store format as its number, and a
/// feature store as `{"features": <manifest>}`.
pub(crate) fn store_json(s: &DumpStore) -> Value {
    match s {
        DumpStore::Legacy(n) => json!(n),
        DumpStore::Features(m) => json!({ "features": m }),
    }
}

/// What the dump file `p` holds, by its envelope and a whole read: its formats, chain facts,
/// trailer, length, batch and frame counts, project file names, each batch's start and its
/// dropped frames' count. Its own name is `name`.
pub(crate) fn file_facts(name: &str, p: &Path) -> Value {
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
        "store_format": store_json(&header.store),
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
pub(crate) fn segment_digests(project: &Path, end: Location) -> Value {
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
pub(crate) fn tail_bytes(project: &Path, from: Location, to: Option<Location>) -> Vec<u8> {
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
pub(crate) fn removal_facts(r: &Rdy, upto: Location) -> Value {
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
pub(crate) fn carried_facts(at: Location, hash: Hash, payload: &[u8]) -> Value {
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
pub(crate) fn canon_blocks(r: &Rdy) -> Value {
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
pub(crate) fn store_facts(r: &Rdy, upto: Location) -> Value {
    json!({
        "facts": every_kind::facts(r),
        "identity": every_kind::identity_facts(r, Some(upto)),
        "views": r.read().view_names(),
        "projections": every_kind::projections(r),
        "canon_blocks": canon_blocks(r),
        "removals": removal_facts(r, upto),
        "plans": plan_facts(r, upto),
        "reflects": reflects_facts(r, upto),
        "features": every_kind::feature_facts(r, upto),
    })
}

/// The store format 18 facts of the log of `r` before `upto`, in log order: every change whose
/// justification is a Reflects (its place, hash, view, the justification as the engine reads
/// it, and the blocks it writes); every present version of a source, a finding, a decision of
/// authority basis `delegated` and a block with a `reflects` link (its place, id, type, version,
/// pins, links and the fields that make it so); and every PLAN seal that names such a decision
/// (its hash and the seal's decisions and items).
pub(crate) fn reflects_facts(r: &Rdy, upto: Location) -> Value {
    let (mut changes, mut blocks, mut seals) = (vec![], vec![], vec![]);
    let mut delegated = BTreeSet::new();
    r.store()
        .reader()
        .for_each_record_from(ORIGIN, upto, |at, f, _| {
            let StoredFrame::Present {
                kind,
                hash,
                payload,
            } = f
            else {
                return Ok(());
            };
            match kind {
                kinds::CHANGE => {
                    let c: rdy_model::Change = rdy_store::bounded::decode(payload).unwrap();
                    if let Some(j @ rdy_model::Justification::Reflects { .. }) = &c.justification {
                        changes.push(json!({
                            "at": loc(at),
                            "hash": hash.to_string(),
                            "view": c.view,
                            "justification": rdy_engine::JustificationInfo::from(j),
                            "blocks": c.ops.iter().map(|o| o.block_id().to_string()).collect::<Vec<_>>(),
                        }));
                    }
                }
                kinds::BLOCK => {
                    let b: rdy_model::Block = rdy_store::bounded::decode(payload).unwrap();
                    let is_delegated = b.block_type == "decision"
                        && b.fields.get("authority_basis")
                            == Some(&rdy_model::FieldValue::Str("delegated".into()));
                    let supporting = matches!(b.block_type.as_str(), "source" | "finding");
                    let reflecting = b.links.contains_key("reflects");
                    if is_delegated {
                        delegated.insert(b.id.clone());
                    }
                    if is_delegated || supporting || reflecting {
                        let keep: BTreeMap<&String, &rdy_model::FieldValue> = b
                            .fields
                            .iter()
                            .filter(|(k, _)| {
                                matches!(
                                    k.as_str(),
                                    "authority_basis"
                                        | "attested_by"
                                        | "evidence"
                                        | "kind"
                                        | "version"
                                        | "reflects"
                                        | "reflects_at"
                                )
                            })
                            .collect();
                        blocks.push(json!({
                            "at": loc(at),
                            "id": b.id.to_string(),
                            "type": b.block_type,
                            "version": hash.to_string(),
                            "pins": b.pins.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>(),
                            "links": b.links.iter().map(|(k, v)| (k.clone(), v.iter().map(|l| l.target.to_string()).collect::<Vec<_>>())).collect::<BTreeMap<_, _>>(),
                            "fields": keep,
                        }));
                    }
                }
                kinds::PLAN => {
                    let p = rdy_store::plan::decode_plan(payload).unwrap();
                    if let Some(s) = &p.seal
                        && s.decisions.iter().any(|d| delegated.contains(d))
                    {
                        seals.push(json!({
                            "at": loc(at),
                            "hash": hash.to_string(),
                            "landing": s.landing.to_string(),
                            "decisions": s.decisions.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
                            "items": s.items.iter().map(|i| json!([i.kind.as_str(), i.subject, i.cause.to_string(), i.edge, i.outcome.as_str()])).collect::<Vec<_>>(),
                        }));
                    }
                }
                _ => {}
            }
            Ok(())
        })
        .unwrap();
    json!({"changes": changes, "blocks": blocks, "seals": seals})
}

/// Every PLAN record of the log of `r` before `upto`, in log order: its place, hash, view,
/// chain links, bound proposal, its seal's landing, version and decisions, and its answers' and
/// items' acts and outcomes; and every row of the four plan tables, as stored (hex).
pub(crate) fn plan_facts(r: &Rdy, upto: Location) -> Value {
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
pub(crate) fn summary_facts(s: &LoadSummary) -> Value {
    json!({
        "dump_format": s.dump_format,
        "store_format": store_json(&s.store_format),
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
pub(crate) fn chain_end(files: &[PathBuf]) -> Location {
    let mut f = std::fs::File::open(files.last().unwrap()).unwrap();
    read_envelope(&mut f).unwrap().1.log_end
}

/// A whole load of `files` into `to`: its summary, the loaded log's segment digests up to the
/// source end, and the store's facts.
pub(crate) fn whole_facts(files: &[PathBuf], to: &Path, holder: bool) -> Value {
    let s = chain_load(files, to, None);
    whole_facts_of(&s, files, to, holder)
}

/// The facts of the whole load of `files` into `to` that gave `s`.
pub(crate) fn whole_facts_of(s: &LoadSummary, files: &[PathBuf], to: &Path, holder: bool) -> Value {
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
pub(crate) fn stop_facts(files: &[PathBuf], selector: &str, to: &Path, holder: bool) -> Value {
    let s = chain_load(files, to, Some(selector));
    stop_facts_of(&s, selector, to, holder, None)
}

/// The facts of the load into `to`, stopped at `selector`, that gave `s`. `end` is the end of
/// the carried batch as the expect file pins it: the tail and the store's facts stop there, and
/// any upgrade batch after it is checked apart (`upgrade_after`). With no `end` (the writer, of
/// this rdy's own store format), the carried batch ends at the log's end.
pub(crate) fn stop_facts_of(
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
pub(crate) fn chain_expect(
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
pub(crate) fn standalone_expect(
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
pub(crate) fn write_new(p: &Path, bytes: &[u8]) {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)
        .unwrap_or_else(|e| panic!("{}: never rewrite a fixture member: {e}", p.display()));
    f.write_all(bytes).unwrap();
    f.sync_all().unwrap();
}

pub(crate) fn pretty(v: &Value) -> Vec<u8> {
    (serde_json::to_string_pretty(v).unwrap() + "\n").into_bytes()
}

/// Writes the twelve members of the pair `(3, 18)` once. Run by hand, never by the gate:
/// `cargo test -p rdy-qa --release --test io rf4_write_format18_fixtures_once -- --ignored`.
/// It refuses a member that exists.
#[test]
#[ignore]
fn rf4_write_format18_fixtures_once() {
    for (m, _) in MEMBERS {
        assert!(
            !fixture(m).exists(),
            "{m}: the v3-s18 fixtures are written once: never rewrite them"
        );
    }
    assert_eq!(
        DUMP_FORMAT, 3,
        "the v3-s18 fixtures hold dump format 3 and store format 18: this rdy writes another \
         format, and a released set is never regenerated"
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

    let c = chain_fixture::project_chain_v18();
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

    let (hc, ups) = chain_fixture::holder_chain_v18();
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

/// One frozen input of the pair `(3, 18)`: its files in order, their names, its expect file, its
/// mode, its fixed stops, and the canaries that only removed payloads hold.
pub(crate) struct Input {
    pub(crate) names: &'static [&'static str],
    pub(crate) expect: &'static str,
    pub(crate) holder: bool,
    pub(crate) stops: &'static [&'static str],
    pub(crate) canaries: &'static [&'static [u8]],
}

impl Input {
    fn files(&self) -> Vec<PathBuf> {
        self.names.iter().map(|n| fixture(n)).collect()
    }
}

pub(crate) const PROJECT_CANARIES: &[&[u8]] = &[
    chain_fixture::EARLY_CANARY.as_bytes(),
    chain_fixture::LATE_CANARY.as_bytes(),
    chain_fixture::BLOB_CANARY,
    chain_fixture::STANDALONE_CANARY.as_bytes(),
    every_kind::CANARY_BODY.as_bytes(),
    every_kind::CANARY_FIELD.as_bytes(),
];
pub(crate) const HOLDER_CANARIES: &[&[u8]] = &[
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
pub(crate) fn segs(project: &Path) -> BTreeMap<String, Vec<u8>> {
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

pub(crate) fn holds(project: &Path, needle: &[u8]) -> bool {
    segs(project)
        .values()
        .any(|b| b.windows(needle.len()).any(|w| w == needle))
}

/// `want`, the facts of a store as the expect file pins them, as this rdy must read them: with
/// the features ref its load's upgrade batch writes (the historical `meta/store_format` stays),
/// and with the frames of `upgrade` upgrade batch data frames counted.
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
        "reflects",
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
    let format = 18;
    let end = loc_of(&want["end"]);
    let got = stop_facts_of(&s, selector, to, holder, Some(end));
    (got, upgrade_after(to, end, holder, format))
}

/// The frames of the expect file's files: their sum.
pub(crate) fn expect_frames(expect: &Value) -> Value {
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
pub(crate) struct Watch(pub(crate) Rc<Cell<bool>>);

impl Drop for Watch {
    fn drop(&mut self) {
        on_segment_write(None);
    }
}

pub(crate) fn watch(needles: Vec<&'static [u8]>) -> Watch {
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
pub(crate) fn tail_carried(stop: &Value) -> Vec<Value> {
    let end = loc_of(&stop["position"]);
    stop["store"]["removals"]["carried"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| loc_of(&c["at"]) >= end)
        .cloned()
        .collect()
}

pub(crate) fn helds(c: &Value) -> Vec<bool> {
    c["record"]["spans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["held"].as_bool().unwrap())
        .collect()
}

/// Whether the store at `stop` holds the payload with `canary`, as the expect file pins it: a
/// held span of a carried record, or a payload no removal selected before the stop.
pub(crate) fn stop_holds(stop: &Value, input: &Input, canary: &[u8]) -> bool {
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
fn rf4_format18_fixture_bytes_never_change() {
    // A change to any member needs a founder decision. The checksums are fixed here, apart from
    // any writer: a later writer that writes other bytes changes nothing in this check.
    for (name, digest) in MEMBERS {
        let p = fixture(name);
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(Hash::of(&bytes).to_string(), digest, "{name}");
        // the writer refuses a member that exists, and leaves its bytes as they were
        let refused = std::panic::catch_unwind(|| write_new(&p, b"never"));
        assert!(
            refused.is_err(),
            "{name}: the writer refuses an existing member"
        );
        assert_eq!(std::fs::read(&p).unwrap(), bytes, "{name}");
    }
}

#[test]
fn rf4_format18_project_and_holder_load_with_every_hash() {
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
                (Some(3), Some(18)),
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
        let upgrade = upgrade_after(&to, chain_end(&files), input.holder, 18);
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
            let upgrade = upgrade_after(&to, chain_end(&files[..1]), input.holder, 18);
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

/// The rows of `v` (an array).
fn rows(v: &Value) -> &Vec<Value> {
    v.as_array().unwrap()
}

/// The hashes of the frames of kind `kind` of each of `files`, in order.
fn kind_by_file(files: &[PathBuf], kind: u8) -> Vec<BTreeSet<Hash>> {
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
                            .filter(|f| f.kind() == kind)
                            .map(|f| *f.hash()),
                    );
                }
            }
            out
        })
        .collect()
}

/// Whether some REVIEW record of the store at `dir` cites a block (the renamed review slot).
fn holds_citations(dir: &Path) -> bool {
    let r = open(dir, false);
    let mut found = false;
    r.store()
        .reader()
        .for_each_record_from(ORIGIN, r.store().log_end(), |_, f, _| {
            if let StoredFrame::Present {
                kind: kinds::REVIEW,
                payload,
                ..
            } = f
            {
                let v: rdy_model::Review = rdy_store::bounded::decode(payload).unwrap();
                found |= !v.citations.is_empty();
            }
            Ok(())
        })
        .unwrap();
    r.close().unwrap();
    found
}

#[test]
fn rf4_format18_fixtures_cover_reflects_supporting_and_basis() {
    // the standalone project holds every kind of store format 18 (those of 17)
    let expect = read_expect(FULL_EXPECT);
    let objects = &expect["whole"]["store"]["facts"]["objects"];
    for k in V18_KINDS {
        assert!(
            objects[k.to_string()].as_u64().unwrap_or(0) >= 1,
            "kind {k}"
        );
    }
    assert!(objects.get("12").is_none(), "kind 12 is unassigned");
    for name in [
        CHAIN_EXPECT,
        FULL_EXPECT,
        HOLDER_CHAIN_EXPECT,
        HOLDER_EXPECT,
    ] {
        let rf = &read_expect(name)["whole"]["store"]["reflects"];
        // both Reflects forms: a source alone, and code alone
        let forms: Vec<&Value> = rows(&rf["changes"])
            .iter()
            .map(|c| &c["justification"]["reflects"])
            .collect();
        assert!(
            forms
                .iter()
                .any(|f| f["code"] == false && !rows(&f["sources"]).is_empty()),
            "{name}: a source-only Reflects"
        );
        assert!(
            forms
                .iter()
                .any(|f| f["code"] == true && rows(&f["sources"]).is_empty()),
            "{name}: a code-only Reflects"
        );
        let blocks = rows(&rf["blocks"]);
        let of = |ty: &str| -> Vec<&Value> { blocks.iter().filter(|b| b["type"] == ty).collect() };
        // a source, changed once
        let sources = of("source");
        assert_eq!(sources.len(), 2, "{name}: the source and its change");
        let source = sources[0]["id"].as_str().unwrap();
        assert_eq!(sources[1]["id"], source);
        assert_eq!(
            sources[1]["fields"]["version"],
            json!({"Str": "2"}),
            "{name}"
        );
        // a finding whose evidence quotes and cites the source, and pins it
        let finding = of("finding")[0];
        let evidence = serde_json::to_string(&finding["fields"]["evidence"]).unwrap();
        assert!(evidence.contains(every_kind::REFLECTS_QUOTE), "{name}");
        assert!(evidence.contains(&format!("{{{{{source}}}}}")), "{name}");
        assert!(
            finding["pins"].get(source).is_some(),
            "{name}: an evidence pin"
        );
        let finding = finding["id"].as_str().unwrap();
        // a delegated decision with its attestation, which depends on the finding and pins it
        let decision = of("decision")[0];
        assert_eq!(
            decision["fields"]["authority_basis"],
            json!({"Str": "delegated"})
        );
        assert_eq!(
            decision["fields"]["attested_by"],
            json!({"Str": every_kind::REFLECTS_ATTESTED_BY})
        );
        assert_eq!(decision["links"]["depends_on"], json!([finding]), "{name}");
        assert!(decision["pins"].get(finding).is_some(), "{name}");
        let decision = decision["id"].as_str().unwrap();
        // the claim reflects the source, then declares globs and a recorded code state
        let contents = of("claim");
        assert!(
            contents
                .iter()
                .all(|c| c["links"]["reflects"] == json!([source])
                    && c["pins"].get(source).is_some()),
            "{name}"
        );
        let content = contents[0]["id"].as_str().unwrap();
        assert!(
            contents
                .iter()
                .any(|c| c["fields"].get("reflects").is_some()
                    && c["fields"].get("reflects_at").is_some()),
            "{name}: the code case's globs and reflects_at"
        );
        for c in rows(&rf["changes"]) {
            assert_eq!(c["blocks"], json!([content]), "{name}");
        }
        // the last seal (the source's change) names the decision as the cause and the claim as
        // the subject; the landings before it sealed their own engine plans
        let last = rows(&rf["seals"]).last().expect("a seal");
        assert_eq!(last["decisions"], json!([decision]), "{name}");
        assert_eq!(
            last["items"],
            json!([["confirm", content, decision, "reflects", "confirmed"]]),
            "{name}"
        );
    }
    // the standalone fixtures keep both forms; a project's pair review keeps its citations
    let d = tempfile::tempdir().unwrap();
    let to = d.path().join("full");
    chain_load(&[fixture(FULL)], &to, None);
    assert!(holds_citations(&to), "the renamed review slot");
    // each chain: the seal is in increment 2 and resolves the decision of the full dump, its
    // landing and its chain once the files load as one log
    for (expect, files, holder) in [
        (CHAIN_EXPECT, chain_files(), false),
        (HOLDER_CHAIN_EXPECT, holder_chain_files(), true),
    ] {
        let rf = read_expect(expect)["whole"]["store"]["reflects"].clone();
        let seal: Hash = rows(&rf["seals"]).last().unwrap()["hash"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let plans = kind_by_file(&files, kinds::PLAN);
        assert!(
            plans[2].contains(&seal),
            "{expect}: the seal is in increment 2"
        );
        let blocks = kind_by_file(&files, kinds::BLOCK);
        let decision = rows(&rf["blocks"])
            .iter()
            .find(|b| b["type"] == "decision")
            .unwrap();
        let first: Hash = decision["version"].as_str().unwrap().parse().unwrap();
        assert!(
            blocks[0].contains(&first),
            "{expect}: the decision is in the full dump"
        );
        let changes = kind_by_file(&files, kinds::CHANGE);
        for c in rows(&rf["changes"]) {
            let h: Hash = c["hash"].as_str().unwrap().parse().unwrap();
            assert!(
                changes[1].contains(&h),
                "{expect}: a Reflects change in increment 1"
            );
        }
        let to = d.path().join(format!("{expect}-whole"));
        chain_load(&files, &to, None);
        let r = open(&to, holder);
        let end = r.store().log_end();
        let p = r.store().plan(&seal).unwrap();
        let s = p.seal.as_ref().unwrap();
        assert_eq!(
            r.store().plan_of_landing(&s.landing, end).unwrap(),
            Some(seal)
        );
        assert!(r.store().has_kind(kinds::PROPOSAL, &s.version).unwrap());
        for link in [p.stable, p.prev].into_iter().flatten() {
            assert!(r.store().has_kind(kinds::PLAN, &link).unwrap(), "{expect}");
        }
        for dec in &s.decisions {
            assert!(
                r.store()
                    .plans_of_decision(dec, end)
                    .unwrap()
                    .contains(&seal)
            );
        }
        for c in rows(&rf["changes"]) {
            let h: Hash = c["hash"].as_str().unwrap().parse().unwrap();
            let got = r.store().change(&h).unwrap();
            assert!(matches!(
                got.justification,
                Some(rdy_model::Justification::Reflects { .. })
            ));
            assert_eq!(got.id(), h);
        }
        r.close().unwrap();
    }
}
