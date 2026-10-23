//! Fixtures of the needs-you queue (plan Decision intake): a proposal that is one batch with an
//! exception of each kind its decisions can carry and an advised tension beside it, versions that
//! are ready and versions that are not, and the answers a person records on them.
use crate::edit_loop::{Spec, spec_doc};
use crate::fixtures::{land, writer};
use crate::tensions::{ag, created, put, try_briefed, upd};
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::answer::{AnswerInfo, AnswerRequest};
use rdy_engine::consequence::propose_and_report;
use rdy_engine::engine::Engine;
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, Pending, PendingQuery};
use rdy_engine::{DecisionInput, Local, ProposalInfo, Rdy, Scope, Session};
use rdy_model::*;
use serde_json::{Value, json};
use std::path::Path;

/// The view of the batch: agent `agent:batch`'s own view.
pub const BATCH_VIEW: &str = "batch";
/// The batch's title.
pub const BATCH_TITLE: &str = "Guest mode";
/// The batch's summary (FW-G7c): the author's words that lead the card.
pub const BATCH_SUMMARY: &str =
    "Lets a visitor read a shared doc without an account, and records five decisions on access.";
/// The person every answer of these fixtures names.
pub const PERSON: &str = "human:qa";
/// The one summary a test gives a version that lands a proposed decision (Task 17 ruling): a
/// version is approved through a card, and readiness needs a summary (D-4PG9Y, D-3JZP4).
pub const TEST_SUMMARY: &str = "test summary";

/// What [`batch_project`] built.
#[derive(Clone, Debug)]
pub struct Batch {
    /// Canon: the edit loop's spec doc; its decision is live.
    pub spec: Spec,
    /// The reply of `propose_and_report` for the batch's version.
    pub report: Value,
    /// `P-x@n` of the version.
    pub r#ref: String,
    /// A plain decision: the only member of the batch.
    pub member: BlockId,
    /// A decision declared `one_way`.
    pub one_way: BlockId,
    /// A decision that supersedes the spec's live decision.
    pub against: BlockId,
    /// A decision that no content applies.
    pub unapplied: BlockId,
    /// A decision whose author recommends `reject`.
    pub not_recommended: BlockId,
    /// The claim a reviewer judged against the spec's claim C, advising that it block.
    pub advised: BlockId,
}

/// A decision under the spec's section, with card words, applied by its `affects` link to claim
/// C (an empty `affects` leaves it unapplied).
pub fn decision(s: &Spec, statement: &str, title: &str) -> DecisionInput {
    DecisionInput {
        parent: s.section.to_string(),
        statement: statement.into(),
        title: Some(title.into()),
        why: Some(format!("{title}: the person decides it")),
        reversibility: Some("two_way".into()),
        affects: vec![s.c.to_string()],
        ..Default::default()
    }
}

/// Decides `d` as `agent:<view>` in its view `view`, briefed as needed; the decision's id.
pub fn decide_in(r: &mut Rdy, view: &str, d: DecisionInput) -> BlockId {
    let w = try_briefed(r, view, |r, brief| {
        r.decide(&ag(view), view, DecisionInput { brief, ..d.clone() })
    })
    .unwrap_or_else(|e| panic!("decide {:?}: {e}", d.statement));
    created(&w, "$decision")
}

/// Supersedes `old` with `d` as `agent:<view>` in its view `view`; the new decision's id.
pub fn supersede_in(r: &mut Rdy, view: &str, old: &BlockId, d: DecisionInput) -> BlockId {
    let w = try_briefed(r, view, |r, brief| {
        r.supersede(
            &ag(view),
            view,
            &old.to_string(),
            None,
            DecisionInput { brief, ..d.clone() },
        )
    })
    .unwrap_or_else(|e| panic!("supersede {old}: {e}"));
    created(&w, "$decision")
}

/// Records `actor`'s `tension` verdict on blocks `x` and `y` at their contents in `scope`,
/// advising that it block when `advise`, as `submit_verdict` records it.
pub fn verdict_on(r: &mut Rdy, scope: &Scope, x: &BlockId, y: &BlockId, actor: &str, advise: bool) {
    let m = r.read().materialized(scope).expect("materialized");
    let content = |id: &BlockId| {
        m.get(id)
            .unwrap_or_else(|| panic!("{id} in {scope:?}"))
            .content_hash_with(r.store().tracking())
    };
    let mut items = vec![
        PairItem {
            block: x.clone(),
            content: content(x),
        },
        PairItem {
            block: y.clone(),
            content: content(y),
        },
    ];
    items.sort();
    let key = pair_key(&items);
    let review = Review {
        subject: ReviewSubject::Pair {
            key,
            items: items.clone(),
        },
        case_hash: Some(Hash([9; 32])),
        verdict: Verdict::Tension,
        reason: String::new(),
        citations: vec![Citation {
            block: x.clone(),
            note: "these two cannot both hold".into(),
        }],
        provenance: Provenance {
            actor: actor.into(),
            run: Some("r".into()),
            role: Role::Reviewer,
            at_ms: 1,
        },
    };
    let first = SemanticTension {
        key,
        items,
        review: Hash::of(&postcard::to_allocvec(&review).expect("encode")),
        advice: advise.then(|| "a visitor would read stale access rules".into()),
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance: review.provenance.clone(),
    };
    r.record_verdict(&review, Some(&first)).expect("verdict");
}

/// Proposes `view` as `agent:<view>` with `summary` through `propose_and_report` (the reply an
/// agent gets), then closes the project and opens it again: the reply and the reopened project.
pub fn propose_reported(
    dir: &Path,
    r: Rdy,
    view: &str,
    title: &str,
    summary: Option<&str>,
) -> (Rdy, Value) {
    let e = Local::new(r);
    let (view, title, summary) = (
        view.to_string(),
        title.to_string(),
        summary.map(str::to_string),
    );
    let report = propose_and_report(&e, None, move |r| {
        r.propose_subset(&ag(&view), &view, &title, None, summary.as_deref())
    })
    .expect("propose");
    e.close().expect("close");
    (Rdy::open(dir).expect("reopen"), report)
}

/// One proposal from view [`BATCH_VIEW`] (`agent:batch`) on the spec doc, with
/// [`BATCH_SUMMARY`]. Its decisions: a plain one (the batch's only member), one declared
/// `one_way`, one superseding the spec's live decision, one that no content applies and one whose
/// author recommends `reject`. Its claim Q stands in an advised (not accepted) semantic tension
/// with the spec's claim C.
pub fn batch_project() -> (tempfile::TempDir, Rdy, Batch) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut r = Rdy::init(dir.path(), "rdy-standard").expect("init");
    let spec = spec_doc(&mut r);
    let v = BATCH_VIEW;
    let member = decide_in(
        &mut r,
        v,
        decision(
            &spec,
            "Guest readers need no account",
            "Guest readers need no account",
        ),
    );
    let one_way = decide_in(
        &mut r,
        v,
        DecisionInput {
            reversibility: Some("one_way".into()),
            ..decision(
                &spec,
                "Guest links never expire",
                "Guest links never expire",
            )
        },
    );
    let against = supersede_in(
        &mut r,
        v,
        &spec.decision,
        DecisionInput {
            affects: vec![spec.a.to_string()],
            ..decision(&spec, "Keep A for members only", "Keep A for members only")
        },
    );
    let unapplied = decide_in(
        &mut r,
        v,
        DecisionInput {
            affects: vec![],
            ..decision(&spec, "Guests see a banner", "Guests see a banner")
        },
    );
    let not_recommended = decide_in(
        &mut r,
        v,
        DecisionInput {
            recommend: Some("reject".into()),
            recommend_reason: Some("guests should not edit".into()),
            ..decision(&spec, "Guests may edit", "Guests may edit")
        },
    );
    let advised = created(
        &put(
            &mut r,
            v,
            json!([{"op": "create", "type": "claim", "id": "$q", "parent": spec.section.to_string(),
                    "body": "Guests read every rule of the spec."}]),
        ),
        "$q",
    );
    verdict_on(&mut r, &Scope::Inflight, &advised, &spec.c, "agent:r", true);
    let (r, report) = propose_reported(dir.path(), r, v, BATCH_TITLE, Some(BATCH_SUMMARY));
    let r#ref = report["ref"].as_str().expect("ref").to_string();
    let batch = Batch {
        spec,
        report,
        r#ref,
        member,
        one_way,
        against,
        unapplied,
        not_recommended,
        advised,
    };
    (dir, r, batch)
}

/// What [`ready_and_not_ready`] built: one ready version and one version for each failed check.
#[derive(Clone, Debug)]
pub struct Readiness {
    pub spec: Spec,
    /// A version that passes every check.
    pub ready: ProposalInfo,
    /// A version proposed with no summary.
    pub no_summary: ProposalInfo,
    /// A version whose landing would put a blocking tension (a premise shift) into canon.
    pub blocking: ProposalInfo,
    /// A version that carries a conflict with canon on the block it writes.
    pub conflict: ProposalInfo,
}

/// The spec doc in canon, then four proposals: `ready` (a new claim, with a summary),
/// `no_summary` (a new claim, no summary), `blocking` (a claim that cites A at the content a
/// landing has changed since) and `conflict` (an edit of claim C that a landing has edited too).
pub fn ready_and_not_ready() -> (tempfile::TempDir, Rdy, Readiness) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut r = Rdy::init(dir.path(), "rdy-standard").expect("init");
    let spec = spec_doc(&mut r);
    // a conflict: two views edit C from canon's version, and one lands
    put(&mut r, "edit-c", upd(&spec.c, "C as edit-c has it."));
    put(&mut r, "conflict", upd(&spec.c, "C as conflict has it."));
    land(&mut r, "edit-c").expect("land edit-c");
    // a blocking tension: a view cites A, and an edit of A lands
    put(
        &mut r,
        "blocking",
        json!([{"op": "create", "type": "claim", "id": "$y", "parent": spec.section.to_string(),
                "body": format!("Builds on {{{{{}}}}}.", spec.a)}]),
    );
    put(&mut r, "edit-a", upd(&spec.a, "A changed."));
    land(&mut r, "edit-a").expect("land edit-a");
    for view in ["ready", "no-summary"] {
        put(
            &mut r,
            view,
            json!([{"op": "create", "type": "claim", "parent": spec.section.to_string(),
                    "body": format!("A claim of {view}.")}]),
        );
    }
    let mut propose = |view: &str, summary: Option<&str>| {
        r.propose_subset(&ag(view), view, view, None, summary)
            .unwrap_or_else(|e| panic!("propose {view}: {e}"))
    };
    let ready = propose("ready", Some("Adds one claim."));
    let no_summary = propose("no-summary", None);
    let blocking = propose("blocking", Some("Adds a claim that builds on A."));
    let conflict = propose("conflict", Some("Rewrites C."));
    let made = Readiness {
        spec,
        ready,
        no_summary,
        blocking,
        conflict,
    };
    (dir, r, made)
}

/// The queue as the person [`PERSON`] sees it, every card whole, no session (nothing snoozed).
pub fn pending(r: &Rdy) -> Pending {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending")
}

/// A local answer of `act` by [`PERSON`] to `item`, choosing option `option` (label `label`).
pub fn answer(item: &str, act: AnswerAct, option: &str, label: &str) -> Answer {
    Answer {
        item: item.into(),
        act,
        option: Some(option.into()),
        label: Some(label.into()),
        options: vec![],
        card_hash: None,
        targets: vec![],
        person: writer().provenance(),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    }
}

/// Writes `a` with `Store::put_answer`, then closes the project and opens it again: a published
/// state shows only what was published.
pub fn put_answer(dir: &Path, mut r: Rdy, a: &Answer) -> Rdy {
    r.store_mut().put_answer(a).expect("put_answer");
    r.close().expect("close");
    Rdy::open(dir).expect("reopen")
}

/// Whether one of `changes` writes a decision block. Only a proposed decision can be an
/// exception, so the landing reads the exceptions only then (F2).
pub fn changes_write_decision(r: &Rdy, changes: &[Hash]) -> bool {
    changes.iter().any(|c| {
        r.store().change(c).expect("change").ops.iter().any(|op| {
            matches!(op, Op::Create { block } | Op::Update { block, .. }
                if block.block_type == decision::DECISION)
        })
    })
}

/// Whether version `id` (a full version hash, as `ProposalInfo::id` gives it) writes a decision.
pub fn writes_decision(r: &Rdy, id: &str) -> bool {
    let h: Hash = id.parse().unwrap_or_else(|e| panic!("version {id}: {e}"));
    let p = r.store().proposal(&h).expect("proposal");
    changes_write_decision(r, &p.changes)
}

/// The summary a test proposes view `view` with: [`TEST_SUMMARY`] when one of its included
/// changes writes a decision (a version that lands one is approved through a card), else none.
pub fn summary_for(r: &Rdy, view: &str) -> Option<&'static str> {
    let changes = r.read().included(view);
    changes_write_decision(r, &changes).then_some(TEST_SUMMARY)
}

/// The sweep rule of F2 (D-DKJS5) before a landing of version `p`: when it writes a decision,
/// the human session `s` confirms each exception beside it ([`confirm_exceptions`]).
pub fn confirm_before_landing(r: &mut Rdy, s: &Session, p: &ProposalInfo) -> Vec<AnswerInfo> {
    if !writes_decision(r, &p.id) {
        return vec![];
    }
    confirm_exceptions(r, s, &p.r#ref)
}

/// [`confirm_exceptions`] through engine `e` (a `Local`, as the MCP and daemon paths hold one),
/// as the human session `s`, on the batch `pref`.
pub fn confirm_exceptions_in(e: &dyn Engine, s: &Session, pref: &str) {
    let (s, pref) = (s.clone(), pref.to_string());
    e.write(Box::new(move |r| {
        confirm_exceptions(r, &s, &pref);
        Ok(Value::Null)
    }))
    .expect("confirm the exceptions");
}

/// Confirms each exception that stands beside the batch `pref` in the queue, in queue order, as
/// the human session `s` answering for itself (`Rdy::answer`, option `confirm`, no relay): each
/// is its own answer. The batch must be ready, or its exceptions are no items. Returns the
/// answers.
pub fn confirm_exceptions(r: &mut Rdy, s: &Session, pref: &str) -> Vec<AnswerInfo> {
    let q = r
        .read()
        .pending(&PendingQuery {
            person: s.actor.clone(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending");
    let refs: Vec<String> = q
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(pref))
        .map(|i| i.r#ref.clone())
        .collect();
    refs.iter()
        .map(|item| {
            r.answer(
                s,
                &s.actor,
                AnswerRequest {
                    r#ref: item.clone(),
                    option: "confirm".into(),
                    card_hash: None,
                    note: None,
                    relay: RelayInput::default(),
                    brief: None,
                },
            )
            .unwrap_or_else(|e| panic!("confirm {item}: {e}"))
        })
        .collect()
}
