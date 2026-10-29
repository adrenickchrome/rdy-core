//! Change-plan reads and answers (D-4JMPH; reference/mcp-tools p-92F5M, p-CSBA7, p-JJNYH,
//! p-PZX3T). A read recomputes the items from a named version and recorded HEADs
//! ([`compute::compute`]); a landing's read returns its seal as recorded. An answer request
//! writes one cumulative, answer-only plan version and its ref (`Store::put_plan`), and changes
//! no block, change, proposal or canon ref. Reads write nothing: no plan, ref or cache file.
//!
//! # The chain a view reads
//!
//! `plan/<view>` names the view's latest plan version; a published state captures every such
//! ref. Its chain is live unless that version is sealed, or the proposal version the chain last
//! bound (also behind answer-only versions) has landed without a seal (O6): then the view's next
//! answer starts a new chain. A read counts only a landing before its state's sequence. A live
//! chain's answers are the view's answers, and its first version's HEADs are the baseline of its
//! code items. With no live chain, the HEADs captured for the read are both ends; a read alone
//! never fixes a baseline.
use super::compute::{self, ComputedPlan, PlanCandidate, allowed_acts};
use super::group::{self, PlanGroup};
use super::page;
use crate::brief::is_lower_hex;
use crate::code_index::{check_top, code_repos, head_of};
use crate::consequence::Consequences;
use crate::error::{E_BAD_INPUT, E_LIMIT, E_PERMISSION, E_PLAN_ITEM};
use crate::history::{Author, role_str};
use crate::{Delta, OpInput, Rdy, RdyError, ReadState, Scope, Session, WriteRequest};
use rdy_model::engine_fields::MAX_REASON_CHARS;
use rdy_model::plan::{
    ITEM_DISPLAY_PREFIX, MAX_PLAN_ANSWERS, Plan, PlanAct, PlanAnswer, PlanHead, PlanKind,
    PlanOutcome, SealedItem, item_display,
};
use rdy_model::{BlockId, Hash, Proposal, Provenance};
use rdy_store::plan::PLAN_REF;
use rdy_store::{StoreReader, kinds};
use serde::{Deserialize, Serialize};
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

/// A recorded answer whose subject or cause changed after it (p-5TZ28).
pub const W_PLAN_ANSWER_LAPSED: &str = "W_PLAN_ANSWER_LAPSED";
/// The fix of [`W_PLAN_ANSWER_LAPSED`].
pub const PLAN_ANSWER_LAPSED_FIX: &str = "answer the item again";
/// The fix of `E_PLAN_ITEM` (p-EVM5J).
pub const PLAN_ITEM_FIX: &str =
    "read the plan with rdy_plan / rdy plan and pass an item id it lists";
/// The budget of a plan read when none is given, in tokens (the briefing's).
pub const PLAN_DEFAULT_TOKENS: usize = crate::brief::BRIEF_DEFAULT_TOKENS;
/// The largest budget a plan read takes, in tokens.
pub const PLAN_MAX_TOKENS: usize = 100_000;

/// The kinds, in their wire order (p-S33GZ); the tension item is last (D-ME2HD).
const KINDS: [PlanKind; 6] = [
    PlanKind::Update,
    PlanKind::Rejustify,
    PlanKind::Reaffirm,
    PlanKind::Confirm,
    PlanKind::UpdateImplementation,
    PlanKind::Tension,
];

/// The eleven outcomes, in their wire order; `dismissed` is last (D-ME2HD).
const OUTCOMES: [PlanOutcome; 11] = [
    PlanOutcome::Updated,
    PlanOutcome::Relinked,
    PlanOutcome::Repinned,
    PlanOutcome::Reaffirmed,
    PlanOutcome::Retired,
    PlanOutcome::Committed,
    PlanOutcome::Confirmed,
    PlanOutcome::Done,
    PlanOutcome::Deferred,
    PlanOutcome::Open,
    PlanOutcome::Dismissed,
];

/// The prefix of a tension item's display id: the tension's own id (O5).
pub(crate) const TENSION_DISPLAY_PREFIX: &str = "tn-";

/// The display id of an item: a tension item shows its tension's id, `tn-` and the first 12 hex
/// of its subject (O5, `tension_label`); every other item `pi-` and 16 hex ([`item_display`]).
fn item_label(kind: PlanKind, id: &Hash, subject: &str) -> String {
    match kind {
        PlanKind::Tension => format!(
            "{TENSION_DISPLAY_PREFIX}{}",
            subject.get(..12).unwrap_or(subject)
        ),
        _ => item_display(id),
    }
}

/// One answer to a plan item, as `rdy_plan` and `rdy_propose` take it (p-JJNYH): `item` is a
/// `pi-` id (a prefix of one), a tension item's `tn-` id or a `cq-` consequence id.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanAnswerInput {
    pub item: String,
    pub act: PlanAct,
    #[serde(default)]
    pub reason: Option<String>,
}

/// One answer to a plan, as `rdy_plan` and `rdy plan` take it on the wire (D-ME2HD, O13, O14):
/// it names one `item`, as [`PlanAnswerInput`] does, or a `group` of open items, one answer for
/// each. `repin` goes with a `confirm` group only. With `repin`, the repin change takes the
/// justification of `rdy_write` (ruling final F2): `implements`, `editorial` or `reflects`, at
/// most one, checked by [`crate::parse_justification`]; a spec without `repin` takes none.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanAnswerSpec {
    #[serde(default)]
    pub item: Option<String>,
    #[serde(default)]
    pub group: Option<PlanGroup>,
    pub act: PlanAct,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub repin: bool,
    /// The decisions the repin change carries out: ids, keys or paths.
    #[serde(default)]
    pub implements: Option<Vec<String>>,
    /// The repin change is editorial: its class and reason.
    #[serde(default)]
    pub editorial: Option<EditorialArg>,
    /// The repin change follows these sources, as `rdy_write`'s `reflects`.
    #[serde(default)]
    pub reflects: Option<crate::ReflectsArg>,
}

/// An `editorial` justification as `rdy_write` takes it: a class (typo, restructure or clarify)
/// and a reason.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorialArg {
    pub class: String,
    pub reason: String,
}

impl PlanAnswerSpec {
    /// Whether the spec gives a justification of its own.
    fn justified(&self) -> bool {
        self.implements.is_some() || self.editorial.is_some() || self.reflects.is_some()
    }
}

/// The first block, in reading order, that a repin op of `ops` writes and that is normative and
/// stable, in the view state `m` or in canon: the tests the unjustified-change detector judges a
/// write with (ruling final F2, D-ME2HD; the block named, TASK-933).
fn repins_stable_content(
    st: &ReadState,
    m: &crate::Materialized,
    ops: &[OpInput],
) -> Result<Option<BlockId>, RdyError> {
    let canon = st.materialized(&Scope::Canon)?;
    let mut stable: BTreeSet<BlockId> = BTreeSet::new();
    for op in ops {
        let OpInput::Repin { id, .. } = op else {
            continue;
        };
        let Ok(b) = BlockId::parse(id) else {
            continue;
        };
        let Some(block) = m.get(&b) else {
            continue;
        };
        if crate::tensions::index::is_normative(st.pack(), block)
            && (crate::brief::effective_stable(m, Some(&b))
                || crate::brief::effective_stable(&canon, Some(&b)))
        {
            stable.insert(b);
        }
    }
    // the first such block in reading order (TASK-933)
    if stable.is_empty() {
        return Ok(None);
    }
    Ok(crate::diff::reading_order(m)
        .into_iter()
        .find(|b| stable.contains(b)))
}

/// What a plan read names (p-92F5M): at most one of the four selectors (none: the session's
/// view), or a `page` from a cut read's `next`, alone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanQuery {
    #[serde(default)]
    pub view: Option<String>,
    #[serde(default)]
    pub proposal: Option<String>,
    #[serde(default)]
    pub landing: Option<String>,
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub page: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<usize>,
}

/// The state of a live item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    Open,
    Closed,
    Deferred,
}

/// The answer a live item shows: the latest valid one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AnswerRow {
    pub act: PlanAct,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub by: Author,
}

/// An item of a view's or a proposal version's plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LiveItem {
    /// `pi-` and 16 hex digits; a tension item's `tn-` and 12 (O5).
    pub id: String,
    pub kind: PlanKind,
    pub subject: String,
    pub cause: String,
    pub edge: String,
    pub state: ItemState,
    /// The computed close, only for an item the version closes itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub how: Option<PlanOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<AnswerRow>,
    /// Whether the subject is stable content (D-FQ7AK): what the change-plan gate reads; no
    /// reply shows it.
    #[serde(skip)]
    pub stable: bool,
}

/// An item as a landing sealed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SealedRow {
    pub id: String,
    pub kind: PlanKind,
    pub subject: String,
    pub cause: String,
    pub edge: String,
    pub outcome: PlanOutcome,
    /// The ANSWER that accepted the deferral (full hash).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

/// One seal that names a decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SealRow {
    pub landing: String,
    pub version: String,
    /// How many items it seals.
    pub items: usize,
    /// Its items per outcome, all ten named.
    pub outcomes: BTreeMap<&'static str, usize>,
}

/// One entry of `items`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum PlanRow {
    Live(LiveItem),
    Sealed(SealedRow),
    Seal(SealRow),
}

/// Items in each state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StateCounts {
    pub open: usize,
    pub closed: usize,
    pub deferred: usize,
}

/// The counts over every item (not one page): closed holds the computed closes, the confirmed,
/// the dismissed and the done items; `by_kind` names all six kinds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlanCounts {
    pub open: usize,
    pub closed: usize,
    pub deferred: usize,
    pub by_kind: BTreeMap<&'static str, StateCounts>,
}

impl Default for PlanCounts {
    fn default() -> PlanCounts {
        PlanCounts {
            open: 0,
            closed: 0,
            deferred: 0,
            by_kind: KINDS
                .iter()
                .map(|k| (k.as_str(), StateCounts::default()))
                .collect(),
        }
    }
}

impl PlanCounts {
    fn add(&mut self, kind: PlanKind, state: ItemState) {
        let k = self.by_kind.entry(kind.as_str()).or_default();
        let (all, one) = match state {
            ItemState::Open => (&mut self.open, &mut k.open),
            ItemState::Closed => (&mut self.closed, &mut k.closed),
            ItemState::Deferred => (&mut self.deferred, &mut k.deferred),
        };
        *all += 1;
        *one += 1;
    }
}

/// The code files the code items name; `unavailable` only when no checkout is linked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PlanCode {
    pub files: usize,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unavailable: bool,
}

/// A warning of a plan read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlanNote {
    pub code: String,
    pub message: String,
    pub fix: String,
}

/// The reply of a plan read (p-CSBA7, O17).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlanReply {
    /// `view:<name>`, `proposal:P-x@n`, `landing:<full hash>` or `decision:<id>`.
    pub scope: String,
    /// The plan version read (full hash), or null when nothing is recorded.
    pub plan: Option<String>,
    pub items: Vec<PlanRow>,
    pub counts: PlanCounts,
    pub code: PlanCode,
    /// Sorted by code, then item.
    pub notes: Vec<PlanNote>,
    /// Pass it as `page` to read the items after this page; only on a cut page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// What a read names, as a cursor's selector spells it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Selector {
    View(String),
    Proposal(String),
    Landing(String),
    Decision(String),
}

impl Selector {
    /// The selector of `text` (`view:<name>`, `proposal:P-x@n`, `landing:<64 hex>` or
    /// `decision:<id>`), or `None` when it is none of them.
    pub(crate) fn parse(text: &str) -> Option<Selector> {
        if let Some(v) = text.strip_prefix("view:") {
            return (!v.is_empty()).then(|| Selector::View(v.to_string()));
        }
        if let Some(p) = text.strip_prefix("proposal:") {
            return explicit_version(p).then(|| Selector::Proposal(p.to_string()));
        }
        if let Some(l) = text.strip_prefix("landing:") {
            let full = l.len() == 64 && l.bytes().all(is_lower_hex);
            return full.then(|| Selector::Landing(l.to_string()));
        }
        if let Some(d) = text.strip_prefix("decision:") {
            return BlockId::is_valid(d).then(|| Selector::Decision(d.to_string()));
        }
        None
    }
}

/// `P-`, hex digits, `@` and a version number: a proposal version named explicitly.
fn explicit_version(text: &str) -> bool {
    crate::is_proposal_ref(text) && text.contains('@')
}

/// The refusal of an expansion past the bound: `have` answers built, `add` more to come. Group
/// expansion calls it before it builds the answers, so a hostile request is an error, not an
/// allocation of specs times matched items (tb-A4DAV).
fn check_expansion(have: usize, add: usize) -> Result<(), RdyError> {
    if have.saturating_add(add) > MAX_PLAN_ANSWERS {
        return Err(answers_limit());
    }
    Ok(())
}

fn bad(message: &str, fix: &str) -> RdyError {
    RdyError::new(E_BAD_INPUT, message, fix)
}

/// The refusal of more than `MAX_PLAN_ANSWERS` answers, in one request or in one plan.
fn answers_limit() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("change plan exceeds {MAX_PLAN_ANSWERS} plan answers"),
        "split the answers into smaller plans",
    )
}

/// The input rules of a plan request with `answers` (p-92F5M, p-JJNYH, p-PZX3T), before
/// anything is read: a page goes alone (with `max_tokens`), at most one selector is given, and
/// answers go with a view only. The surfaces call it before they answer or read.
pub fn check_plan_request(q: &PlanQuery, answers: &[PlanAnswerInput]) -> Result<(), RdyError> {
    let selectors = [&q.view, &q.proposal, &q.landing, &q.decision]
        .iter()
        .filter(|s| s.is_some())
        .count();
    if q.page.is_some() && (selectors > 0 || !answers.is_empty()) {
        return Err(bad(
            "page continues a plan: pass it alone",
            "pass page and max_tokens only",
        ));
    }
    if selectors > 1 {
        return Err(bad(
            "plan takes at most one of view, proposal, landing and decision",
            "pass one selector, or omit them for the session view",
        ));
    }
    if !answers.is_empty() && (q.proposal.is_some() || q.landing.is_some() || q.decision.is_some())
    {
        return Err(bad(
            "plan answers go with view only",
            "answer the plan in its view",
        ));
    }
    if answers.len() > MAX_PLAN_ANSWERS {
        return Err(answers_limit());
    }
    Ok(())
}

/// The `max_tokens` refusal the surfaces give (the same text).
fn check_tokens(max: Option<usize>) -> Result<usize, RdyError> {
    match max {
        Some(n) if n > PLAN_MAX_TOKENS => Err(RdyError::new(
            E_LIMIT,
            format!("max_tokens {n} is over the limit of {PLAN_MAX_TOKENS}"),
            format!("ask for at most {PLAN_MAX_TOKENS} tokens and page through the rest"),
        )),
        Some(n) => Ok(n),
        None => Ok(PLAN_DEFAULT_TOKENS),
    }
}

/// The HEAD of every linked checkout, sorted by configured repo name (`None`: unborn). No linked
/// checkout gives none. A configured checkout that is not a usable repository keeps the code
/// index's error.
pub(crate) fn capture_heads(st: &ReadState) -> Result<Vec<PlanHead>, RdyError> {
    let mut repos = code_repos(st.root())?;
    repos.sort_by(|a, b| a.configured.cmp(&b.configured));
    repos.dedup_by(|a, b| a.configured == b.configured);
    repos
        .iter()
        .map(|r| {
            check_top(r)?;
            Ok(PlanHead {
                repo: r.configured.clone(),
                head: head_of(r)?,
            })
        })
        .collect()
}

/// A view's live chain: its latest version and the HEADs of its first version.
pub(crate) struct Chain {
    pub(crate) head: Hash,
    pub(crate) plan: Plan,
    pub(crate) first_heads: Vec<PlanHead>,
}

impl Chain {
    /// The chain fields of the version that follows this chain's head.
    pub(crate) fn next_links(&self) -> (Option<Hash>, Option<Hash>) {
        (Some(self.plan.stable.unwrap_or(self.head)), Some(self.head))
    }
}

thread_local! {
    /// The `plan/<view>` refs this thread's publishes have read (see `plan_ref_reads_for_tests`).
    static PLAN_REF_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Tests only: how many `plan/<view>` refs the publishes on this thread have read to capture
/// the plan heads, so a test can check that a plan write re-reads only its own views' refs.
#[doc(hidden)]
pub fn plan_ref_reads_for_tests() -> u64 {
    PLAN_REF_READS.with(std::cell::Cell::get)
}

fn count_plan_ref_reads(n: usize) {
    PLAN_REF_READS.with(|c| c.set(c.get() + n as u64));
}

/// Every `plan/<view>` ref `reader` holds now, by view: what a published state captures. The
/// full scan of an open, a reindex or another full publish.
pub(crate) fn plan_heads_in(reader: &StoreReader) -> Result<imbl::OrdMap<String, Hash>, RdyError> {
    let mut heads = imbl::OrdMap::new();
    let refs = reader.refs_with_prefix(PLAN_REF)?;
    count_plan_ref_reads(refs.len());
    for (name, v) in refs {
        let view = name[PLAN_REF.len()..].to_string();
        let head = plan_ref_head(&view, &v)?;
        heads.insert(view, head);
    }
    Ok(heads)
}

/// `prev` (the heads the state before captured) after a batch whose PLAN frames are `plans`:
/// only the plan writers move a `plan/<view>` ref, each beside a PLAN frame of that view, so
/// only those views' refs are read again. The map is shared with `prev` but for them. A holder
/// holds no plan ref: its views read none.
pub(crate) fn plan_heads_after(
    reader: &StoreReader,
    prev: &imbl::OrdMap<String, Hash>,
    plans: &[Hash],
) -> Result<imbl::OrdMap<String, Hash>, RdyError> {
    let mut views = std::collections::BTreeSet::new();
    for h in plans {
        views.insert(reader.plan(h)?.view);
    }
    let mut heads = prev.clone();
    count_plan_ref_reads(views.len());
    for view in views {
        match reader.get_ref(&rdy_store::plan::plan_ref(&view))? {
            Some(v) => {
                let head = plan_ref_head(&view, &v)?;
                heads.insert(view, head);
            }
            None => {
                heads.remove(&view);
            }
        }
    }
    Ok(heads)
}

/// The plan a `plan/<view>` ref value names.
fn plan_ref_head(view: &str, v: &[u8]) -> Result<Hash, RdyError> {
    Ok(Hash(v.try_into().map_err(|_| {
        RdyError::from(rdy_store::StoreError::Decode(format!(
            "plan ref of view {view:?} does not name a plan"
        )))
    })?))
}

/// Whether a change of proposal version `p` landed in the log `st` was published at: a landing
/// whose frame lies before the state's sequence.
fn landed_in(st: &ReadState, p: &Proposal) -> Result<bool, RdyError> {
    let reader = st.reader();
    for c in &p.changes {
        if let Some(l) = reader.landing_of(c)?
            && reader.has_kind_before(kinds::LANDING, &l, st.seq())?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `view`'s live chain (see the module docs) as `st` was published, or `None`: its plan ref
/// captured with the state, and a landing only when it lies before the state's sequence.
pub(crate) fn live_chain(st: &ReadState, view: &str) -> Result<Option<Chain>, RdyError> {
    let reader = st.reader();
    let Some(head) = st.plan_head(view) else {
        return Ok(None);
    };
    let plan = reader.plan(&head)?;
    if plan.seal.is_some() {
        return Ok(None);
    }
    // the proposal the chain last bound, also behind answer-only versions (O6)
    if let Some(bound) = reader.last_bound_proposal(&plan)?
        && landed_in(st, &reader.proposal(&bound)?)?
    {
        return Ok(None);
    }
    let first_heads = match plan.stable {
        Some(first) => reader.plan(&first)?.heads,
        None => plan.heads.clone(),
    };
    Ok(Some(Chain {
        head,
        plan,
        first_heads,
    }))
}

/// A view's current plan: its items, its live chain and the HEADs captured for it.
pub(crate) struct ViewPlan {
    pub(crate) computed: ComputedPlan,
    pub(crate) chain: Option<Chain>,
    pub(crate) heads: Vec<PlanHead>,
}

impl ViewPlan {
    /// The plan of `view`'s included changes on canon, with code evidence at the HEADs
    /// captured now against its chain's first HEADs (none: the captured ones).
    pub(crate) fn read(st: &ReadState, view: &str) -> Result<ViewPlan, RdyError> {
        let heads = capture_heads(st)?;
        let chain = live_chain(st, view)?;
        let first = chain.as_ref().map_or(&heads, |c| &c.first_heads);
        let changes = st.included(view);
        let computed = compute::compute(st, view, &changes, st.canon_head(), None, &heads, first)?;
        Ok(ViewPlan {
            computed,
            chain,
            heads,
        })
    }

    /// The answers the view's live chain holds.
    pub(crate) fn prior(&self) -> &[PlanAnswer] {
        self.chain.as_ref().map_or(&[], |c| &c.plan.answers)
    }
}

/// `p` as a reply shows who answered.
fn author(p: &Provenance) -> Author {
    Author {
        actor: p.actor.clone(),
        run: p.run.clone(),
        role: role_str(p.role).into(),
        at_ms: p.at_ms,
    }
}

/// The live rows of `computed` with `answers` (sorted by item), their counts and the lapse
/// warnings, keyed by item for their order. A computed close wins before any answer is
/// consulted; a valid answer sets the state; a lapsed one is left out and warned of, naming the
/// subject when it changed, else the cause.
pub(crate) fn live_rows(
    computed: &ComputedPlan,
    answers: &[PlanAnswer],
) -> (Vec<PlanRow>, PlanCounts, Vec<(Hash, PlanNote)>) {
    let mut counts = PlanCounts::default();
    let mut notes = vec![];
    let mut rows = Vec::with_capacity(computed.items.len());
    for c in &computed.items {
        let (state, how, answer) = match c.close {
            Some(close) => (ItemState::Closed, Some(close), None),
            None => match answers.binary_search_by(|a| a.item.cmp(&c.id)) {
                Err(_) => (ItemState::Open, None, None),
                Ok(i) => {
                    let a = &answers[i];
                    let subject_moved = a.subject != c.subject_hash;
                    if subject_moved || a.cause != c.cause_hash {
                        let block = if subject_moved {
                            c.subject.clone()
                        } else {
                            c.cause.to_string()
                        };
                        notes.push((
                            c.id,
                            PlanNote {
                                code: W_PLAN_ANSWER_LAPSED.into(),
                                message: format!(
                                    "the answer to {} lapsed: {block} changed",
                                    item_label(c.kind, &c.id, &c.subject)
                                ),
                                fix: PLAN_ANSWER_LAPSED_FIX.into(),
                            },
                        ));
                        (ItemState::Open, None, None)
                    } else {
                        let state = match a.act {
                            PlanAct::Defer => ItemState::Deferred,
                            PlanAct::Confirm | PlanAct::Done | PlanAct::Dismiss => {
                                ItemState::Closed
                            }
                        };
                        let row = AnswerRow {
                            act: a.act,
                            reason: (a.act != PlanAct::Done).then(|| a.reason.clone()).flatten(),
                            by: author(&a.by),
                        };
                        (state, None, Some(row))
                    }
                }
            },
        };
        counts.add(c.kind, state);
        rows.push(PlanRow::Live(LiveItem {
            id: item_label(c.kind, &c.id, &c.subject),
            kind: c.kind,
            subject: c.subject.clone(),
            cause: c.cause.to_string(),
            edge: c.edge.clone(),
            state,
            how,
            answer,
            stable: c.stable,
        }));
    }
    (rows, counts, notes)
}

/// The state a sealed outcome counts as.
fn sealed_state(o: PlanOutcome) -> ItemState {
    match o {
        PlanOutcome::Open => ItemState::Open,
        PlanOutcome::Deferred => ItemState::Deferred,
        _ => ItemState::Closed,
    }
}

fn sealed_row(i: &SealedItem) -> PlanRow {
    PlanRow::Sealed(SealedRow {
        id: item_label(i.kind, &i.id, &i.subject),
        kind: i.kind,
        subject: i.subject.clone(),
        cause: i.cause.to_string(),
        edge: i.edge.clone(),
        outcome: i.outcome,
        answer: i.answer.map(|h| h.to_string()),
    })
}

/// The reply with every item (no page cut): scope, plan, items, counts, code and notes.
fn reply(
    scope: String,
    plan: Option<Hash>,
    (items, counts, notes): (Vec<PlanRow>, PlanCounts, Vec<(Hash, PlanNote)>),
    code: PlanCode,
) -> PlanReply {
    let mut notes = notes;
    notes.sort_by(|(a, x), (b, y)| x.code.cmp(&y.code).then(a.cmp(b)));
    PlanReply {
        scope,
        plan: plan.map(|h| h.to_string()),
        items,
        counts,
        code,
        notes: notes.into_iter().map(|(_, n)| n).collect(),
        next: None,
    }
}

fn computed_code(c: &ComputedPlan) -> PlanCode {
    PlanCode {
        files: c.code_files,
        unavailable: c.code_unavailable,
    }
}

impl ReadState {
    /// Reads a change plan (p-92F5M): `q`'s selector, else view `default_view` (the session's),
    /// or the page of an earlier read that `q.page` continues. Cut to `q.max_tokens` (default
    /// [`PLAN_DEFAULT_TOKENS`], at most [`PLAN_MAX_TOKENS`]) at item boundaries; a cut page
    /// carries `next`. Writes nothing.
    pub fn change_plan(&self, q: &PlanQuery, default_view: &str) -> Result<PlanReply, RdyError> {
        check_plan_request(q, &[])?;
        let max = check_tokens(q.max_tokens)?;
        if let Some(cursor) = &q.page {
            let (offset, digest, selector) = page::parse(cursor)?;
            let full = self.full_plan(&selector, default_view)?;
            return page::continue_at(full, offset, digest, max);
        }
        let selector = if let Some(v) = &q.view {
            Selector::View(v.clone())
        } else if let Some(p) = &q.proposal {
            if !explicit_version(p) {
                return Err(bad(
                    "plan proposal needs an explicit version",
                    "pass P-<id>@<n>",
                ));
            }
            Selector::Proposal(p.clone())
        } else if let Some(l) = &q.landing {
            Selector::Landing(l.clone())
        } else if let Some(d) = &q.decision {
            Selector::Decision(d.clone())
        } else {
            Selector::View(default_view.to_string())
        };
        let full = self.full_plan(&selector, default_view)?;
        Ok(page::first(full, max))
    }

    /// The whole reply of view `view`'s plan, with every item and no cursor. A caller that holds
    /// it cuts it with [`page::first`](super::page::first), so one state gives both the answers'
    /// effect and the pages that follow. Writes nothing.
    pub fn view_plan(&self, view: &str) -> Result<PlanReply, RdyError> {
        self.full_plan(&Selector::View(view.to_string()), view)
    }

    /// The whole reply of `selector`, uncut.
    pub(crate) fn full_plan(
        &self,
        selector: &Selector,
        default_view: &str,
    ) -> Result<PlanReply, RdyError> {
        match selector {
            Selector::View(v) => {
                let view =
                    match self.resolve_read_scope(&format!("view:{v}"), Some(default_view))? {
                        Scope::View(name) => name,
                        _ => unreachable!("a view: scope text reads as a view"),
                    };
                let vp = ViewPlan::read(self, &view)?;
                let rows = live_rows(&vp.computed, vp.prior());
                Ok(reply(
                    format!("view:{view}"),
                    vp.chain.as_ref().map(|c| c.head),
                    rows,
                    computed_code(&vp.computed),
                ))
            }
            Selector::Proposal(text) => self.proposal_plan(text),
            Selector::Landing(text) => self.landing_plan(text),
            Selector::Decision(text) => self.decision_plans(text),
        }
    }

    /// A proposal version's plan, from its recorded canon and changes: with its bound plan's
    /// HEADs against its chain's first HEADs and its answers; with no bound plan (O19), the HEADs
    /// captured once as both ends and no answers (so no lapse).
    fn proposal_plan(&self, text: &str) -> Result<PlanReply, RdyError> {
        let pref = crate::gate::resolve_in(self, text)?;
        let reader = self.reader();
        let p = reader.proposal(&pref.version)?;
        let bound = reader.plan_of_proposal(&pref.version, self.seq())?;
        let (heads, first, answers) = match bound {
            Some(h) => {
                let plan = reader.plan(&h)?;
                let first = match plan.stable {
                    Some(s) => reader.plan(&s)?.heads,
                    None => plan.heads.clone(),
                };
                (plan.heads, first, plan.answers)
            }
            None => {
                let heads = capture_heads(self)?;
                (heads.clone(), heads, vec![])
            }
        };
        let fixed = compute::fixed_tensions(self, &pref.version)?;
        let computed = compute::compute(
            self,
            &p.view,
            &p.changes,
            p.canon_at,
            fixed.as_ref(),
            &heads,
            &first,
        )?;
        Ok(reply(
            format!("proposal:{pref}"),
            bound,
            live_rows(&computed, &answers),
            computed_code(&computed),
        ))
    }

    /// A landing's seal, as recorded; no seal is an empty plan.
    fn landing_plan(&self, text: &str) -> Result<PlanReply, RdyError> {
        let landing = self.resolve_landing(text)?;
        let scope = format!("landing:{landing}");
        let reader = self.reader();
        let Some(h) = reader.plan_of_landing(&landing, self.seq())? else {
            return Ok(reply(
                scope,
                None,
                (vec![], PlanCounts::default(), vec![]),
                PlanCode::default(),
            ));
        };
        let plan = reader.plan(&h)?;
        let seal = plan.seal.as_ref().ok_or_else(|| {
            RdyError::from(rdy_store::StoreError::Decode(format!(
                "plan {h} is indexed as a seal but has none"
            )))
        })?;
        let mut counts = PlanCounts::default();
        let mut files = std::collections::BTreeSet::new();
        for i in &seal.items {
            counts.add(i.kind, sealed_state(i.outcome));
            if i.kind == PlanKind::UpdateImplementation {
                files.insert(i.subject.as_str());
            }
        }
        let code = PlanCode {
            files: files.len(),
            unavailable: plan.heads.is_empty(),
        };
        let rows = seal.items.iter().map(sealed_row).collect();
        Ok(reply(scope, Some(h), (rows, counts, vec![]), code))
    }

    /// The seals that name a decision, newest first by their landings' log position.
    fn decision_plans(&self, text: &str) -> Result<PlanReply, RdyError> {
        let id = self.resolve_id(&Scope::Canon, text)?;
        let reader = self.reader();
        let mut seals = reader.plans_of_decision(&id, self.seq())?;
        seals.reverse();
        let mut counts = PlanCounts::default();
        let mut rows = Vec::with_capacity(seals.len());
        for h in seals {
            let plan = reader.plan(&h)?;
            let Some(seal) = plan.seal else {
                return Err(rdy_store::StoreError::Decode(format!(
                    "plan {h} is indexed as a seal but has none"
                ))
                .into());
            };
            let mut outcomes: BTreeMap<&'static str, usize> =
                OUTCOMES.iter().map(|o| (o.as_str(), 0)).collect();
            for i in &seal.items {
                counts.add(i.kind, sealed_state(i.outcome));
                *outcomes.entry(i.outcome.as_str()).or_default() += 1;
            }
            rows.push(PlanRow::Seal(SealRow {
                landing: seal.landing.to_string(),
                version: seal.version.to_string(),
                items: seal.items.len(),
                outcomes,
            }));
        }
        Ok(reply(
            format!("decision:{id}"),
            None,
            (rows, counts, vec![]),
            PlanCode::default(),
        ))
    }
}

/// The refusal of an item `text` that the plan of `scope` does not hold.
fn not_an_item(text: &str, scope: &str) -> RdyError {
    RdyError::new(
        E_PLAN_ITEM,
        format!(
            "{} is not an item of the change plan for {scope}",
            crate::views::clip(text)
        ),
        PLAN_ITEM_FIX,
    )
}

/// The candidate `text` names in `computed`: a `pi-` prefix of exactly one item, a `tn-` id
/// (12 to 64 hex) that starts the subject of exactly one tension item (O5), or a `cq-` id of the
/// view's current consequence report (`report` reads it on first use) whose subject, cause and
/// kind match exactly one item. Anything else is `E_PLAN_ITEM`.
fn resolve_item<'c>(
    computed: &'c ComputedPlan,
    text: &str,
    scope: &str,
    report: &dyn Fn() -> Result<Consequences, RdyError>,
    held: &OnceCell<Consequences>,
) -> Result<&'c PlanCandidate, RdyError> {
    let miss = || not_an_item(text, scope);
    if let Some(hex) = text.strip_prefix(ITEM_DISPLAY_PREFIX) {
        if hex.is_empty() || hex.len() > 64 || !hex.bytes().all(is_lower_hex) {
            return Err(miss());
        }
        // the items are sorted by full hash, so the ones with the prefix are contiguous
        let items = &computed.items;
        let start = items.partition_point(|c| c.id.to_string().as_str() < hex);
        let mut found = items[start..]
            .iter()
            .take_while(|c| c.id.to_string().starts_with(hex));
        return match (found.next(), found.next()) {
            (Some(c), None) => Ok(c),
            _ => Err(miss()),
        };
    }
    if let Some(hex) = text.strip_prefix(TENSION_DISPLAY_PREFIX) {
        if !(12..=64).contains(&hex.len()) || !hex.bytes().all(is_lower_hex) {
            return Err(miss());
        }
        let mut found = computed
            .items
            .iter()
            .filter(|c| c.kind == PlanKind::Tension && c.subject.starts_with(hex));
        return match (found.next(), found.next()) {
            (Some(c), None) => Ok(c),
            _ => Err(miss()),
        };
    }
    if !text.starts_with("cq-") {
        return Err(miss());
    }
    let report = match held.get() {
        Some(r) => r,
        None => {
            let r = report()?;
            held.get_or_init(|| r)
        }
    };
    let Some(cq) = report.items.iter().find(|c| c.id == text) else {
        return Err(miss());
    };
    let kinds: &[PlanKind] = match cq.follow_up {
        crate::brief::FollowUp::Update => &[PlanKind::Update],
        crate::brief::FollowUp::Reaffirm => &[PlanKind::Reaffirm],
        crate::brief::FollowUp::Confirm => &[PlanKind::Confirm, PlanKind::Rejustify],
        crate::brief::FollowUp::Resolve => &[],
    };
    let mut found = computed.items.iter().filter(|c| {
        c.subject == cq.block && c.cause.as_str() == cq.because.changed && kinds.contains(&c.kind)
    });
    match (found.next(), found.next()) {
        (Some(c), None) => Ok(c),
        _ => Err(miss()),
    }
}

/// The id of `c` in full, as [`resolve_item`] takes it: `pi-` and 64 hex, or a tension item's
/// `tn-` and its tension's 64 hex.
fn full_item_id(c: &PlanCandidate) -> String {
    match c.kind {
        PlanKind::Tension => format!("{TENSION_DISPLAY_PREFIX}{}", c.subject),
        _ => format!("{ITEM_DISPLAY_PREFIX}{}", c.id),
    }
}

/// The refusal for a reason that is missing, empty or over `MAX_REASON_CHARS`.
fn reason_refusal(act: PlanAct) -> RdyError {
    bad(
        &format!("{} needs a reason of 1 to 2000 characters", act.as_str()),
        "give a reason of 1 to 2000 characters",
    )
}

/// A trimmed reason of 1 to `MAX_REASON_CHARS` Unicode scalar values, or the refusal.
fn reason_of(act: PlanAct, reason: Option<&str>) -> Result<String, RdyError> {
    let r = reason.map(str::trim).unwrap_or("");
    if !(1..=MAX_REASON_CHARS).contains(&r.chars().count()) {
        return Err(reason_refusal(act));
    }
    Ok(r.to_string())
}

/// Refuses a reason over `MAX_REASON_CHARS` (trimmed) for an act that keeps one, with the text
/// [`reason_of`] gives. It allocates nothing, so it runs on a spec before a group expands to
/// one clone of the reason per matched item (tb-A4DAV). A missing or empty reason is left to
/// `reason_of`, and done keeps no reason.
fn check_spec_reason(act: PlanAct, reason: Option<&str>) -> Result<(), RdyError> {
    match (act, reason) {
        (PlanAct::Done, _) | (_, None) => Ok(()),
        (act, Some(r)) if r.trim().chars().count() > MAX_REASON_CHARS => Err(reason_refusal(act)),
        _ => Ok(()),
    }
}

/// The reason a spec passes on to an answer input: none for done, which keeps no reason
/// (`prepare_plan_answers` drops it), so a done group never clones an unchecked reason once per
/// matched item (tb-A4DAV).
fn input_reason(act: PlanAct, reason: Option<&str>) -> Option<String> {
    match act {
        PlanAct::Done => None,
        _ => reason.map(str::to_string),
    }
}

/// Validates the whole of `inputs` against `computed` (the plan of `view`'s scope) and returns
/// the cumulative answers: `prior` with each new answer added or replacing its item's, sorted by
/// item. Nothing is written. Each item resolves to one full item; an item answered twice, an
/// act its kind does not take, or a confirm or defer without a reason of 1 to 2000 characters
/// is refused before any answer is kept. Done keeps no reason. Each answer keeps the item's
/// subject and cause content hashes and `by` as its author (the writer reads its clock once).
pub(crate) fn prepare_plan_answers(
    st: &ReadState,
    by: &Provenance,
    view: &str,
    computed: &ComputedPlan,
    prior: &[PlanAnswer],
    inputs: &[PlanAnswerInput],
) -> Result<Vec<PlanAnswer>, RdyError> {
    if inputs.len() > MAX_PLAN_ANSWERS {
        return Err(answers_limit());
    }
    let scope = format!("view:{view}");
    let report = || -> Result<Consequences, RdyError> {
        match st.included(view).last() {
            Some(last) => st.consequences(&Scope::View(view.to_string()), last, Some(usize::MAX)),
            None => Ok(Consequences::default()),
        }
    };
    let held = OnceCell::new();
    let mut fresh: BTreeMap<Hash, PlanAnswer> = BTreeMap::new();
    for input in inputs {
        let c = resolve_item(computed, &input.item, &scope, &report, &held)?;
        if fresh.contains_key(&c.id) {
            return Err(bad(
                &format!(
                    "duplicate plan answer for {}",
                    item_label(c.kind, &c.id, &c.subject)
                ),
                "answer each item once per call",
            ));
        }
        // D-ME2HD: dismiss answers a tension item only, and defer does not answer one
        if input.act == PlanAct::Dismiss && c.kind != PlanKind::Tension {
            return Err(RdyError::new(
                E_PLAN_ITEM,
                "dismiss answers a tension item only",
                "answer it with confirm, defer or done",
            ));
        }
        if input.act == PlanAct::Defer && c.kind == PlanKind::Tension {
            return Err(RdyError::new(
                E_PLAN_ITEM,
                "defer does not answer a tension item",
                "answer it with confirm or dismiss, with a reason",
            ));
        }
        if !allowed_acts(c.kind).contains(&input.act) {
            return Err(bad(
                &format!(
                    "{} does not answer a {} item",
                    input.act.as_str(),
                    c.kind.as_str()
                ),
                "read the plan and use an act the item accepts",
            ));
        }
        let reason = match input.act {
            PlanAct::Done => None,
            act => Some(reason_of(act, input.reason.as_deref())?),
        };
        fresh.insert(
            c.id,
            PlanAnswer {
                item: c.id,
                act: input.act,
                reason,
                subject: c.subject_hash,
                cause: c.cause_hash,
                by: by.clone(),
            },
        );
    }
    let mut all: BTreeMap<Hash, PlanAnswer> = prior.iter().map(|a| (a.item, a.clone())).collect();
    all.extend(fresh);
    if all.len() > MAX_PLAN_ANSWERS {
        return Err(answers_limit());
    }
    Ok(all.into_values().collect())
}

impl Rdy {
    /// Records `answers` in `view`'s change plan (p-JJNYH, D-4JMPH): one cumulative,
    /// answer-only plan version and its `plan/<view>` ref, with the HEADs captured now, in one
    /// batch; no block, change, proposal or canon ref moves. The view's live chain continues;
    /// with none (no plan yet, a sealed one, or one whose last bound proposal landed with no
    /// seal, O6) the version starts a chain, and its HEADs become the chain's baseline. A reader is
    /// `E_PERMISSION`; an agent answers only a view it owns (`E_VIEW_OWNER`). The whole request is
    /// validated before anything is written. Returns the version's id.
    pub fn answer_plan(
        &mut self,
        session: &Session,
        view: &str,
        answers: &[PlanAnswerInput],
    ) -> Result<Hash, RdyError> {
        if !session.role.can_write() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot write", session.role),
                "start the session as a writer",
            ));
        }
        let st = self.read();
        crate::write::check_view_access(&st, view, session, crate::views::Action::Include)?;
        if answers.len() > MAX_PLAN_ANSWERS {
            return Err(answers_limit());
        }
        let vp = ViewPlan::read(&st, view)?;
        let cumulative = prepare_plan_answers(
            &st,
            &session.provenance(),
            view,
            &vp.computed,
            vp.prior(),
            answers,
        )?;
        let (stable, prev) = vp.chain.as_ref().map_or((None, None), Chain::next_links);
        let plan = Plan {
            stable,
            prev,
            view: view.to_string(),
            proposal: None,
            answers: cumulative,
            heads: vp.heads,
            provenance: session.provenance(),
            seal: None,
        };
        drop(st);
        let id = self.store.put_plan(&plan)?;
        self.publish(Delta::Seq)?;
        Ok(id)
    }

    /// [`Rdy::answer_plan`] for answers as the wire gives them (O13, O14): each spec names one
    /// item or a group of the open items, which expands to one answer per item before anything
    /// is written. The whole request is validated first: a group that matches nothing, or that
    /// holds an item the act does not answer, is refused with nothing written. With `repin` on
    /// a confirm group, rdy first writes one change to the view, one repin op per matched
    /// subject block whose pin is not current, self-briefed like the daemon's repin, with the
    /// group's justification (ruling final F2: one change per repin group; a repin of stable
    /// normative content with none is refused before anything is written); then it records the
    /// answers in one plan version. Returns that version's id.
    pub fn answer_plan_specs(
        &mut self,
        session: &Session,
        view: &str,
        specs: &[PlanAnswerSpec],
    ) -> Result<Hash, RdyError> {
        if !session.role.can_write() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot write", session.role),
                "start the session as a writer",
            ));
        }
        if specs.len() > MAX_PLAN_ANSWERS {
            return Err(answers_limit());
        }
        for s in specs {
            if s.item.is_some() == s.group.is_some() {
                return Err(bad(
                    "an answer names one of item and group",
                    "pass item for one item, or group for many",
                ));
            }
        }
        for s in specs {
            if s.repin && !(s.act == PlanAct::Confirm && s.group.is_some()) {
                return Err(bad(
                    "repin goes with a confirm group",
                    "set act to confirm and pass a group",
                ));
            }
        }
        for s in specs {
            if !s.repin && s.justified() {
                return Err(bad(
                    "a justification goes with repin",
                    "set repin to true, or remove the justification",
                ));
            }
        }
        let st = self.read();
        crate::write::check_view_access(&st, view, session, crate::views::Action::Include)?;
        // each repin group's justification, by rdy_write's rule and with its errors
        let view_scope = Scope::View(view.to_string());
        let mut justifications = Vec::with_capacity(specs.len());
        for s in specs {
            justifications.push(if s.repin {
                Some(crate::parse_justification(
                    &st,
                    &view_scope,
                    s.implements.clone(),
                    s.editorial.clone().map(|e| (e.class, e.reason)),
                    s.reflects.clone(),
                )?)
            } else {
                None
            });
        }
        let vp = ViewPlan::read(&st, view)?;
        let (rows, _, _) = live_rows(&vp.computed, vp.prior());
        let open: BTreeSet<Hash> = vp
            .computed
            .items
            .iter()
            .zip(&rows)
            .filter(|(_, row)| matches!(row, PlanRow::Live(l) if l.state == ItemState::Open))
            .map(|(c, _)| c.id)
            .collect();
        let m = st.materialized(&Scope::View(view.to_string()))?;
        let scope = format!("view:{view}");
        let mut inputs: Vec<PlanAnswerInput> = Vec::with_capacity(specs.len());
        // the items each repin group matched, by spec
        let mut to_repin: Vec<(usize, Vec<&PlanCandidate>)> = vec![];
        for (n, s) in specs.iter().enumerate() {
            // before any clone of the reason: a group copies it once per matched item
            check_spec_reason(s.act, s.reason.as_deref())?;
            match (&s.item, &s.group) {
                (Some(item), None) => {
                    check_expansion(inputs.len(), 1)?;
                    inputs.push(PlanAnswerInput {
                        item: item.clone(),
                        act: s.act,
                        reason: input_reason(s.act, s.reason.as_deref()),
                    });
                }
                (None, Some(g)) => {
                    let matched = g.expand(&vp.computed, &open, &m, &scope)?;
                    check_expansion(inputs.len(), matched.len())?;
                    let refused: Vec<String> = matched
                        .iter()
                        .filter(|c| !allowed_acts(c.kind).contains(&s.act))
                        .map(|c| item_label(c.kind, &c.id, &c.subject))
                        .collect();
                    if !refused.is_empty() {
                        return Err(RdyError::new(
                            E_PLAN_ITEM,
                            format!(
                                "{} does not answer {} items of the group: {}",
                                s.act.as_str(),
                                refused.len(),
                                refused
                                    .iter()
                                    .take(5)
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                            "narrow the group, or answer those items one by one",
                        ));
                    }
                    if s.repin {
                        to_repin.push((n, matched.clone()));
                    }
                    for c in matched {
                        inputs.push(PlanAnswerInput {
                            item: full_item_id(c),
                            act: s.act,
                            reason: input_reason(s.act, s.reason.as_deref()),
                        });
                    }
                }
                _ => unreachable!("checked above"),
            }
        }
        // the answers are checked now, so a refusal leaves the repin unwritten
        prepare_plan_answers(
            &st,
            &session.provenance(),
            view,
            &vp.computed,
            vp.prior(),
            &inputs,
        )?;
        // one repin change per repin group, each with that group's justification; a block an
        // earlier group repins is not repinned again. Every change is judged before any is
        // written (ruling final F2): a repin of stable normative content needs a justification
        let op_block = |op: &OpInput| match op {
            OpInput::Repin { id, .. } => BlockId::parse(id).ok(),
            _ => None,
        };
        let mut repinned: BTreeSet<BlockId> = BTreeSet::new();
        let mut writes: Vec<(Vec<OpInput>, crate::ParsedJustification)> = vec![];
        for (n, items) in &to_repin {
            let ops: Vec<OpInput> = group::repin_ops(&m, self.store.tracking(), items)
                .into_iter()
                .filter(|op| op_block(op).is_none_or(|b| repinned.insert(b)))
                .collect();
            if ops.is_empty() {
                continue;
            }
            let Some(j) = justifications[*n].take() else {
                continue;
            };
            if j.stored.is_none()
                && j.reflects.is_none()
                && let Some(b) = repins_stable_content(&st, &m, &ops)?
            {
                return Err(bad(
                    &format!("the repin changes stable block {b}, so it needs a justification"),
                    "pass implements, editorial or reflects, as rdy_write takes",
                ));
            }
            writes.push((ops, j));
        }
        drop((st, m));
        for (ops, j) in writes {
            let ids: BTreeSet<BlockId> = ops.iter().filter_map(op_block).collect();
            let briefing = self.read().brief_ids(&view_scope, &ids, None)?;
            briefing.register(&self.briefs);
            let token = briefing.base.token.clone();
            let req = WriteRequest {
                view: view.to_string(),
                ops,
                justification: j.stored,
                message: String::new(),
            };
            match j.reflects {
                Some(reflects) => self.write_reflecting(
                    session,
                    req,
                    reflects,
                    rdy_model::ChangeLinks::default(),
                    Some(&token),
                )?,
                None => self.write_briefed(session, req, Some(token))?,
            };
        }
        self.answer_plan(session, view, &inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_model::Role;

    /// O13: a group expands to at most `MAX_PLAN_ANSWERS` answers in all, checked before the
    /// answers are built (tb-A4DAV). The bound itself fits; one past it is `E_LIMIT`.
    #[test]
    fn rg9_expansion_is_bounded_before_it_is_built() {
        assert!(check_expansion(0, MAX_PLAN_ANSWERS).is_ok());
        assert!(check_expansion(MAX_PLAN_ANSWERS - 1, 1).is_ok());
        assert!(check_expansion(MAX_PLAN_ANSWERS, 0).is_ok());
        for (have, add) in [
            (0, MAX_PLAN_ANSWERS + 1),
            (MAX_PLAN_ANSWERS, 1),
            (usize::MAX, 2),
        ] {
            let e = check_expansion(have, add).unwrap_err();
            assert_eq!(e.code, E_LIMIT, "{have} + {add}");
            assert_eq!(
                e.message,
                format!("change plan exceeds {MAX_PLAN_ANSWERS} plan answers")
            );
        }
    }

    /// A reason over `MAX_REASON_CHARS` is refused by the spec check, before a group expands to
    /// one clone of it per matched item (tb-A4DAV). Done keeps no reason, so it is not checked;
    /// a missing or empty reason is left to `reason_of`, after the group has expanded.
    #[test]
    fn rg9_an_over_long_reason_is_refused_before_expansion() {
        let long = "x".repeat(MAX_REASON_CHARS + 1);
        let padded = format!("  {}  ", "x".repeat(MAX_REASON_CHARS));
        for act in [PlanAct::Confirm, PlanAct::Defer, PlanAct::Dismiss] {
            let e = check_spec_reason(act, Some(&long)).unwrap_err();
            let want = reason_of(act, Some(&long)).unwrap_err();
            assert_eq!(
                (e.code, e.message, e.fix),
                (want.code, want.message, want.fix)
            );
            assert!(
                check_spec_reason(act, Some(&padded)).is_ok(),
                "trimmed length counts"
            );
            assert!(check_spec_reason(act, None).is_ok());
            assert!(check_spec_reason(act, Some("")).is_ok());
        }
        assert!(check_spec_reason(PlanAct::Done, Some(&long)).is_ok());
    }

    /// Done keeps no reason, so a spec for done passes none on to the answer input: a done
    /// group of N items must not clone an unchecked reason N times (tb-A4DAV). Other acts pass
    /// the reason on.
    #[test]
    fn rg9_done_passes_no_reason_to_an_answer_input() {
        let long = "x".repeat(MAX_REASON_CHARS + 1);
        assert_eq!(input_reason(PlanAct::Done, Some(&long)), None);
        assert_eq!(input_reason(PlanAct::Done, None), None);
        for act in [PlanAct::Confirm, PlanAct::Defer, PlanAct::Dismiss] {
            assert_eq!(input_reason(act, Some("why")), Some("why".to_string()));
            assert_eq!(input_reason(act, None), None);
        }
    }

    fn cand(id: Hash) -> PlanCandidate {
        PlanCandidate {
            id,
            kind: PlanKind::Confirm,
            subject: "c-BBBBB".into(),
            cause: BlockId::parse("c-AAAAA").unwrap(),
            edge: "references".into(),
            subject_hash: Some(Hash([1; 32])),
            cause_hash: Hash([2; 32]),
            close: None,
            stable: false,
        }
    }

    fn plan_of(ids: &[Hash]) -> ComputedPlan {
        ComputedPlan {
            items: ids.iter().copied().map(cand).collect(),
            ..Default::default()
        }
    }

    /// p-JJNYH: a `pi-` prefix names exactly one item; a prefix of two, of none, or not
    /// lowercase hex is no item.
    #[test]
    fn cp8_pi_prefixes_resolve_one_item_or_refuse() {
        let plan = plan_of(&[Hash([0x11; 32]), Hash([0x12; 32]), Hash([0x21; 32])]);
        let none = || Ok(Consequences::default());
        let held = OnceCell::new();
        let get = |t: &str| resolve_item(&plan, t, "view:w", &none, &held).map(|c| c.id);
        assert_eq!(get("pi-12").unwrap(), Hash([0x12; 32]));
        assert_eq!(get("pi-2").unwrap(), Hash([0x21; 32]));
        let full = format!("pi-{}", Hash([0x11; 32]));
        assert_eq!(get(&full).unwrap(), Hash([0x11; 32]));
        for t in [
            "pi-1",
            "pi-3",
            "pi-",
            "pi-1G",
            "pi-AB",
            "11",
            &format!("{full}1"),
        ] {
            let e = get(t).unwrap_err();
            assert_eq!(e.code, E_PLAN_ITEM, "{t}");
            assert_eq!(
                e.message,
                format!("{t} is not an item of the change plan for view:w")
            );
            assert_eq!(e.fix, PLAN_ITEM_FIX);
        }
    }

    /// A plan holds at most `MAX_PLAN_ANSWERS` answers: a new item past it is refused, and a
    /// replaced answer does not grow it.
    #[test]
    fn cp8_cumulative_answers_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let st = r.read();
        let s = Session::human("human:qa".into(), Role::Writer).unwrap();
        let target = Hash([0x11; 32]);
        let plan = plan_of(&[target]);
        let mut prior: Vec<PlanAnswer> = (0..MAX_PLAN_ANSWERS as u64)
            .map(|i| PlanAnswer {
                item: Hash::of(&i.to_le_bytes()),
                act: PlanAct::Defer,
                reason: Some("x".into()),
                subject: None,
                cause: Hash([3; 32]),
                by: s.provenance(),
            })
            .collect();
        prior.sort_by_key(|a| a.item);
        let input = [PlanAnswerInput {
            item: "pi-11".into(),
            act: PlanAct::Defer,
            reason: Some("later".into()),
        }];
        let by = s.provenance();
        let e = prepare_plan_answers(&st, &by, "w", &plan, &prior, &input).unwrap_err();
        assert_eq!(e.code, E_LIMIT);
        assert_eq!(e.message, "change plan exceeds 100000 plan answers");
        assert_eq!(e.fix, "split the answers into smaller plans");
        // replacing an answer the plan holds keeps the count
        prior.pop();
        prior.push(PlanAnswer {
            item: target,
            ..prior[0].clone()
        });
        prior.sort_by_key(|a| a.item);
        let out = prepare_plan_answers(&st, &by, "w", &plan, &prior, &input).unwrap();
        assert_eq!(out.len(), MAX_PLAN_ANSWERS);
        let mine = out.iter().find(|a| a.item == target).unwrap();
        assert_eq!(
            (mine.reason.as_deref(), mine.subject, mine.cause),
            (Some("later"), Some(Hash([1; 32])), Hash([2; 32]))
        );
    }
}
