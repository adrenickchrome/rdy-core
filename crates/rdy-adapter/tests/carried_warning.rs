//! hc6 (D-QF2HQ, D-0VG72, D-WDKWQ): `StoreHandle::redaction_warnings` reads the carried-removal
//! warnings of the store now, in both modes, without a full check. A holder has no local request
//! or approval: its warning ends when takes have removed every held payload of the carried
//! record.
//!
//! The sources are built with core calls and closed before a handle opens a store. Each store
//! with a carried record is built through the checked path of a stopped load: the closed store
//! is moved aside and ingested again with the record over the frames it names.

use rdy_adapter::take::UploadTarget;
use rdy_adapter::{CarriedWarning, Hash, RepoId, StoreHandle};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest, stream_export};
use rdy_model::redaction::{
    BlockResidue, CarriedSpan, ChangeResidue, FramePlace, ReasonClass, RedactTarget,
    RedactionApply, RedactionCarried, RedactionResidue,
};
use rdy_model::{Block, Change, Op, Provenance, Role};
use rdy_store::LandRequest;
use rdy_store::dump::Frame;
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::redaction_carried::{HOLDER_FIX, PROJECT_FIX};
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

const CANARY: &str = "ZEBRA-7731";
const OTHER: &str = "other text";

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

fn write(r: &mut Rdy, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "hc6".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy, title: &str) {
    let p = r
        .propose(&session(Role::Writer), "work", title, Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
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

fn landing_of(r: &Rdy, tag: Hash) -> Hash {
    r.store().read_tag(&tag).unwrap().landing
}

/// A source project that holds the canary block and another block, landed, with the secret's
/// id.
fn project(dir: &Path) -> (Rdy, String) {
    std::fs::create_dir_all(dir).unwrap();
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": format!("The launch code is {CANARY}.")},
            {"op": "create", "type": "prose", "id": "$q", "parent": "$d", "body": OTHER}
        ]),
    );
    land(&mut r, "first");
    let secret = w.created["$p"].clone();
    (r, secret)
}

/// The provenance of a removal a source store really applied: the request, the apply record it
/// wrote, the landing that carries it, and the person who approved that landing.
#[derive(Clone)]
struct Source {
    request: Hash,
    apply: Hash,
    landing: Hash,
    approver: Provenance,
}

/// Removes every version of block `target` through the store's own landing path. Returns what
/// the source wrote, read back from its log.
fn remove(r: &mut Rdy, target: &str) -> Source {
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
    let request = req.redaction.expect("the recorded request");
    let p = r.propose(&s, "work", "Remove the code", None).unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    let approver = Provenance {
        actor: "human:t".into(),
        run: None,
        role: Role::Approver,
        at_ms: 1_000_000,
    };
    let landed = r
        .store_mut()
        .land_atomic(LandRequest {
            proposal: version,
            accept: None,
            approver: approver.clone(),
            alt: None,
            relay: None,
            evidence: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
    // the apply record that the landing wrote, and the landing it names
    let mut applies: Vec<(Hash, RedactionApply)> = vec![];
    let store = r.store();
    store
        .reader()
        .for_each_record_from(
            Location {
                segment: 0,
                offset: 0,
            },
            store.log_end(),
            |_, f, _| {
                if let StoredFrame::Present {
                    kind: kinds::REDACTION_APPLY,
                    hash,
                    payload,
                } = f
                {
                    applies.push((hash, rdy_store::bounded::decode(payload).unwrap()));
                }
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(applies.len(), 1, "one removal in the source");
    let (apply, record) = applies.remove(0);
    assert_eq!(record.request, request);
    assert_eq!(record.landing, landed.landing);
    Source {
        request,
        apply,
        landing: landed.landing,
        approver,
    }
}

fn export(r: &Rdy, tag: Hash) -> Vec<u8> {
    let mut out = Vec::new();
    stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut out).unwrap();
    out
}

fn upload(dir: &Path, name: &str, bytes: &[u8]) -> Vec<File> {
    let p = dir.join(format!("{name}.rdysync"));
    std::fs::write(&p, bytes).unwrap();
    vec![File::open(p).unwrap()]
}

fn target(tag: &str, landing: Hash) -> UploadTarget {
    UploadTarget {
        tag: tag.into(),
        landing,
    }
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

/// One frame a carried record can name.
#[derive(Clone)]
struct Found {
    loc: Location,
    kind: u8,
    hash: Hash,
    len: u32,
    residue: RedactionResidue,
    /// The block of a block frame.
    block: Option<Block>,
    /// A block frame's body, or the bodies of the blocks a change frame writes.
    text: String,
}

/// The block and change frames of the closed store at `store`.
fn scan(store: &Path) -> Vec<Found> {
    let s = rdy_store::Store::open(store).unwrap();
    let mut out = vec![];
    s.reader()
        .for_each_record_from(
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
                    let len = payload.len() as u32;
                    if kind == kinds::BLOCK
                        && let Ok(b) = rdy_store::bounded::decode::<Block>(payload)
                    {
                        out.push(Found {
                            loc,
                            kind,
                            hash,
                            len,
                            residue: RedactionResidue::Block(BlockResidue::of(&b)),
                            text: b.body.clone(),
                            block: Some(b),
                        });
                    }
                    if kind == kinds::CHANGE
                        && let Ok(c) = rdy_store::bounded::decode::<Change>(payload)
                    {
                        out.push(Found {
                            loc,
                            kind,
                            hash,
                            len,
                            residue: RedactionResidue::Change(ChangeResidue::of(&c)),
                            block: None,
                            text: c
                                .ops
                                .iter()
                                .filter_map(|op| match op {
                                    Op::Create { block } | Op::Update { block, .. } => {
                                        Some(block.body.as_str())
                                    }
                                    Op::Delete { .. } => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                        });
                    }
                }
                Ok(())
            },
        )
        .unwrap();
    out
}

/// Which of the closed store's frames a carried record names, and whether each is `held`.
#[derive(Clone, Copy)]
enum Names {
    /// The canary block's frame and the change that wrote it.
    Secret { held: bool },
    /// The other block's frame.
    Other { held: bool },
}

fn secret_target(frames: &[Found]) -> RedactTarget {
    let b = frames
        .iter()
        .find(|f| f.kind == kinds::BLOCK && f.text.contains(CANARY))
        .and_then(|f| f.block.clone())
        .expect("the canary block");
    RedactTarget::Version {
        id: b.id.clone(),
        version: b.version_hash(),
    }
}

/// Where a carried record's provenance comes from.
#[derive(Clone)]
enum Origin {
    /// Made-up hashes named by a label: an approved apply of some other store.
    Label(&'static str),
    /// What the source really wrote when it removed the payload.
    Source(Source),
}

/// The store directory `store`, closed, moved aside and ingested again with one carried record
/// of `origin` over `names` (the checked path of a stopped load). Returns the record and its
/// hash.
fn carry(store: &Path, origin: &Origin, names: &[Names]) -> (Hash, RedactionCarried) {
    let aside = store.with_file_name("store-aside");
    std::fs::rename(store, &aside).unwrap();
    let frames = scan(&aside);
    let mut spans: Vec<CarriedSpan> = vec![];
    let mut residues: Vec<RedactionResidue> = vec![];
    let mut add = |f: &Found, held: bool| {
        residues.push(f.residue.clone());
        spans.push(CarriedSpan {
            place: FramePlace {
                segment: f.loc.segment,
                offset: f.loc.offset,
            },
            kind: f.kind,
            hash: f.hash,
            len: f.len,
            residue: Some(residue_id(&f.residue)),
            held,
        });
    };
    for n in names {
        match *n {
            Names::Secret { held } => {
                for f in frames.iter().filter(|f| f.text.contains(CANARY)) {
                    add(f, held);
                }
            }
            Names::Other { held } => {
                for f in frames
                    .iter()
                    .filter(|f| f.kind == kinds::BLOCK)
                    .filter(|f| f.text == OTHER)
                {
                    add(f, held);
                }
            }
        }
    }
    spans.sort_by_key(|s| s.place);
    let (request, apply, landing, approved_by) = match origin {
        Origin::Label(label) => (
            Hash::of(format!("request {label}").as_bytes()),
            Hash::of(format!("apply {label}").as_bytes()),
            Hash::of(format!("landing {label}").as_bytes()),
            Provenance {
                actor: "human:founder".into(),
                run: None,
                role: Role::Approver,
                at_ms: 777,
            },
        ),
        Origin::Source(src) => (src.request, src.apply, src.landing, src.approver.clone()),
    };
    let rec = RedactionCarried {
        request,
        apply,
        landing,
        approved_by,
        reason_class: ReasonClass::Secret,
        targets: vec![secret_target(&frames)],
        spans,
    };
    let carried = Hash::of(&enc(&rec));
    let drops: BTreeMap<Location, Option<Hash>> = rec
        .spans
        .iter()
        .filter(|s| !s.held)
        .map(|s| {
            (
                Location {
                    segment: s.place.segment,
                    offset: s.place.offset,
                },
                s.residue,
            )
        })
        .collect();
    let s = rdy_store::Store::open(&aside).unwrap();
    let mut batches = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |mut b| {
            let mut at = b.start;
            for fr in b.frames.iter_mut() {
                let len = fr.original_len();
                if let Some(residue) = drops.get(&at) {
                    *fr = Frame::Dropped {
                        kind: fr.kind(),
                        hash: *fr.hash(),
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
    drop(s);
    let mut i = rdy_store::ingest::Ingest::create(store).unwrap();
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    i.append_carried(&residues, std::slice::from_ref(&rec))
        .unwrap();
    i.upgrade(&rdy_store::format::DumpStore::Features(
        rdy_store::format::baseline_features(),
    ))
    .unwrap();
    i.finish().unwrap();
    (carried, rec)
}

fn store_of(root: &Path) -> PathBuf {
    root.join(".rdy").join(rdy_store::layout::STORE_DIR)
}

/// What the holder tests share: a source with the canary removed after `v1.0.0`, the exports of
/// both tags, and a holder at `holder` that took `v1.0.0` and then carries `names`.
struct Fixture {
    dir: tempfile::TempDir,
    id: RepoId,
    land2: Hash,
    export2: Vec<u8>,
    holder: PathBuf,
    carried: Hash,
    record: RedactionCarried,
}

/// A fixture whose carried record names made-up hashes of another store's approved apply.
fn fixture(names: &[Names]) -> Fixture {
    fixture_from(names, false)
}

/// A fixture whose carried record names the removal the source really applied.
fn fixture_of_the_source(names: &[Names]) -> Fixture {
    fixture_from(names, true)
}

fn fixture_from(names: &[Names], real: bool) -> Fixture {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let (mut r, secret) = project(&dir.path().join("src"));
    let v1 = tag(&mut r, "v1.0.0");
    let export1 = export(&r, v1);
    let source = remove(&mut r, &secret);
    let v2 = tag(&mut r, "v2.0.0");
    let export2 = export(&r, v2);
    let id = r.store().repo_id().unwrap().unwrap();
    let (land1, land2) = (landing_of(&r, v1), landing_of(&r, v2));
    r.close().unwrap();
    // a holder that took the history before the removal
    let holder = dir.path().join("holder");
    let mut h = StoreHandle::create_holder(&holder, id).unwrap();
    h.take(
        &upload(dir.path(), "up1", &export1),
        target("v1.0.0", land1),
    )
    .unwrap();
    h.close().unwrap();
    let origin = if real {
        Origin::Source(source)
    } else {
        Origin::Label("source")
    };
    let (carried, record) = carry(&store_of(&holder), &origin, names);
    Fixture {
        dir,
        id,
        land2,
        export2,
        holder,
        carried,
        record,
    }
}

fn held_and_removed(w: &CarriedWarning) -> String {
    let m = &w.message;
    let at = m.find("It holds ").expect("the counts");
    m[at..].to_string()
}

#[test]
fn hc6_holder_warning_uses_the_holder_fix() {
    let f = fixture(&[Names::Secret { held: true }]);
    let h = StoreHandle::open_holder(&f.holder).unwrap();
    let w = h.redaction_warnings().unwrap();
    assert_eq!(w.len(), 1);
    let w = &w[0];
    assert_eq!(w.code, "W_STOP_BEFORE_REDACTION");
    assert_eq!(w.fix, HOLDER_FIX);
    assert_eq!(
        w.fix,
        "Ask the uploader to send the removal again, and take the upload that carries it."
    );
    assert_eq!(
        w.message,
        format!(
            "This store stops before redaction apply {} of its source, approved by human:founder at landing {}. It holds 2 removed payloads again, and 0 stay removed.",
            f.record.apply, f.record.landing
        )
    );
    assert_eq!(w.carried, f.carried);
    assert_eq!(w.request, f.record.request);
    assert_eq!(w.issues.len(), 1);
    assert!(w.issues[0].target.contains('@'), "{}", w.issues[0].target);
    h.close().unwrap();
}

/// Whether `stream` carries a frame of hash `h` (every frame header holds its hash).
fn carries(stream: &[u8], h: &Hash) -> bool {
    stream.windows(32).any(|w| w == &h.0[..])
}

#[test]
fn hc6_holder_take_of_the_source_apply_ends_the_warning() {
    // the record names the apply, landing, request and approver the source really wrote
    let f = fixture_of_the_source(&[Names::Secret { held: true }]);
    assert!(
        !carries(&f.export2, &f.carried),
        "a carried record is no part of an upload"
    );
    assert!(
        carries(&f.export2, &f.record.apply),
        "the upload holds the source's own apply"
    );
    assert!(carries(&f.export2, &f.record.landing));
    let mut h = StoreHandle::open_holder(&f.holder).unwrap();
    let before = h.redaction_warnings().unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].apply, f.record.apply);
    assert_eq!(before[0].landing, f.record.landing);
    h.take(
        &upload(f.dir.path(), "up2", &f.export2),
        target("v2.0.0", f.land2),
    )
    .unwrap();
    assert!(h.redaction_warnings().unwrap().is_empty());
    assert!(h.fsck().unwrap().warnings.is_empty());
    h.close().unwrap();
    // the reopened holder derives the same end
    let h = StoreHandle::open_holder(&f.holder).unwrap();
    assert!(h.redaction_warnings().unwrap().is_empty());
    h.close().unwrap();
}

#[test]
fn hc6_holder_take_of_another_approved_apply_ends_the_warning() {
    // the record names another store's apply (made-up hashes); a take of any approved apply that
    // matches the span's kind, hash, length and residue erases the held payload
    let f = fixture(&[Names::Secret { held: true }]);
    assert!(
        !carries(&f.export2, &f.record.apply),
        "the upload does not hold the apply that the record names"
    );
    let mut h = StoreHandle::open_holder(&f.holder).unwrap();
    assert_eq!(h.redaction_warnings().unwrap().len(), 1);
    h.take(
        &upload(f.dir.path(), "up2", &f.export2),
        target("v2.0.0", f.land2),
    )
    .unwrap();
    assert!(h.redaction_warnings().unwrap().is_empty());
    assert!(h.fsck().unwrap().warnings.is_empty());
    h.close().unwrap();
}

#[test]
fn hc6_holder_partial_take_lowers_the_held_count() {
    let f = fixture(&[Names::Secret { held: true }, Names::Other { held: true }]);
    let mut h = StoreHandle::open_holder(&f.holder).unwrap();
    let before = h.redaction_warnings().unwrap();
    assert_eq!(before.len(), 1);
    assert!(
        held_and_removed(&before[0]).starts_with("It holds 3 removed payloads again, and 0 stay"),
        "{}",
        before[0].message
    );
    h.take(
        &upload(f.dir.path(), "up2", &f.export2),
        target("v2.0.0", f.land2),
    )
    .unwrap();
    // the take removed the two payloads its apply covers; the other block is still held
    let after = h.redaction_warnings().unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(
        held_and_removed(&after[0]),
        "It holds 1 removed payloads again, and 2 stay removed."
    );
    assert_eq!(after[0].carried, before[0].carried);
    assert_eq!(after[0].issues, before[0].issues);
    assert_eq!(after[0].fix, HOLDER_FIX);
    h.close().unwrap();
}

#[test]
fn hc6_redaction_warnings_match_fsck_in_both_modes() {
    // the holder
    let f = fixture(&[Names::Secret { held: true }, Names::Other { held: false }]);
    let h = StoreHandle::open_holder(&f.holder).unwrap();
    let w = h.redaction_warnings().unwrap();
    assert_eq!(w.len(), 1);
    assert_eq!(h.fsck().unwrap().warnings, w);
    assert_eq!(w[0].fix, HOLDER_FIX);
    assert_eq!(
        held_and_removed(&w[0]),
        "It holds 2 removed payloads again, and 1 stay removed."
    );
    h.close().unwrap();

    // a project
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = project(&dir.path().join("p"));
    r.close().unwrap();
    let root = dir.path().join("p");
    let (carried, rec) = carry(
        &store_of(&root),
        &Origin::Label("source"),
        &[Names::Secret { held: true }],
    );
    let p = StoreHandle::open(&root).unwrap();
    let w = p.redaction_warnings().unwrap();
    assert_eq!(w.len(), 1);
    assert_eq!(p.fsck().unwrap().warnings, w);
    assert_eq!(w[0].fix, PROJECT_FIX);
    assert_eq!(w[0].carried, carried);
    assert_eq!(w[0].apply, rec.apply);
    // the same warning as a holder's, bar the fix
    assert_eq!(
        held_and_removed(&w[0]),
        "It holds 2 removed payloads again, and 0 stay removed."
    );
    p.close().unwrap();

    // a store with no carried record has no warning in either mode
    let (r, _) = project(&dir.path().join("plain"));
    r.close().unwrap();
    let plain = StoreHandle::open(&dir.path().join("plain")).unwrap();
    assert!(plain.redaction_warnings().unwrap().is_empty());
    plain.close().unwrap();
    let empty = StoreHandle::create_holder(&dir.path().join("empty"), f.id).unwrap();
    assert!(empty.redaction_warnings().unwrap().is_empty());
    empty.close().unwrap();
}
