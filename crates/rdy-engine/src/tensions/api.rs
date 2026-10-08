//! The tensions requests (plan M2d2 Task 13; mten1). The one dispatcher behind MCP's
//! `rdy_tensions` and `rdy_resolve` and the ctl `tensions` and `resolve` ops, as `view_api` is
//! for views. A listing reads one snapshot; a dismissal runs on the writer.
use super::{SideRef, Tension, TensionKind, TensionSeverity, TensionStatus};
use crate::brief::BRIEF_DEFAULT_TOKENS;
use crate::engine::{Engine, write_typed};
use crate::error::*;
use crate::gate::open_proposals;
use crate::history::role_str;
use crate::session::{now_ms, rfc3339};
use crate::views::no_view;
use crate::{RdyError, ReadState, Scope, Session};
use rdy_model::{BlockId, Hash};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

/// `rdy_schema`'s paragraph on tensions.
pub const TENSIONS_DOC: &str = "rdy_tensions {scope?, kind?, severity?, status?, view?, area?, involving?, reviews?, max_tokens?} lists tensions: disagreements between in-flight changes, between a change and canon, or inside canon. Kinds: edit_collision, fork, premise_shift, area_overlap, duplicate_definition, unjustified_change, deprecated_reference. scope: inflight (default, every tension), canon (tensions whose sides are all canon) or a view. status: open (default) or dismissed. A side is a change id or canon@<landing>; intent overlaps (read_time) have an intent side. A fork, duplicate definition or deprecated reference (from content that is not itself deprecated) stays open in canon after its last change lands, until its trigger stops; the other kinds end when their last change lands (a stale pin in canon is a suspect, not a tension). Members of one alternative set never raise edit_collision with each other. Normative text duplicates need at least duplicate_text_min_words words (a pack setting, default 3). A blocking tension refuses a landing only when a side is in the proposal and every side would be in canon after it (E_BLOCKING_TENSION); a tension whose sides are all canon already never refuses. Resolve a tension with a change (edit or repin a side); dismiss one with rdy_resolve {tension, dismiss: {reason}}: anyone who writes may dismiss an advisory one; a blocking one needs the reviewer role or a human, of another actor than every side's author (E_PERMISSION, E_SELF_REVIEW). A dismissal stays with the tension (status dismissed) until a side's content changes.";

/// One tensions request: which tensions to list. Every filter given must match.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TensionsRequest {
    /// `inflight` (default), `canon` or a view name.
    pub scope: Option<String>,
    /// A kind's wire name.
    pub kind: Option<String>,
    /// `blocking` or `advisory`.
    pub severity: Option<String>,
    /// `open` (default) or `dismissed`.
    pub status: Option<String>,
    /// Only tensions with a change side in this view's included set.
    pub view: Option<String>,
    /// Only tensions that involve a block in this pack area.
    pub area: Option<String>,
    /// A block (id, key or path) or a change (hex prefix, at least 6 digits).
    pub involving: Option<String>,
    /// Also list every dismissal review.
    pub reviews: Option<bool>,
    /// Cut the answer to about this many tokens.
    pub max_tokens: Option<usize>,
}

/// One resolve request: the tension, and the dismissal (the only way to resolve one here).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveRequest {
    /// `tn-<hex>` or a unique prefix of at least 6 hex digits.
    pub tension: String,
    pub dismiss: Option<DismissRequest>,
}

/// Why a tension needs no action.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DismissRequest {
    pub reason: String,
}

fn bad(message: String, fix: &str) -> RdyError {
    RdyError::new(E_BAD_INPUT, message, fix)
}

/// What a listing keeps, resolved against the state.
struct Filter {
    /// Only tensions whose sides are all canon.
    canon: bool,
    kind: Option<TensionKind>,
    severity: Option<TensionSeverity>,
    dismissed: bool,
    view: Option<String>,
    area: Option<String>,
    involving: Option<Involving>,
}

/// `involving`, resolved: a block, or a change.
enum Involving {
    Block(BlockId),
    Change(Hash),
}

fn parse_filter(st: &ReadState, req: &TensionsRequest) -> Result<Filter, RdyError> {
    let kind = req
        .kind
        .as_deref()
        .map(|k| {
            k.parse::<TensionKind>()
                .map_err(|e| bad(e.to_string(), "pass one of the listed kinds"))
        })
        .transpose()?;
    let severity = match req.severity.as_deref() {
        None => None,
        Some("blocking") => Some(TensionSeverity::Blocking),
        Some("advisory") => Some(TensionSeverity::Advisory),
        Some(other) => {
            return Err(bad(
                format!("severity {other:?} is not blocking or advisory"),
                "pass severity blocking or advisory",
            ));
        }
    };
    let dismissed = match req.status.as_deref() {
        None | Some("open") => false,
        Some("dismissed") => true,
        Some(other) => {
            return Err(bad(
                format!("status {other} is not open or dismissed"),
                "pass status open or dismissed; resolved tensions are not kept (a resolution is an inbox event)",
            ));
        }
    };
    let named_view = |v: &str| -> Result<String, RdyError> {
        let v = v.strip_prefix("view:").unwrap_or(v);
        if st.has_view(v) {
            Ok(v.to_string())
        } else {
            Err(no_view(v))
        }
    };
    let mut view = req.view.as_deref().map(named_view).transpose()?;
    let canon = match req.scope.as_deref() {
        None | Some("inflight") => false,
        Some("canon") => true,
        Some(v) => {
            let v = named_view(v)?;
            match &view {
                Some(w) if *w != v => {
                    return Err(bad(
                        format!("scope {v} and view {w} name different views"),
                        "pass one of them: a scope that is a view is the same as view",
                    ));
                }
                _ => view = Some(v),
            }
            false
        }
    };
    let area = match req.area.as_deref() {
        None => None,
        Some(a) if st.pack().areas.contains_key(a) => Some(a.to_string()),
        Some(a) => {
            let names: Vec<&str> = st.pack().areas.keys().map(String::as_str).collect();
            return Err(bad(
                format!("no area {a:?}"),
                &if names.is_empty() {
                    "the pack declares no areas".to_string()
                } else {
                    format!("pass area one of: {}", names.join(", "))
                },
            ));
        }
    };
    let involving = req
        .involving
        .as_deref()
        .map(|t| resolve_involving(st, t))
        .transpose()?;
    Ok(Filter {
        canon,
        kind,
        severity,
        dismissed,
        view,
        area,
        involving,
    })
}

/// A change hash, from `text`, a hex prefix of at least 6 digits, among the changes tensions
/// name or that are in flight. `None` when `text` is not hex or matches none.
fn change_prefix(st: &ReadState, text: &str) -> Result<Option<Hash>, RdyError> {
    let hex = text.to_ascii_lowercase();
    if !(6..=64).contains(&hex.len()) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let mut changes: BTreeSet<Hash> = st.inflight_set()?.changes.iter().copied().collect();
    for t in st.tensions()?.iter() {
        changes.extend(t.sides.iter().filter_map(|s| match s {
            SideRef::Change(c) => Some(*c),
            _ => None,
        }));
    }
    let found: Vec<Hash> = changes
        .into_iter()
        .filter(|c| c.to_string().starts_with(&hex))
        .collect();
    match found.as_slice() {
        [] => Ok(None),
        [c] => Ok(Some(*c)),
        many => Err(RdyError::new(
            E_NOT_FOUND,
            format!("change {text} is ambiguous ({} changes)", many.len()),
            format!(
                "use more characters: it matches {}",
                many.iter()
                    .take(5)
                    .map(Hash::short)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

fn resolve_involving(st: &ReadState, text: &str) -> Result<Involving, RdyError> {
    if let Ok(b) = BlockId::parse(text) {
        return Ok(Involving::Block(b));
    }
    if let Some(c) = change_prefix(st, text)? {
        return Ok(Involving::Change(c));
    }
    Ok(Involving::Block(st.resolve_id(&Scope::Inflight, text)?))
}

impl Filter {
    /// Whether `t` passes every filter but the status.
    fn keeps(
        &self,
        st: &ReadState,
        t: &Tension,
        changes_of_view: &BTreeSet<Hash>,
    ) -> Result<bool, RdyError> {
        if self.kind.is_some_and(|k| k != t.kind) || self.severity.is_some_and(|s| s != t.severity)
        {
            return Ok(false);
        }
        if self.canon && !t.sides.iter().all(|s| matches!(s, SideRef::Canon { .. })) {
            return Ok(false);
        }
        if self.view.is_some()
            && !t
                .sides
                .iter()
                .any(|s| matches!(s, SideRef::Change(c) if changes_of_view.contains(c)))
        {
            return Ok(false);
        }
        match &self.involving {
            Some(Involving::Block(b)) if !t.blocks().contains(b) => return Ok(false),
            Some(Involving::Change(c)) if !t.sides.contains(&SideRef::Change(*c)) => {
                return Ok(false);
            }
            _ => {}
        }
        if let Some(area) = &self.area {
            let mut hit = false;
            for b in t.blocks() {
                if block_areas(st, &b)?.iter().any(|a| a == area) {
                    hit = true;
                    break;
                }
            }
            if !hit {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// The areas of `b` in flight, else in canon; none for a block that neither holds.
fn block_areas(st: &ReadState, b: &BlockId) -> Result<Vec<String>, RdyError> {
    for scope in [Scope::Inflight, Scope::Canon] {
        match st.areas_of(&scope, b) {
            Err(e) if e.code == E_UNKNOWN_BLOCK => continue,
            r => return r,
        }
    }
    Ok(vec![])
}

/// Every tension a listing can show: the event-derived set and the read-time overlaps as of
/// now, once each, by id.
fn candidates(st: &ReadState) -> Result<Vec<Arc<Tension>>, RdyError> {
    let mut all: std::collections::BTreeMap<Hash, Arc<Tension>> = st
        .tensions()?
        .iter()
        .map(|t| (t.id, Arc::clone(t)))
        .collect();
    for t in st.intent_tensions(now_ms())? {
        all.entry(t.id).or_insert_with(|| Arc::new(t));
    }
    Ok(all.into_values().collect())
}

/// The vkeys of every tension that exists now: the listed ones and the landing form of every
/// open proposal. A dismissal whose vkey is in it still holds.
fn live_vkeys(st: &ReadState, listed: &[Arc<Tension>]) -> Result<BTreeSet<Hash>, RdyError> {
    let mut out: BTreeSet<Hash> = listed.iter().map(|t| t.vkey).collect();
    for (_, p) in open_proposals(st)? {
        out.extend(
            st.landing_tensions(&p.view, &p.changes)?
                .iter()
                .map(|t| t.vkey),
        );
    }
    Ok(out)
}

/// Every dismissal review, by time then hash: the review, the tension it dismissed as it was,
/// who and when, and whether it still holds.
fn review_rows(st: &ReadState, listed: &[Arc<Tension>]) -> Result<Vec<Value>, RdyError> {
    let live = live_vkeys(st, listed)?;
    let mut rows = vec![];
    for (vkey, review) in st.dismissals.iter() {
        let r = st.reader().review(review)?;
        let rdy_model::ReviewSubject::Tension {
            id,
            kind,
            severity,
            sides,
            blocks,
            ..
        } = &r.subject;
        let p = &r.provenance;
        rows.push((
            p.at_ms,
            *review,
            json!({
                "review": review.to_string(),
                "tension": id.to_string(),
                "kind": kind,
                "severity": severity,
                "sides": sides,
                "blocks": blocks.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "reason": r.reason,
                "findings": r.findings.iter().map(|f| json!({"block": f.block.to_string(), "note": f.note})).collect::<Vec<_>>(),
                "actor": p.actor,
                "run": p.run,
                "role": role_str(p.role),
                "at": rfc3339(p.at_ms),
                "at_ms": p.at_ms,
                "case_hash": r.case_hash.map(|h| h.to_string()),
                "holds": live.contains(vkey),
            }),
        ));
    }
    rows.sort_by_key(|r| (r.0, r.1));
    Ok(rows.into_iter().map(|(_, _, v)| v).collect())
}

/// Lists the tensions `req` selects, blocking first, then by kind, then by id, cut from the end
/// to `max_tokens` (default `BRIEF_DEFAULT_TOKENS`; four bytes a token) with `omitted`. The
/// reply is `{tensions, counts: {open, blocking_open, dismissed}, omitted?, reviews?}`; the
/// counts cover the selection but for the status.
pub fn run_tensions(
    engine: &dyn Engine,
    _s: &Session,
    req: TensionsRequest,
) -> Result<Value, RdyError> {
    let st = engine.read();
    let f = parse_filter(&st, &req)?;
    let in_view: BTreeSet<Hash> = f
        .view
        .as_deref()
        .map(|v| st.included(v).iter().copied().collect())
        .unwrap_or_default();
    let listed = candidates(&st)?;
    let mut kept = vec![];
    for t in &listed {
        if f.keeps(&st, t, &in_view)? {
            kept.push(Arc::clone(t));
        }
    }
    kept.sort_by_key(|t| (t.severity, t.kind, t.id));
    let (mut open, mut blocking_open, mut dismissed) = (0, 0, 0);
    let mut infos = vec![];
    for t in &kept {
        match st.tension_status(t) {
            TensionStatus::Open => {
                open += 1;
                if t.severity == TensionSeverity::Blocking {
                    blocking_open += 1;
                }
            }
            TensionStatus::Dismissed(_) => dismissed += 1,
        }
        if (st.tension_status(t) != TensionStatus::Open) == f.dismissed {
            infos.push(st.tension_info(t));
        }
    }
    let mut reviews = if req.reviews == Some(true) {
        Some(review_rows(&st, &listed)?)
    } else {
        None
    };
    let mut out = json!({
        "tensions": infos,
        "counts": {"open": open, "blocking_open": blocking_open, "dismissed": dismissed},
    });
    if let Some(rows) = &reviews {
        out["reviews"] = json!(rows);
    }
    let budget = req
        .max_tokens
        .unwrap_or(BRIEF_DEFAULT_TOKENS)
        .saturating_mul(4);
    let size = |v: &Value| v.to_string().len();
    let (mut omitted, mut reviews_omitted) = (0usize, 0usize);
    while size(&out) > budget {
        if let Some(list) = out["tensions"].as_array_mut()
            && list.pop().is_some()
        {
            omitted += 1;
        } else if let Some(rows) = reviews.as_mut()
            && rows.pop().is_some()
        {
            reviews_omitted += 1;
            out["reviews"] = json!(rows);
        } else {
            break;
        }
    }
    if omitted > 0 {
        out["omitted"] = json!(omitted);
    }
    if reviews_omitted > 0 {
        out["reviews_omitted"] = json!(reviews_omitted);
    }
    Ok(out)
}

/// Dismisses the tension `req` names with its reason: `{id, tension, kind, severity, review,
/// status}`. Without `dismiss` it is `E_BAD_INPUT`: a tension is resolved by a change.
pub fn run_resolve(
    engine: &dyn Engine,
    s: &Session,
    req: ResolveRequest,
) -> Result<Value, RdyError> {
    let Some(d) = req.dismiss else {
        return Err(bad(
            "a tension is resolved by a change; pass dismiss: {reason} to dismiss it".to_string(),
            "edit or repin a side of the tension, or pass dismiss: {reason}",
        ));
    };
    let s = s.clone();
    write_typed(engine, move |r| {
        r.dismiss_tension(&s, &req.tension, &d.reason)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mten1_the_tensions_doc_names_every_kind() {
        for k in rdy_schema::TensionKind::ALL {
            assert!(TENSIONS_DOC.contains(k.name()), "{k}");
        }
    }
}
