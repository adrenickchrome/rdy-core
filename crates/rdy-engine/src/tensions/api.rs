//! The tensions requests (plan M2d2 Task 13; mten1). The one dispatcher behind MCP's
//! `rdy_tensions` and `rdy_resolve` and the ctl `tensions` and `resolve` ops, as `view_api` is
//! for views. A listing reads one snapshot; a dismissal runs on the writer.
use super::info::InfoCache;
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
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// `rdy_schema`'s paragraph on tensions.
pub const TENSIONS_DOC: &str = "rdy_tensions {scope?, kind?, severity?, status?, view?, area?, involving?, reviews?, max_tokens?} lists tensions: disagreements between in-flight changes, between a change and canon, or inside canon. Kinds: edit_collision, fork, premise_shift, area_overlap, duplicate_definition, unjustified_change, deprecated_reference, value_disagreement. scope: inflight (default, every tension), canon (tensions whose sides are all canon) or a view. status: open (default) or dismissed. A side is a change id or canon@<landing>; intent overlaps (read_time) have an intent side and are resolved through their intent, never dismissed. The listing also shows the landing form of an open proposal (proposal: P-x@n) when it differs from the in-flight one: the gate judges that form, and a refusal names it. A fork, duplicate definition, deprecated reference (from content that is not itself deprecated) or value disagreement stays open in canon after its last change lands, until its trigger stops; the other kinds end when their last change lands (a stale pin in canon is a suspect, not a tension). Members of one alternative set never raise edit_collision with each other. A duplicate_definition is one tension per group: every block that defines one name, or every normative block with one text, is a member. An unjustified_change is one tension per block that a view changes: a stable normative block with no implements or editorial on any change of the view that writes it. Its sides are the view's latest changes on the block; creating a decision, or editing one still proposed, never counts. A proposal and its landing judge the proposal's net change, and a listing scoped to a view judges the view's net change. A value_disagreement is one tension per pair of a block and a value it describes (by a reference, a describes link or an affects link) and states a different number in the value's unit: the value needs a unit and a decimal, a stated number counts when the unit text follows it directly or after one space, and there are no unit aliases and no conversion. It blocks in stable content and is advisory in draft. Normative text duplicates need at least duplicate_text_min_words words (a pack setting, default 3). A blocking tension refuses a landing only when a side is in the proposal and every side would be in canon after it (E_BLOCKING_TENSION); a tension whose sides are all canon already never refuses. Resolve a tension with a change (edit or repin a side); dismiss one with rdy_resolve {tension, dismiss: {reason}}: anyone who writes may dismiss an advisory one; a blocking one needs the reviewer role or a human, of another actor than every side's author (E_PERMISSION, E_SELF_REVIEW). A dismissal stays with the tension (status dismissed) until a side's content changes; for a duplicate_definition, also until the group's membership changes (a member added, edited away or removed).";

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
    /// Only tensions with a change side in this view's included set. A kind judged on the net
    /// change (unjustified_change) is listed in the landing form of the whole view, as the
    /// view's proposal lists it.
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

/// One tension a listing can show, with the open proposal whose landing form it is (for a form
/// only a landing gives).
struct Candidate {
    t: Arc<Tension>,
    proposal: Option<String>,
}

/// Every tension a listing can show, once each, by id: the event-derived set, the read-time
/// overlaps as of now, and the landing forms of every open proposal that the set does not hold
/// (tgat1: the gate refuses with a landing form, so the listing a refusal names must show it).
fn candidates(st: &ReadState) -> Result<Vec<Candidate>, RdyError> {
    let mut all: BTreeMap<Hash, Candidate> = st
        .tensions()?
        .iter()
        .map(|t| {
            (
                t.id,
                Candidate {
                    t: Arc::clone(t),
                    proposal: None,
                },
            )
        })
        .collect();
    for t in st.intent_tensions(now_ms())? {
        all.entry(t.id).or_insert_with(|| Candidate {
            t: Arc::new(t),
            proposal: None,
        });
    }
    for (pref, p) in open_proposals(st)? {
        for t in st.landing_tensions(&p.view, &p.changes)? {
            all.entry(t.id).or_insert_with(|| Candidate {
                t,
                proposal: Some(pref.to_string()),
            });
        }
    }
    Ok(all.into_values().collect())
}

/// Every dismissal review, by time then hash: the review, the tension it dismissed as it was,
/// who and when, and whether it still holds (its vkey is that of a tension that exists now:
/// `live`, every candidate of the listing, landing forms included).
fn review_rows(st: &ReadState, live: &BTreeSet<Hash>) -> Result<Vec<Value>, RdyError> {
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
        } = &r.subject
        else {
            continue;
        };
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

/// How many of `sizes` (each row's JSON length) fit `room` bytes as a JSON array's elements,
/// counted from the front: each row after the first adds a comma.
fn fitting(sizes: &[usize], room: usize) -> usize {
    let mut used = 0usize;
    for (i, n) in sizes.iter().enumerate() {
        used += n + usize::from(i > 0);
        if used > room {
            return i;
        }
    }
    sizes.len()
}

/// The bytes `sizes` take as a JSON array's elements.
fn array_bytes(sizes: &[usize]) -> usize {
    sizes.iter().sum::<usize>() + sizes.len().saturating_sub(1)
}

/// The cut of a listing (checklist 45: each row is measured once, so the cost follows the
/// rows): how many tensions and reviews to keep, from the front, so that the reply fits
/// `budget` bytes. `frame` is the reply with both lists empty. Tensions go first, from the end;
/// reviews go only when no tension is left. Each `…omitted` count a cut adds is counted in.
fn cut(frame: usize, tensions: &[usize], reviews: &[usize], budget: usize) -> (usize, usize) {
    let (t, r) = (array_bytes(tensions), array_bytes(reviews));
    if frame + t + r <= budget {
        return (tensions.len(), reviews.len());
    }
    // a count's key at its widest
    let key = |name: &str, n: usize| {
        if n == 0 {
            0
        } else {
            format!(",\"{name}\":{n}").len()
        }
    };
    let frame = frame + key("omitted", tensions.len());
    let keep_t = fitting(tensions, budget.saturating_sub(frame + r));
    if keep_t > 0 || frame + r <= budget {
        return (keep_t, reviews.len());
    }
    let frame = frame + key("reviews_omitted", reviews.len());
    (0, fitting(reviews, budget.saturating_sub(frame)))
}

/// Lists the tensions `req` selects, blocking first, then by kind, then by id, cut from the end
/// to `max_tokens` (default `BRIEF_DEFAULT_TOKENS`; four bytes a token) with `omitted`: the
/// tensions first, then the reviews, each row measured once. The reply is `{tensions, counts:
/// {open, blocking_open, dismissed}, omitted?, reviews?, reviews_omitted?}`; the counts cover
/// the selection but for the status. A landing-only form carries its `proposal`.
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
    // FW-JU1: a view listing judges a net-change kind on the view's own net change, as the view's
    // proposal does: its landing form, never another view's in-flight form that the view holds
    let (own, extra) = match f.view.as_deref() {
        Some(v) => {
            let net = st.view_net_tensions(v)?;
            let own: BTreeSet<Hash> = net.iter().map(|t| t.id).collect();
            let ids: BTreeSet<Hash> = listed.iter().map(|c| c.t.id).collect();
            let extra: Vec<Candidate> = net
                .into_iter()
                .filter(|t| !ids.contains(&t.id))
                .map(|t| Candidate { t, proposal: None })
                .collect();
            (Some(own), extra)
        }
        None => (None, vec![]),
    };
    let mut kept = vec![];
    for c in listed.iter().chain(&extra) {
        if let Some(own) = &own
            && super::judged_on_the_net_change(c.t.kind)
            && !own.contains(&c.t.id)
        {
            continue;
        }
        if f.keeps(&st, &c.t, &in_view)? {
            kept.push(c);
        }
    }
    kept.sort_by_key(|c| (c.t.severity, c.t.kind, c.t.id));
    let budget = req
        .max_tokens
        .unwrap_or(BRIEF_DEFAULT_TOKENS)
        .saturating_mul(4);
    let size = |v: &Value| v.to_string().len();
    let (mut open, mut blocking_open, mut dismissed) = (0, 0, 0);
    // the rows listed, built (each measured once) only while they fit the budget alone: a row
    // past it is never built, and counts at `budget + 1` bytes, which no cut keeps
    let (mut infos, mut sizes, mut used) = (vec![], vec![], 0usize);
    let mut cache = InfoCache::default();
    for c in &kept {
        let t = &c.t;
        let status = st.tension_status(t);
        match status {
            TensionStatus::Open => {
                open += 1;
                if t.severity == TensionSeverity::Blocking {
                    blocking_open += 1;
                }
            }
            TensionStatus::Dismissed(_) => dismissed += 1,
        }
        if (status != TensionStatus::Open) == f.dismissed {
            if used > budget {
                sizes.push(budget.saturating_add(1));
                continue;
            }
            let mut info = st.tension_info_with(t, &mut cache);
            info.proposal = c.proposal.clone();
            let v = serde_json::to_value(&info).expect("a tension info serializes");
            let n = size(&v);
            used = used.saturating_add(n + 1);
            sizes.push(n);
            infos.push(v);
        }
    }
    let reviews = if req.reviews == Some(true) {
        let live: BTreeSet<Hash> = listed.iter().map(|c| c.t.vkey).collect();
        Some(review_rows(&st, &live)?)
    } else {
        None
    };
    let mut out = json!({
        "tensions": [],
        "counts": {"open": open, "blocking_open": blocking_open, "dismissed": dismissed},
    });
    if reviews.is_some() {
        out["reviews"] = json!([]);
    }
    let rows = reviews.unwrap_or_default();
    let (keep_t, keep_r) = cut(
        size(&out),
        &sizes,
        &rows.iter().map(size).collect::<Vec<_>>(),
        budget,
    );
    let (omitted, reviews_omitted) = (sizes.len() - keep_t, rows.len() - keep_r);
    infos.truncate(keep_t);
    out["tensions"] = Value::Array(infos);
    if req.reviews == Some(true) {
        let mut rows = rows;
        rows.truncate(keep_r);
        out["reviews"] = Value::Array(rows);
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

    /// A row-at-a-time cut: drop the last tension (else the last review) while the reply, with
    /// each omitted count at its widest, is over budget.
    fn naive(frame: usize, t: &[usize], r: &[usize], budget: usize) -> (usize, usize) {
        let (mut kt, mut kr) = (t.len(), r.len());
        let total = |kt: usize, kr: usize| {
            let key = |name: &str, kept: usize, all: usize| {
                if kept == all {
                    0
                } else {
                    format!(",\"{name}\":{all}").len()
                }
            };
            frame
                + array_bytes(&t[..kt])
                + array_bytes(&r[..kr])
                + key("omitted", kt, t.len())
                + key("reviews_omitted", kr, r.len())
        };
        while total(kt, kr) > budget {
            if kt > 0 {
                kt -= 1;
            } else if kr > 0 {
                kr -= 1;
            } else {
                break;
            }
        }
        (kt, kr)
    }

    #[test]
    fn mten1_the_cut_keeps_what_a_row_at_a_time_cut_keeps() {
        let t: Vec<usize> = (0..300).map(|i| 200 + (i * 37) % 150).collect();
        let r: Vec<usize> = (0..40).map(|i| 300 + (i * 53) % 200).collect();
        for budget in (0..120_000).step_by(997) {
            for (tt, rr) in [(&t[..], &r[..]), (&t[..], &[][..]), (&[][..], &r[..])] {
                assert_eq!(
                    cut(60, tt, rr, budget),
                    naive(60, tt, rr, budget),
                    "budget {budget}"
                );
            }
        }
    }

    /// FW-TN2: the doc says a duplicate is one tension per group and that a membership change
    /// voids a dismissal.
    #[test]
    fn mten1_the_tensions_doc_says_a_duplicate_is_one_tension_per_group() {
        assert!(TENSIONS_DOC.contains("A duplicate_definition is one tension per group"));
        assert!(TENSIONS_DOC.contains("also until the group's membership changes"));
    }

    #[test]
    fn mten1_the_tensions_doc_names_every_kind() {
        for k in rdy_schema::TensionKind::ALL {
            assert!(TENSIONS_DOC.contains(k.name()), "{k}");
        }
    }

    /// FW-JU1: the doc says an unjustified change is judged per view, on the net change.
    #[test]
    fn msw21_the_tensions_doc_says_an_unjustified_change_is_judged_on_the_net_change() {
        assert!(
            TENSIONS_DOC
                .contains("An unjustified_change is one tension per block that a view changes")
        );
        assert!(
            TENSIONS_DOC
                .contains("creating a decision, or editing one still proposed, never counts")
        );
        assert!(
            TENSIONS_DOC.contains("A proposal and its landing judge the proposal's net change")
        );
    }
}
