//! The dump chains of the dump format 3 fixtures (store format 16, D-2FMRR, D-QF2HQ, D-69AY3):
//! a full dump and two increments of the every-kind project, and of a holder filled from the
//! every-kind holder's uploads, each with a person's approved removal in its last increment. The
//! fixture writer (`hc9_write_format_three_fixtures_once`) builds them once; the tests read only
//! the frozen files.
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
//! Each standalone full fixture is a dump of a stopped load: the full dump written after the
//! removals, loaded at the missing stop (every carried span is zero), then written to again. The
//! project gets a new block, an ordinary tagged landing and a person's approved removal of that
//! block; the holder takes `v2.0.0` and `v3.0.0` again, and the take of the matching apply ends
//! its warning. No holder workflow and no approval path is added here: every step is an existing
//! engine call.
use crate::every_kind;
use crate::fixtures::{approver, land, write};
use crate::tensions::{ag, created};
use rdy_engine::dump_stop::DumpStop;
use rdy_engine::load::load_chain;
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, pin_clock};
use rdy_model::RepoId;
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

fn position(l: Location) -> String {
    format!("pos:{}.{}", l.segment, l.offset)
}

/// The project chain (see the module).
pub fn project_chain() -> Chain {
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
    let full = dir.path().join("chain-full.rdydump");
    let s0 = r.read().dump(&full).unwrap();
    // increment 1: an ordinary landing and its tag
    landed_block(&mut r, "harrier-ordinary", "An ordinary block.");
    tag(&mut r, ORDINARY_TAG);
    let inc1 = dir.path().join("chain-1.rdydump");
    let s1 = r.read().dump_after(&s0.chain_end(), &inc1).unwrap();
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
    let ups: Vec<Upload> = names
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
    take(&mut h, &ups[0]);
    let full = built.dir.path().join("holder-chain-full.rdydump");
    let s0 = h.read().dump(&full).unwrap();
    take(&mut h, &ups[1]);
    let inc1 = built.dir.path().join("holder-chain-1.rdydump");
    let s1 = h.read().dump_after(&s0.chain_end(), &inc1).unwrap();
    take(&mut h, &ups[2]);
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
