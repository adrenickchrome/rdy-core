//! The dump chains of the dump format 3 fixtures (D-2FMRR, D-QF2HQ, D-69AY3): a full dump and two
//! increments of the every-kind project, and of a holder filled from the every-kind holder's
//! uploads, each with a person's approved removal in its last increment. The fixture writers
//! (`hc9_write_format_three_fixtures_once` wrote the store format 16 members, and
//! `cp3_write_format17_fixtures_once` writes the store format 17 members) build them once; the
//! tests read only the frozen files. These builders write this rdy's store format: the members
//! of an earlier format are the frozen files alone.
//!
//! Store format 17 (D-4JMPH): the project chain's full dump holds a proposal with its bound
//! plan (an unsealed PLAN, [`PLAN_VIEW`]); increment 1 holds that proposal's landing and its
//! seal, which names the earlier file's plan. The holder's second source (`b`) writes a proposal
//! with its bound plan, and a clone of it (`c`) is made; each lands that proposal with its own
//! approver and seal, and tags it ([`PLAN_TAG`] in `b`, [`PLAN_FORK_TAG`] in `c`). Each history
//! reaches the holder through an export, a batch check and a take: increment 1 takes `b`'s seal,
//! its chain and its acceptance; increment 2 takes `c`'s seal, whose chain names the bound plan
//! that increment 1 holds. The standalone holder takes both histories again after its other
//! takes.
//!
//! - The project chain starts from [`every_kind::rolling_project`]. Before the full dump it adds a
//!   removable prose block (its version and the change that embeds it, with [`EARLY_CANARY`]) and
//!   a blob of two chunks (its manifest, a full first chunk and a tail with [`BLOB_CANARY`]).
//!   Increment 1 holds an ordinary landing and the tag [`ORDINARY_TAG`]. Increment 2 holds a
//!   block first written there ([`LATE_CANARY`]) and its landing (the held stop), the request and
//!   proposal of the first removal (the missing stop is the end of that proposal), the person's
//!   approval of the first removal (the early block and the blob), the person's approval of a
//!   second removal (the late block), and a last tagged landing.
//! - The holder chain takes the uploads of [`every_kind::holder`] into a new holder of the same
//!   repo: `v1.0.0` before the full dump, `v2.0.0` in increment 1, and `v3.0.0` (the source's
//!   approved removal) in increment 2, then the earlier uploads again, which append nothing.
//!
//! Store format 18 (D-4WC6R): [`project_chain_v18`] and [`holder_chain_v18`] run the same
//! sequence with the three stages of the reflects fixture (`every_kind::add_reflects_fixture_records`,
//! `rewrite_reflects_fixture` and `finish_reflects_fixture`): before the full dump, in increment 1
//! and in increment 2 (before its late block). The project writes them under a doc of its own;
//! the holder takes three uploads of a copy of the second source (`r`, copied before the plan
//! uploads change `b`), each tagged ([`REFLECTS_TAGS`]) and exported, checked and taken at the
//! same boundaries. [`project_chain`] and [`holder_chain`] select no stage, so their output is
//! the format-17 sequence's, unchanged.
//!
//! The current set (D-WWAK7, D-69AY3): [`project_chain_current`] and [`holder_chain_current`]
//! select the stages of the store this rdy writes, over the same shared builders. With every
//! kind's latest encoding 0 and no registered feature, that is the store format 18 sequence;
//! the current writer gives it dump format 4, a feature manifest and the latest-encoding map
//! ([`current_formats`]). A later encoding or feature extends the current stage selector, and
//! its task freezes a new complete set; a released set is never regenerated.
//!
//! Each standalone full fixture is a dump of a stopped load: the full dump written after the
//! removals, loaded at the missing stop (every carried span is zero), then written to again. The
//! project gets a new block, an ordinary tagged landing and a person's approved removal of that
//! block; the holder takes `v2.0.0` and `v3.0.0` again, and the take of the matching apply ends
//! its warning. No holder workflow and no approval path is added here: every step is an existing
//! engine call.
use crate::every_kind;
use crate::fixtures::{approver, land, write};
use crate::intake::PERSON;
use crate::tensions::{ag, created};
use rdy_engine::dump_stop::DumpStop;
use rdy_engine::load::load_chain;
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, pin_clock};
use rdy_model::plan::{
    Plan, PlanAct, PlanAnswer, PlanHead, PlanKind, PlanOutcome, PlanSeal, SealedItem, item_hash,
};
use rdy_model::{Answer, AnswerAct, BlockId, Hash, Provenance, RepoId, Role};
use rdy_store::kinds;
use rdy_store::segment::Location;
use rdy_store::take_batch::UploadTarget;
use serde_json::json;
use std::fs::File;
use std::path::PathBuf;

/// The segment size limit of every store the chains are written from: batches roll over.
pub const SEGMENT_MAX: u64 = 4096;
/// The id seed of the project chain's writer.
pub const PROJECT_SEED: u64 = 16;
/// The id seed of the holder chain's writer: another stream, so the holder's repo is not the
/// project's.
pub const HOLDER_SEED: u64 = 0x1616;
/// The clock of every provenance the writer records.
pub const CLOCK_MS: i64 = 1_760_000_000_000;
/// The body of the removable block the full dump holds.
pub const EARLY_CANARY: &str = "Harrier early canary.";
/// The body of the block first written in increment 2, which the second removal selects.
pub const LATE_CANARY: &str = "Harrier late canary.";
/// The body of the block the standalone project fixture writes and then removes.
pub const STANDALONE_CANARY: &str = "Harrier standalone canary.";
/// The tail of the removable blob: its second chunk holds it.
pub const BLOB_CANARY: &[u8] = b"Harrier blob canary tail.";
/// The tag of increment 1's ordinary landing.
pub const ORDINARY_TAG: &str = "v0.3.0";
/// The tag of the last landing of increment 2, after both removals.
pub const LATE_TAG: &str = "v0.5.0";
/// The tag of the standalone project fixture's ordinary landing, before its removal.
pub const STANDALONE_TAG: &str = "v0.6.0";
/// The view of the project chain's plan: bound in the full dump, sealed in increment 1.
pub const PLAN_VIEW: &str = "harrier-plan";
/// The tag of the holder's second source's sealed plan landing, taken in the holder chain's
/// first increment.
pub const PLAN_TAG: &str = "v4.0.0";
/// The tag of the other seal of that plan, in a clone of the second source, taken in the holder
/// chain's last increment.
pub const PLAN_FORK_TAG: &str = "v4.1.0";
/// The approver of the clone's landing of the plan's proposal: another landing of one version.
pub const PLAN_FORK_PERSON: &str = "human:qa-fork";
/// The doc the project chain's reflects fixture writes under (store format 18).
pub const REFLECTS_DOC: &str = "harrier-reflects";
/// The tags of the holder's reflects source after each of the three stages (store format 18).
pub const REFLECTS_TAGS: [&str; 3] = ["v5.0.0", "v5.1.0", "v5.2.0"];

/// The store format 18 stages a chain builder adds: none for the store format 17 builders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stages {
    /// The store format 17 sequence alone.
    None,
    /// The reflects fixture's three stages (D-4WC6R).
    Reflects,
}

/// The stages of the store this rdy writes: every stage of the shared builders.
const CURRENT_STAGES: Stages = Stages::Reflects;

/// What the current set pins of the written formats: the dump and sync formats, the feature
/// manifest a new store writes and the latest encoding of every assigned kind.
pub fn current_formats() -> serde_json::Value {
    let encodings: serde_json::Map<String, serde_json::Value> =
        rdy_store::dump::current_frame_kinds()
            .iter()
            .map(|k| (k.to_string(), json!(rdy_store::codec::latest(*k))))
            .collect();
    json!({
        "dump_format": rdy_store::dump::DUMP_FORMAT,
        "stream_format": rdy_store::sync_stream::STREAM_FORMAT,
        "features": rdy_store::format::baseline_features(),
        "encodings": encodings,
    })
}

/// The removable blob: a full first chunk (1 MiB of one byte) and a short second chunk.
pub fn blob_bytes() -> Vec<u8> {
    let mut b = vec![0x6b_u8; 1 << 20];
    b.extend_from_slice(BLOB_CANARY);
    b
}

/// A built chain: the store it was written from, the three files, the full dump written after
/// the removals (no fixture), and the two stops.
pub struct Chain {
    pub dir: tempfile::TempDir,
    /// The directory of the source store, when it is not under `dir`.
    pub source: Option<tempfile::TempDir>,
    pub root: PathBuf,
    pub full: PathBuf,
    pub inc1: PathBuf,
    pub inc2: PathBuf,
    /// The full dump of the source after its removals: the standalone fixture's input.
    pub post: PathBuf,
    /// A stop before the removal's apply: the earlier files' payloads are held.
    pub held_stop: String,
    /// A stop before the removal's apply; in the full dump written after the removal, every
    /// payload it selects before the stop is missing.
    pub missing_stop: String,
}

impl Chain {
    pub fn files(&self) -> Vec<PathBuf> {
        vec![self.full.clone(), self.inc1.clone(), self.inc2.clone()]
    }
}

/// A built standalone full fixture: the stopped store written to again, its dump, and the stop
/// before its own later apply.
pub struct Standalone {
    pub dir: tempfile::TempDir,
    pub root: PathBuf,
    pub dump: PathBuf,
    pub stop: String,
}

fn seal(r: &mut Rdy) {
    r.seal_open().unwrap();
}

/// Agent `name` asks to remove `targets` in view `name`; the person approves the proposal.
/// Returns the end of the proposal's batch, before the approval's.
fn remove(r: &mut Rdy, name: &str, targets: Vec<String>, why: &str) -> Location {
    let rq = ag(name);
    r.redact_in(
        &rq,
        name,
        RedactInput {
            targets,
            reason_class: "personal".into(),
            reason: why.into(),
            dry_run: false,
        },
    )
    .unwrap()
    .redaction
    .expect("the request is recorded");
    let p = r.propose(&rq, name, name, Some(why)).unwrap();
    seal(r);
    let before = r.store().log_end();
    r.land(&approver(), &p.id).unwrap();
    seal(r);
    before
}

/// One prose block with `body` in a new doc `name`, landed through view `name`. Returns the
/// block's id and the landing's hash.
fn landed_block(r: &mut Rdy, name: &str, body: &str) -> (String, String) {
    let w = write(
        r,
        name,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": name, "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": body}
        ]),
    )
    .unwrap();
    let landing = land(r, name).unwrap();
    seal(r);
    (created(&w, "$p").to_string(), landing)
}

fn tag(r: &mut Rdy, name: &str) {
    r.tag(&approver(), name, None, None).unwrap();
    seal(r);
}

/// A new doc `name`, landed through view `name`. Returns its id.
fn landed_doc(r: &mut Rdy, name: &str) -> BlockId {
    let w = write(
        r,
        name,
        json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": name, "doc_type": "page"}}]),
    )
    .unwrap();
    land(r, name).unwrap();
    seal(r);
    created(&w, "$d")
}

fn position(l: Location) -> String {
    format!("pos:{}.{}", l.segment, l.offset)
}

fn plan_prov(actor: &str, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: None,
        role,
        at_ms: CLOCK_MS,
    }
}

/// A proposal of view `view` (a prose block in a new doc) with its bound plan, in one batch, as
/// the plan writers compose them. Returns the proposal and the bound version.
fn bound_plan(r: &mut Rdy, view: &str, body: &str) -> (Hash, Plan) {
    write(
        r,
        view,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": view, "title": view, "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": body}
        ]),
    )
    .unwrap();
    seal(r);
    let proposal = r
        .store_mut()
        .propose(view, view, plan_prov("agent:plan", Role::Writer))
        .unwrap();
    let bound = Plan {
        stable: None,
        prev: None,
        view: view.into(),
        proposal: Some(proposal),
        answers: vec![],
        heads: vec![PlanHead {
            repo: "app".into(),
            head: Some("c".repeat(40)),
        }],
        provenance: plan_prov("agent:plan", Role::Writer),
        seal: None,
    };
    r.store_mut()
        .write_guarded_for_tests(every_kind::plan_frames(&bound))
        .unwrap();
    r.invalidate();
    (proposal, bound)
}

/// The landing of `proposal` by `approver` and its seal, which ends `bound`'s chain, in one
/// batch; the seal's deferred item names an AcceptDeferrals answer of an earlier batch. Returns
/// the landing and the seal.
fn sealed_landing(r: &mut Rdy, proposal: Hash, bound: &Plan, approver: &str) -> (Hash, Hash) {
    let person = plan_prov(approver, Role::Approver);
    let accept = Answer {
        item: format!("defer:P-{}@1", &proposal.to_string()[..8]),
        act: AnswerAct::AcceptDeferrals,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![],
        person: person.clone(),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    };
    let accept = r.store_mut().put_answer(&accept).unwrap();
    r.invalidate();
    let landing = r.store_mut().land(&proposal, person.clone()).unwrap();
    let version = r.store().landing(&landing).unwrap().proposal;
    let cause = BlockId::parse("D-00002").unwrap();
    let subject = "b-00001";
    let item = item_hash(&cause, subject, PlanKind::Rejustify);
    let seal = Plan {
        stable: Some(bound.id()),
        prev: Some(bound.id()),
        proposal: Some(version),
        answers: vec![PlanAnswer {
            item,
            act: PlanAct::Defer,
            reason: Some("the next release owns it".into()),
            subject: Some(Hash([0x61; 32])),
            cause: Hash([0x62; 32]),
            by: plan_prov("agent:plan", Role::Writer),
        }],
        provenance: person,
        seal: Some(PlanSeal {
            landing,
            version,
            decisions: vec![cause.clone()],
            items: vec![SealedItem {
                id: item,
                kind: PlanKind::Rejustify,
                subject: subject.into(),
                cause,
                edge: "reflects".into(),
                outcome: PlanOutcome::Deferred,
                answer: Some(accept),
            }],
        }),
        ..bound.clone()
    };
    r.store_mut()
        .write_guarded_for_tests(every_kind::plan_frames(&seal))
        .unwrap();
    r.invalidate();
    (landing, seal.id())
}

/// The project chain (see the module).
pub fn project_chain() -> Chain {
    project_chain_with(Stages::None)
}

/// The project chain with the store format 18 stages (see the module).
pub fn project_chain_v18() -> Chain {
    project_chain_with(Stages::Reflects)
}

/// The project chain of the store this rdy writes (see the module).
pub fn project_chain_current() -> Chain {
    project_chain_with(CURRENT_STAGES)
}

fn project_chain_with(stages: Stages) -> Chain {
    let _clock = pin_clock(CLOCK_MS);
    rdy_model::seed_ids_for_tests(PROJECT_SEED);
    let dir = tempfile::tempdir().unwrap();
    let src = every_kind::rolling_project(SEGMENT_MAX);
    let root = src.path().to_path_buf();
    let mut r = Rdy::open(&root).unwrap();
    r.store_mut().set_segment_max(SEGMENT_MAX);
    // before the full dump: the removable block and its change, and the blob of two chunks
    let (early, _) = landed_block(&mut r, "harrier-early", EARLY_CANARY);
    let blob = r.store_mut().put_blob(&blob_bytes()).unwrap();
    seal(&mut r);
    // store format 18: the reflects fixture's records, under a doc of their own
    let reflects = (stages == Stages::Reflects).then(|| {
        let doc = landed_doc(&mut r, REFLECTS_DOC);
        let fx = every_kind::add_reflects_fixture_records(&mut r, &doc);
        seal(&mut r);
        fx
    });
    // store format 17: a proposal with its bound plan, sealed in increment 1
    let (proposal, bound) = bound_plan(&mut r, PLAN_VIEW, "A planned block.");
    let full = dir.path().join("chain-full.rdydump");
    let s0 = r.read().dump(&full).unwrap();
    // increment 1: the plan's landing and seal, then an ordinary landing and its tag
    sealed_landing(&mut r, proposal, &bound, PERSON);
    landed_block(&mut r, "harrier-ordinary", "An ordinary block.");
    tag(&mut r, ORDINARY_TAG);
    // store format 18: the two Reflects changes, landed
    if let Some(fx) = &reflects {
        every_kind::rewrite_reflects_fixture(&mut r, fx);
        seal(&mut r);
    }
    let inc1 = dir.path().join("chain-1.rdydump");
    let s1 = r.read().dump_after(&s0.chain_end(), &inc1).unwrap();
    // store format 18: the source's change and its sealed plan, first in increment 2
    if let Some(fx) = &reflects {
        every_kind::finish_reflects_fixture(&mut r, fx);
        seal(&mut r);
    }
    // increment 2: the late block, then both removals and a last tagged landing
    let (late, late_landing) = landed_block(&mut r, "harrier-late", LATE_CANARY);
    let missing = remove(
        &mut r,
        "rq-early",
        vec![early, format!("blob:{blob}")],
        "the early block names a person",
    );
    remove(
        &mut r,
        "rq-late",
        vec![late],
        "the late block names a person",
    );
    landed_block(&mut r, "harrier-after", "A block after the removals.");
    tag(&mut r, LATE_TAG);
    let inc2 = dir.path().join("chain-2.rdydump");
    r.read().dump_after(&s1.chain_end(), &inc2).unwrap();
    let post = dir.path().join("post.rdydump");
    r.read().dump(&post).unwrap();
    r.close().unwrap();
    rdy_model::unseed_ids_for_tests();
    Chain {
        dir,
        source: Some(src),
        root,
        full,
        inc1,
        inc2,
        post,
        held_stop: format!("landing:{late_landing}"),
        missing_stop: position(missing),
    }
}

/// The standalone project fixture's store (see the module), built from `c`.
pub fn project_standalone(c: &Chain) -> Standalone {
    let _clock = pin_clock(CLOCK_MS + 1_000);
    rdy_model::seed_ids_for_tests(PROJECT_SEED + 1);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("standalone");
    let stop = DumpStop::parse(&c.missing_stop).unwrap();
    load_chain(std::slice::from_ref(&c.post), &root, Some(&stop)).unwrap();
    let mut r = Rdy::open(&root).unwrap();
    r.store_mut().set_segment_max(SEGMENT_MAX);
    let (target, _) = landed_block(&mut r, "harrier-standalone", STANDALONE_CANARY);
    tag(&mut r, STANDALONE_TAG);
    remove(
        &mut r,
        "rq-standalone",
        vec![target],
        "the standalone block names a person",
    );
    let dump = dir.path().join("standalone.rdydump");
    r.read().dump(&dump).unwrap();
    r.close().unwrap();
    rdy_model::unseed_ids_for_tests();
    Standalone {
        dir,
        root,
        dump,
        stop: format!("tag:{STANDALONE_TAG}"),
    }
}

/// The upload files of the every-kind holder's tag `i` (0, 1 or 2), in the order its builder took
/// them, and the tag's target.
struct Upload {
    files: Vec<PathBuf>,
    target: UploadTarget,
}

impl Upload {
    fn open(&self) -> Vec<File> {
        self.files.iter().map(|p| File::open(p).unwrap()).collect()
    }
}

fn take(h: &mut Rdy, u: &Upload) {
    rdy_engine::stream_take::take(h, &u.open(), u.target.clone()).unwrap();
}

/// The holder chain (see the module). Its store's repo is the every-kind holder's.
pub fn holder_chain() -> (Chain, HolderUploads) {
    holder_chain_with(Stages::None)
}

/// The holder chain with the store format 18 stages (see the module).
pub fn holder_chain_v18() -> (Chain, HolderUploads) {
    holder_chain_with(Stages::Reflects)
}

/// The holder chain of the store this rdy writes (see the module).
pub fn holder_chain_current() -> (Chain, HolderUploads) {
    holder_chain_with(CURRENT_STAGES)
}

fn holder_chain_with(stages: Stages) -> (Chain, HolderUploads) {
    let _clock = pin_clock(CLOCK_MS);
    rdy_model::seed_ids_for_tests(HOLDER_SEED);
    let built = every_kind::holder(SEGMENT_MAX);
    let (repo, targets) = {
        let h = Rdy::open_holder(&built.root).unwrap();
        let repo: RepoId = h.store().repo_id().unwrap().unwrap();
        let targets: Vec<UploadTarget> = every_kind::HOLDER_TAGS
            .iter()
            .map(|name| {
                let (.., t) = h
                    .store()
                    .tags()
                    .unwrap()
                    .into_iter()
                    .find(|(n, ..)| n == name)
                    .unwrap();
                UploadTarget {
                    tag: t.name,
                    landing: t.landing,
                }
            })
            .collect();
        h.close().unwrap();
        (repo, targets)
    };
    let uploads = built.dir.path().join("uploads");
    let names: [&[&str]; 3] = [
        &["v1-0.rdysync"],
        &["v2-0.rdysync", "v2-1.rdysync"],
        &["v3-0.rdysync"],
    ];
    let mut ups: Vec<Upload> = names
        .iter()
        .zip(targets)
        .map(|(files, target)| Upload {
            files: files.iter().map(|f| uploads.join(f)).collect(),
            target,
        })
        .collect();
    let root = built.dir.path().join("chain-holder");
    let mut h = Rdy::create_holder(&root, repo).unwrap();
    h.store_mut().set_segment_max(SEGMENT_MAX);
    // store format 18: the reflects source's three uploads, from a copy of the second source
    // made before the plan uploads change it
    let reflects = (stages == Stages::Reflects).then(|| reflects_uploads(&built, repo));
    // store format 17: the second source's plan, sealed in it and in its clone
    let [plan, fork] = plan_uploads(&built, repo);
    ups.push(plan);
    ups.push(fork);
    take(&mut h, &ups[0]);
    if let Some(rf) = &reflects {
        take(&mut h, &rf[0]);
    }
    let full = built.dir.path().join("holder-chain-full.rdydump");
    let s0 = h.read().dump(&full).unwrap();
    take(&mut h, &ups[1]);
    // the second source's seal, its chain (the bound plan) and its acceptance
    take(&mut h, &ups[3]);
    if let Some(rf) = &reflects {
        take(&mut h, &rf[1]);
    }
    let inc1 = built.dir.path().join("holder-chain-1.rdydump");
    let s1 = h.read().dump_after(&s0.chain_end(), &inc1).unwrap();
    take(&mut h, &ups[2]);
    // the clone's seal: its chain's bound plan is the one increment 1 holds
    take(&mut h, &ups[4]);
    if let Some(rf) = &reflects {
        take(&mut h, &rf[2]);
    }
    // the earlier uploads again: every key is held, so nothing is appended
    let end = h.store().log_end();
    take(&mut h, &ups[1]);
    take(&mut h, &ups[0]);
    assert_eq!(h.store().log_end(), end, "a repeated take appends nothing");
    let inc2 = built.dir.path().join("holder-chain-2.rdydump");
    h.read().dump_after(&s1.chain_end(), &inc2).unwrap();
    let post = built.dir.path().join("holder-post.rdydump");
    h.read().dump(&post).unwrap();
    h.close().unwrap();
    rdy_model::unseed_ids_for_tests();
    ups.extend(reflects.into_iter().flatten());
    let chain = Chain {
        source: None,
        root,
        full,
        inc1,
        inc2,
        post,
        held_stop: format!("tag:{}", every_kind::HOLDER_TAGS[1]),
        missing_stop: format!("tag:{}", every_kind::HOLDER_TAGS[0]),
        dir: built.dir,
    };
    (chain, HolderUploads(ups))
}

/// The uploads of [`PLAN_TAG`] and [`PLAN_FORK_TAG`]: the every-kind holder's second source
/// (`b`) gets a proposal with its bound plan; a clone of it (`c`) is made; each of the two lands
/// the proposal with its own approver and seal and tags the landing. Each history is exported,
/// checked and written as one batch file.
fn plan_uploads(built: &every_kind::HolderBuild, repo: RepoId) -> [Upload; 2] {
    let (b_root, c_root) = (built.dir.path().join("b"), built.dir.path().join("c"));
    let mut b = Rdy::open(&b_root).unwrap();
    let (proposal, bound) = bound_plan(&mut b, "plan", "A planned prose.");
    b.close().unwrap();
    every_kind::copy_tree(&b_root, &c_root);
    [
        (&b_root, PERSON, PLAN_TAG, "v4-0.rdysync"),
        (&c_root, PLAN_FORK_PERSON, PLAN_FORK_TAG, "v4-1-0.rdysync"),
    ]
    .map(|(root, person, name, file)| {
        let mut r = Rdy::open(root).unwrap();
        let (landing, _) = sealed_landing(&mut r, proposal, &bound, person);
        r.tag(&approver(), name, None, None).unwrap();
        seal(&mut r);
        let tag = r
            .store()
            .tags()
            .unwrap()
            .into_iter()
            .find(|(n, ..)| n == name)
            .unwrap()
            .1;
        let bytes = every_kind::export_tag(&r, tag);
        r.close().unwrap();
        rdy_engine::stream_check::check_batch(bytes.as_slice(), Some(repo)).unwrap();
        let path = built.dir.path().join("uploads").join(file);
        std::fs::write(&path, &bytes).unwrap();
        Upload {
            files: vec![path],
            target: UploadTarget {
                tag: name.into(),
                landing,
            },
        }
    })
}

/// The uploads of [`REFLECTS_TAGS`] (store format 18): a copy (`r`) of the every-kind holder's
/// second source gets the reflects fixture's three stages under a doc of its own, and is tagged
/// after each; each tag's history is exported, checked and written as one batch file.
fn reflects_uploads(built: &every_kind::HolderBuild, repo: RepoId) -> [Upload; 3] {
    let (b_root, r_root) = (built.dir.path().join("b"), built.dir.path().join("r"));
    every_kind::copy_tree(&b_root, &r_root);
    let mut r = Rdy::open(&r_root).unwrap();
    let doc = landed_doc(&mut r, REFLECTS_DOC);
    let fx = every_kind::add_reflects_fixture_records(&mut r, &doc);
    seal(&mut r);
    let first = tagged_upload(&mut r, built, repo, REFLECTS_TAGS[0], "v5-0-0.rdysync");
    every_kind::rewrite_reflects_fixture(&mut r, &fx);
    seal(&mut r);
    let second = tagged_upload(&mut r, built, repo, REFLECTS_TAGS[1], "v5-1-0.rdysync");
    every_kind::finish_reflects_fixture(&mut r, &fx);
    seal(&mut r);
    let third = tagged_upload(&mut r, built, repo, REFLECTS_TAGS[2], "v5-2-0.rdysync");
    r.close().unwrap();
    [first, second, third]
}

/// Tags canon's head of `r` `name`, then exports, checks and writes the tag's history as the
/// upload file `file`.
fn tagged_upload(
    r: &mut Rdy,
    built: &every_kind::HolderBuild,
    repo: RepoId,
    name: &str,
    file: &str,
) -> Upload {
    tag(r, name);
    let t = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, ..)| n == name)
        .unwrap()
        .1;
    let landing = r.store().read_tag(&t).unwrap().landing;
    let bytes = every_kind::export_tag(r, t);
    rdy_engine::stream_check::check_batch(bytes.as_slice(), Some(repo)).unwrap();
    let path = built.dir.path().join("uploads").join(file);
    std::fs::write(&path, &bytes).unwrap();
    Upload {
        files: vec![path],
        target: UploadTarget {
            tag: name.into(),
            landing,
        },
    }
}

/// The every-kind holder's uploads, which the holder chain took.
pub struct HolderUploads(Vec<Upload>);

/// The standalone holder fixture's store (see the module), built from `c` and the uploads it
/// took.
pub fn holder_standalone(c: &Chain, ups: &HolderUploads) -> Standalone {
    let _clock = pin_clock(CLOCK_MS + 1_000);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("standalone-holder");
    let stop = DumpStop::parse(&c.missing_stop).unwrap();
    load_chain(std::slice::from_ref(&c.post), &root, Some(&stop)).unwrap();
    let mut h = Rdy::open_holder(&root).unwrap();
    h.store_mut().set_segment_max(SEGMENT_MAX);
    take(&mut h, &ups.0[1]);
    take(&mut h, &ups.0[2]);
    // store format 17: both sealed plan landings' histories again, after the removal
    take(&mut h, &ups.0[3]);
    take(&mut h, &ups.0[4]);
    // store format 18: the reflects source's histories again, when the chain took them
    for u in &ups.0[5..] {
        take(&mut h, u);
    }
    let dump = dir.path().join("standalone-holder.rdydump");
    h.read().dump(&dump).unwrap();
    h.close().unwrap();
    Standalone {
        dir,
        root,
        dump,
        stop: format!("tag:{}", every_kind::HOLDER_TAGS[1]),
    }
}

/// The record kinds a frozen store format 16 project may hold: every kind of store format 15 and
/// the carried removal (kind 23).
pub const V16_KINDS: [u8; 21] = [
    kinds::BLOCK,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::BLOB_CHUNK,
    kinds::BLOB_MANIFEST,
    kinds::REF_UPDATE,
    kinds::TAG,
    kinds::ANSWER,
    kinds::SIGNATURE,
    kinds::VIEW_OP,
    kinds::INTENT,
    kinds::ALT_SET,
    kinds::REVIEW,
    kinds::TENSION,
    kinds::TRANSITION,
    kinds::REDACTION,
    kinds::REDACTION_RESIDUE,
    kinds::REDACTION_APPLY,
    kinds::REDACTION_CARRIED,
];

/// The record kinds a store format 18 project may hold: those of store format 17, since 18 only
/// appends a justification variant (D-4WC6R).
pub const V18_KINDS: [u8; 22] = V17_KINDS;

/// The record kinds a frozen store format 17 project may hold: every kind of store format 16
/// and the change plan (kind 24).
pub const V17_KINDS: [u8; 22] = [
    kinds::BLOCK,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::BLOB_CHUNK,
    kinds::BLOB_MANIFEST,
    kinds::REF_UPDATE,
    kinds::TAG,
    kinds::ANSWER,
    kinds::SIGNATURE,
    kinds::VIEW_OP,
    kinds::INTENT,
    kinds::ALT_SET,
    kinds::REVIEW,
    kinds::TENSION,
    kinds::TRANSITION,
    kinds::REDACTION,
    kinds::REDACTION_RESIDUE,
    kinds::REDACTION_APPLY,
    kinds::REDACTION_CARRIED,
    kinds::PLAN,
];
