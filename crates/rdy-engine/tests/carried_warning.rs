//! hc6 (D-QF2HQ: "rdy status and rdy fsck repeat it while a span with held: true still holds its
//! payload"; D-0VG72; D-CC90E): the carried-removal warning is derived from the log and the
//! current mask. It is stored nowhere, it requests and approves nothing, and in a Project it ends
//! at a new local apply that a person approves.
//!
//! Each store is built through the checked path of a stopped load: the project's store is moved
//! aside and ingested again, with carried records over the secret block's frame and the change
//! frame that wrote it.
use rdy_engine::carried_warning::{self, W_STOP_BEFORE_REDACTION};
use rdy_engine::redaction::RedactInput;
use rdy_engine::*;
use rdy_model::redaction::{
    BlockResidue, CarriedSpan, ChangeResidue, FramePlace, ReasonClass, RedactTarget,
    RedactionCarried, RedactionResidue,
};
use rdy_model::{Block, BlockId, Change, Hash, Op, Provenance, Role};
use rdy_store::dump::Frame;
use rdy_store::holder::StoreMode;
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::redaction_carried::PROJECT_FIX;
use rdy_store::segment::Location;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

const SECRET: &str = "The launch code is ZEBRA-7731.";
const SECOND: &str = "The backup code is ZEBRA-9999.";
const PERSON: &str = "human:t";

fn writer() -> Session {
    Session::human(PERSON.into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human(PERSON.into(), Role::Approver).unwrap()
}

fn agent() -> Session {
    Session::agent("agent:a".into(), Some("run-1".into()), Role::Writer).unwrap()
}

/// The frames a carried record can name: the secret block's and the change that wrote it.
struct Frames {
    block: (Location, Hash, u32, RedactionResidue),
    change: (Location, Hash, u32, RedactionResidue),
    id: BlockId,
    version: Hash,
}

/// A project whose canon holds two secret prose blocks, each written by a change of its own,
/// closed. Returns the ids of the blocks.
fn project() -> (tempfile::TempDir, BlockId, BlockId) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = r
        .write(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "design", "title": "Design"}},
                    {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Doc", "doc_type": "page"}},
                    {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": SECRET}
                ]))
                .unwrap(),
                justification: None,
                message: "hc6".into(),
            },
        )
        .unwrap();
    let w2 = r
        .write(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "prose", "id": "$q", "parent": w.created["$d"], "body": SECOND}
                ]))
                .unwrap(),
                justification: None,
                message: "hc6 second".into(),
            },
        )
        .unwrap();
    let p = r
        .propose(&writer(), "work", "Base", Some("test summary"))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    r.close().unwrap();
    (
        dir,
        BlockId::parse(&w.created["$p"]).unwrap(),
        BlockId::parse(&w2.created["$q"]).unwrap(),
    )
}

/// The id of a new residue `r`: the hash of its frame in its kind's latest encoding, as an rdy of
/// this version writes one (`rdy_store::codec`).
fn residue_id(r: &RedactionResidue) -> Hash {
    Hash::of(&rdy_store::codec::frame_latest(kinds::REDACTION_RESIDUE, enc(r)).1)
}

/// `value`'s record encoding, through the model's bounded encoder.
fn enc<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let mut buf = vec![0u8; 1 << 20];
    rdy_model::redaction::encode_into(value, &mut buf)
        .unwrap()
        .to_vec()
}

fn frames_of(store: &Path, needle: &str) -> Frames {
    let s = rdy_store::Store::open(store).unwrap();
    let r = s.reader();
    let mut block = None;
    let mut change = None;
    let mut version = None;
    let mut id = None;
    r.for_each_record_from(
        Location {
            segment: 0,
            offset: 0,
        },
        s.log_end(),
        |loc, f, _| {
            if let StoredFrame::Present {
                kind,
                hash,
                payload,
            } = f
            {
                if kind == kinds::BLOCK
                    && let Ok(b) = rdy_store::bounded::decode::<Block>(payload)
                    && b.body.contains(needle)
                {
                    version = Some(b.version_hash());
                    id = Some(b.id.clone());
                    block = Some((
                        loc,
                        hash,
                        payload.len() as u32,
                        RedactionResidue::Block(BlockResidue::of(&b)),
                    ));
                }
                if kind == kinds::CHANGE
                    && let Ok(c) = rdy_store::bounded::decode::<Change>(payload)
                    && c.ops.iter().any(|op| match op {
                        Op::Create { block } | Op::Update { block, .. } => {
                            block.body.contains(needle)
                        }
                        Op::Delete { .. } => false,
                    })
                {
                    change = Some((
                        loc,
                        hash,
                        payload.len() as u32,
                        RedactionResidue::Change(ChangeResidue::of(&c)),
                    ));
                }
            }
            Ok(())
        },
    )
    .unwrap();
    Frames {
        block: block.expect("the secret block frame"),
        change: change.expect("the secret change frame"),
        id: id.unwrap(),
        version: version.unwrap(),
    }
}

/// One carried record to ingest: a source apply tag, and the held flag of each frame it names
/// (None: the record does not name that frame).
#[derive(Clone, Copy)]
struct Spec {
    /// The secret whose frames the record names.
    needle: &'static str,
    apply: &'static str,
    block: Option<bool>,
    change: Option<bool>,
}

fn record_of(f: &Frames, spec: Spec) -> RedactionCarried {
    let span = |x: &(Location, Hash, u32, RedactionResidue), kind: u8, held: bool| CarriedSpan {
        place: FramePlace {
            segment: x.0.segment,
            offset: x.0.offset,
        },
        kind,
        hash: x.1,
        len: x.2,
        residue: Some(residue_id(&x.3)),
        held,
    };
    let mut spans = vec![];
    if let Some(h) = spec.block {
        spans.push(span(&f.block, kinds::BLOCK, h));
    }
    if let Some(h) = spec.change {
        spans.push(span(&f.change, kinds::CHANGE, h));
    }
    spans.sort_by_key(|s| s.place);
    RedactionCarried {
        request: Hash::of(format!("request {}", spec.apply).as_bytes()),
        apply: Hash::of(format!("apply {}", spec.apply).as_bytes()),
        landing: Hash::of(format!("landing {}", spec.apply).as_bytes()),
        approved_by: Provenance {
            actor: "human:founder".into(),
            run: None,
            role: Role::Approver,
            at_ms: 777,
        },
        reason_class: ReasonClass::Secret,
        targets: vec![RedactTarget::Version {
            id: f.id.clone(),
            version: f.version,
        }],
        spans,
    }
}

/// The project's store moved aside and ingested again with `specs` as carried records, in
/// order, each `held: false` span written as a dropped frame. Returns the records and their
/// hashes.
fn carry(dir: &Path, specs: &[Spec]) -> Vec<(Hash, RedactionCarried)> {
    let store = dir.join(".rdy").join(rdy_store::layout::STORE_DIR);
    let aside = dir.join("store-aside");
    std::fs::rename(&store, &aside).unwrap();
    let frames: Vec<Frames> = specs.iter().map(|s| frames_of(&aside, s.needle)).collect();
    let records: Vec<(Hash, RedactionCarried)> = specs
        .iter()
        .zip(&frames)
        .map(|(s, f)| {
            let r = record_of(f, *s);
            (Hash::of(&enc(&r)), r)
        })
        .collect();
    // a held: false span is a dropped frame that names its first record
    let mut drops: BTreeMap<Location, (Hash, Option<Hash>)> = BTreeMap::new();
    for (h, r) in &records {
        for sp in r.spans.iter().filter(|s| !s.held) {
            drops
                .entry(Location {
                    segment: sp.place.segment,
                    offset: sp.place.offset,
                })
                .or_insert((*h, sp.residue));
        }
    }
    let s = rdy_store::Store::open(&aside).unwrap();
    let mut batches = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |mut b| {
            let mut at = b.start;
            for fr in b.frames.iter_mut() {
                let len = fr.original_len();
                if let Some((apply, residue)) = drops.get(&at) {
                    *fr = Frame::Dropped {
                        kind: fr.kind(),
                        hash: *fr.hash(),
                        len,
                        apply: *apply,
                        residue: *residue,
                    };
                }
                at.offset += 37 + u64::from(len);
            }
            batches.push(b);
            Ok(())
        })
        .unwrap();
    drop(s);
    let mut i = rdy_store::ingest::Ingest::create(&store).unwrap();
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    let mut residues: Vec<RedactionResidue> = vec![];
    for (s, f) in specs.iter().zip(&frames) {
        if s.block.is_some() {
            residues.push(f.block.3.clone());
        }
        if s.change.is_some() {
            residues.push(f.change.3.clone());
        }
    }
    let only: Vec<RedactionCarried> = records.iter().map(|(_, r)| r.clone()).collect();
    i.append_carried(&residues, &only).unwrap();
    i.upgrade(&rdy_store::format::DumpStore::Features(
        rdy_store::format::baseline_features(),
    ))
    .unwrap();
    i.finish().unwrap();
    records
}

/// Whether any segment file of the project holds `needle`.
fn on_disk(dir: &Path, needle: &str) -> bool {
    let segs = dir.join(".rdy/store/segments");
    std::fs::read_dir(segs).unwrap().any(|e| {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
    })
}

fn input(targets: Vec<String>, dry_run: bool) -> RedactInput {
    RedactInput {
        targets,
        reason_class: "secret".into(),
        reason: "a leaked code".into(),
        dry_run,
    }
}

const BOTH_HELD: Spec = Spec {
    needle: "ZEBRA-7731",
    apply: "a",
    block: Some(true),
    change: Some(true),
};

#[test]
fn hc6_warning_has_exact_text_provenance_and_targets() {
    let (dir, _, _) = project();
    let f = frames_of(
        &dir.path().join(".rdy").join(rdy_store::layout::STORE_DIR),
        "ZEBRA-7731",
    );
    let records = carry(
        dir.path(),
        &[Spec {
            needle: "ZEBRA-7731",
            apply: "a",
            block: Some(true),
            change: Some(false),
        }],
    );
    let (carried, rec) = &records[0];
    let r = Rdy::open(dir.path()).unwrap();
    let w = r.read().carried_warnings().unwrap();
    assert_eq!(w.len(), 1);
    let w = &w[0];
    assert_eq!(w.code, "W_STOP_BEFORE_REDACTION");
    assert_eq!(w.code, W_STOP_BEFORE_REDACTION);
    assert_eq!(
        w.message,
        format!(
            "This store stops before redaction apply {} of its source, approved by human:founder at landing {}. It holds 1 removed payloads again, and 1 stay removed.",
            rec.apply, rec.landing
        )
    );
    assert_eq!(w.fix, PROJECT_FIX);
    assert_eq!(
        w.fix,
        "Propose the redaction of the listed targets again, and have a person approve it."
    );
    assert_eq!(
        w.issues,
        vec![carried_warning::CarriedTargetIssue {
            target: format!("{}@{}", f.id, f.version)
        }]
    );
    // JSON prints the four hashes in full, lowercase
    let j = serde_json::to_value(w).unwrap();
    for (k, h) in [
        ("request", rec.request),
        ("apply", rec.apply),
        ("landing", rec.landing),
        ("carried", *carried),
    ] {
        assert_eq!(j[k], json!(h.to_string()), "{k}");
        let text = j[k].as_str().unwrap();
        assert_eq!(text.len(), 64);
        assert_eq!(text, text.to_lowercase());
    }
    assert_eq!(j["code"], "W_STOP_BEFORE_REDACTION");
    assert_eq!(
        j["issues"],
        json!([{"target": format!("{}@{}", f.id, f.version)}])
    );
}

#[test]
fn hc6_load_warns_for_all_false_but_status_does_not() {
    let (dir, _, _) = project();
    let records = carry(
        dir.path(),
        &[Spec {
            needle: "ZEBRA-7731",
            apply: "a",
            block: Some(false),
            change: Some(false),
        }],
    );
    // the load reports the record that it appended, though nothing is held
    let w = carried_warning::for_load(StoreMode::Project, &records).unwrap();
    assert_eq!(w.len(), 1);
    assert!(
        w[0].message
            .ends_with("It holds 0 removed payloads again, and 2 stay removed."),
        "{}",
        w[0].message
    );
    assert_eq!(w[0].fix, PROJECT_FIX);
    let h = carried_warning::for_load(StoreMode::Holder, &records).unwrap();
    assert_eq!(
        h[0].message, w[0].message,
        "the message does not follow the mode"
    );
    assert_eq!(h[0].issues, w[0].issues);
    assert_eq!(h[0].fix, rdy_store::redaction_carried::HOLDER_FIX);
    // the later reads do not: no span holds a payload
    let r = Rdy::open(dir.path()).unwrap();
    assert!(r.read().carried_warnings().unwrap().is_empty());
    let report = r.fsck().unwrap();
    assert!(report.warnings.is_empty());
    let j = serde_json::to_value(&report).unwrap();
    assert!(j.get("warnings").is_none(), "an empty list is omitted");
}

#[test]
fn hc6_status_and_fsck_repeat_until_new_human_apply() {
    let _clock = pin_clock(1_000_000);
    let (dir, prose, _) = project();
    carry(dir.path(), &[BOTH_HELD]);
    let mut r = Rdy::open(dir.path()).unwrap();
    // every read repeats it; fsck still succeeds
    for _ in 0..2 {
        let report = r.fsck().unwrap();
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].code, W_STOP_BEFORE_REDACTION);
        assert!(
            report.warnings[0]
                .message
                .ends_with("It holds 2 removed payloads again, and 0 stay removed.")
        );
        let j = serde_json::to_value(&report).unwrap();
        assert_eq!(j["warnings"][0]["code"], "W_STOP_BEFORE_REDACTION");
        assert_eq!(r.read().carried_warnings().unwrap(), report.warnings);
    }
    assert!(on_disk(dir.path(), SECRET), "the held payload stays");
    // a person approves a new local removal of the listed target
    let res = r
        .redact_in(&writer(), "redact", input(vec![prose.to_string()], false))
        .unwrap();
    assert!(res.redaction.is_some());
    let prop = r
        .propose(&writer(), "redact", "Remove the code", Some("Removes it."))
        .unwrap();
    r.land(&approver(), &prop.id).unwrap();
    assert!(r.read().carried_warnings().unwrap().is_empty());
    assert!(r.fsck().unwrap().warnings.is_empty());
    assert!(!on_disk(dir.path(), SECRET), "the new apply erased it");
}

#[test]
fn hc6_agent_and_carried_approval_cannot_remove() {
    let _clock = pin_clock(2_000_000);
    let (dir, prose, _) = project();
    carry(dir.path(), &[BOTH_HELD]);
    let mut r = Rdy::open(dir.path()).unwrap();
    // the carried approver is a person of another store: it erased nothing here
    assert!(on_disk(dir.path(), SECRET));
    assert_eq!(r.read().block(&Scope::Canon, &prose).unwrap().body, SECRET);
    assert_eq!(r.read().carried_warnings().unwrap().len(), 1);
    // an agent can record a request but cannot land it, and the warning is not a request
    let res = r
        .redact(&agent(), input(vec![prose.to_string()], false))
        .unwrap();
    assert!(res.redaction.is_some());
    let prop = r
        .propose(&agent(), "run-1", "Remove the code", Some("Removes it."))
        .unwrap();
    let end = r.store().log_end();
    let e = r.land(&agent(), &prop.id).unwrap_err();
    assert_eq!(e.code, E_REDACTION_HUMAN, "{e}");
    assert_eq!(r.store().log_end(), end, "nothing is written");
    assert!(on_disk(dir.path(), SECRET));
    assert_eq!(r.read().block(&Scope::Canon, &prose).unwrap().body, SECRET);
    let warnings = r.read().carried_warnings().unwrap();
    assert_eq!(warnings.len(), 1, "the warning stands");
    assert_eq!(r.fsck().unwrap().warnings, warnings);
}

#[test]
fn hc6_new_report_skips_already_removed_spans() {
    let (dir, first, second) = project();
    let both = vec![first.to_string(), second.to_string()];
    let report = |dir: &Path, targets: Vec<String>| {
        let r = Rdy::open(dir).unwrap();
        let rep = r.read().redaction_report(&input(targets, true)).unwrap();
        (
            rep.drop_frames,
            rep.drop_bytes,
            rep.already_redacted
                .iter()
                .map(rdy_engine::redaction::targets::selector_text)
                .collect::<Vec<_>>(),
        )
    };
    // before the load: each secret has a block frame and a change frame to drop
    let (frames, bytes, redacted) = report(dir.path(), both.clone());
    assert!(redacted.is_empty());
    assert_eq!(frames, 4);
    // the carried record removes the first secret's frames (zeros) and holds the second's
    carry(
        dir.path(),
        &[
            Spec {
                needle: "ZEBRA-7731",
                apply: "a",
                block: Some(false),
                change: Some(false),
            },
            Spec {
                needle: "ZEBRA-9999",
                apply: "b",
                block: Some(true),
                change: Some(true),
            },
        ],
    );
    // the carried zeros already join the mask: the report skips the first secret, names it as
    // already removed, and counts only the frames that still hold a payload
    let (f2, b2, redacted) = report(dir.path(), both.clone());
    assert_eq!(f2, 2, "only the second secret's frames");
    assert!(b2 < bytes);
    assert_eq!(redacted, vec![first.to_string()]);
    // the second secret alone reports the same drops; the first alone has nothing to drop
    let (alone, alone_bytes, none) = report(dir.path(), vec![second.to_string()]);
    assert_eq!((alone, alone_bytes), (f2, b2));
    assert!(none.is_empty());
    let (zero, zero_bytes, removed) = report(dir.path(), vec![first.to_string()]);
    assert_eq!((zero, zero_bytes), (0, 0));
    assert_eq!(removed, vec![first.to_string()]);
    // the warning is for the held span only
    let r = Rdy::open(dir.path()).unwrap();
    let w = r.read().carried_warnings().unwrap();
    assert_eq!(w.len(), 1);
    assert!(
        w[0].message
            .ends_with("It holds 2 removed payloads again, and 0 stay removed.")
    );
}

#[test]
fn hc6_warnings_follow_source_order_and_survive_reindex() {
    let (dir, _, _) = project();
    // two source applies, the second named first in the log
    let specs = [
        Spec {
            needle: "ZEBRA-7731",
            apply: "second",
            block: None,
            change: Some(true),
        },
        Spec {
            needle: "ZEBRA-7731",
            apply: "first",
            block: Some(true),
            change: None,
        },
    ];
    let records = carry(dir.path(), &specs);
    let mut r = Rdy::open(dir.path()).unwrap();
    let applies = |r: &Rdy| -> Vec<Hash> {
        r.read()
            .carried_warnings()
            .unwrap()
            .iter()
            .map(|w| w.apply)
            .collect()
    };
    let expected: Vec<Hash> = records.iter().map(|(_, rec)| rec.apply).collect();
    assert_eq!(applies(&r), expected, "physical order of the carried batch");
    let before = r.read().carried_warnings().unwrap();
    r.reindex().unwrap();
    assert_eq!(r.read().carried_warnings().unwrap(), before);
    r.close().unwrap();
    // and a fresh open
    let r = Rdy::open(dir.path()).unwrap();
    assert_eq!(r.read().carried_warnings().unwrap(), before);
}

#[test]
fn hc6_one_target_with_a_removed_block_and_a_held_change_is_reapproved() {
    let _clock = pin_clock(3_000_000);
    let (dir, first, _) = project();
    // one target: the carried record zeros its block frame and holds the change that wrote it
    carry(
        dir.path(),
        &[Spec {
            needle: "ZEBRA-7731",
            apply: "mixed",
            block: Some(false),
            change: Some(true),
        }],
    );
    assert!(on_disk(dir.path(), SECRET), "the change frame holds it");
    let mut r = Rdy::open(dir.path()).unwrap();
    assert_eq!(r.read().carried_warnings().unwrap().len(), 1);
    // the report keeps the target, skips the removed block span and counts the held change
    let rep = r
        .read()
        .redaction_report(&input(vec![first.to_string()], true))
        .unwrap();
    assert_eq!(rep.drop_frames, 1, "only the change frame");
    assert!(
        rep.already_redacted.is_empty(),
        "{:?}",
        rep.already_redacted
    );
    // a person approves a new local removal of the target: it erases the retained copy
    let res = r
        .redact_in(&writer(), "redact", input(vec![first.to_string()], false))
        .unwrap();
    assert!(res.redaction.is_some());
    let prop = r
        .propose(&writer(), "redact", "Remove the code", Some("Removes it."))
        .unwrap();
    r.land(&approver(), &prop.id).unwrap();
    assert!(!on_disk(dir.path(), SECRET), "the fresh apply erased it");
    assert!(r.read().carried_warnings().unwrap().is_empty());
    assert!(r.fsck().unwrap().warnings.is_empty());
    // ruling task-6-round2: the new approval is the one in force, so the stub names the new request
    let stub = r.read().block(&Scope::Canon, &first).unwrap();
    assert!(stub.is_redacted());
    assert_eq!(
        stub.read_identity.as_ref().map(|i| i.request),
        res.redaction,
        "the stub names the new request"
    );
    // the rebuilt index agrees: the same mask, the same empty warning, a clean fsck
    r.reindex().unwrap();
    assert!(r.read().carried_warnings().unwrap().is_empty());
    assert!(r.fsck().unwrap().warnings.is_empty());
    r.close().unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    assert!(r.fsck().unwrap().warnings.is_empty());
}
