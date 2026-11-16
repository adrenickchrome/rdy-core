//! scc5 (D-FNRXT, D-CC90E, D-3M81W, D-HVZC9, D-47HVF): removals through sync.
//!
//! A push carries each removal that a person approved, with its landing, request, residues and
//! carrier change; the holder's take checks the proof and drops the bytes. A fetch takes a
//! removal only when a person's approval proves it, applies it in the fetch batch and erases what
//! it covers; every view then reads the stub. A local change that holds the removed content and
//! that no removal covers keeps its bytes, and warns once per change on every read. A pull
//! replay never copies removed text. A removal landing that leaves canon stays applied, and the
//! next push carries it outside canon.
use rdy_engine::redaction::RedactInput;
use rdy_engine::stream_export;
use rdy_engine::sync_fetch::{self, FetchOutcome, HubState};
use rdy_engine::sync_redaction::{UncoveredWarning, uncovered_warning};
use rdy_engine::sync_replay::StopReason;
use rdy_engine::sync_replay::{self, PULLED, PULLED_WITH_CONFLICTS, PullOutcome, PullRequest};
use rdy_engine::*;
use rdy_model::redaction::{CONTENT_REMOVED, RedactionApply};
use rdy_model::{Assurance, BlockId, Hash, LandingRecord, RepoId};
use rdy_qa::fixtures::{land, write, writer};
use rdy_store::closure::write_batch;
use rdy_store::hub_canon::{CanonMove, Moved};
use rdy_store::object_refs::ObjectKey;
use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint};
use rdy_store::sync_state::{SyncCanon, SyncStep};
use rdy_store::sync_stream::{StreamHeader, StreamPurpose, StreamReader, StreamRecord};
use rdy_store::take_batch::{FetchTarget, HistoryTaken, HistoryTarget};
use rdy_store::{codec, kinds};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::fs::File;
use std::path::{Path, PathBuf};

const URL: &str = "https://127.0.0.1:9/r/demo";
/// The prose the hub's removal removes.
const SECRET: &str = "scc5-secret-token";
/// The text the prose holds between the two changes of an uncovered carrier.
const AWAY: &str = "scc5-away-text";
/// The light and the dark image of the figure.
const LIGHT: &[u8] = b"\x89PNG scc5-light-variant";
const DARK: &[u8] = b"\x89PNG scc5-dark-variant";
/// The holder's clock of every move (injected).
const AT: i64 = 1_763_164_800_000;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// Whether a segment file of the store at `root` holds `needle`.
fn on_disk(root: &Path, needle: &[u8]) -> bool {
    std::fs::read_dir(root.join(".rdy/store/segments"))
        .unwrap()
        .any(|e| {
            let bytes = std::fs::read(e.unwrap().path()).unwrap();
            bytes.windows(needle.len()).any(|w| w == needle)
        })
}

fn holds(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle)
}

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Lands view `view` of `r` as a person; returns the landing.
fn land_view(r: &mut Rdy, view: &str) -> Hash {
    h(&land(r, view).unwrap())
}

/// Records the removal of `targets` in view `view` of `r` and lands it as a person; returns the
/// landing.
fn remove_on(r: &mut Rdy, view: &str, targets: Vec<String>) -> Hash {
    r.redact_in(
        &writer(),
        view,
        RedactInput {
            targets,
            reason_class: "secret".into(),
            reason: "a leaked token".into(),
            dry_run: false,
        },
    )
    .unwrap();
    land_view(r, view)
}

/// The canon head of `r`.
fn head(r: &Rdy) -> Hash {
    r.store().reader().canon_head().unwrap().unwrap()
}

/// The canon history of `src` at `root` above `base`, as one batch.
fn history(src: &Rdy, root: Hash, base: Option<Hash>) -> Vec<u8> {
    let mut out = Vec::new();
    stream_export::write_history(
        &src.read(),
        StreamPurpose::CanonHistory,
        root,
        base,
        &BTreeSet::new(),
        &mut out,
    )
    .unwrap();
    out
}

/// The header and the records of batch `bytes`, in wire order.
fn records_of(bytes: &[u8]) -> (StreamHeader, Vec<StreamRecord>) {
    let (mut r, header) = StreamReader::open_history(bytes).unwrap();
    let mut out = vec![];
    while let Some(x) = r.next().unwrap() {
        out.push(x);
    }
    (header, out)
}

fn keys_of(bytes: &[u8]) -> Vec<ObjectKey> {
    records_of(bytes).1.iter().map(StreamRecord::key).collect()
}

fn present<T: serde::de::DeserializeOwned + 'static>(r: &StreamRecord) -> T {
    match r {
        StreamRecord::Present {
            key,
            encoding,
            payload,
        } => codec::decode_record(codec::raw_kind(key.kind, *encoding), payload).unwrap(),
        StreamRecord::Dropped { .. } => panic!("a dropped record"),
    }
}

fn framed<T: serde::Serialize + 'static>(kind: u8, v: &T) -> StreamRecord {
    let (raw, payload) = codec::item(kind, v).unwrap();
    let key = ObjectKey {
        kind,
        hash: Hash::of(&payload),
    };
    StreamRecord::Present {
        key,
        encoding: codec::split(raw, &payload).unwrap().version,
        payload,
    }
}

/// How a forged batch differs from the hub's.
#[derive(Clone, Copy, Debug)]
enum Forgery {
    /// The apply names a copy of its landing approved by policy.
    Policy,
    /// The apply names another report.
    Report,
}

/// Batch `bytes` with its removal apply forged as `how` says; every drop names the forged apply.
fn forged(bytes: &[u8], how: Forgery) -> Vec<u8> {
    let (header, mut recs) = records_of(bytes);
    let at = recs
        .iter()
        .position(|r| r.key().kind == kinds::REDACTION_APPLY)
        .expect("the batch carries the apply");
    let real_hash = recs[at].key().hash;
    let real: RedactionApply = present(&recs[at]);
    let fake = match how {
        Forgery::Policy => {
            let landing = recs
                .iter()
                .find(|r| {
                    r.key()
                        == ObjectKey {
                            kind: kinds::LANDING,
                            hash: real.landing,
                        }
                })
                .expect("the batch carries the human landing");
            let mut l: LandingRecord = present(landing);
            l.assurance = Assurance::Policy;
            l.read_identity = None;
            let rec = framed(kinds::LANDING, &l);
            let lh = rec.key().hash;
            recs.insert(at, rec);
            RedactionApply {
                landing: lh,
                ..real.clone()
            }
        }
        Forgery::Report => RedactionApply {
            report_hash: Hash([1; 32]),
            ..real.clone()
        },
    };
    let new = framed(kinds::REDACTION_APPLY, &fake);
    let new_hash = new.key().hash;
    let at = recs.iter().position(|r| r.key().hash == real_hash).unwrap();
    recs[at] = new;
    for r in &mut recs {
        if let StreamRecord::Dropped { apply, .. } = r
            && *apply == real_hash
        {
            *apply = new_hash;
        }
    }
    let mut map: BTreeMap<ObjectKey, StreamRecord> =
        recs.iter().map(|r| (r.key(), r.clone())).collect();
    let keys: Vec<ObjectKey> = recs.iter().map(StreamRecord::key).collect();
    write_batch(&mut map, &header, &keys, Vec::new()).unwrap().0
}

/// The hub's copy (`hub`) and the local project (`local`) after one shared landing `base`: a doc
/// with a section that holds the prose `prose` and the figure `figure`, whose light and dark
/// images are the blobs `light` and `dark`.
struct World {
    dir: tempfile::TempDir,
    hub: Rdy,
    local: Rdy,
    base: Hash,
    doc: String,
    prose: String,
    figure: String,
    light: String,
    dark: String,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("hub");
    let mut hub = Rdy::init(&a, "rdy-standard").unwrap();
    let light = hub.store_mut().put_blob(LIGHT).unwrap().to_string();
    let dark = hub.store_mut().put_blob(DARK).unwrap().to_string();
    let w = write(
        &mut hub,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": SECRET},
            {"op": "create", "type": "figure", "id": "$f", "parent": "$s", "fields": {
                "blob": light, "media_type": "image/png", "alt": "chart",
                "dark_blob": dark, "dark_media_type": "image/png"}}
        ]),
    )
    .unwrap();
    let base = land_view(&mut hub, "seed");
    drop(hub);
    let b = dir.path().join("local");
    copy_dir(&a, &b);
    World {
        hub: Rdy::open(&a).unwrap(),
        local: Rdy::open(&b).unwrap(),
        base,
        doc: w.created["$d"].clone(),
        prose: w.created["$p"].clone(),
        figure: w.created["$f"].clone(),
        light,
        dark,
        dir,
    }
}

impl World {
    fn repo(&self) -> RepoId {
        self.hub.store().reader().repo_id().unwrap().unwrap()
    }

    fn local_dir(&self) -> PathBuf {
        self.dir.path().join("local")
    }

    fn holder_dir(&self) -> PathBuf {
        self.dir.path().join("holder")
    }

    /// Records that the hub held local landing `applied` when this project last synced.
    fn applied(&mut self, applied: Hash) {
        let state = SyncCanon {
            repo_id: self.repo(),
            url: URL.into(),
            fetched: Some(applied),
            applied: Some(applied),
            held: None,
        };
        self.local
            .store_mut()
            .write_sync("origin", vec![SyncStep::Canon(state)])
            .unwrap();
    }

    /// Fetches batch `bytes` of the hub's canon at `root` into the local project.
    fn fetch_bytes(&mut self, bytes: &[u8], root: Hash) -> Result<FetchOutcome, RdyError> {
        let path = self.dir.path().join("batch");
        std::fs::write(&path, bytes).unwrap();
        let target = FetchTarget {
            remote: "origin".into(),
            url: URL.into(),
            purpose: StreamPurpose::CanonHistory,
            root,
            view: None,
        };
        sync_fetch::fetch(&mut self.local, &[File::open(&path).unwrap()], &target)
    }

    /// Fetches the hub's canon above `base` into the local project.
    fn fetch(&mut self) -> FetchOutcome {
        let tip = head(&self.hub);
        let bytes = history(&self.hub, tip, Some(self.base));
        self.fetch_bytes(&bytes, tip).unwrap()
    }

    fn hub_state(&self) -> HubState {
        HubState {
            repo_id: Some(self.repo()),
            canon: Some(head(&self.hub)),
        }
    }

    /// The pull's second batch after a fetch, by the person who owns `pull-<n>`.
    fn pull_only(&mut self) -> PullOutcome {
        let hub = self.hub_state();
        sync_replay::pull(
            &mut self.local,
            &writer(),
            &PullRequest {
                remote: "origin".into(),
                hub,
                take: false,
                strict: false,
            },
        )
        .unwrap()
    }

    /// Closes the local project and opens it again.
    fn reopen(&mut self) {
        let spare = self.dir.path().join("spare");
        if !spare.exists() {
            drop(Rdy::init(&spare, "rdy-standard").unwrap());
        }
        drop(std::mem::replace(
            &mut self.local,
            Rdy::open(&spare).unwrap(),
        ));
        self.local = Rdy::open(&self.local_dir()).unwrap();
    }

    /// The prose's body as scope `scope` of the local project reads it.
    fn prose_in(&self, scope: Scope) -> String {
        self.local
            .read()
            .block(&scope, &id(&self.prose))
            .unwrap()
            .body
    }

    /// The removed version of the prose, as the hub wrote it before any removal.
    fn secret_version(&self) -> Hash {
        self.local
            .store()
            .reader()
            .state_of(Some(self.base))
            .unwrap()
            .blocks
            .get(&id(&self.prose))
            .copied()
            .unwrap()
    }

    /// The holder seeded with the hub's canon at `base`.
    fn holder(&self) -> Rdy {
        let mut hd = Rdy::create_holder(&self.holder_dir(), self.repo()).unwrap();
        push(&mut hd, &self.hub, None, CanonMove::Seed, None);
        hd
    }
}

/// Pushes the canon of `src` above `base` into holder `hd` and moves its canon by `mv`
/// (`expected` its tip before); returns what the take sealed.
fn push(
    hd: &mut Rdy,
    src: &Rdy,
    base: Option<Hash>,
    mv: CanonMove,
    expected: Option<Hash>,
) -> HistoryTaken {
    let tip = head(src);
    let bytes = history(src, tip, base);
    let path = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(path.path(), &bytes).unwrap();
    // as a holder's take does: the writer's held batches are off for the take
    hd.seal_open().unwrap();
    hd.store_mut().set_held(false).unwrap();
    let taken = hd
        .store_mut()
        .take_history::<Infallible>(
            &[File::open(path.path()).unwrap()],
            &HistoryTarget {
                purpose: StreamPurpose::CanonHistory,
                root: tip,
            },
        )
        .unwrap();
    let b = hd.store().reader().check_move_batch(vec![], tip).unwrap();
    assert_eq!(
        hd.store_mut().move_canon(&b, &mv, expected, AT).unwrap(),
        Moved::Moved { tip }
    );
    taken
}

/// The canon chain of holder `hd` from its tip, newest first.
fn holder_chain(hd: &Rdy, tip: Hash) -> Vec<Hash> {
    let reader = hd.store().reader();
    let mut out = vec![];
    let mut at = Some(tip);
    while let Some(l) = at {
        out.push(l);
        at = reader.landing(&l).unwrap().prev;
    }
    out
}

/// The removal request of landing `l` of `r`.
fn request_of(r: &Rdy, l: &Hash) -> Hash {
    let rec = r.store().reader().landing(l).unwrap();
    assert_eq!(rec.checks.len(), 1, "one removal: {rec:?}");
    rec.checks[0]
}

/// Writes the prose away from its text and back in view `view`: the second change produces the
/// removed version again. Returns that change.
fn away_and_back(w: &mut World, view: &str) -> Hash {
    let prose = w.prose.clone();
    write(
        &mut w.local,
        view,
        json!([{"op": "update", "id": prose, "body": AWAY}]),
    )
    .unwrap();
    let back = write(
        &mut w.local,
        view,
        json!([{"op": "update", "id": prose, "body": SECRET}]),
    )
    .unwrap();
    let v = w
        .local
        .read()
        .block(&Scope::View(view.into()), &id(&w.prose))
        .unwrap()
        .version_hash();
    assert_eq!(
        v,
        w.secret_version(),
        "the change writes the removed version"
    );
    h(&back.change)
}

#[test]
fn scc5_push_carries_human_removal_proofs() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    let mut hd = w.holder();
    assert!(on_disk(&w.holder_dir(), SECRET.as_bytes()));
    let v = w.secret_version();
    let prose = w.prose.clone();
    let l = remove_on(&mut w.local, "redact", vec![prose]);
    let rec = w.local.store().reader().landing(&l).unwrap();
    assert_eq!(rec.assurance, Assurance::Local, "a person approved it here");
    let request = request_of(&w.local, &l);
    let carrier = rec
        .changes
        .iter()
        .copied()
        .find(|c| {
            w.local
                .store()
                .reader()
                .redaction_for_carrier(c)
                .unwrap()
                .is_some()
        })
        .expect("the removal's carrier");
    // the upload carries the landing, the request, its residues, the carrier and the apply
    let keys = keys_of(&history(&w.local, l, Some(w.base)));
    let has = |kind: u8, hash: Hash| keys.contains(&ObjectKey { kind, hash });
    assert!(has(kinds::LANDING, l));
    assert!(has(kinds::REDACTION, request));
    assert!(has(kinds::CHANGE, carrier));
    assert!(keys.iter().any(|k| k.kind == kinds::REDACTION_APPLY));
    assert!(keys.iter().any(|k| k.kind == kinds::REDACTION_RESIDUE));
    // the holder checks the proof and drops the bytes
    let taken = push(
        &mut hd,
        &w.local,
        Some(w.base),
        CanonMove::Extend,
        Some(w.base),
    );
    assert_eq!(taken.applies.len(), 1);
    assert!(taken.redacted_objects > 0, "{taken:?}");
    assert_eq!(
        hd.store().reader().redaction_of(kinds::BLOCK, &v).unwrap(),
        Some(request)
    );
    assert!(!on_disk(&w.holder_dir(), SECRET.as_bytes()));
    hd.store().reader().check_objects().unwrap();
}

#[test]
fn scc5_pull_applies_removal_to_all_views() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    let v = w.secret_version();
    // a local view of other work, on the same base
    let doc = w.doc.clone();
    write(
        &mut w.local,
        "side",
        json!([{"op": "update", "id": doc, "fields": {"title": "Side"}}]),
    )
    .unwrap();
    assert_eq!(w.prose_in(Scope::View("side".into())), SECRET);
    let prose = w.prose.clone();
    let l = remove_on(&mut w.hub, "redact", vec![prose]);
    let request = request_of(&w.hub, &l);
    let f = w.fetch();
    assert_eq!(f.removals.len(), 1, "{f:?}");
    assert!(f.redacted_objects > 0, "{f:?}");
    assert!(f.warnings.is_empty(), "{:?}", f.warnings);
    // the fetch batch applied it: every view reads the stub before the pull moves canon
    assert_eq!(w.prose_in(Scope::Canon), CONTENT_REMOVED);
    assert_eq!(w.prose_in(Scope::View("side".into())), CONTENT_REMOVED);
    let out = w.pull_only();
    assert_eq!(out.status, PULLED, "{out:?}");
    assert_eq!(w.prose_in(Scope::Canon), CONTENT_REMOVED);
    assert_eq!(w.prose_in(Scope::View("side".into())), CONTENT_REMOVED);
    // the proof is the hub's: its request and its human landing
    let reader = w.local.store().reader();
    assert_eq!(
        reader.redaction_of(kinds::BLOCK, &v).unwrap(),
        Some(request)
    );
    assert_eq!(
        reader.landing(&l).unwrap().assurance,
        Assurance::Local,
        "the local copy holds the hub's human landing"
    );
    assert!(!on_disk(&w.local_dir(), SECRET.as_bytes()));
    reader.check_objects().unwrap();
}

#[test]
fn scc5_uncovered_carrier_keeps_bytes_and_repeats_warning() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    let back = away_and_back(&mut w, "keep");
    let prose = w.prose.clone();
    let l = remove_on(&mut w.hub, "redact", vec![prose]);
    let request = request_of(&w.hub, &l);
    let f = w.fetch();
    let want = vec![uncovered_warning(back, request)];
    assert_eq!(f.warnings, want, "one warning, for the uncovered change");
    // the uncovered change keeps its bytes; the covered copies are gone
    let reader = w.local.store().reader();
    assert_eq!(reader.redaction_of(kinds::CHANGE, &back).unwrap(), None);
    let kept = serde_json::to_string(&reader.change(&back).unwrap()).unwrap();
    assert!(kept.contains(SECRET), "{kept}");
    assert_eq!(
        reader
            .redaction_of(kinds::BLOCK, &w.secret_version())
            .unwrap(),
        Some(request)
    );
    drop(reader);
    // each read repeats it, once per change, and a reopen and a reindex keep it
    assert_eq!(w.local.read().uncovered_warnings().unwrap(), want);
    assert_eq!(w.local.read().uncovered_warnings().unwrap(), want);
    w.reopen();
    assert_eq!(w.local.read().uncovered_warnings().unwrap(), want);
    w.local.store_mut().reindex().unwrap();
    assert_eq!(w.local.read().uncovered_warnings().unwrap(), want);
    // a fetch again writes nothing and still warns once
    let again = w.fetch();
    assert!(again.unchanged, "{again:?}");
    assert_eq!(again.warnings, want);
}

#[test]
fn scc5_policy_or_forged_proof_changes_nothing() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    let v = w.secret_version();
    let prose = w.prose.clone();
    let l = remove_on(&mut w.hub, "redact", vec![prose]);
    let tip = head(&w.hub);
    assert_eq!(tip, l);
    let bytes = history(&w.hub, tip, Some(w.base));
    for how in [Forgery::Policy, Forgery::Report] {
        let before = w.local.store().log_end();
        let e = w.fetch_bytes(&forged(&bytes, how), tip).unwrap_err();
        assert_eq!(w.local.store().log_end(), before, "{how:?}: {e:?}");
        assert_eq!(w.prose_in(Scope::Canon), SECRET, "{how:?}");
        let reader = w.local.store().reader();
        assert_eq!(reader.redaction_of(kinds::BLOCK, &v).unwrap(), None);
        assert!(reader.sync_canon("origin").unwrap().is_none(), "{how:?}");
        assert!(w.local.read().uncovered_warnings().unwrap().is_empty());
    }
    assert!(on_disk(&w.local_dir(), SECRET.as_bytes()));
    // the hub's own batch, with a person's approval, applies
    let f = w.fetch_bytes(&bytes, tip).unwrap();
    assert_eq!(f.removals.len(), 1);
    assert_eq!(w.prose_in(Scope::Canon), CONTENT_REMOVED);
}

#[test]
fn scc5_replay_stops_without_removed_text() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    let back = away_and_back(&mut w, "keep");
    let lu = land_view(&mut w.local, "keep");
    write(
        &mut w.local,
        "later",
        json!([{"op": "create", "type": "doc", "fields": {"name": "later", "title": "Later", "doc_type": "page"}}]),
    )
    .unwrap();
    let l3 = land_view(&mut w.local, "later");
    let prose = w.prose.clone();
    let l = remove_on(&mut w.hub, "redact", vec![prose.clone()]);
    let request = request_of(&w.hub, &l);
    let f = w.fetch();
    assert_eq!(f.warnings, vec![uncovered_warning(back, request)]);
    let out = w.pull_only();
    assert_eq!(out.status, PULLED_WITH_CONFLICTS, "{out:?}");
    assert!(out.replayed.is_empty(), "{out:?}");
    let stop = &out.not_replayed[0];
    assert_eq!(stop.landing, lu.to_string());
    assert_eq!(stop.reason, StopReason::Removal);
    assert_eq!(stop.removals, vec![request.to_string()]);
    assert_eq!(out.not_replayed[1].landing, l3.to_string());
    assert_eq!(out.not_replayed[1].reason, StopReason::AfterStop);
    // the block of the change that holds removed content: named with its removal, no text
    assert_eq!(out.removed.len(), 1, "{out:?}");
    assert_eq!(out.removed[0].block, prose);
    assert_eq!(out.removed[0].change, back.to_string());
    assert_eq!(out.removed[0].removal, request.to_string());
    let reply = serde_json::to_string(&out).unwrap();
    assert!(!reply.contains(SECRET), "{reply}");
    // pull-<n> holds the later work and no change that holds the removed text
    let view = out.view.clone().expect("pull-<n>");
    let held = w.local.store().included(&view).unwrap();
    let reader = w.local.store().reader();
    for c in held.iter() {
        let change = reader.change(c).unwrap();
        assert_ne!(change.links.replayed_from, Some(back));
        let text = serde_json::to_string(&change).unwrap();
        assert!(!text.contains(SECRET), "{text}");
    }
    let m = w
        .local
        .read()
        .materialized(&Scope::View(view.clone()))
        .unwrap();
    assert!(
        m.iter().all(|b| !b.body.contains(SECRET)),
        "the view reads no removed text"
    );
    assert_ne!(w.prose_in(Scope::View(view)), SECRET);
    assert_eq!(w.prose_in(Scope::Canon), CONTENT_REMOVED);
}

#[test]
fn scc5_removal_crash_and_reindex_keep_erasure() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    let back = away_and_back(&mut w, "keep");
    let v = w.secret_version();
    let prose = w.prose.clone();
    let l = remove_on(&mut w.hub, "redact", vec![prose]);
    let request = request_of(&w.hub, &l);
    let tip = head(&w.hub);
    let bytes = history(&w.hub, tip, Some(w.base));
    // the fetch seals its removal, then fails before the erasure finishes
    arm_failpoint(Some((Failpoint::AfterApplySync, FailAction::Fail)));
    let e = w.fetch_bytes(&bytes, tip).unwrap_err();
    arm_failpoint(None);
    assert_eq!(e.code, E_REDACTION_PENDING, "{e:?}");
    let spare = w.dir.path().join("spare");
    drop(Rdy::init(&spare, "rdy-standard").unwrap());
    let old = std::mem::replace(&mut w.local, Rdy::open(&spare).unwrap());
    let closes = rdy_store::unclean_closes_for_tests(&w.local_dir(), || drop(old));
    assert_eq!(closes.len(), 1, "{closes:?}");
    // the next open finishes the exact authorized removal, with the hub's proof
    w.local = Rdy::open(&w.local_dir()).unwrap();
    let want = vec![uncovered_warning(back, request)];
    let check = |w: &World| {
        let reader = w.local.store().reader();
        assert_eq!(
            reader.redaction_of(kinds::BLOCK, &v).unwrap(),
            Some(request)
        );
        assert_eq!(w.prose_in(Scope::Canon), CONTENT_REMOVED);
        assert_eq!(w.local.read().uncovered_warnings().unwrap(), want);
        assert!(reader.sync_canon("origin").unwrap().is_some());
        reader.check_objects().unwrap();
    };
    check(&w);
    // the covered copies are gone; the uncovered change keeps its bytes
    let reader = w.local.store().reader();
    assert_eq!(reader.redaction_of(kinds::CHANGE, &back).unwrap(), None);
    drop(reader);
    w.local.store_mut().reindex().unwrap();
    check(&w);
    w.reopen();
    check(&w);
    // the retry writes nothing
    let end = w.local.store().log_end();
    let again = w.fetch_bytes(&bytes, tip).unwrap();
    assert!(again.unchanged);
    assert_eq!(w.local.store().log_end(), end);
}

#[test]
fn scc5_removed_dark_variant_never_leaks() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    assert!(on_disk(&w.local_dir(), DARK));
    assert!(on_disk(&w.local_dir(), LIGHT));
    let targets = vec![
        w.figure.clone(),
        format!("blob:{}", w.light),
        format!("blob:{}", w.dark),
    ];
    remove_on(&mut w.hub, "redact", targets);
    assert!(!on_disk(&w.dir.path().join("hub"), DARK));
    let f = w.fetch();
    assert_eq!(f.removals.len(), 1);
    let out = w.pull_only();
    assert_eq!(out.status, PULLED, "{out:?}");
    // neither image survives here, and no read serves either
    assert!(!on_disk(&w.local_dir(), DARK));
    assert!(!on_disk(&w.local_dir(), LIGHT));
    let reader = w.local.store().reader();
    for blob in [&w.light, &w.dark] {
        assert!(
            reader
                .redaction_of(kinds::BLOB_MANIFEST, &h(blob))
                .unwrap()
                .is_some(),
            "{blob}"
        );
    }
    // an export of the base carries neither image's bytes
    let bytes = history(&w.local, w.base, None);
    assert!(!holds(&bytes, DARK));
    assert!(!holds(&bytes, LIGHT));
}

#[test]
fn scc5_stale_snapshot_cannot_export_removed_bytes() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    let prose = w.prose.clone();
    remove_on(&mut w.hub, "redact", vec![prose]);
    // a read taken before the fetch
    let stale = w.local.read();
    let before = history(&w.local, w.base, None);
    assert!(holds(&before, SECRET.as_bytes()));
    w.fetch();
    let e = stream_export::write_history(
        &stale,
        StreamPurpose::CanonHistory,
        w.base,
        None,
        &BTreeSet::new(),
        Vec::new(),
    )
    .unwrap_err();
    assert!(!e.message.contains(SECRET), "{e:?}");
    drop(stale);
    // a current read exports the base with the removed bytes as drops
    let bytes = history(&w.local, w.base, None);
    assert!(!holds(&bytes, SECRET.as_bytes()));
    let (_, recs) = records_of(&bytes);
    assert!(
        recs.iter()
            .any(|r| matches!(r, StreamRecord::Dropped { key, .. } if key.kind == kinds::BLOCK))
    );
}

#[test]
fn scc5_replaced_removal_stays_applied_after_reopen_and_reindex() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    let v = w.secret_version();
    let prose = w.prose.clone();
    let l = remove_on(&mut w.local, "redact", vec![prose]);
    let request = request_of(&w.local, &l);
    write(
        &mut w.local,
        "later",
        json!([{"op": "create", "type": "doc", "fields": {"name": "later", "title": "Later", "doc_type": "page"}}]),
    )
    .unwrap();
    let l3 = land_view(&mut w.local, "later");
    write(
        &mut w.hub,
        "hub",
        json!([{"op": "create", "type": "doc", "fields": {"name": "hub", "title": "Hub", "doc_type": "page"}}]),
    )
    .unwrap();
    land_view(&mut w.hub, "hub");
    w.fetch();
    let out = w.pull_only();
    assert_eq!(out.status, PULLED_WITH_CONFLICTS);
    assert_eq!(out.not_replayed[0].landing, l.to_string());
    assert_eq!(out.not_replayed[0].reason, StopReason::Removal);
    assert_eq!(out.replaced, Some(l3.to_string()));
    let check = |w: &World| {
        let reader = w.local.store().reader();
        assert_eq!(reader.canon_head().unwrap(), Some(head(&w.hub)));
        assert_eq!(
            reader.redaction_of(kinds::BLOCK, &v).unwrap(),
            Some(request)
        );
        assert!(reader.landing(&l).is_ok(), "L stays in the store");
        assert_eq!(w.prose_in(Scope::Canon), CONTENT_REMOVED);
        assert!(!on_disk(&w.local_dir(), SECRET.as_bytes()));
        reader.check_objects().unwrap();
    };
    check(&w);
    w.reopen();
    check(&w);
    w.local.store_mut().reindex().unwrap();
    check(&w);
}

#[test]
fn scc5_next_push_carries_replaced_removal_landing_and_proof() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    let mut hd = w.holder();
    w.applied(w.base);
    let v = w.secret_version();
    // L: an ordinary change beside the removal
    write(
        &mut w.local,
        "redact",
        json!([{"op": "create", "type": "doc", "fields": {"name": "beside", "title": "Beside", "doc_type": "page"}}]),
    )
    .unwrap();
    let prose = w.prose.clone();
    let l = remove_on(&mut w.local, "redact", vec![prose]);
    let request = request_of(&w.local, &l);
    write(
        &mut w.hub,
        "hub",
        json!([{"op": "create", "type": "doc", "fields": {"name": "hub", "title": "Hub", "doc_type": "page"}}]),
    )
    .unwrap();
    land_view(&mut w.hub, "hub");
    let hub_tip = head(&w.hub);
    push(
        &mut hd,
        &w.hub,
        Some(w.base),
        CanonMove::Extend,
        Some(w.base),
    );
    w.fetch();
    let out = w.pull_only();
    assert_eq!(out.status, PULLED_WITH_CONFLICTS);
    assert_eq!(out.not_replayed[0].removals, vec![request.to_string()]);
    // the person lands the view again; the removal needs no new approval
    let view = out.view.expect("pull-<n>");
    let t = land_view(&mut w.local, &view);
    assert_eq!(
        w.local.store().reader().landing(&t).unwrap().checks,
        Vec::<Hash>::new()
    );
    // the next push carries L outside canon, with its request, residues, carrier and apply
    let bytes = history(&w.local, t, Some(hub_tip));
    let keys = keys_of(&bytes);
    assert!(keys.contains(&ObjectKey {
        kind: kinds::LANDING,
        hash: l
    }));
    assert!(keys.contains(&ObjectKey {
        kind: kinds::REDACTION,
        hash: request
    }));
    assert!(keys.iter().any(|k| k.kind == kinds::REDACTION_APPLY));
    assert!(keys.iter().any(|k| k.kind == kinds::REDACTION_RESIDUE));
    assert!(!holds(&bytes, SECRET.as_bytes()));
    assert!(on_disk(&w.holder_dir(), SECRET.as_bytes()));
    let taken = push(
        &mut hd,
        &w.local,
        Some(hub_tip),
        CanonMove::Extend,
        Some(hub_tip),
    );
    assert_eq!(taken.applies.len(), 1, "{taken:?}");
    // the holder checks the proof, drops the bytes, and L never enters its canon
    let reader = hd.store().reader();
    assert_eq!(
        reader.redaction_of(kinds::BLOCK, &v).unwrap(),
        Some(request)
    );
    assert!(reader.landing(&l).is_ok());
    let chain = holder_chain(&hd, t);
    assert!(!chain.contains(&l), "{chain:?}");
    assert!(chain.contains(&hub_tip));
    assert!(!on_disk(&w.holder_dir(), SECRET.as_bytes()));
    reader.check_objects().unwrap();
}

#[test]
fn scc5_uncovered_warning_code_message_and_fix_are_exact() {
    assert_eq!(W_REDACTION_UNCOVERED, "W_REDACTION_UNCOVERED");
    assert_eq!(
        SYNC_UNCOVERED_MESSAGE,
        "change <change> holds content removed by <removal>"
    );
    assert_eq!(
        SYNC_UNCOVERED_FIX,
        "redact this content here; a person approves the removal"
    );
    let (c, r) = (Hash([0xab; 32]), Hash([0xcd; 32]));
    let got: UncoveredWarning = uncovered_warning(c, r);
    assert_eq!(got.code, W_REDACTION_UNCOVERED);
    assert_eq!(
        got.message,
        format!("change {c} holds content removed by {r}")
    );
    assert_eq!(got.fix, SYNC_UNCOVERED_FIX);
    assert_eq!(
        serde_json::to_value(&got).unwrap(),
        json!({
            "code": "W_REDACTION_UNCOVERED",
            "message": format!("change {c} holds content removed by {r}"),
            "fix": "redact this content here; a person approves the removal",
            "change": c.to_string(),
            "removal": r.to_string(),
        })
    );
}

/// Fix round 1, F1: a removal landed here and a fetched removal of another block of the same
/// change compose: the open, a reindex and a reopen keep both, and the native change residue's
/// reference to the block the fetched removal erased resolves through the fetched residue.
#[test]
fn scc5_native_and_fetched_removals_compose() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    let prose = w.prose.clone();
    let figure = w.figure.clone();
    let mine = remove_on(&mut w.local, "redact", vec![prose.clone()]);
    let mine_request = request_of(&w.local, &mine);
    let theirs = remove_on(&mut w.hub, "redact", vec![figure.clone()]);
    let theirs_request = request_of(&w.hub, &theirs);
    let f = w.fetch();
    assert_eq!(f.removals.len(), 1, "{f:?}");
    let check = |w: &World| {
        let read = w.local.read();
        let at = Scope::At(w.base);
        assert_eq!(
            read.block(&at, &id(&w.prose)).unwrap().body,
            CONTENT_REMOVED
        );
        let fig = read.block(&at, &id(&w.figure)).unwrap();
        assert_eq!(
            w.local
                .store()
                .reader()
                .redaction_of(kinds::BLOCK, &fig.version_hash())
                .unwrap(),
            Some(theirs_request)
        );
        assert_eq!(
            w.local
                .store()
                .reader()
                .redaction_of(kinds::BLOCK, &w.secret_version())
                .unwrap(),
            Some(mine_request)
        );
        assert!(!on_disk(&w.local_dir(), SECRET.as_bytes()));
        w.local.store().reader().check_objects().unwrap();
    };
    check(&w);
    w.reopen();
    check(&w);
    w.local.store_mut().reindex().unwrap();
    check(&w);
    w.reopen();
    check(&w);
    // a dump of it loads
    let loaded = dump_and_load(&w, "copy");
    loaded.store().reader().check_objects().unwrap();
    assert!(!on_disk(&w.dir.path().join("copy"), SECRET.as_bytes()));
}

/// Fix round 1, F1: the warning's fix works. After a fetched removal leaves an uncovered local
/// carrier, a person here approves a removal of that content: it lands, the carrier's bytes go,
/// the warning stops, and an open and a reindex keep it so.
#[test]
fn scc5_uncovered_carrier_fix_lands_locally() {
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    let back = away_and_back(&mut w, "keep");
    let prose = w.prose.clone();
    let l = remove_on(&mut w.hub, "redact", vec![prose.clone()]);
    let request = request_of(&w.hub, &l);
    let f = w.fetch();
    assert_eq!(f.warnings, vec![uncovered_warning(back, request)]);
    remove_on(&mut w.local, "keep", vec![prose]);
    let check = |w: &World| {
        assert!(w.local.read().uncovered_warnings().unwrap().is_empty());
        assert!(
            w.local
                .store()
                .reader()
                .redaction_of(kinds::CHANGE, &back)
                .unwrap()
                .is_some()
        );
        assert!(!on_disk(&w.local_dir(), SECRET.as_bytes()));
        w.local.store().reader().check_objects().unwrap();
    };
    check(&w);
    w.reopen();
    check(&w);
    w.local.store_mut().reindex().unwrap();
    check(&w);
    // a dump of it loads, and the loaded copy warns of nothing
    let loaded = dump_and_load(&w, "copy");
    assert!(loaded.read().uncovered_warnings().unwrap().is_empty());
    assert!(!on_disk(&w.dir.path().join("copy"), SECRET.as_bytes()));
}

/// Dumps `w`'s local project and loads the dump into a new directory; returns the loaded copy.
fn dump_and_load(w: &World, name: &str) -> Rdy {
    let file = w.dir.path().join(format!("{name}.rdydump"));
    w.local.read().dump(&file).unwrap();
    let dest = w.dir.path().join(name);
    std::fs::create_dir(&dest).unwrap();
    rdy_engine::load::load(&file, &dest).unwrap();
    Rdy::open(&dest).unwrap()
}

/// Fix round 1, F2: a project whose log layout differs from the hub's fetches a removal of a
/// version the hub wrote after the shared base; its dump loads, the loaded copy reads the stub
/// and holds no removed byte, and it dumps and loads again.
#[test]
fn scc5_fetched_removal_dump_loads_with_divergent_layout() {
    const NEWER: &str = "scc5-newer-removed-version";
    let _clock = pin_clock(1_000_000);
    let mut w = world();
    w.applied(w.base);
    let doc = w.doc.clone();
    write(
        &mut w.local,
        "title",
        json!([{"op": "update", "id": doc, "fields": {"title": "Local title"}}]),
    )
    .unwrap();
    land_view(&mut w.local, "title");
    let prose = w.prose.clone();
    write(
        &mut w.hub,
        "newer",
        json!([{"op": "update", "id": prose, "body": NEWER}]),
    )
    .unwrap();
    land_view(&mut w.hub, "newer");
    let l = remove_on(&mut w.hub, "redact", vec![prose.clone()]);
    let request = request_of(&w.hub, &l);
    let f = w.fetch();
    assert_eq!(f.removals.len(), 1, "{f:?}");
    assert!(!on_disk(&w.local_dir(), NEWER.as_bytes()));
    let loaded = dump_and_load(&w, "copy");
    let check = |r: &Rdy, dir: &Path| {
        let tip = w
            .local
            .store()
            .reader()
            .sync_canon("origin")
            .unwrap()
            .unwrap();
        let at = Scope::At(tip.fetched.unwrap());
        let b = r.read().block(&at, &id(&prose)).unwrap();
        assert_eq!(b.body, CONTENT_REMOVED);
        assert_eq!(
            r.store()
                .reader()
                .redaction_of(kinds::BLOCK, &b.version_hash())
                .unwrap(),
            Some(request)
        );
        assert!(!on_disk(dir, NEWER.as_bytes()));
        assert!(!on_disk(dir, SECRET.as_bytes()));
        r.store().reader().check_objects().unwrap();
    };
    check(&loaded, &w.dir.path().join("copy"));
    // the loaded copy dumps again, and that dump loads
    let file = w.dir.path().join("again.rdydump");
    loaded.read().dump(&file).unwrap();
    let dest = w.dir.path().join("again");
    std::fs::create_dir(&dest).unwrap();
    rdy_engine::load::load(&file, &dest).unwrap();
    check(&Rdy::open(&dest).unwrap(), &dest);
}
