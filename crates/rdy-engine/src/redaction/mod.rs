//! Redaction requests and their blast radius (plan Redaction; D-6B1W1: a report "Before a
//! redaction is proposed"; D-14SAY, D-EPK1Z, D-33JR0).
//!
//! A request names targets ([`targets`]), a reason class and a reason. The engine walks the whole
//! log, not only current blocks, and computes the complete report ([`report`]): every selected
//! version and blob, the payloads the removal would drop, every record that embeds, references
//! or quotes the content, six fixed outside-copy rows and the quarantine names. A dry run returns
//! the report and changes nothing. Recording a request writes its carrier, residues and request
//! in one batch through the store's checked preparation; nothing is erased until a person
//! approves it. [`presentation`] is the one text form of a result.
pub mod presentation;
pub mod report;
mod scan;
pub mod targets;

pub use presentation::{ReportText, report_text};
pub use report::{QuoteRow, RedactionReport, ReportCopies, compute_report};
pub use scan::{ASSIGNED_KINDS, TextPolicy, require_text_policies, text_policy};

use crate::error::{E_PERMISSION, RdyError};
use crate::views::{Action, commit_claim};
use crate::write::check_view_access;
use crate::{Delta, Rdy, ReadState, Session};
use rdy_model::redaction::{AssessmentSupport, ReasonClass, RedactionWarning, carrier};
use rdy_model::{Change, Hash, Redaction};
use rdy_store::StoreError;
use serde::{Deserialize, Serialize};
use std::cell::Cell;

/// What a redaction request asks for: the selectors, the reason class (a wire name), the reason
/// and whether it is a dry run. JSON accepts only these keys; an omitted `dry_run` is false.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactInput {
    pub targets: Vec<String>,
    pub reason_class: String,
    pub reason: String,
    #[serde(default)]
    pub dry_run: bool,
}

/// The result of a redaction call: the validated class and trimmed reason, the carrier change
/// and the request (both null for a dry run or when everything selected was already removed),
/// the complete report and its warnings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactResult {
    pub dry_run: bool,
    pub reason_class: ReasonClass,
    pub reason: String,
    #[serde(with = "rdy_store::transition::hex::opt")]
    pub change: Option<Hash>,
    #[serde(with = "rdy_store::transition::hex::opt")]
    pub redaction: Option<Hash>,
    pub report: RedactionReport,
    pub warnings: Vec<RedactionWarning>,
}

/// The numeric bounds of a report (inclusive maxima). The selector, selector-byte and reason
/// bounds are fixed in [`targets`]; these are the ones a whole store can reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Selected block versions plus blob manifests.
    pub selected: usize,
    /// Data frames one scan reads.
    pub frames: u64,
    /// Frame bytes (header plus payload) one scan reads.
    pub bytes: u64,
    /// Report rows across all sections; a quote row and its impact row count separately.
    pub rows: usize,
    /// Bytes of one encoded report or record.
    pub record: usize,
    /// UTF-8 bytes of one source string (a selected target's own text). Other strings are
    /// searched at any length; the scanned-bytes bound bounds that work.
    pub string: usize,
    /// Nested field containers of a selected target. Elsewhere the quote search stops at this
    /// depth and the report says how many records it did not read below it.
    pub levels: usize,
    /// Exact-match source strings.
    pub sources: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        use rdy_store::redaction as r;
        Limits {
            selected: r::MAX_SELECTED,
            frames: r::MAX_SCANNED_FRAMES,
            bytes: r::MAX_SCANNED_BYTES,
            rows: 100_000,
            record: rdy_model::redaction::MAX_RECORD_BYTES,
            string: 1_048_576,
            levels: 64,
            sources: 4096,
        }
    }
}

#[cfg(feature = "test-support")]
thread_local! {
    static LIMITS: Cell<Option<Limits>> = const { Cell::new(None) };
}

thread_local! {
    static COMPUTATIONS: Cell<u64> = const { Cell::new(0) };
    static PREVIEW_FAULTS: Cell<u32> = const { Cell::new(0) };
}

/// Tests only: the next `n` removal previews on this thread fail (`E_INTERNAL`), before the
/// batch that would authorize the removal is sealed.
#[doc(hidden)]
pub fn fail_next_redaction_previews_for_tests(n: u32) {
    PREVIEW_FAULTS.with(|c| c.set(n));
}

/// The injected preview failure, if one is armed on this thread (it disarms one).
pub(crate) fn preview_fault() -> Result<(), RdyError> {
    PREVIEW_FAULTS.with(|c| match c.get() {
        0 => Ok(()),
        n => {
            c.set(n - 1);
            Err(RdyError::new(
                crate::E_INTERNAL,
                "internal error: injected redaction preview failure",
                "report this as a bug in rdy",
            ))
        }
    })
}

/// Tests only: how many complete reports this thread has computed, so a test can check that a
/// request computes its report once.
#[doc(hidden)]
pub fn report_computations_for_tests() -> u64 {
    COMPUTATIONS.with(Cell::get)
}

/// Counts one complete report computation on this thread.
pub(crate) fn count_computation() {
    COMPUTATIONS.with(|c| c.set(c.get().wrapping_add(1)));
}

/// The bounds this thread computes reports under: the fixed defaults (a build with the
/// `test-support` feature lets a test replace them for its thread).
pub fn limits() -> Limits {
    #[cfg(feature = "test-support")]
    if let Some(l) = LIMITS.with(Cell::get) {
        return l;
    }
    Limits::default()
}

/// Tests only (the `test-support` feature, which no normal build enables): computes this
/// thread's reports under `l` until the guard drops, so a test can reach each bound and the
/// bound plus one with a small store. The store's own record checks keep their fixed bounds.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn override_limits_for_tests(l: Limits) -> LimitsGuard {
    LimitsGuard(LIMITS.with(|c| c.replace(Some(l))))
}

/// Restores the bounds [`override_limits_for_tests`] replaced when dropped.
#[cfg(feature = "test-support")]
#[doc(hidden)]
#[must_use = "the bounds are lowered only while the guard lives"]
pub struct LimitsGuard(Option<Limits>);

#[cfg(feature = "test-support")]
impl Drop for LimitsGuard {
    fn drop(&mut self) {
        LIMITS.with(|c| c.set(self.0));
    }
}

thread_local! {
    static SUPPORT: Cell<Option<AssessmentSupport>> = const { Cell::new(None) };
}

/// The status this build's reports give the recorded assessment kinds: `Carried`, as the scan
/// reads them (a build with the `test-support` feature lets a test replace it for its thread).
pub(crate) fn current_support() -> AssessmentSupport {
    #[cfg(feature = "test-support")]
    if let Some(s) = SUPPORT.with(Cell::get) {
        return s;
    }
    AssessmentSupport::Carried
}

/// Tests only (the `test-support` feature, which no normal build enables): this thread's
/// reports carry `support` as their assessment status until the guard drops, so a test can
/// record a request as an earlier release did.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn override_assessment_support_for_tests(support: AssessmentSupport) -> SupportGuard {
    SupportGuard(SUPPORT.with(|c| c.replace(Some(support))))
}

/// Restores the status [`override_assessment_support_for_tests`] replaced when dropped.
#[cfg(feature = "test-support")]
#[doc(hidden)]
#[must_use = "the status is replaced only while the guard lives"]
pub struct SupportGuard(Option<AssessmentSupport>);

#[cfg(feature = "test-support")]
impl Drop for SupportGuard {
    fn drop(&mut self) {
        SUPPORT.with(|c| c.set(self.0));
    }
}

/// The dry run of `input` against `st`: the complete report, nothing recorded. Any role may run
/// it (a reader too).
pub fn dry_run(st: &ReadState, input: &RedactInput) -> Result<RedactResult, RdyError> {
    let full = report::compute(st, input, &crate::quarantine::names(st.root()))?;
    Ok(result_of(full, true))
}

/// May `s` record a request in `view`? A writer, reviewer or approver may (a reader may not),
/// under the existing view ownership rules of a commit (`check_view_access`): the carrier's
/// commit creates or claims the session's own view exactly as an ordinary first commit does.
pub fn check_recorder(st: &ReadState, s: &Session, view: &str) -> Result<(), RdyError> {
    if !s.role.can_write() {
        return Err(RdyError::new(
            E_PERMISSION,
            format!("role {:?} cannot write", s.role),
            "start the session as a writer, or run the redaction as a dry run",
        ));
    }
    check_view_access(st, view, s, Action::Commit)
}

/// Records the request `input` with its `report` in `session`'s default view, through the
/// store's checked preparation and commit, as one batch that erases nothing. See
/// [`prepare_request_in`].
pub fn prepare_request(
    rdy: &mut Rdy,
    session: &Session,
    input: RedactInput,
    report: RedactionReport,
) -> Result<RedactResult, RdyError> {
    let view = session.default_view();
    prepare_request_in(rdy, session, &view, input, report)
}

/// Records the request `input` with its `report` in view `view`. The manifest is recomputed
/// from the published state with the report's own quarantine names; a report whose hash or
/// targets no longer match is `E_REDACTION_STALE`. When every target was already removed nothing
/// is written and both hashes are null. Otherwise the carrier (no ops, the session's provenance,
/// message `Request content removal.`), the residues and the request are prepared and committed
/// by the store in one batch. The batch stays open: the caller publishes it (`Rdy::redact` does).
pub fn prepare_request_in(
    rdy: &mut Rdy,
    session: &Session,
    view: &str,
    input: RedactInput,
    report: RedactionReport,
) -> Result<RedactResult, RdyError> {
    let st = rdy.read();
    check_recorder(&st, session, view)?;
    let full = report::compute(&st, &input, &report.quarantine)?;
    drop(st);
    if full.report.report_hash != report.report_hash || full.report.targets != report.targets {
        return Err(StoreError::RedactionStale.into());
    }
    let (result, parts) = split(full);
    record(rdy, session, view, result, parts)
}

/// What a request records beside its result: the manifest, the residues and its own targets.
struct Parts {
    manifest: rdy_model::redaction::RedactionManifest,
    residues: Vec<rdy_model::redaction::RedactionResidue>,
    request_targets: Vec<rdy_model::redaction::RedactTarget>,
}

/// A computed report as a result with null hashes, and the parts a request records.
fn split(full: report::Full) -> (RedactResult, Parts) {
    let parts = Parts {
        manifest: full.manifest,
        residues: full.residues,
        request_targets: full.request_targets,
    };
    let result = RedactResult {
        dry_run: false,
        reason_class: full.class,
        reason: full.reason,
        change: None,
        redaction: None,
        warnings: full.report.warnings.clone(),
        report: full.report,
    };
    (result, parts)
}

/// Records the request of `result` in `view` from its computed `parts`, without computing the
/// report again: the carrier (no ops, the session's provenance), the residues and the request,
/// through the store's checked preparation and commit, as one batch. The carrier's commit claims
/// the view as an ordinary first commit does (`commit_claim`). When every target was already
/// removed nothing is written and both hashes stay null.
fn record(
    rdy: &mut Rdy,
    session: &Session,
    view: &str,
    mut result: RedactResult,
    parts: Parts,
) -> Result<RedactResult, RdyError> {
    if parts.request_targets.is_empty() {
        return Ok(result);
    }
    let claim = commit_claim(&rdy.read(), session, view)?;
    let provenance = session.provenance();
    let carrier = carrier(view.to_string(), vec![], provenance.clone());
    let request = Redaction {
        carrier: carrier.id(),
        actor: provenance,
        targets: parts.request_targets,
        reason_class: result.reason_class,
        reason: result.reason.clone(),
        manifest: parts.manifest,
    };
    let prepared =
        rdy.store()
            .prepare_redaction_claiming(&carrier, &request, &parts.residues, claim)?;
    let change = prepared.carrier();
    let redaction = rdy.store_mut().commit_redaction(prepared)?;
    result.change = Some(change);
    result.redaction = Some(redaction);
    Ok(result)
}

/// The result of a computed report.
fn result_of(full: report::Full, dry_run: bool) -> RedactResult {
    RedactResult {
        dry_run,
        ..split(full).0
    }
}

impl Rdy {
    /// A redaction request in `session`'s default view: see [`Rdy::redact_in`].
    pub fn redact(
        &mut self,
        session: &Session,
        input: RedactInput,
    ) -> Result<RedactResult, RdyError> {
        let view = session.default_view();
        self.redact_in(session, &view, input)
    }

    /// A redaction request in view `view`. A dry run (any role) returns the complete report and
    /// changes nothing. Otherwise the session must be able to record in `view`
    /// ([`check_recorder`]); the report is computed from the published state
    /// (`ReadState::redaction_report`), recorded by [`prepare_request_in`], and its batch sealed
    /// and published. Recording authorizes no erasure: a person approves it later.
    pub fn redact_in(
        &mut self,
        session: &Session,
        view: &str,
        input: RedactInput,
    ) -> Result<RedactResult, RdyError> {
        self.redact_checked(session, view, input, &mut |_| Ok(()))
    }

    /// [`Rdy::redact_in`], where `check` reads the result before anything is recorded (its
    /// hashes still null): an error from it refuses the request and writes nothing. The report
    /// is computed once, from the published state, and recorded from that computation; no
    /// second computation runs on the writer. A dry run returns before `check`.
    pub fn redact_checked(
        &mut self,
        session: &Session,
        view: &str,
        input: RedactInput,
        check: &mut dyn FnMut(&RedactResult) -> Result<(), RdyError>,
    ) -> Result<RedactResult, RdyError> {
        let st = self.read();
        if input.dry_run {
            return dry_run(&st, &input);
        }
        check_recorder(&st, session, view)?;
        let full = report::compute(&st, &input, &crate::quarantine::names(st.root()))?;
        drop(st);
        let (result, parts) = split(full);
        check(&result)?;
        let result = record(self, session, view, result, parts)?;
        if result.change.is_some() {
            self.publish(Delta::Full)?;
        }
        Ok(result)
    }
}

/// The requests that the changes `changes` carry (each change that is a request's carrier),
/// sorted and distinct: what landing those changes would authorize.
pub(crate) fn requests_carried(st: &ReadState, changes: &[Hash]) -> Result<Vec<Hash>, RdyError> {
    let mut out = vec![];
    for c in changes {
        if let Some(r) = st.reader().redaction_for_carrier(c)? {
            out.push(r);
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// The freshness check of recorded request `request` (plan Redaction, Task 10), before its
/// removal card is shown and again under the writer before its approval lands: its complete
/// report is computed again from `st`, through the one report path with the project's current
/// quarantine names, leaving out only the batches the request itself wrote ([`report::Own`]).
/// A manifest hash or a target set that differs from the recorded one is `E_REDACTION_STALE`;
/// nothing is refreshed. `prospective` holds the changes the landing would write beside the log
/// (its acceptance and re-key change): one that would join the report (a new version of an
/// all-versions target, a new copy or quote of the content) is `E_REDACTION_STALE` too, so the
/// landing never writes a copy outside the removal the person approved.
pub(crate) fn check_fresh(
    st: &ReadState,
    request: &Hash,
    prospective: &[Change],
) -> Result<(), RdyError> {
    let r = st.reader().redaction_request(request)?;
    let input = RedactInput {
        targets: r.targets.iter().map(targets::selector_text).collect(),
        reason_class: targets::reason_class_name(r.reason_class).to_string(),
        reason: r.reason.clone(),
        dry_run: false,
    };
    let own = report::Own {
        carrier: r.carrier,
        request: *request,
    };
    let names = crate::quarantine::names(st.root());
    let full = report::compute_excluding(st, &input, &names, Some(&own), prospective)?;
    let mut buf = report::buffer(limits().record)?;
    let bound = r
        .manifest
        .report_hash(&mut buf)
        .map_err(|_| crate::error::redaction_limit(rdy_store::redaction::limit::RECORD))?;
    if full.prospective || full.report.report_hash != bound || full.request_targets != r.targets {
        return Err(StoreError::RedactionStale.into());
    }
    Ok(())
}

impl ReadState {
    /// The redaction epoch this state's reader captured.
    pub fn redaction_epoch(&self) -> u64 {
        self.reader().captured_redaction_epoch()
    }

    /// `E_REDACTED_STATE` when content was removed after this state was read (its captured epoch
    /// is behind the store's); the store's poisoned-handle error after a failed write.
    pub fn ensure_current_epoch(&self) -> Result<(), RdyError> {
        Ok(self.reader().check_redaction_epoch()?)
    }
}
