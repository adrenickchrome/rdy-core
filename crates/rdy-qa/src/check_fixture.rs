//! The check-request fixtures of rdy 0.32.0 (TASK-1026): one repo where rdy's own product
//! writers made a CHECK_REQUEST, a Check REVIEW with verdict concern and a person's
//! AcceptVerdict ANSWER on it, and a later landed path that carries the request and its review
//! to a holder. Every record comes from an engine call an agent or a person makes; no record is
//! written by hand.
//!
//! The world, under a pinned clock ([`CK_CLOCK_MS`]) and seeded ids ([`CK_SEED`]):
//!
//! 1. A project (`rdy-standard`) lands a doc `notes` with a section and a claim `x`.
//! 2. Agent `agent:a1` edits `x` with an editorial (typo) justification in its view `a1`: the
//!    change [`CheckWorld::change`]. It proposes the view: the first version.
//! 3. The agent runs `meaning_preservation` on its own change (`rdy_review` `{op: "run"}`,
//!    `check::api::run_review`): the CHECK_REQUEST, subject the change, with its `check/` ref and
//!    the store feature `check_request`.
//! 4. The agent submits verdict `concern` on its request (`rdy_review_submit`, the check submit
//!    path): the Check REVIEW.
//! 5. The person accepts the concern as blocking on the first version's approve card (option
//!    `accept_verdict:<review>:<version>`, `answer::verdict`): the AcceptVerdict ANSWER. The
//!    first version cannot land while the case holds.
//! 6. The second, landed path: the agent proposes the view again (a second version that selects
//!    the same change), the person approves it on its card, and the person tags canon
//!    [`CK_TAG`]. The tag's closure holds the change, so it holds the request whose subject names
//!    the change and the request's review (`rdy_store::closure`).
//!
//! The members:
//!
//! - the project dump: a dump of the project, which holds all three records;
//! - the sync batch: the tag's history, exported from the project, with the request and the
//!   review. The AcceptVerdict answer is not in it: a tag's closure takes an ANSWER only when
//!   one of its targets is a closure landing or tag (`closure.rs`, `attach_answers`), and an
//!   AcceptVerdict answer has no targets (the store refuses one that has,
//!   `carried_records.rs`);
//! - the holder dump: a new holder of the project's repo takes the sync batch, then dumps. It
//!   holds the request and the review, and for the same reason never the answer.
use crate::fixtures::{approver, land, project};
use crate::intake::{PERSON, TEST_SUMMARY};
use crate::tensions::{ag, created, j_typo, put_j, upd};
use rdy_engine::answer::AnswerRequest;
use rdy_engine::check::api::{ReviewRequest, run_review};
use rdy_engine::engine::{Local, with_writer_result};
use rdy_engine::semantic::api::run_review_submit;
use rdy_engine::semantic::submit::{CitationRequest, ReviewSubmitRequest};
use rdy_engine::{Engine, Rdy, RdyError, pin_clock};
use rdy_model::{BlockId, Hash, RepoId};
use rdy_store::take_batch::UploadTarget;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The id seed of the world's writer.
pub const CK_SEED: u64 = 0xC4EC;
/// The clock of every provenance the world records.
pub const CK_CLOCK_MS: i64 = 1_761_000_000_000;
/// The clock of the holder's take: after every project stamp.
pub const CK_HOLDER_CLOCK_MS: i64 = CK_CLOCK_MS + 50_000;
/// The tag of the second, landed path.
pub const CK_TAG: &str = "v1.0.0";
/// The check the agent runs on its own change.
pub const CK_CHECK: &str = "meaning_preservation";
/// The agent's view, and the agent `agent:a1`.
pub const CK_VIEW: &str = "a1";
/// The reason of the concern.
pub const CK_REASON: &str = "the edit reads otherwise";
/// The citation note of the concern.
pub const CK_NOTE: &str = "the meaning moved";
/// The body of the claim before the edit.
pub const CK_BEFORE: &str = "X holds.";
/// The body of the claim after the agent's edit.
pub const CK_AFTER: &str = "X holds, now.";

/// The three product-written records, by hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckRecords {
    /// The CHECK_REQUEST (kind 28), subject the agent's change.
    pub request: Hash,
    /// The Check REVIEW (kind 17) of the request, verdict concern.
    pub review: Hash,
    /// The AcceptVerdict ANSWER (kind 10), item `verdict:<review>`, no targets.
    pub answer: Hash,
}

/// The built world: its directories, the three records, the change the request names, both
/// versions, the tag, and the three members' bytes.
pub struct CheckWorld {
    /// The project's directory.
    pub dir: tempfile::TempDir,
    /// The holder's directory and the scratch files.
    pub out: tempfile::TempDir,
    /// The project's root.
    pub root: PathBuf,
    /// The holder's root.
    pub holder_root: PathBuf,
    pub repo: RepoId,
    pub records: CheckRecords,
    /// The agent's change of the claim.
    pub change: Hash,
    /// The claim the change edits.
    pub claim: BlockId,
    /// The held version (`P-x@1`) and the landed one (`P-x@2`): their refs and full hashes.
    pub held: (String, String),
    pub landed: (String, String),
    /// The tag of the landed path: its hash and landing.
    pub tag: Hash,
    pub tag_landing: Hash,
    /// The project dump: all three records.
    pub project_dump: Vec<u8>,
    /// The tag's history as one sync batch: the request and the review.
    pub sync_batch: Vec<u8>,
    /// The holder dump, after the holder took the sync batch: the request and the review.
    pub holder_dump: Vec<u8>,
}

fn on<T: Send + 'static>(
    e: &dyn Engine,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> T {
    with_writer_result(e, f).unwrap_or_else(|err| panic!("{err:#?}"))
}

fn review(e: &dyn Engine, v: Value) -> Value {
    run_review(
        e,
        &ag(CK_VIEW),
        PERSON,
        true,
        None,
        ReviewRequest::from_value(v.clone()).unwrap(),
    )
    .unwrap_or_else(|err| panic!("{v}: {err:#?}"))
}

fn propose(e: &dyn Engine) -> (String, String) {
    let p = on(e, |r| {
        r.propose(&ag(CK_VIEW), CK_VIEW, CK_VIEW, Some(TEST_SUMMARY))
    });
    assert!(p.ready, "{:#?}", p.issues);
    (p.r#ref, p.id)
}

/// The person's own answer `option` to item `r` (no card hash: the card bound now).
fn person_answers(e: &dyn Engine, r: &str, option: &str) -> rdy_engine::answer::AnswerInfo {
    let req = AnswerRequest {
        r#ref: r.into(),
        option: option.into(),
        ..Default::default()
    };
    on(e, move |rdy| rdy.answer(&approver(), PERSON, req))
}

/// Builds the world (see the module) in new temporary directories.
pub fn build() -> CheckWorld {
    let _clock = pin_clock(CK_CLOCK_MS);
    rdy_model::seed_ids_for_tests(CK_SEED);
    let (dir, mut r) = project();
    let root = dir.path().to_path_buf();
    // 1. the seed doc, landed
    let seed = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": CK_BEFORE}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    r.seal_open().unwrap();
    let claim = created(&seed, "$x");
    // 2. the agent's editorial edit of the claim, proposed
    let w = put_j(&mut r, CK_VIEW, upd(&claim, CK_AFTER), j_typo());
    let change: Hash = w.change.parse().unwrap();
    let e = Local::new(r);
    let held = propose(&e);
    // 3. the author-run check of the change
    let q = review(
        &e,
        json!({"op": "run", "check": CK_CHECK, "subject": change.to_string()}),
    );
    let label = q["request"].as_str().expect("a request").to_string();
    let case = q["case_hash"].as_str().unwrap().to_string();
    // 4. the agent's concern
    let submit = ReviewSubmitRequest {
        request: label.clone(),
        case_hash: case,
        verdict: "concern".into(),
        citations: vec![CitationRequest {
            block: claim.to_string(),
            note: CK_NOTE.into(),
        }],
        severity: None,
        reason: Some(CK_REASON.into()),
    };
    let v = run_review_submit(&e, &ag(CK_VIEW), submit)
        .unwrap_or_else(|err| panic!("submit {label}: {err:#?}"));
    let review_hash: Hash = v["review"].as_str().unwrap().parse().unwrap();
    // 5. the person accepts the concern as blocking on the held version's card
    let accept = format!("accept_verdict:{review_hash}:{}", held.1);
    let done = person_answers(&e, &held.0, &accept);
    assert_eq!(done.act, "accept_verdict");
    let answer: Hash = done.answer.expect("the answer's hash").parse().unwrap();
    // 6. the second, landed path: a new version of the same change, approved and tagged
    let landed = propose(&e);
    assert_ne!(landed.1, held.1, "a second version");
    let approved = person_answers(&e, &landed.0, "approve");
    assert!(approved.landing.is_some(), "{approved:#?}");
    on(&e, |r| {
        r.tag(&approver(), CK_TAG, None, None)?;
        r.seal_open()
    });
    let st = e.read();
    let request = st
        .reader()
        .refs_with_prefix(rdy_store::carried_records::CHECK_REF_PREFIX)
        .unwrap();
    assert_eq!(request.len(), 1, "one check request: {request:?}");
    let request = Hash(request[0].1.as_slice().try_into().unwrap());
    let (_, tag, t) = st
        .reader()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, ..)| n == CK_TAG)
        .unwrap();
    let repo = st.reader().repo_id().unwrap().unwrap();
    // the project dump and the tag's history
    let out = tempfile::tempdir().unwrap();
    let project_dump = {
        let p = out.path().join("project.rdydump");
        st.dump(&p).unwrap();
        std::fs::read(&p).unwrap()
    };
    let sync_batch = {
        let mut b = Vec::new();
        rdy_engine::stream_export::write(&st, tag, &Default::default(), &mut b).unwrap();
        b
    };
    drop(st);
    drop(e);
    rdy_model::unseed_ids_for_tests();
    drop(_clock);
    // the holder takes the tag's history, then dumps
    let holder_root = out.path().join("holder");
    let holder_dump = holder_of(out.path(), &holder_root, repo, &sync_batch, t.landing);
    CheckWorld {
        records: CheckRecords {
            request,
            review: review_hash,
            answer,
        },
        change,
        claim,
        held,
        landed,
        tag,
        tag_landing: t.landing,
        repo,
        root,
        holder_root,
        project_dump,
        sync_batch,
        holder_dump,
        dir,
        out,
    }
}

/// A new holder of `repo` at `root` takes `batch` (the tag [`CK_TAG`] at `landing`), checked
/// first as an upload is, then dumps: the dump's bytes. Scratch files go under `scratch`.
fn holder_of(scratch: &Path, root: &Path, repo: RepoId, batch: &[u8], landing: Hash) -> Vec<u8> {
    let _clock = pin_clock(CK_HOLDER_CLOCK_MS);
    rdy_engine::stream_check::check_batch(batch, Some(repo)).unwrap();
    let up = scratch.join("upload.rdysync");
    std::fs::write(&up, batch).unwrap();
    let mut h = Rdy::create_holder(root, repo).unwrap();
    rdy_engine::stream_take::take(
        &mut h,
        &[std::fs::File::open(&up).unwrap()],
        UploadTarget {
            tag: CK_TAG.into(),
            landing,
        },
    )
    .unwrap();
    let p = scratch.join("holder.rdydump");
    h.read().dump(&p).unwrap();
    h.close().unwrap();
    std::fs::read(&p).unwrap()
}

/// Which of the three records a store or batch holds, as the expect files write it: each
/// record's kind and hash, and whether it is present.
pub fn records_json(records: &CheckRecords, holds: impl Fn(u8, &Hash) -> bool) -> Value {
    let row = |name: &str, kind: u8, h: &Hash, what: &str| {
        json!({
            "record": name,
            "kind": kind,
            "hash": h.to_string(),
            "what": what,
            "present": holds(kind, h),
        })
    };
    json!([
        row(
            "check_request",
            rdy_store::kinds::CHECK_REQUEST,
            &records.request,
            "CHECK_REQUEST of the agent's change, written by rdy_review run (check::api::run_review)",
        ),
        row(
            "check_review",
            rdy_store::kinds::REVIEW,
            &records.review,
            "REVIEW subject Check, verdict concern, written by rdy_review_submit (check/submit.rs)",
        ),
        row(
            "accept_verdict_answer",
            rdy_store::kinds::ANSWER,
            &records.answer,
            "ANSWER act AcceptVerdict of the review, no targets, the person's answer (rdy_answer's engine call)",
        ),
    ])
}
