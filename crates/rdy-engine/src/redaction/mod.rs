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

use crate::error::{E_PERMISSION, E_VIEW_OWNER, RdyError};
use crate::views::Action;
use crate::write::check_view_access;
use crate::{Delta, Rdy, ReadState, Session, SessionKind};
use rdy_model::redaction::{ReasonClass, RedactionWarning, carrier};
use rdy_model::{Hash, Redaction, hash_of};
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
    /// UTF-8 bytes of one scanned string.
    pub string: usize,
    /// Nested field containers.
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

thread_local! {
    static LIMITS: Cell<Option<Limits>> = const { Cell::new(None) };
}

/// The bounds this thread computes reports under: the defaults, unless a test lowered them.
pub fn limits() -> Limits {
    LIMITS.with(|l| l.get()).unwrap_or_default()
}

/// Tests only: computes this thread's reports under `l` until the guard drops, so a test can
/// reach each bound and the bound plus one with a small store. The store's own record checks
/// keep their fixed bounds.
#[doc(hidden)]
pub fn override_limits_for_tests(l: Limits) -> LimitsGuard {
    LimitsGuard(LIMITS.with(|c| c.replace(Some(l))))
}

/// Restores the bounds [`override_limits_for_tests`] replaced when dropped.
#[doc(hidden)]
#[must_use = "the bounds are lowered only while the guard lives"]
pub struct LimitsGuard(Option<Limits>);

impl Drop for LimitsGuard {
    fn drop(&mut self) {
        LIMITS.with(|c| c.set(self.0));
    }
}

/// The dry run of `input` against `st`: the complete report, nothing recorded. Any role may run
/// it (a reader too).
pub fn dry_run(st: &ReadState, input: &RedactInput) -> Result<RedactResult, RdyError> {
    let full = report::compute(st, input, &crate::quarantine::names(st.root()))?;
    Ok(result_of(full, true, None, None))
}

/// May `s` record a request in `view`? A writer, reviewer or approver may (a reader may not);
/// the view must be one `s` may commit to; an agent records only in a view that exists and that
/// it owns (a request is never the commit that claims a view).
pub fn check_recorder(st: &ReadState, s: &Session, view: &str) -> Result<(), RdyError> {
    if !s.role.can_write() {
        return Err(RdyError::new(
            E_PERMISSION,
            format!("role {:?} cannot write", s.role),
            "start the session as a writer, or run the redaction as a dry run",
        ));
    }
    check_view_access(st, view, s, Action::Commit)?;
    if s.kind == SessionKind::Agent {
        let owned = st
            .view_head(view)
            .and_then(|h| h.owner.as_ref())
            .is_some_and(|o| (&o.actor, &o.run) == (&s.actor, &s.run));
        if !owned {
            return Err(RdyError::new(
                E_VIEW_OWNER,
                format!("view {view:?} is not a view this session owns"),
                "record the request in your own view: write to it first, then record the request",
            ));
        }
    }
    Ok(())
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
    if full.request_targets.is_empty() {
        return Ok(result_of(full, false, None, None));
    }
    let provenance = session.provenance();
    let carrier = carrier(view.to_string(), vec![], provenance.clone());
    let request = Redaction {
        carrier: hash_of(&carrier),
        actor: provenance,
        targets: full.request_targets.clone(),
        reason_class: full.class,
        reason: full.reason.clone(),
        manifest: full.manifest.clone(),
    };
    let prepared = rdy
        .store()
        .prepare_redaction(&carrier, &request, &full.residues)?;
    let change = prepared.carrier();
    let redaction = rdy.store_mut().commit_redaction(prepared)?;
    Ok(result_of(full, false, Some(change), Some(redaction)))
}

/// The result of a computed report.
fn result_of(
    full: report::Full,
    dry_run: bool,
    change: Option<Hash>,
    redaction: Option<Hash>,
) -> RedactResult {
    RedactResult {
        dry_run,
        reason_class: full.class,
        reason: full.reason,
        change,
        redaction,
        warnings: full.report.warnings.clone(),
        report: full.report,
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
        let st = self.read();
        if input.dry_run {
            return dry_run(&st, &input);
        }
        check_recorder(&st, session, view)?;
        let report = st.redaction_report(&input)?;
        drop(st);
        let result = prepare_request_in(self, session, view, input, report)?;
        if result.change.is_some() {
            self.publish(Delta::Full)?;
        }
        Ok(result)
    }
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
