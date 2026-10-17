//! The one text form of a redaction result (plan Redaction, shared presentation contract). Pure:
//! no store or filesystem access. The MCP and CLI surfaces call [`report_text`]; they repeat
//! none of its texts.
//!
//! The order is: title, normalized targets, counts, inside rows, quote rows, outside rows,
//! quarantine names, assessment status, reason, warnings, result. Each element is one line;
//! quote text, quarantine names and the reason are JSON strings, so a newline stays escaped.
use super::RedactResult;
use super::targets::{reason_class_name, selector_text};
use crate::error::{E_BAD_INPUT, RdyError, redaction_budget};
use rdy_model::redaction::{CopyKind, ImpactKind, ImpactRelation};

/// The smallest output budget, in tokens (inclusive).
pub const MIN_TOKENS: usize = 256;
/// The largest output budget, in tokens (inclusive).
pub const MAX_TOKENS: usize = 20_000;
/// The budget when none is given.
pub const DEFAULT_TOKENS: usize = 4000;

/// The title of a dry run.
pub const DRY_RUN_TITLE: &str = "Redaction dry run";
/// The title of a request.
pub const REQUEST_TITLE: &str = "Redaction request";
/// The line that replaces the row sections when the whole report does not fit.
pub const ROWS_OMITTED: &str = "The row list was omitted. Increase max_tokens to read it.";
/// The assessment line: no assessment kind exists before M3b.
pub const ASSESSMENTS: &str = "Recorded assessments: not implemented.";
/// The result line of a dry run.
pub const DRY_RUN_RESULT: &str = "No request was recorded.";
/// The result line when every target was already removed.
pub const NOOP_RESULT: &str = "The selected content was already removed. No request was recorded.";
/// The last line of a recorded request.
pub const PROPOSE_NEXT: &str = "Propose the view to ask for approval.";

/// A redaction result as text, and whether its row sections were omitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportText {
    pub text: String,
    pub truncated: bool,
}

/// `E_BAD_INPUT` for a budget outside 256 to 20000 tokens.
pub fn check_budget(max_tokens: usize) -> Result<(), RdyError> {
    if !(MIN_TOKENS..=MAX_TOKENS).contains(&max_tokens) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "max_tokens must be from 256 to 20000.",
            "Choose a budget in that range.",
        ));
    }
    Ok(())
}

/// The wire name of an impact kind.
pub fn kind_name(k: ImpactKind) -> &'static str {
    match k {
        ImpactKind::BlockVersion => "block_version",
        ImpactKind::Change => "change",
        ImpactKind::Landing => "landing",
        ImpactKind::Tag => "tag",
        ImpactKind::View => "view",
        ImpactKind::Proposal => "proposal",
        ImpactKind::StateSnapshot => "state_snapshot",
        ImpactKind::AuditText => "audit_text",
        ImpactKind::Review => "review",
        ImpactKind::Tension => "tension",
        ImpactKind::Answer => "answer",
    }
}

/// The wire name of an impact relation.
pub fn relation_name(r: ImpactRelation) -> &'static str {
    match r {
        ImpactRelation::Removed => "removed",
        ImpactRelation::Embeds => "embeds",
        ImpactRelation::References => "references",
        ImpactRelation::Quotes => "quotes",
    }
}

/// The wire name of a copy kind.
pub fn copy_name(k: CopyKind) -> &'static str {
    match k {
        CopyKind::Render => "render",
        CopyKind::Export => "export",
        CopyKind::Dump => "dump",
        CopyKind::Clone => "clone",
        CopyKind::Published => "published",
        CopyKind::WorkingCopy => "working_copy",
    }
}

fn json(text: &str) -> String {
    serde_json::to_string(text).expect("a string serializes")
}

/// The result lines: a recorded request, a dry run, or a request whose targets were all removed
/// already.
fn result_lines(r: &RedactResult) -> Vec<String> {
    match (r.dry_run, r.redaction, r.change) {
        (true, _, _) => vec![DRY_RUN_RESULT.to_string()],
        (false, Some(redaction), Some(change)) => vec![
            format!("Recorded redaction {redaction} in change {change}."),
            String::new(),
            PROPOSE_NEXT.to_string(),
        ],
        _ => vec![NOOP_RESULT.to_string()],
    }
}

/// `result` as text within `max_tokens` (256 to 20000; a token is 4 UTF-8 bytes, rounded down).
/// When the whole report does not fit, its row sections are replaced by one line and `truncated`
/// is set; when even that does not fit, `E_REDACTION_BUDGET`. No warning is ever dropped.
pub fn report_text(result: &RedactResult, max_tokens: usize) -> Result<ReportText, RdyError> {
    check_budget(max_tokens)?;
    let rep = &result.report;
    let mut head = vec![
        if result.dry_run {
            DRY_RUN_TITLE
        } else {
            REQUEST_TITLE
        }
        .to_string(),
    ];
    head.extend(
        rep.targets
            .iter()
            .map(|t| format!("Target: {}", selector_text(t))),
    );
    head.push(format!(
        "Versions: {}; blobs: {}; shared blobs: {}.",
        rep.versions.len(),
        rep.blobs.len(),
        rep.shared_blobs.len()
    ));
    head.push(format!(
        "Payloads: {}; bytes: {}.",
        rep.drop_frames, rep.drop_bytes
    ));
    let mut rows: Vec<String> = vec![];
    rows.extend(rep.inside.iter().map(|r| {
        format!(
            "{}: {} ({})",
            kind_name(r.kind),
            r.record,
            relation_name(r.relation)
        )
    }));
    rows.extend(rep.quotes.iter().map(|q| {
        format!(
            "{}: {}: {} (erasable: {}).",
            kind_name(q.kind),
            q.record,
            json(&q.quote),
            q.erasable
        )
    }));
    rows.extend(
        rep.outside
            .iter()
            .map(|o| format!("{}: location unknown.", copy_name(o.kind))),
    );
    rows.extend(
        rep.quarantine
            .iter()
            .map(|n| format!("Quarantine: {} (possible copy; not erased).", json(n))),
    );
    let mut tail = vec![
        ASSESSMENTS.to_string(),
        format!(
            "Reason ({}): {}",
            reason_class_name(result.reason_class),
            json(&result.reason)
        ),
    ];
    tail.extend(
        result
            .warnings
            .iter()
            .map(|w| format!("{}: {}", w.code, w.message)),
    );
    tail.extend(result_lines(result));
    let fits = |t: &str| t.len() / 4 <= max_tokens;
    let full = [head.as_slice(), rows.as_slice(), tail.as_slice()]
        .concat()
        .join("\n");
    if fits(&full) {
        return Ok(ReportText {
            text: full,
            truncated: false,
        });
    }
    let short = [
        head.as_slice(),
        &[ROWS_OMITTED.to_string()],
        tail.as_slice(),
    ]
    .concat()
    .join("\n");
    if fits(&short) {
        return Ok(ReportText {
            text: short,
            truncated: true,
        });
    }
    Err(redaction_budget())
}

/// Whether the text of `dry`, as a recorded request with the same report, fits `max_tokens`: the
/// check a caller makes before it records, so a budget refusal never follows a write. A recorded
/// result's text differs from the dry run's only in its title and result lines, whose hashes have
/// a fixed length.
pub fn check_recorded_fits(dry: &RedactResult, max_tokens: usize) -> Result<(), RdyError> {
    let recorded = dry.report.targets.len() > dry.report.already_redacted.len();
    let zero = rdy_model::Hash([0; 32]);
    let probe = RedactResult {
        dry_run: false,
        change: recorded.then_some(zero),
        redaction: recorded.then_some(zero),
        ..dry.clone()
    };
    report_text(&probe, max_tokens).map(drop)
}
