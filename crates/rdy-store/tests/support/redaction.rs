//! The shared raw redaction batches of the store integration tests (M9). A test binary includes
//! this file with `#[path = "support/redaction.rs"] mod redaction_fixture;`.
//!
//! [`authorized_batch_fixture`] writes raw log bytes through the segment log, as a dump load does,
//! and never through a store writer: kinds 20 to 22 reach the log only through the store's checked
//! redaction writers, so a test that needs a stored apply (or a forged one) writes it here. The
//! store must be closed when the fixture runs; the caller opens it afterwards, and the open
//! replays the new batches into the index.
//!
//! The layout, per request (in request-hash order), is one sealed preparation batch:
//!
//! 1. the residues, in the order of the spans that first name them;
//! 2. the zero-op carrier change;
//! 3. the request;
//! 4. the carrier's view ops (the view's create when it has none, then the commit);
//! 5. the view's head ref.
//!
//! Then, unless the options stop after preparation, one sealed landing batch:
//!
//! 1. the proposal of the view's included changes;
//! 2. the snapshot and the landing, which carries the approval proof and names the requests of
//!    the carriers it lands in `checks`, sorted (as `land_atomic` records them);
//! 3. the proposing view's rebase op, then the refs (canon, the proposal, the view head);
//! 4. one REDACTION_APPLY per request, in request-hash order, binding the landing hash, the
//!    request hash and the report hash;
//! 5. the optional TRANSITION as the last data frame;
//! 6. the batch marker over the original frame hashes.
//!
//! Records are encoded with `rdy_model::redaction::encode_into`, the model's bounded encoder.
#![allow(dead_code)]

use rdy_model::redaction::{
    AssessmentSupport, BlockResidue, ChangeResidue, CopyKind, CopyRow, CopyStatus, DropSpan,
    FramePlace, MAX_RECORD_BYTES, ReasonClass, RedactTarget, Redaction, RedactionApply,
    RedactionManifest, RedactionResidue, carrier, encode_into,
};
use rdy_model::{
    ActorRun, ApprovalEvidence, Assurance, BlockId, Change, EvidenceKind, Hash, LandingRecord,
    Proposal, Provenance, RelayEvidence, Role,
};
use rdy_store::kinds::{self, BlobManifest, RefUpdate};
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::{Location, SegmentLog};
use rdy_store::{Store, StoreReader, ViewHead, ViewOp, ViewOpKind};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The segment size the store opens with.
const SEGMENT_MAX: u64 = 64 * 1024 * 1024;

/// The start of the log.
pub const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

/// `value`'s postcard bytes, through the model's bounded encoder.
pub fn enc<T: Serialize>(value: &T) -> Vec<u8> {
    let mut size = 4096;
    loop {
        let mut buf = vec![0u8; size];
        match encode_into(value, &mut buf) {
            Ok(used) => return used.to_vec(),
            Err(_) if size < MAX_RECORD_BYTES => size = (size * 4).min(MAX_RECORD_BYTES),
            Err(e) => panic!("a fixture record does not fit {MAX_RECORD_BYTES} bytes: {e}"),
        }
    }
}

/// The hash of `value`'s postcard bytes.
pub fn hash_of<T: Serialize>(value: &T) -> Hash {
    Hash::of(&enc(value))
}

/// The frame of a new residue `r` as the store writes one: its raw kind and payload, in its
/// kind's latest encoding.
pub fn residue_frame(r: &RedactionResidue) -> (u8, Vec<u8>) {
    rdy_store::codec::frame_latest(kinds::REDACTION_RESIDUE, enc(r))
}

/// The hash of a new residue `r`: its frame's ([`residue_frame`]).
pub fn residue_id(r: &RedactionResidue) -> Hash {
    Hash::of(&residue_frame(r).1)
}

/// The six fixed outside-copy rows (D-14SAY), in CopyKind order.
pub fn outside_rows() -> Vec<CopyRow> {
    [
        CopyKind::Render,
        CopyKind::Export,
        CopyKind::Dump,
        CopyKind::Clone,
        CopyKind::Published,
        CopyKind::WorkingCopy,
    ]
    .into_iter()
    .map(|kind| CopyRow {
        kind,
        status: CopyStatus::Unknown,
    })
    .collect()
}

/// The approval proof a landing carries. Each field can be altered on its own.
#[derive(Clone, Debug)]
pub struct Proof {
    pub approved_by: Provenance,
    pub assurance: Assurance,
    pub relayed_by: Option<ActorRun>,
    pub relay_evidence: Option<RelayEvidence>,
    pub evidence: Option<ApprovalEvidence>,
}

impl Proof {
    /// A person approving on their own machine: an Approver, Local, no relay fields.
    pub fn local_human() -> Proof {
        Proof {
            approved_by: Provenance {
                actor: "human:alice".into(),
                run: None,
                role: Role::Approver,
                at_ms: 50,
            },
            assurance: Assurance::Local,
            relayed_by: None,
            relay_evidence: None,
            evidence: None,
        }
    }

    /// A person's approval relayed by an agent from the shown card: an Approver, Relayed, both
    /// relay fields and card evidence.
    pub fn relayed_human() -> Proof {
        Proof {
            approved_by: Provenance {
                actor: "human:alice".into(),
                run: None,
                role: Role::Approver,
                at_ms: 50,
            },
            assurance: Assurance::Relayed,
            relayed_by: Some(ActorRun {
                actor: "agent:relay".into(),
                run: Some("r1".into()),
            }),
            relay_evidence: Some(RelayEvidence {
                quoted: "Approve".into(),
                session_ref: "session-1".into(),
                question: Some("Remove the content?".into()),
            }),
            evidence: Some(ApprovalEvidence {
                kind: EvidenceKind::Card,
                options: vec!["Approve".into(), "Reject".into()],
                choice: Some("Approve".into()),
                card_hash: Some(Hash::of(b"redaction card")),
            }),
        }
    }
}

/// One request the fixture records.
#[derive(Clone, Debug)]
pub struct RequestSpec {
    pub view: String,
    pub requester: Provenance,
    pub targets: Vec<RedactTarget>,
    pub reason_class: ReasonClass,
    pub reason: String,
}

impl RequestSpec {
    /// A request in view `view` by a writer agent, for `targets`.
    pub fn new(view: &str, targets: Vec<RedactTarget>) -> RequestSpec {
        RequestSpec {
            view: view.into(),
            requester: Provenance {
                actor: "agent:requester".into(),
                run: Some("q1".into()),
                role: Role::Writer,
                at_ms: 40,
            },
            targets,
            reason_class: ReasonClass::Secret,
            reason: "a leaked key".into(),
        }
    }

    /// The same request made at `at_ms`: two requests of one requester in one view need two
    /// times, or their carriers are the same change.
    pub fn at(mut self, at_ms: i64) -> RequestSpec {
        self.requester.at_ms = at_ms;
        self
    }
}

/// A request as the store's preparation takes it: the carrier, the request and its residues.
#[derive(Clone, Debug)]
pub struct Draft {
    pub carrier: Change,
    pub request: Redaction,
    pub residues: Vec<RedactionResidue>,
}

impl Draft {
    /// The carrier's id (its deps are the store's: a zero-op change has none).
    pub fn carrier_id(&self) -> Hash {
        self.carrier.id()
    }

    /// The request's hash.
    pub fn request_id(&self) -> Hash {
        hash_of(&self.request)
    }

    /// The report hash the request binds.
    pub fn report_hash(&self) -> Hash {
        let mut buf = vec![0u8; MAX_RECORD_BYTES];
        self.request.manifest.report_hash(&mut buf).unwrap()
    }
}

/// Points each block and change span at the residue whose recorded version or change is the
/// span's hash, and each chunk span at none.
pub fn relink(request: &mut Redaction, residues: &[RedactionResidue]) {
    relink_in(
        request,
        residues,
        rdy_store::codec::latest(kinds::REDACTION_RESIDUE),
    );
}

/// The frame of residue `r` in encoding `encoding`.
pub fn residue_frame_in(r: &RedactionResidue, encoding: u16) -> (u8, Vec<u8>) {
    let body = rdy_store::codec::residue_body(r, encoding)
        .expect("the residue has a form in this encoding");
    rdy_store::codec::frame(kinds::REDACTION_RESIDUE, encoding, body).unwrap()
}

/// [`relink`], naming each residue by its frame in encoding `encoding`.
pub fn relink_in(request: &mut Redaction, residues: &[RedactionResidue], encoding: u16) {
    for span in &mut request.manifest.spans {
        span.residue = residues
            .iter()
            .find(|r| match r {
                RedactionResidue::Block(b) => span.kind == kinds::BLOCK && b.version == span.hash,
                RedactionResidue::Change(c) => span.kind == kinds::CHANGE && c.change == span.hash,
            })
            .map(|r| Hash::of(&residue_frame_in(r, encoding).1));
    }
}

fn place(loc: Location) -> FramePlace {
    FramePlace {
        segment: loc.segment,
        offset: loc.offset,
    }
}

/// One frame of the log: where it is, its kind, hash and payload length, and its payload while
/// the store still serves it.
#[derive(Clone, Debug)]
pub struct LogFrame {
    pub loc: Location,
    pub kind: u8,
    pub hash: Hash,
    pub len: u32,
    pub payload: Option<Vec<u8>>,
}

/// Every frame of the log up to `end`, each by its logical kind (`rdy_store::codec`).
pub fn frames(reader: &StoreReader, end: Location) -> Vec<LogFrame> {
    let mut out = vec![];
    reader
        .for_each_record_from(ORIGIN, end, |loc, frame, _| {
            match frame {
                StoredFrame::Present {
                    kind,
                    hash,
                    payload,
                } => out.push(LogFrame {
                    loc,
                    kind: rdy_store::codec::logical_kind(kind),
                    hash,
                    len: payload.len() as u32,
                    payload: Some(payload.to_vec()),
                }),
                StoredFrame::Dropped {
                    kind, hash, len, ..
                } => out.push(LogFrame {
                    loc,
                    kind: rdy_store::codec::logical_kind(kind),
                    hash,
                    len,
                    payload: None,
                }),
            }
            Ok(())
        })
        .unwrap();
    out
}

/// The request `spec` describes, built from the store as it is now: every version the targets
/// select, every intact BLOCK frame of them and every intact CHANGE frame of their producers,
/// every intact chunk frame of the selected blobs, a residue for each block and change span, and
/// the blobs sharing a selected chunk. Frames an earlier apply already removed are left out
/// (their residual references remain).
pub fn build_request(store: &Store, spec: &RequestSpec) -> Draft {
    let reader = store.reader();
    let all = frames(&reader, store.log_end());
    let mut versions: BTreeSet<(BlockId, Hash)> = BTreeSet::new();
    let mut blobs: BTreeSet<Hash> = BTreeSet::new();
    for t in &spec.targets {
        match t {
            RedactTarget::AllVersions(id) => {
                for LogFrame { kind, hash, .. } in &all {
                    if *kind == kinds::BLOCK && reader.get_block(hash).unwrap().id == *id {
                        versions.insert((id.clone(), *hash));
                    }
                }
            }
            RedactTarget::Version { id, version } => {
                versions.insert((id.clone(), *version));
            }
            RedactTarget::Blob(h) => {
                blobs.insert(*h);
            }
        }
    }
    let version_set: BTreeSet<Hash> = versions.iter().map(|(_, v)| *v).collect();
    let mut changes: BTreeSet<Hash> = BTreeSet::new();
    for v in &version_set {
        changes.extend(reader.producers(v).unwrap());
    }
    let mut chunks: BTreeSet<Hash> = BTreeSet::new();
    for b in &blobs {
        let m: BlobManifest = reader.get_typed(kinds::BLOB_MANIFEST, b).unwrap();
        chunks.extend(m.chunks);
    }
    let mut shared_blobs = BTreeSet::new();
    let mut spans = vec![];
    let mut residues: Vec<RedactionResidue> = vec![];
    for LogFrame {
        loc,
        kind,
        hash,
        len,
        payload,
    } in &all
    {
        if payload.is_none() {
            continue;
        }
        if *kind == kinds::BLOB_MANIFEST && !blobs.contains(hash) {
            let m: BlobManifest = reader.get_typed(kinds::BLOB_MANIFEST, hash).unwrap();
            if m.chunks.iter().any(|c| chunks.contains(c)) {
                shared_blobs.insert(*hash);
            }
        }
        let selected = (*kind == kinds::BLOCK && version_set.contains(hash))
            || (*kind == kinds::CHANGE && changes.contains(hash))
            || (*kind == kinds::BLOB_CHUNK && chunks.contains(hash));
        if !selected {
            continue;
        }
        let residue = match *kind {
            kinds::BLOCK => Some(RedactionResidue::Block(BlockResidue::of(
                &reader.get_block(hash).unwrap(),
            ))),
            kinds::CHANGE => Some(RedactionResidue::Change(ChangeResidue::of(
                &reader.change(hash).unwrap(),
            ))),
            _ => None,
        };
        if let Some(r) = &residue
            && !residues.contains(r)
        {
            residues.push(r.clone());
        }
        spans.push(DropSpan {
            place: place(*loc),
            kind: *kind,
            hash: *hash,
            len: *len,
            residue: residue.as_ref().map(residue_id),
        });
    }
    let carrier = carrier(spec.view.clone(), vec![], spec.requester.clone());
    let request = Redaction {
        carrier: carrier.id(),
        actor: spec.requester.clone(),
        targets: spec.targets.clone(),
        reason_class: spec.reason_class,
        reason: spec.reason.clone(),
        manifest: RedactionManifest {
            versions: versions.into_iter().collect(),
            blobs: blobs.into_iter().collect(),
            shared_blobs: shared_blobs.into_iter().collect(),
            spans,
            inside: vec![],
            outside: outside_rows(),
            quarantine: vec![],
            assessment: AssessmentSupport::NotImplemented,
        },
    };
    Draft {
        carrier,
        request,
        residues,
    }
}

/// A hook that alters a draft before its preparation batch is encoded.
pub type DraftHook = Box<dyn Fn(usize, &mut Draft)>;
/// A hook that alters the landing record before its hash is taken.
pub type LandingHook = Box<dyn Fn(&mut LandingRecord)>;
/// A hook that alters the apply records (in request-hash order) before they are encoded.
pub type ApplyHook = Box<dyn Fn(&mut Vec<RedactionApply>)>;

/// What the fixture writes, and how.
pub struct FixtureOptions {
    /// The approval proof of the landing.
    pub proof: Proof,
    /// Write the landing batch; false stops after the preparation batches.
    pub land: bool,
    /// A TRANSITION payload to write as the landing batch's last data frame.
    pub transition: Option<Vec<u8>>,
    /// End the landing batch with its marker; false leaves an unsealed tail.
    pub seal: bool,
    pub on_draft: Option<DraftHook>,
    pub on_landing: Option<LandingHook>,
    pub on_applies: Option<ApplyHook>,
    /// The encoding the residue frames are written in: the latest (a new residue), or 0 (a
    /// residue an rdy before content-2 wrote). Spans name residues as [`relink_in`] does.
    pub residue_encoding: u16,
}

impl FixtureOptions {
    /// A sealed landing approved by `proof`, with no transition and no hooks.
    pub fn approved(proof: Proof) -> FixtureOptions {
        FixtureOptions {
            proof,
            land: true,
            transition: None,
            seal: true,
            on_draft: None,
            on_landing: None,
            on_applies: None,
            residue_encoding: rdy_store::codec::latest(kinds::REDACTION_RESIDUE),
        }
    }

    /// Only the preparation batches.
    pub fn prepared_only() -> FixtureOptions {
        FixtureOptions {
            land: false,
            ..FixtureOptions::approved(Proof::local_human())
        }
    }
}

/// One recorded request.
#[derive(Clone, Debug)]
pub struct FixtureRequest {
    pub draft: Draft,
    pub carrier: Hash,
    pub request: Hash,
    /// The kind and hash of each data frame of its preparation batch, in order.
    pub batch: Vec<(u8, Hash)>,
}

/// What the fixture wrote.
#[derive(Clone, Debug)]
pub struct Fixture {
    /// The requests, in request-hash order.
    pub requests: Vec<FixtureRequest>,
    pub proposal: Option<Hash>,
    pub landing: Option<Hash>,
    pub landing_record: Option<LandingRecord>,
    /// Each apply's hash and record, in request-hash order.
    pub applies: Vec<(Hash, RedactionApply)>,
    /// Where the landing batch starts.
    pub landing_start: Option<Location>,
    /// The kind and hash of each data frame of the landing batch, in order.
    pub landing_batch: Vec<(u8, Hash)>,
}

/// The refs of a batch as the store salts them: the i-th ref's `at` is the batch start plus i.
fn ref_frames(at: Location, refs: Vec<(String, Vec<u8>)>) -> Vec<(u8, Vec<u8>)> {
    refs.into_iter()
        .enumerate()
        .map(|(i, (name, value))| {
            let at = Location {
                segment: at.segment,
                offset: at.offset + i as u64,
            };
            (kinds::REF_UPDATE, enc(&RefUpdate { name, value, at }))
        })
        .collect()
}

/// Appends `frames` to the closed store at `dir` as one batch: with its marker when `seal`.
fn append(dir: &Path, frames: &[(u8, Vec<u8>)], seal: bool) -> Location {
    let (mut log, quarantined) =
        SegmentLog::open_marked(&dir.join("segments"), SEGMENT_MAX, None).unwrap();
    assert!(
        quarantined.is_empty(),
        "the fixture's store has a torn tail"
    );
    let start = if seal {
        log.append_batch(frames).unwrap()[0].0
    } else {
        log.append_frames(frames, true).unwrap()[0].0
    };
    log.sync().unwrap();
    start
}

/// Writes, to the closed store at `dir`, one preparation batch per request of `specs` and then
/// (unless `opts.land` is false) one landing batch that lands every carrier and applies every
/// request. All the requests share one view. See the module docs for the layout.
pub fn authorized_batch_fixture(
    dir: &Path,
    specs: &[RequestSpec],
    opts: &FixtureOptions,
) -> Fixture {
    assert!(
        !specs.is_empty(),
        "the fixture records at least one request"
    );
    let view = specs[0].view.clone();
    assert!(
        specs.iter().all(|s| s.view == view),
        "the fixture's requests share one view"
    );
    // the drafts, from the store before any of them
    let mut drafts = {
        let store = Store::open(dir).unwrap();
        let drafts = specs
            .iter()
            .map(|s| build_request(&store, s))
            .collect::<Vec<_>>();
        let carriers: BTreeSet<Hash> = drafts.iter().map(Draft::carrier_id).collect();
        assert_eq!(
            carriers.len(),
            drafts.len(),
            "two requests share a carrier: give them distinct times (RequestSpec::at)"
        );
        for c in &carriers {
            assert!(
                !store.has_kind(kinds::CHANGE, c).unwrap(),
                "carrier {c} is already stored: give the request another time (RequestSpec::at)"
            );
        }
        drafts
    };
    for (i, d) in drafts.iter_mut().enumerate() {
        if let Some(hook) = &opts.on_draft {
            hook(i, d);
        }
    }
    drafts.sort_by_key(Draft::request_id);
    let mut requests = vec![];
    for draft in drafts {
        let (head, canon, end) = {
            let store = Store::open(dir).unwrap();
            let head = store.view_head(&view).unwrap();
            (head, store.canon_head().unwrap(), store.log_end())
        };
        let mut items: Vec<(u8, Vec<u8>)> = vec![];
        let mut seen = BTreeSet::new();
        for r in &draft.residues {
            let frame = residue_frame_in(r, opts.residue_encoding);
            if seen.insert(Hash::of(&frame.1)) {
                items.push(frame);
            }
        }
        // the carrier's frame in CHANGE's latest encoding (`rdy_store::codec`)
        let (carrier_kind, carrier_bytes) =
            rdy_store::codec::item(kinds::CHANGE, &draft.carrier).unwrap();
        let carrier_id = Hash::of(&carrier_bytes);
        items.push((carrier_kind, carrier_bytes));
        let request_bytes = enc(&draft.request);
        let request_id = Hash::of(&request_bytes);
        items.push((kinds::REDACTION, request_bytes));
        let by = &draft.carrier.provenance;
        let op = |prev: Option<Hash>, op: ViewOpKind| ViewOp {
            view: view.clone(),
            prev,
            op,
            actor: by.actor.clone(),
            run: by.run.clone(),
            at_ms: by.at_ms,
        };
        let (prev, base, len, owner, pinned) = match head {
            Some(h) => (h.head, h.base, h.len, h.owner, h.pinned),
            None => {
                let owner = Some(rdy_store::Owner {
                    actor: by.actor.clone(),
                    run: by.run.clone(),
                });
                let create = enc(&op(
                    None,
                    ViewOpKind::Create {
                        base: canon,
                        owner: owner.clone(),
                        pinned: false,
                    },
                ));
                let id = Hash::of(&create);
                items.push((kinds::VIEW_OP, create));
                (id, canon, 0, owner, false)
            }
        };
        let commit = enc(&op(Some(prev), ViewOpKind::Commit { change: carrier_id }));
        let commit_id = Hash::of(&commit);
        items.push((kinds::VIEW_OP, commit));
        let new_head = ViewHead {
            head: commit_id,
            base,
            len: len + 1,
            owner,
            pinned,
            closed: false,
            last_write_ms: by.at_ms,
        };
        let at = Location {
            segment: end.segment,
            offset: end.offset + items.len() as u64,
        };
        items.extend(ref_frames(
            at,
            vec![(format!("view/{view}"), enc(&new_head))],
        ));
        let batch = items.iter().map(|(k, p)| (*k, Hash::of(p))).collect();
        append(dir, &items, true);
        requests.push(FixtureRequest {
            draft,
            carrier: carrier_id,
            request: request_id,
            batch,
        });
    }
    let mut fixture = Fixture {
        requests,
        proposal: None,
        landing: None,
        landing_record: None,
        applies: vec![],
        landing_start: None,
        landing_batch: vec![],
    };
    if !opts.land {
        return fixture;
    }
    // the landing batch
    let store = Store::open(dir).unwrap();
    let head = store.view_head(&view).unwrap().expect("the carriers' view");
    let included = store.included(&view).unwrap().as_ref().clone();
    let changes = included
        .iter()
        .map(|c| store.change(c).unwrap())
        .collect::<Vec<_>>();
    let canon = store.canon_head().unwrap();
    let state = store
        .merge_checked(&store.canon_state().unwrap(), &changes)
        .unwrap()
        .0
        .state;
    let requester = &fixture.requests[0].draft.carrier.provenance;
    let proposal = Proposal {
        stable: None,
        prev: None,
        title: "Remove content".into(),
        view: view.clone(),
        changes: included.clone(),
        alternative: None,
        provenance: requester.clone(),
        canon_at: canon,
        summary: None,
    };
    let proposal_bytes = enc(&proposal);
    let pid = Hash::of(&proposal_bytes);
    let state_bytes = enc(&state);
    let sid = Hash::of(&state_bytes);
    let p = &opts.proof;
    // a removal landing names its requests, sorted, as `land_atomic` records them
    let mut checks: Vec<Hash> = included
        .iter()
        .filter_map(|c| store.redaction_for_carrier(c).unwrap())
        .collect();
    checks.sort();
    checks.dedup();
    let mut landing = LandingRecord {
        proposal: pid,
        prev: canon,
        state: sid,
        approved_by: p.approved_by.clone(),
        changes: included.clone(),
        chosen: None,
        checks,
        assurance: p.assurance,
        relayed_by: p.relayed_by.clone(),
        relay_evidence: p.relay_evidence.clone(),
        approved_version: Some(pid),
        rebased_from: vec![],
        evidence: p.evidence.clone(),
        assessment: None,
        policy: None,
        read_identity: None,
    };
    if let Some(hook) = &opts.on_landing {
        hook(&mut landing);
    }
    // the landing's frame in LANDING's latest encoding (`rdy_store::codec`)
    let (landing_kind, landing_bytes) = rdy_store::codec::item(kinds::LANDING, &landing).unwrap();
    let lid = Hash::of(&landing_bytes);
    let rebase = enc(&ViewOp {
        view: view.clone(),
        prev: Some(head.head),
        op: ViewOpKind::Rebase {
            landing: lid,
            dropped: included.clone(),
            remaining: vec![],
            conflicts: vec![],
        },
        actor: p.approved_by.actor.clone(),
        run: p.approved_by.run.clone(),
        at_ms: p.approved_by.at_ms,
    });
    let new_head = ViewHead {
        head: Hash::of(&rebase),
        base: Some(lid),
        len: 0,
        owner: head.owner.clone(),
        pinned: head.pinned,
        closed: head.closed,
        last_write_ms: head.last_write_ms,
    };
    let mut items = vec![
        (kinds::PROPOSAL, proposal_bytes),
        (kinds::SNAPSHOT, state_bytes),
        (landing_kind, landing_bytes),
        (kinds::VIEW_OP, rebase),
    ];
    let end = store.log_end();
    let at = Location {
        segment: end.segment,
        offset: end.offset + items.len() as u64,
    };
    items.extend(ref_frames(
        at,
        vec![
            ("canon".into(), lid.0.to_vec()),
            (format!("proposal/{pid}"), pid.0.to_vec()),
            (format!("view/{view}"), enc(&new_head)),
        ],
    ));
    drop(store);
    let mut applies: Vec<RedactionApply> = fixture
        .requests
        .iter()
        .map(|r| RedactionApply {
            request: r.request,
            landing: lid,
            report_hash: r.draft.report_hash(),
        })
        .collect();
    if let Some(hook) = &opts.on_applies {
        hook(&mut applies);
    }
    for a in &applies {
        items.push((kinds::REDACTION_APPLY, enc(a)));
    }
    if let Some(t) = &opts.transition {
        items.push((kinds::TRANSITION, t.clone()));
    }
    fixture.landing_batch = items.iter().map(|(k, p)| (*k, Hash::of(p))).collect();
    fixture.landing_start = Some(append(dir, &items, opts.seal));
    fixture.proposal = Some(pid);
    fixture.landing = Some(lid);
    fixture.landing_record = Some(landing);
    fixture.applies = applies.into_iter().map(|a| (hash_of(&a), a)).collect();
    fixture
}

/// The bytes of every segment file of the store at `dir`, by file name.
pub fn segment_bytes(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(dir.join("segments")).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("seg-") && name.ends_with(".log") {
            out.insert(name, std::fs::read(entry.path()).unwrap());
        }
    }
    out
}
