//! Durable check requests (D-JQD5X, D-33BSB): what a stored CHECK_REQUEST names, the states its
//! case reads, whether that case still holds, who wrote its subject, and the verdicts that answer
//! it. Every read here streams the log at the state's captured end; nothing writes.
//!
//! A request's case is rebuilt with the case builder against the request's scope as it stands
//! now ([`case_freshness`]). That classifier is the only one: request status, the queue and a
//! submission all read it. A missing checkout, an unavailable commit or a failed git read is
//! [`CaseFreshness::Unavailable`], never a changed case; store damage stays an error.
use super::case::{BuiltCase, CaseInputs, build_case};
use super::catalogue::AgentCheck;
use crate::tensions::SideRef;
use crate::{
    E_BAD_INPUT, E_CONFIG, E_IO, E_LIMIT, E_UNKNOWN_BLOCK, Materialized, RdyError, ReadState, Scope,
};
use rdy_model::check_request::{CheckMode, CheckRequest, CheckSubject};
use rdy_model::{BlockId, Change, Hash, Review, ReviewSubject, Verdict};
use rdy_store::StoreError;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The fewest hex digits that name a durable request.
pub const MIN_DURABLE_HEX: usize = 12;

/// The label of a durable request: `rq-` and its 64 hex digits.
pub fn durable_label(h: &Hash) -> String {
    format!("rq-{h}")
}

/// Whether a request's recorded case still holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaseFreshness {
    /// The case rebuilds to the recorded hash.
    Matching,
    /// The case rebuilds to another hash, or a primary subject is gone or of the wrong kind.
    Changed,
    /// Code evidence the case reads cannot be read on this machine now.
    Unavailable,
}

/// A rebuild of a request's case.
#[derive(Clone, Debug)]
pub(crate) enum Rebuilt {
    Built(BuiltCase),
    Changed,
    /// The code-evidence refusal the build returned.
    Unavailable(RdyError),
}

/// The states one case reads, owned: what [`CaseInputs`] borrows.
pub(crate) struct Holder {
    before: Arc<Materialized>,
    after: Arc<Materialized>,
    changes: Vec<Change>,
}

impl Holder {
    pub(crate) fn inputs<'a>(&'a self, st: &'a ReadState) -> CaseInputs<'a> {
        CaseInputs {
            st,
            before: &self.before,
            after: &self.after,
            changes: &self.changes,
        }
    }

    /// The state the case judges.
    pub(crate) fn after(&self) -> &Materialized {
        &self.after
    }
}

/// The subject a recipe builds from: a change or version subject without the primary blocks a
/// request records beside it.
pub(crate) fn build_subject(s: &CheckSubject) -> CheckSubject {
    let mut b = s.clone();
    if b.change.is_some() || b.version.is_some() {
        b.blocks.clear();
    }
    b
}

/// The scope a stored scope label names: `canon`, the in-flight scope, else a view.
fn scope_of(label: &str) -> Scope {
    match label {
        "canon" => Scope::Canon,
        crate::INFLIGHT_SCOPE => Scope::Inflight,
        _ => Scope::View(label.to_string()),
    }
}

/// Canon at landing `base`; empty before the first landing.
fn at_base(st: &ReadState, base: Option<Hash>) -> Result<Arc<Materialized>, RdyError> {
    match base {
        Some(h) => st.materialized(&Scope::At(h)),
        None => Ok(Arc::new(Materialized::default())),
    }
}

/// The states the case of `subject` (as a request stores it) reads now. A block subject reads
/// its scope; a change reads its view against the view's base, with that change only; a proposal
/// version reads its view (else the version itself) against its base, with its own changes.
/// `None` when the scope's view is gone.
pub(crate) fn holder(
    st: &ReadState,
    scope: &str,
    subject: &CheckSubject,
) -> Result<Option<Holder>, RdyError> {
    let reader = st.reader();
    if let Some(v) = &subject.version {
        let p = st.proposal_version(v)?;
        let after = if st.has_view(&p.view) {
            st.materialized(&Scope::View(p.view.clone()))?
        } else {
            st.materialized(&Scope::Proposal(*v))?
        };
        let mut changes = Vec::with_capacity(p.changes.len());
        for c in &p.changes {
            changes.push(reader.change(c)?);
        }
        return Ok(Some(Holder {
            before: at_base(st, p.canon_at)?,
            after,
            changes,
        }));
    }
    if let Some(c) = &subject.change {
        let change = reader.change(c)?;
        if !st.has_view(&change.view) {
            return Ok(None);
        }
        let before = at_base(st, st.effective_base(&change.view))?;
        let after = st.materialized(&Scope::View(change.view.clone()))?;
        return Ok(Some(Holder {
            before,
            after,
            changes: vec![change],
        }));
    }
    let s = scope_of(scope);
    if let Scope::View(v) = &s
        && !st.has_view(v)
    {
        return Ok(None);
    }
    let m = st.materialized(&s)?;
    Ok(Some(Holder {
        before: Arc::clone(&m),
        after: m,
        changes: vec![],
    }))
}

/// Whether `e` is a code-evidence refusal: a checkout that cannot be read, a recorded commit the
/// checkout does not hold, or a git read that failed. Store damage never matches.
pub(crate) fn is_unavailable(e: &RdyError) -> bool {
    let m = e.message.as_str();
    match e.code {
        E_BAD_INPUT => {
            (m.starts_with("check ") && m.contains(" needs a readable checkout for "))
                || (m.starts_with("code revision ") && m.contains(" is not a commit in "))
        }
        E_IO => {
            m.starts_with("git ") || m.starts_with("running git") || m.starts_with("code repo ")
        }
        E_CONFIG => m.starts_with("code repo "),
        E_LIMIT => m.starts_with("code evidence exceeds "),
        _ => false,
    }
}

/// Whether `e` says the subject no longer fits its check: a primary block is gone, the subject is
/// now of the wrong kind or empty, its anchors are gone, or it outgrew the case bound.
fn is_changed(e: &RdyError) -> bool {
    let (m, f) = (e.message.as_str(), e.fix.as_str());
    match e.code {
        E_UNKNOWN_BLOCK => true,
        E_BAD_INPUT => {
            (f.starts_with("pass ")
                && f.ends_with(": rdy_review {op: \"checks\"} / rdy review checks"))
                || (m.starts_with("check ") && m.contains(" has no anchored code for "))
        }
        E_LIMIT => m.starts_with("check case exceeds "),
        _ => false,
    }
}

/// The case of `check` on `subject` rebuilt from `inputs`, classified.
pub(crate) fn rebuild(
    check: AgentCheck,
    subject: &CheckSubject,
    inputs: CaseInputs<'_>,
) -> Result<Rebuilt, RdyError> {
    match build_case(check, subject, inputs) {
        Ok(c) => Ok(Rebuilt::Built(c)),
        Err(e) if is_unavailable(&e) => Ok(Rebuilt::Unavailable(e)),
        Err(e) if is_changed(&e) => Ok(Rebuilt::Changed),
        Err(e) => Err(e),
    }
}

/// Whether `request`'s case still holds against `inputs`: equal complete hashes are
/// [`CaseFreshness::Matching`], a rebuilt other hash or a known removed or wrong-kind primary
/// subject is [`CaseFreshness::Changed`], and code evidence that cannot be read now is
/// [`CaseFreshness::Unavailable`]. A malformed record or any other read error is returned.
pub(crate) fn case_freshness(
    request: &CheckRequest,
    inputs: CaseInputs<'_>,
) -> Result<CaseFreshness, RdyError> {
    let Some(check) = AgentCheck::parse(&request.check) else {
        return Ok(CaseFreshness::Changed);
    };
    Ok(
        match rebuild(check, &build_subject(&request.subject), inputs)? {
            Rebuilt::Built(c) if c.hash == request.case_hash => CaseFreshness::Matching,
            Rebuilt::Built(_) | Rebuilt::Changed => CaseFreshness::Changed,
            Rebuilt::Unavailable(_) => CaseFreshness::Unavailable,
        },
    )
}

/// `request` rebuilt against its scope in `st` now: [`Rebuilt::Changed`] when its view is gone or
/// its check is unknown.
pub(crate) fn rebuild_now(st: &ReadState, request: &CheckRequest) -> Result<Rebuilt, RdyError> {
    let Some(check) = AgentCheck::parse(&request.check) else {
        return Ok(Rebuilt::Changed);
    };
    let Some(h) = holder(st, &request.scope, &request.subject)? else {
        return Ok(Rebuilt::Changed);
    };
    rebuild(check, &build_subject(&request.subject), h.inputs(st))
}

/// [`case_freshness`] of `request` against its scope in `st` now.
pub(crate) fn freshness_now(
    st: &ReadState,
    request: &CheckRequest,
) -> Result<CaseFreshness, RdyError> {
    let Some(h) = holder(st, &request.scope, &request.subject)? else {
        return Ok(CaseFreshness::Changed);
    };
    case_freshness(request, h.inputs(st))
}

/// The actors who wrote the text block `id` holds in `m`: for each change that produced its
/// version, the existing text-author walk (repins and tracking edits keep the writer). A block
/// `m` does not hold has none.
pub(crate) fn block_authors(
    st: &ReadState,
    m: &Materialized,
    id: &BlockId,
) -> Result<Vec<String>, RdyError> {
    let (Some(b), Some(v)) = (m.get(id), m.version_of(id)) else {
        return Ok(vec![]);
    };
    let content = b.content_hash_with(st.reader().tracking());
    let mut out: Vec<String> = vec![];
    for p in st.reader().producers(&v)? {
        for a in st.text_authors(
            &SideRef::Change(p),
            id,
            Some(v),
            Some(content),
            "this check",
            "run rdy fsck",
        )? {
            if !out.contains(&a) {
                out.push(a);
            }
        }
    }
    Ok(out)
}

/// The actors who wrote primary block `id` of the case `h` reads: its text authors in the
/// judged state; for a block that state no longer holds (the subject deletes it), the actors of
/// the selected changes that delete it and the text authors of the before version the case
/// shows.
pub(crate) fn subject_authors(
    st: &ReadState,
    h: &Holder,
    id: &BlockId,
) -> Result<Vec<String>, RdyError> {
    if h.after.get(id).is_some() {
        return block_authors(st, &h.after, id);
    }
    let mut out: Vec<String> = vec![];
    for c in &h.changes {
        let deletes = c
            .ops
            .iter()
            .any(|op| matches!(op, rdy_model::Op::Delete { id: d, .. } if d == id));
        if deletes && !out.contains(&c.provenance.actor) {
            out.push(c.provenance.actor.clone());
        }
    }
    for a in block_authors(st, &h.before, id)? {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    Ok(out)
}

/// The first of `blocks` whose text `actor` wrote, as [`subject_authors`] reads it.
pub(crate) fn authored_block(
    st: &ReadState,
    h: &Holder,
    blocks: &[BlockId],
    actor: &str,
) -> Result<Option<BlockId>, RdyError> {
    for b in blocks {
        if subject_authors(st, h, b)?.iter().any(|a| a == actor) {
            return Ok(Some(b.clone()));
        }
    }
    Ok(None)
}

/// Runs `f` on every request the log held at `st`'s captured end, in ascending full hash order,
/// while it returns `Ok(true)`: `Ok(false)` stops the walk, and an error ends it.
pub(crate) fn for_each_request(
    st: &ReadState,
    mut f: impl FnMut(Hash, CheckRequest) -> Result<bool, RdyError>,
) -> Result<(), RdyError> {
    let mut failed = None;
    let mut stopped = false;
    let walked = st
        .reader()
        .for_each_check_request(st.seq(), |h, r| match f(h, r) {
            Ok(true) => Ok(()),
            Ok(false) => {
                stopped = true;
                Err(StoreError::Refused(String::new()))
            }
            Err(e) => {
                failed = Some(e);
                Err(StoreError::Refused(String::new()))
            }
        });
    if let Some(e) = failed {
        return Err(e);
    }
    if stopped {
        return Ok(());
    }
    walked.map_err(RdyError::from)
}

/// The durable requests whose full hex starts with `hex` (12 to 64 lowercase hex digits; a
/// shorter prefix names none), in hash order.
pub(crate) fn durable_matches(st: &ReadState, hex: &str) -> Result<Vec<Hash>, RdyError> {
    if hex.len() < MIN_DURABLE_HEX {
        return Ok(vec![]);
    }
    let mut out = vec![];
    for_each_request(st, |h, _| {
        if h.to_string().starts_with(hex) {
            out.push(h);
        }
        Ok(true)
    })?;
    Ok(out)
}

/// The verdicts that answer requests: Check reviews by the request they name, and pair reviews
/// by the case hash they bind (a contradiction request's settlement is its pair's review).
#[derive(Default)]
pub(crate) struct Answers {
    checks: BTreeMap<Hash, Vec<(Hash, Review)>>,
    pairs: BTreeMap<Hash, Vec<(Hash, Review)>>,
}

impl Answers {
    /// Every Check and pair review the log held at `st`'s captured end.
    pub(crate) fn read(st: &ReadState) -> Result<Answers, RdyError> {
        let reader = st.reader();
        let end = st.seq();
        let mut a = Answers::default();
        reader.for_each_object_of(rdy_store::kinds::REVIEW, |h| {
            if !reader.has_kind_before(rdy_store::kinds::REVIEW, &h, end)? {
                return Ok(());
            }
            let r: Review = match reader.get_typed(rdy_store::kinds::REVIEW, &h) {
                Ok(r) => r,
                Err(StoreError::Redacted { .. }) => return Ok(()),
                Err(e) => return Err(e),
            };
            match (&r.subject, r.case_hash) {
                (ReviewSubject::Check { request }, _) => {
                    a.checks.entry(*request).or_default().push((h, r));
                }
                (ReviewSubject::Pair { .. }, Some(case)) => {
                    a.pairs.entry(case).or_default().push((h, r));
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(a)
    }

    /// The reviews that answer request `h`, in hash order.
    pub(crate) fn of(&self, h: &Hash, r: &CheckRequest) -> &[(Hash, Review)] {
        let found = if r.check == AgentCheck::Contradiction.spec().name {
            self.pairs.get(&r.case_hash)
        } else {
            self.checks.get(h)
        };
        found.map(Vec::as_slice).unwrap_or_default()
    }
}

/// The wire name of a verdict.
pub(crate) fn verdict_word(v: Verdict) -> &'static str {
    match v {
        Verdict::Dismiss => "dismiss",
        Verdict::Restore => "restore",
        Verdict::Tension => "tension",
        Verdict::NoTension => "no_tension",
        Verdict::Pass => "pass",
        Verdict::Concern => "concern",
        Verdict::Fail => "fail",
    }
}

/// `{actor, run?}`.
fn actor_json(actor: &str, run: Option<&str>) -> Value {
    let mut o = json!({"actor": actor});
    if let Some(r) = run {
        o["run"] = json!(r);
    }
    o
}

/// A verdict row: `{review, verdict, actor, run?, reason, citations, case_hash}`; an empty reason
/// is null.
pub(crate) fn verdict_row(h: &Hash, r: &Review) -> Value {
    let mut row = json!({
        "review": h.to_string(),
        "verdict": verdict_word(r.verdict),
        "actor": r.provenance.actor,
    });
    if let Some(run) = &r.provenance.run {
        row["run"] = json!(run);
    }
    row["reason"] = if r.reason.is_empty() {
        Value::Null
    } else {
        json!(r.reason)
    };
    row["citations"] = r
        .citations
        .iter()
        .map(|c| json!({"block": c.block.to_string(), "note": c.note}))
        .collect();
    row["case_hash"] = json!(r.case_hash.map(|c| c.to_string()));
    row
}

/// A stored subject as a reply shows it: `{blocks, change, version, code}`.
pub(crate) fn subject_json(s: &CheckSubject) -> Value {
    json!({
        "blocks": s.blocks.iter().map(BlockId::to_string).collect::<Vec<_>>(),
        "change": s.change.map(|h| h.to_string()),
        "version": s.version.map(|h| h.to_string()),
        "code": s.code.as_ref().map(|c| json!({"repo": c.repo, "from": c.from, "to": c.to})),
    })
}

/// The wire name of a mode.
pub(crate) fn mode_word(m: CheckMode) -> &'static str {
    match m {
        CheckMode::Author => "author",
        CheckMode::Independent => "independent",
    }
}

/// A list row of request `h`: `{request, check, subject, scope, mode, requested_by, reason,
/// case_hash, status, verdicts}`.
pub(crate) fn request_row(
    h: &Hash,
    r: &CheckRequest,
    status: &str,
    verdicts: &[(Hash, Review)],
) -> Value {
    json!({
        "request": durable_label(h),
        "check": r.check,
        "subject": subject_json(&r.subject),
        "scope": r.scope,
        "mode": mode_word(r.mode),
        "requested_by": actor_json(&r.requested_by.actor, r.requested_by.run.as_deref()),
        "reason": r.reason,
        "case_hash": r.case_hash.to_string(),
        "status": status,
        "verdicts": verdicts.iter().map(|(h, v)| verdict_row(h, v)).collect::<Vec<_>>(),
    })
}

/// The status of request `h`: `answered` when a review settles it, else `stale` when its case
/// changed, else `open` (an unavailable case included).
pub(crate) fn status_of(
    st: &ReadState,
    answers: &Answers,
    h: &Hash,
    r: &CheckRequest,
) -> Result<&'static str, RdyError> {
    if !answers.of(h, r).is_empty() {
        return Ok("answered");
    }
    Ok(match freshness_now(st, r)? {
        CaseFreshness::Changed => "stale",
        CaseFreshness::Matching | CaseFreshness::Unavailable => "open",
    })
}

/// The first independent request in full hash order that `actor` may claim now: of `only`'s
/// check (any check when `None`), not in `leased`, unanswered, with a case that rebuilds to its
/// hash, and with no primary block `actor` wrote. Returns it with its current case.
pub(crate) fn next_durable(
    st: &ReadState,
    actor: &str,
    leased: &BTreeSet<Hash>,
    only: Option<AgentCheck>,
) -> Result<Option<(Hash, CheckRequest, BuiltCase)>, RdyError> {
    let answers = Answers::read(st)?;
    let mut found = None;
    for_each_request(st, |h, r| {
        if r.mode != CheckMode::Independent
            || leased.contains(&h)
            || only.is_some_and(|c| c.spec().name != r.check)
            || !answers.of(&h, &r).is_empty()
        {
            return Ok(true);
        }
        let Some(check) = AgentCheck::parse(&r.check) else {
            return Ok(true);
        };
        let Some(holder) = holder(st, &r.scope, &r.subject)? else {
            return Ok(true);
        };
        let case = match rebuild(check, &build_subject(&r.subject), holder.inputs(st))? {
            Rebuilt::Built(c) if c.hash == r.case_hash => c,
            _ => return Ok(true),
        };
        if authored_block(st, &holder, &r.subject.blocks, actor)?.is_some() {
            return Ok(true);
        }
        found = Some((h, r, case));
        Ok(false)
    })?;
    Ok(found)
}
