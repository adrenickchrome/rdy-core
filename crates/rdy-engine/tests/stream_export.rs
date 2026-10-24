//! ha10 (D-D2NPN, D-Q0AVZ, D-3M81W): the export of one tag's history as a sync batch from a read
//! state. The root is a tag the state captured, the log is read up to the state's end, a removal
//! never leaves its bytes, and a removal proof applied after the tag was made is part of the tag's
//! batch.
use rdy_engine::redaction::RedactInput;
use rdy_engine::stream_export;
use rdy_engine::*;
use rdy_model::*;
use rdy_store::LandRequest;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::check_batch;
use serde_json::{Value, json};
use std::collections::BTreeSet;

const CANARY: &str = "ZEBRA-7731";

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

fn write(r: &mut Rdy, ops: Value) -> WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "ha10".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy, title: &str) -> Hash {
    let p = r
        .propose(&session(Role::Writer), "work", title, Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id)
        .unwrap()
        .parse()
        .unwrap()
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

/// Removes every version of block `target` through the store's own landing path.
fn remove(r: &mut Rdy, target: String) {
    let s = session(Role::Writer);
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: vec![target],
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

/// A project with a secret prose block, landed and tagged v1.0.0, then a second landing tagged
/// v2.0.0. Returns the project, the secret block's id and the two tag hashes.
fn tagged() -> (tempfile::TempDir, Rdy, String, Hash, Hash) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": format!("The launch code is {CANARY}.")}
        ]),
    );
    let (d, secret) = (w.created["$d"].clone(), w.created["$p"].clone());
    land(&mut r, "first");
    let v1 = tag(&mut r, "v1.0.0");
    write(
        &mut r,
        json!([{"op": "update", "id": d, "fields": {"title": "A2"}}]),
    );
    land(&mut r, "second");
    let v2 = tag(&mut r, "v2.0.0");
    (dir, r, secret, v1, v2)
}

fn export(
    r: &Rdy,
    tag: Hash,
    known: &BTreeSet<ObjectKey>,
) -> Result<(Vec<u8>, rdy_store::sync_stream::BatchReport), RdyError> {
    let mut out = Vec::new();
    let report = stream_export::write(&r.read(), tag, known, &mut out)?;
    Ok((out, report))
}

fn holds(bytes: &[u8], needle: &str) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[test]
fn ha10_export_is_byte_identical_for_one_snapshot() {
    let _clock = pin_clock(1_000_000);
    let (_dir, r, _, v1, v2) = tagged();
    let read = r.read();
    let none = BTreeSet::new();
    let mut first = Vec::new();
    let a = stream_export::write(&read, v2, &none, &mut first).unwrap();
    let mut second = Vec::new();
    let b = stream_export::write(&read, v2, &none, &mut second).unwrap();
    assert_eq!(first, second, "one snapshot, one byte sequence");
    assert_eq!(a, b);
    // what it states is what a checker reads back from the bytes, in wire order
    let checked = check_batch(&first[..]).unwrap();
    assert_eq!(checked, a);
    assert_eq!(a.records as usize, a.keys.len());
    assert_eq!(a.header.tag, v2);
    assert_eq!(a.header.repo_id, read.repo_id().unwrap());
    assert!(a.header.prerequisites.is_empty());
    assert!(a.keys.contains(&ObjectKey {
        kind: kinds::TAG,
        hash: v2
    }));
    // the older tag's batch is a different, smaller one, and its root is the last record
    let old = export(&r, v1, &none).unwrap().1;
    assert!(old.keys.len() < a.keys.len());
    assert_eq!(
        old.keys.last(),
        Some(&ObjectKey {
            kind: kinds::TAG,
            hash: v1
        })
    );
    // a claimed key leaves the batch and a record that needs it names it as a prerequisite
    let landing = r.store().reader().tags().unwrap()[0].2.landing;
    let known = BTreeSet::from([ObjectKey {
        kind: kinds::LANDING,
        hash: landing,
    }]);
    let (bytes, held) = export(&r, v1, &known).unwrap();
    assert_eq!(held.header.prerequisites, [*known.iter().next().unwrap()]);
    assert!(!held.keys.iter().any(|k| known.contains(k)));
    assert_eq!(check_batch(&bytes[..]).unwrap(), held);
}

#[test]
fn ha10_export_contains_later_removal_proofs_for_an_old_tag() {
    let _clock = pin_clock(1_000_000);
    let (_dir, mut r, secret, v1, _) = tagged();
    let none = BTreeSet::new();
    let (before, before_report) = export(&r, v1, &none).unwrap();
    assert!(
        holds(&before, CANARY),
        "before the removal the bytes travel"
    );
    assert!(
        !before_report
            .keys
            .iter()
            .any(|k| k.kind == kinds::REDACTION_APPLY)
    );

    remove(&mut r, secret);
    let (after, report) = export(&r, v1, &none).unwrap();
    assert!(!holds(&after, CANARY), "no removed byte is exported");
    // the apply, its request and its human landing come with the older tag
    for kind in [
        kinds::REDACTION,
        kinds::REDACTION_APPLY,
        kinds::REDACTION_RESIDUE,
    ] {
        assert!(
            report.keys.iter().any(|k| k.kind == kind),
            "kind {kind} is part of the batch"
        );
    }
    // and the batch is whole by itself: its framing and order hold with no prerequisite
    assert!(report.header.prerequisites.is_empty());
    assert_eq!(check_batch(&after[..]).unwrap(), report);
    // the same tag, read again, gives the same bytes
    assert_eq!(after, export(&r, v1, &none).unwrap().0);
}

#[test]
fn ha10_export_refuses_a_stale_state_an_unknown_tag_and_too_many_known_keys() {
    let _clock = pin_clock(1_000_000);
    let (_dir, mut r, secret, v1, _) = tagged();
    let none = BTreeSet::new();
    let stale = r.read();
    // a tag the state did not capture
    let e = stream_export::write(&stale, Hash([9; 32]), &none, Vec::new()).unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
    // more known keys than the cap
    let many: BTreeSet<ObjectKey> = (0..=100_000u32)
        .map(|i| {
            let mut h = [0u8; 32];
            h[..4].copy_from_slice(&i.to_le_bytes());
            ObjectKey {
                kind: kinds::BLOCK,
                hash: Hash(h),
            }
        })
        .collect();
    let e = stream_export::write(&stale, v1, &many, Vec::new()).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_LIMIT,
            "object presence query exceeds 100000 keys",
            "request at most 100000 object keys"
        )
    );
    // a state read before a removal exports nothing after it
    remove(&mut r, secret);
    let e = stream_export::write(&stale, v1, &none, Vec::new()).unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e}");
    // a state read after it does
    stream_export::write(&r.read(), v1, &none, Vec::new()).unwrap();
}

/// `value`'s record encoding, through the model's bounded encoder.
fn enc<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let mut buf = vec![0u8; 1 << 20];
    rdy_model::redaction::encode_into(value, &mut buf)
        .unwrap()
        .to_vec()
}

/// The project's store, moved aside and loaded again with one carried record over the frames
/// of the block whose body holds `CANARY` and of the change that wrote it, as zeros it explains
/// (the checked path of a stopped load). Returns the record's hash.
fn carry_canary(dir: &std::path::Path) -> Hash {
    use rdy_model::redaction::{
        BlockResidue, CarriedSpan, ChangeResidue, FramePlace, ReasonClass, RedactTarget,
        RedactionCarried, RedactionResidue,
    };
    use rdy_store::dump::Frame;
    use rdy_store::redaction::StoredFrame;
    use rdy_store::segment::Location;
    let store = dir.join(".rdy").join(rdy_store::layout::STORE_DIR);
    let aside = dir.join("store-aside");
    std::fs::rename(&store, &aside).unwrap();
    let s = rdy_store::Store::open(&aside).unwrap();
    let r = s.reader();
    // the canary's block frame and the change frame that wrote it
    let mut found: Vec<(Location, u8, Hash, u32, RedactionResidue)> = vec![];
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
                let holds = |b: &Block| b.body.contains(CANARY);
                if kind == kinds::BLOCK
                    && let Ok(b) = rdy_store::bounded::decode::<Block>(payload)
                    && holds(&b)
                {
                    let res = RedactionResidue::Block(BlockResidue::of(&b));
                    found.push((loc, kind, hash, payload.len() as u32, res));
                }
                if kind == kinds::CHANGE
                    && let Ok(c) = rdy_store::bounded::decode::<Change>(payload)
                    && c.ops.iter().any(|op| match op {
                        Op::Create { block } | Op::Update { block, .. } => holds(block),
                        Op::Delete { .. } => false,
                    })
                {
                    let res = RedactionResidue::Change(ChangeResidue::of(&c));
                    found.push((loc, kind, hash, payload.len() as u32, res));
                }
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(found.len(), 2, "the canary's block and change frames");
    let block_id = found
        .iter()
        .find_map(|(_, _, _, _, r)| match r {
            RedactionResidue::Block(b) => Some((b.id.clone(), b.version)),
            _ => None,
        })
        .unwrap();
    let rec = RedactionCarried {
        request: Hash::of(b"source request"),
        apply: Hash::of(b"source apply"),
        landing: Hash::of(b"source landing"),
        approved_by: Provenance {
            actor: "human:founder".into(),
            run: None,
            role: Role::Approver,
            at_ms: 777,
        },
        reason_class: ReasonClass::Secret,
        targets: vec![RedactTarget::Version {
            id: block_id.0,
            version: block_id.1,
        }],
        spans: found
            .iter()
            .map(|(loc, kind, hash, len, res)| CarriedSpan {
                place: FramePlace {
                    segment: loc.segment,
                    offset: loc.offset,
                },
                kind: *kind,
                hash: *hash,
                len: *len,
                residue: Some(Hash::of(&enc(res))),
                held: false,
            })
            .collect(),
    };
    let carried = Hash::of(&enc(&rec));
    let drops: std::collections::BTreeMap<Location, Option<Hash>> = rec
        .spans
        .iter()
        .map(|sp| {
            (
                Location {
                    segment: sp.place.segment,
                    offset: sp.place.offset,
                },
                sp.residue,
            )
        })
        .collect();
    let mut batches = vec![];
    r.for_each_batch(s.log_end(), |mut b| {
        let mut at = b.start;
        for f in b.frames.iter_mut() {
            let len = f.original_len();
            if let Some(residue) = drops.get(&at) {
                *f = Frame::Dropped {
                    kind: f.kind(),
                    hash: *f.hash(),
                    len,
                    apply: carried,
                    residue: *residue,
                };
            }
            at.offset += 37 + u64::from(len);
        }
        batches.push(b);
        Ok(())
    })
    .unwrap();
    drop(r);
    drop(s);
    let mut i = rdy_store::ingest::Ingest::create(&store).unwrap();
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    let residues: Vec<RedactionResidue> = found.into_iter().map(|f| f.4).collect();
    i.append_carried(&residues, &[rec]).unwrap();
    i.upgrade(&rdy_store::format::DumpStore::Features(
        rdy_store::format::baseline_features(),
    ))
    .unwrap();
    i.finish().unwrap();
    carried
}

#[test]
fn hc3_export_error_keeps_code_message_and_fix() {
    let (dir, r, _secret, v1, _v2) = tagged();
    drop(r);
    let carried = carry_canary(dir.path());
    let r = Rdy::open(dir.path()).unwrap();
    let e = export(&r, v1, &BTreeSet::new()).unwrap_err();
    assert_eq!(e.code, "E_REDACTED");
    assert_eq!(
        e.message,
        format!(
            "Tag v1.0.0 reaches content that carried record {carried} names, and no removal in this store covers it."
        )
    );
    assert_eq!(e.fix, rdy_store::redaction_carried::PROJECT_FIX);
    assert_eq!(
        e.fix,
        "Propose the redaction of the listed targets again, and have a person approve it."
    );
}
