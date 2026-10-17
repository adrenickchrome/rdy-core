//! The queue (D-JV6Z8): what waits for a person, computed from the published state. Nothing is
//! stored for it, so it never falls out of step with the records (p-7X6QY).
//!
//! # Batches (D-YFDG3)
//!
//! Each open proposal's latest version that is ready is one `approve` item, ref `P-x@n`. Its
//! card leads with the author's summary (D-4PG9Y), lists the decisions it confirms (`members`),
//! and gives the computed facts in the closed order of O6: Changes, Decisions, Supersedes,
//! Reversibility, Consequences (D-NJ2SP), Tensions (D-45RG6, C7), Coverage (D-C6HHV),
//! Confidence (D-XZXCE), Base and Exceptions. Consequences, Coverage and Confidence are worked
//! out on the canon the version's landing would give, never on its view, which may have moved
//! on. No fact reads an answer, so an answer beside the batch never moves the batch card.
//!
//! # Exceptions (p-FN596, F3; closed)
//!
//! A proposed decision in the version is a decision block that the version's changes create or
//! update and whose status in the prospective canon is `proposed`: exactly the decisions a
//! landing accepts (O25). It stands beside its batch as its own `confirm` item, ref
//! `<D-id>@<version short>` (O3), when one of these holds, in this order (the item names the
//! first; its facts name each):
//! 1. `against`: it supersedes or revokes a decision live in canon, or it is a side of an open
//!    semantic tension (any severity) whose other side is a decision live in canon. `refines` is
//!    not against.
//! 2. `one_way`: its `reversibility` is `one_way`.
//! 3. `unapplied`: no content applies it once it lands (`lands_unapplied`, D-T5S9A).
//! 4. `not_recommended`: its `recommend` is `reject`.
//! 5. `blocking_tension`: an open blocking tension of the version involves it.
//!
//! The version's advised tensions that no one accepted stand beside the batch as one `resolve`
//! item, ref `advice:P-x@n` (D-WMCFW). [`super::tensions`] builds it, with the `resolve` item of
//! each blocking tension a person or a reviewer must dismiss (D-562QR).
//!
//! # Content removal (D-CC90E, D-6B1W1)
//!
//! A change of the version that carries a removal request (`redaction_for_carrier`) puts one
//! [`RemovalCard`] on the version's `approve` item for each request, by request hash, read from
//! the request record: its report hash is the hash of the complete manifest the request binds.
//! The approve option adds the line that approved removal cannot be undone. Such an item is
//! always answered on its own: a batch of recommended answers refuses it
//! (`Rdy::answer_all_recommended`). A card whose mandatory removal lines do not fit the budget is
//! `E_REDACTION_BUDGET` for the queue, never a card that hides them.
//!
//! # Questions and snoozes
//!
//! Each open question in canon that carries authored options is an `answer` item of its own
//! ([`super::questions`], F7). A snooze hides an item for the chat session it was made in
//! ([`super::snooze`], F4).
//!
//! # Readiness (D-3JZP4; plan "Readiness before M3b"; closed)
//!
//! A version is ready when the landing's checks pass (no conflict on a block it writes, no
//! blocking tension it would put into canon, no error in the canon it would give;
//! `gate::landing_readiness`), it carries a summary, and no decision of it was rejected (F1).
//! A version that is not ready is no item: it is listed under `not_ready`, one issue per failed
//! check, for the agent and never on a card.
//!
//! # Answers (O23)
//!
//! The queue reads the answers (`answer/<item>`): a `reject_proposal` answer to `P-x@n` takes the
//! version and everything beside it out of the queue; a `confirm` answer to `<D-id>@<version>`
//! takes that item out; a `reject_decision` answer makes the batch not ready, and the landing
//! refuses it too, whether the decision is an exception now or not ([`approval_holds`]); an
//! `answer_question` answer to `<Q-id>@<version>` takes the question out (its decision waits for
//! a landing).
//!
//! # Order (O5)
//!
//! Blocking items first; then batches, each with its items directly after it (the `approve`
//! item, its exceptions by ref, then its advice); then the items that stand alone (questions);
//! each kind by `raised_at`, oldest first; then by ref. At most [`PENDING_ITEMS_MAX`] items are
//! listed, whole batches only; `omitted` counts the rest.
use super::card::{
    CARD_DEFAULT_TOKENS, CARD_MAX_TOKENS, REMOVAL_IRREVERSIBLE, card_hash, render_checked,
};
use super::item::{ExceptionKind, Fact, Item, ItemKind, ItemOption, RemovalCard, age};
use super::questions::question_items;
use super::snooze::hide_snoozed;
use super::tensions::tension_items;
use crate::coverage::change::{candidate_build, candidate_change};
use crate::coverage::{CandidateCoverage, confidence_in, lands_unapplied};
use crate::error::{E_VALIDATION, RdyError, redaction_limit};
use crate::gate::{ProposalRef, changes_of, landing_readiness, open_proposals};
use crate::read_state::Built;
use crate::session::{now_ms, rfc3339};
use crate::tensions::{Tension, TensionKind, TensionSeverity, TensionStatus, tension_label};
use crate::{Materialized, ReadState, Scope};
use rdy_graph::{DerivedStatus, Graph};
use rdy_model::decision::{self, DecisionStatus, REVOKES, SUPERSEDES};
use rdy_model::redaction::record_buffer;
use rdy_model::{AnswerAct, Block, BlockId, Change, Hash, Op, Proposal};
use rdy_schema::{Issue, Severity};
use rdy_store::redaction::limit;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The most items the queue lists; whole batches past it are counted in `omitted`.
pub const PENDING_ITEMS_MAX: usize = 50;

/// Who asks, and how large a card may be. The default asks for no person and no session, with
/// cards of [`CARD_DEFAULT_TOKENS`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingQuery {
    /// The person the queue is for: the human actor of the project (O11).
    pub person: String,
    /// The chat session the person speaks in (F4: a snooze lasts until it changes).
    pub session_ref: Option<String>,
    /// The budget of each card (O6), at most [`CARD_MAX_TOKENS`].
    pub max_tokens: usize,
}

impl Default for PendingQuery {
    fn default() -> PendingQuery {
        PendingQuery {
            person: String::new(),
            session_ref: None,
            max_tokens: CARD_DEFAULT_TOKENS,
        }
    }
}

/// The queue: its items in order, the versions that are not ready, and what it left out.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Pending {
    pub items: Vec<Item>,
    pub not_ready: Vec<NotReady>,
    /// How many items a snooze hides (F4).
    pub snoozed: usize,
    /// How many items past [`PENDING_ITEMS_MAX`] are left out.
    pub omitted: usize,
}

/// A latest open version that is not ready, and each failed check with its fix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NotReady {
    pub r#ref: String,
    pub issues: Vec<Issue>,
}

/// How many items wait for a person: every item the queue holds, past [`PENDING_ITEMS_MAX`]
/// too, less those a snooze hides (final review fix round 1, R6: the session's summary counts
/// the queue, not its first page).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PendingCounts {
    pub items: usize,
    pub blocking: usize,
}

/// The queue before a snooze or the cap: every item in order, in its groups, and the versions
/// that are not ready. Its cards are not rendered yet ([`finish`]).
pub(crate) struct Eligible {
    groups: Vec<Group>,
    pub not_ready: Vec<NotReady>,
}

impl Eligible {
    /// Each item of ref `r`, its card whole (a decision version beside two batches has two).
    /// Final review fix round 1 (R2): the answer binds here, so neither the cap nor a snooze
    /// hides an item from it.
    pub(crate) fn items_of(&self, r: &str) -> Result<Vec<Item>, RdyError> {
        self.items()
            .filter(|i| i.r#ref == r)
            .map(|i| finish(i.clone(), CARD_MAX_TOKENS))
            .collect()
    }

    /// The first item whose ref starts with `key`, its card whole.
    pub(crate) fn first_with_prefix(&self, key: &str) -> Result<Option<Item>, RdyError> {
        self.items()
            .find(|i| i.r#ref.starts_with(key))
            .map(|i| finish(i.clone(), CARD_MAX_TOKENS))
            .transpose()
    }

    fn items(&self) -> impl Iterator<Item = &Item> {
        self.groups.iter().flat_map(|g| &g.items)
    }
}

impl ReadState {
    /// The needs-you queue of this state for `q` (see the module docs). A pure read: it writes
    /// nothing, and one state and one query give one queue.
    pub fn pending(&self, q: &PendingQuery) -> Result<Pending, RdyError> {
        let e = self.eligible(q)?;
        let max = q.max_tokens.min(CARD_MAX_TOKENS);
        let mut items = vec![];
        let mut omitted = 0;
        let mut snoozed = 0;
        for g in e.groups {
            let (shown, hidden) = hide_snoozed(self, q, g.items);
            snoozed += hidden;
            if shown.is_empty() {
                continue;
            }
            if omitted > 0 || (!items.is_empty() && items.len() + shown.len() > PENDING_ITEMS_MAX) {
                omitted += shown.len();
                continue;
            }
            for i in shown {
                items.push(finish(i, max)?);
            }
        }
        Ok(Pending {
            items,
            not_ready: e.not_ready,
            snoozed,
            omitted,
        })
    }

    /// How many items of the queue for `q` wait, and how many of them block: the whole queue,
    /// past the cap too, less what a snooze hides ([`PendingCounts`]). It renders no card.
    pub fn pending_counts(&self, q: &PendingQuery) -> Result<PendingCounts, RdyError> {
        let e = self.eligible(q)?;
        let mut c = PendingCounts::default();
        for g in e.groups {
            let (shown, _) = hide_snoozed(self, q, g.items);
            c.items += shown.len();
            c.blocking += shown.iter().filter(|i| i.blocking).count();
        }
        Ok(c)
    }

    /// Every item of the queue for `q`, in order and in its groups, before a snooze or the cap
    /// ([`Eligible`]). The open proposals are read once, and the tension items share them.
    pub(crate) fn eligible(&self, q: &PendingQuery) -> Result<Eligible, RdyError> {
        let now = now_ms();
        let mut groups: Vec<Group> = vec![];
        let mut not_ready = vec![];
        let open = open_proposals(self)?;
        if !open.is_empty() {
            // canon's coverage and lexicon first: each version's build carries its structural
            // layer from them, and its coverage its suspects (a read error shows on the facts)
            let _ = self.scope_rollup(&Scope::Canon);
            let _ = self.lexicon(&Scope::Canon);
        }
        for (pref, p) in &open {
            let r = pref.to_string();
            if answered(self, &r, AnswerAct::RejectProposal)? {
                continue;
            }
            let v = Version::read(self, *pref, p.clone())?;
            let issues = v.readiness(self)?;
            if !issues.is_empty() {
                not_ready.push(NotReady { r#ref: r, issues });
                continue;
            }
            let mut items = vec![v.batch_item(self, now)];
            for item in v.exception_items(now) {
                if !answered(self, &item.r#ref, AnswerAct::Confirm)? {
                    items.push(item);
                }
            }
            groups.push(Group { items });
        }
        // the items that stand beside a batch go into its group, after its exceptions; the
        // others are groups of one
        let batches: Vec<Item> = groups.iter().map(|g| g.items[0].clone()).collect();
        let others = tension_items(self, &open, &batches)?
            .into_iter()
            .chain(question_items(self, q)?);
        for item in others {
            let group = item
                .batch
                .as_deref()
                .and_then(|b| groups.iter_mut().find(|g| g.items[0].r#ref == b));
            match group {
                Some(g) => g.items.push(item),
                None => groups.push(Group { items: vec![item] }),
            }
        }
        groups.sort_by_cached_key(Group::key);
        Ok(Eligible { groups, not_ready })
    }
}

/// What holds version `pref` back from landing (F1, F2), read once for the landing.
pub(crate) struct Holds {
    /// The exception items that have no `confirm` answer bound to the decision's current
    /// version (F2): a rejected decision's item is among them while it is an exception. Each card
    /// is cut to [`CARD_DEFAULT_TOKENS`].
    pub open: Vec<Item>,
    /// One issue for each proposed decision of the version whose current version the person
    /// rejected (F1), whether it is an exception now or not: the issue readiness gives (final
    /// review fix round 1, R1: a rejected decision that stops being an exception, say once
    /// content applies it, still never lands).
    pub rejected: Vec<Issue>,
}

/// What holds version `pref` back from landing ([`Holds`]). It does not ask whether the version
/// is ready otherwise; the landing asks that itself. `land_with` calls it (Task 17), so every
/// approval path waits.
pub(crate) fn approval_holds(st: &ReadState, pref: &ProposalRef) -> Result<Holds, RdyError> {
    let p = st.reader().proposal(&pref.version)?;
    let v = Version::read(st, *pref, p)?;
    let mut open = vec![];
    for item in v.exception_items(now_ms()) {
        if !answered(st, &item.r#ref, AnswerAct::Confirm)? {
            open.push(finish(item, CARD_DEFAULT_TOKENS)?);
        }
    }
    Ok(Holds {
        open,
        rejected: v.rejected(st)?,
    })
}

/// The `approve` item of version `pref` (whose record is `p`) with its card whole at
/// [`CARD_MAX_TOKENS`], when a change of the version carries a removal request; `None` when none
/// does (D-CC90E). It reads that version alone: neither readiness, an answer, a snooze nor the
/// queue's cap hides it, and no other version's card is rendered, so another card's budget never
/// refuses it. The item is the one the queue builds; a version that is not ready says why in its
/// `why` line instead of `ready to land`. A card whose mandatory removal lines do not fit is
/// `E_REDACTION_BUDGET` ([`render_checked`]). A direct approval shows this card before it lands.
pub(crate) fn removal_item(
    st: &ReadState,
    pref: ProposalRef,
    p: Proposal,
) -> Result<Option<Item>, RdyError> {
    let reader = st.reader();
    let mut carries = false;
    for h in &p.changes {
        if reader.redaction_for_carrier(h)?.is_some() {
            carries = true;
            break;
        }
    }
    if !carries {
        return Ok(None);
    }
    let v = Version::read(st, pref, p)?;
    let mut item = v.batch_item(st, now_ms());
    let issues = v.readiness(st)?;
    if !issues.is_empty() {
        let why: Vec<&str> = issues.iter().map(|i| i.message.as_str()).collect();
        item.why = format!("not ready to land: {}", why.join("; "));
    }
    finish(item, CARD_MAX_TOKENS).map(Some)
}

/// Whether the latest answer to `item` is of act `act`.
pub(super) fn answered(st: &ReadState, item: &str, act: AnswerAct) -> Result<bool, RdyError> {
    Ok(st
        .reader()
        .answer_of(item)?
        .is_some_and(|(_, a)| a.act == act))
}

/// `item` with its card cut to `max_tokens` and its card's hash (at full size, O7). A card whose
/// mandatory removal lines do not fit is `E_REDACTION_BUDGET` ([`render_checked`]).
fn finish(mut item: Item, max_tokens: usize) -> Result<Item, RdyError> {
    item.card = render_checked(&item, max_tokens)?;
    item.card_hash = card_hash(&item).to_string();
    Ok(item)
}

/// The removal cards of a version, by request hash (None when no change carries a request),
/// and the quarantine names of each report they name, by report hash.
type RemovalFacts = (Option<Vec<RemovalCard>>, BTreeMap<Hash, Vec<String>>);

/// The removal card of each request that a change of `changes` carries, by request hash, and
/// the quarantine names of the report each card names. Each request must still be fresh
/// (`redaction::check_fresh`). Each card is read from the request record: the report hash is
/// the hash of the complete manifest the request binds, never of a shortened presentation, and
/// the names are that manifest's.
fn removal_cards(st: &ReadState, changes: &[Hash]) -> Result<RemovalFacts, RdyError> {
    let reader = st.reader();
    let mut cards: Vec<RemovalCard> = vec![];
    let mut reports: BTreeMap<Hash, Vec<String>> = BTreeMap::new();
    let mut buf: Option<Vec<u8>> = None;
    for h in changes {
        let Some(request) = reader.redaction_for_carrier(h)? else {
            continue;
        };
        // the card shows the removal the request recorded only while its complete report is
        // still that one: the shared freshness check runs first, and a stale request is
        // `E_REDACTION_STALE`, never refreshed (D-6B1W1, D-14SAY)
        crate::redaction::check_fresh(st, &request, &[])?;
        let req = reader.redaction_request(&request)?;
        let buf = match &mut buf {
            Some(b) => b,
            None => buf.insert(record_buffer().map_err(|_| redaction_limit(limit::RECORD))?),
        };
        let m = &req.manifest;
        let report = m
            .report_hash(buf)
            .map_err(|_| redaction_limit(limit::RECORD))?;
        let bytes = m
            .spans
            .iter()
            .try_fold(0u64, |n, s| n.checked_add(u64::from(s.len)))
            .ok_or_else(|| redaction_limit(limit::BYTES))?;
        let n = |len: usize| u64::try_from(len).map_err(|_| redaction_limit(limit::SELECTED));
        reports.insert(report, m.quarantine.clone());
        cards.push(RemovalCard {
            request,
            report,
            versions: n(m.versions.len())?,
            blobs: n(m.blobs.len())?,
            shared_blobs: n(m.shared_blobs.len())?,
            payloads: u64::try_from(m.spans.len()).map_err(|_| redaction_limit(limit::FRAMES))?,
            bytes,
            reason_class: req.reason_class,
            reason: req.reason,
        });
    }
    cards.sort_by_key(|c| c.request);
    cards.dedup_by_key(|c| c.request);
    Ok(((!cards.is_empty()).then_some(cards), reports))
}

/// One batch and the items beside it, or one item on its own.
struct Group {
    items: Vec<Item>,
}

impl Group {
    /// O5: blocking first, then batches, then by `raised_at`, then by ref (of the first item).
    fn key(&self) -> (bool, bool, String, String) {
        let head = &self.items[0];
        (
            !head.blocking,
            head.kind != ItemKind::Approve,
            head.raised_at.clone(),
            head.r#ref.clone(),
        )
    }
}

/// One proposed decision of a version: its block in the prospective canon, its item ref, and
/// each exception kind it carries with its detail, in the closed order.
struct Decision {
    block: Block,
    r#ref: String,
    kinds: Vec<(ExceptionKind, String)>,
}

impl Decision {
    /// The card title: the authored `title`, else the statement.
    fn title(&self) -> String {
        text(&self.block, "title")
            .or_else(|| text(&self.block, "statement"))
            .unwrap_or_default()
            .to_string()
    }
}

/// A non-blank text field of `b`.
pub(super) fn text<'a>(b: &'a Block, name: &str) -> Option<&'a str> {
    b.field_str(name).filter(|s| !s.trim().is_empty())
}

/// One open version, read once: what every item of its batch is built from.
struct Version {
    pref: ProposalRef,
    p: Proposal,
    changes: Vec<Change>,
    /// The blocks the version's changes write.
    touched: BTreeSet<BlockId>,
    /// The canon that landing the version would give: its build, and its materialization.
    built: Built,
    next: Arc<Materialized>,
    /// The proposed decisions, by id.
    decisions: Vec<Decision>,
    /// The version's open tensions: its landing form and the in-flight forms of its changes, as
    /// a proposal lists them, by id.
    tensions: Vec<Arc<Tension>>,
    /// The removal requests the version's changes carry, by request hash.
    removal: Option<Vec<RemovalCard>>,
    /// The quarantine names of each report `removal` names, by report hash.
    removal_reports: BTreeMap<Hash, Vec<String>>,
}

impl Version {
    fn read(st: &ReadState, pref: ProposalRef, p: Proposal) -> Result<Version, RdyError> {
        let changes = changes_of(st, &p.changes)?;
        let touched: BTreeSet<BlockId> = changes
            .iter()
            .flat_map(|c| c.ops.iter().map(|o| o.block_id().clone()))
            .collect();
        let tensions = open_tensions(st, &p)?;
        let (removal, removal_reports) = removal_cards(st, &p.changes)?;
        // canon's graph first, so the prospective canon carries it instead of building another
        let canon_g = st.graph(&Scope::Canon)?;
        let built = candidate_build(st, &changes)?;
        // canon's graph and lexicon carry to this build, and canon's structural layer when
        // it is built (`ReadState::pending` builds it first)
        let g = Arc::clone(built.graph.get_or_init(|| {
            Arc::new(Graph::build_with(
                built.m.iter(),
                Arc::clone(st.reader().tracking()),
            ))
        }));
        let next = Arc::clone(&built.m);
        // the decisions a landing accepts (gate.rs, the acceptance loop)
        let mut ids = BTreeSet::new();
        for c in &changes {
            for op in &c.ops {
                if let Op::Create { block } | Op::Update { block, .. } = op
                    && block.block_type == decision::DECISION
                {
                    ids.insert(block.id.clone());
                }
            }
        }
        let live = |id: &BlockId| {
            matches!(
                canon_g.status(id),
                Some(DerivedStatus::Live | DerivedStatus::PartiallySuperseded { .. })
            )
        };
        let mut decisions = vec![];
        for id in ids {
            let Some(b) = next.get(&id) else { continue };
            if decision::status(b) != Some(DecisionStatus::Proposed) {
                continue;
            }
            let kinds = exception_kinds(b, &live, &tensions, || {
                lands_unapplied(&next, &g, st.pack(), b)
            });
            decisions.push(Decision {
                r#ref: format!("{id}@{}", b.version_hash().short()),
                block: b.clone(),
                kinds,
            });
        }
        Ok(Version {
            pref,
            p,
            changes,
            touched,
            built,
            next,
            decisions,
            tensions,
            removal,
            removal_reports,
        })
    }

    fn r#ref(&self) -> String {
        self.pref.to_string()
    }

    /// Every failed readiness check, one issue each (see the module docs).
    fn readiness(&self, st: &ReadState) -> Result<Vec<Issue>, RdyError> {
        let mut issues = landing_readiness(st, &self.pref, &self.p, &self.changes)?;
        let summary = self.p.summary.as_deref().map(str::trim);
        if summary.is_none_or(str::is_empty) {
            issues.push(Issue {
                severity: Severity::Error,
                code: E_VALIDATION,
                block: None,
                message: format!("{} has no summary", self.r#ref()),
                fix:
                    "propose it again with a summary in plain words for the person who approves it"
                        .into(),
            });
        }
        issues.extend(self.rejected(st)?);
        Ok(issues)
    }

    /// F1: one issue for each proposed decision whose current version has a `reject_decision`
    /// answer, exception or not. Readiness and the landing ([`approval_holds`]) share it.
    fn rejected(&self, st: &ReadState) -> Result<Vec<Issue>, RdyError> {
        let mut issues = vec![];
        for d in &self.decisions {
            if answered(st, &d.r#ref, AnswerAct::RejectDecision)? {
                let id = &d.block.id;
                issues.push(Issue {
                    severity: Severity::Error,
                    code: E_VALIDATION,
                    block: Some(id.clone()),
                    message: format!("{id} was rejected; drop or revise it, then propose again"),
                    fix: format!(
                        "in view {}, drop or revise {id}, then propose again",
                        self.p.view
                    ),
                });
            }
        }
        Ok(issues)
    }

    /// Who raised the version's items, when, and how long ago.
    fn raised(&self, it: &mut Item, now: i64) {
        let at = self.p.provenance.at_ms;
        it.raised_by = self.p.provenance.actor.clone();
        it.raised_at = rfc3339(at);
        it.age = age(at, now);
    }

    /// The version's `approve` item.
    fn batch_item(&self, st: &ReadState, now: i64) -> Item {
        let r = self.r#ref();
        let members: Vec<&Decision> = self
            .decisions
            .iter()
            .filter(|d| d.kinds.is_empty())
            .collect();
        let summary = self
            .p
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let approve = if members.is_empty() {
            format!("Approve {r}")
        } else {
            let ids: Vec<String> = members.iter().map(|d| d.block.id.to_string()).collect();
            format!("Approve {r} and confirm {}", ids.join(", "))
        };
        let mut it = item(ItemKind::Approve, &r, &self.p.title);
        it.why = format!("ready to land: {} changes", self.p.changes.len());
        it.summary.clone_from(&summary);
        it.summary_by = Some(self.p.provenance.actor.clone());
        it.facts = self.facts(st);
        it.members = members
            .iter()
            .map(|d| format!("{}  {}", d.block.id, d.title()))
            .collect();
        let mut approve = option(
            "approve",
            &approve,
            "Lands the proposal in canon.",
            Some(summary.unwrap_or_default()),
        );
        if self.removal.is_some() {
            approve.consequences.push(REMOVAL_IRREVERSIBLE.to_string());
        }
        it.removal.clone_from(&self.removal);
        it.removal_reports.clone_from(&self.removal_reports);
        it.options = vec![
            approve,
            option(
                "reject",
                "Reject",
                "Returns the proposal to its view. Canon does not change.",
                None,
            ),
            later(),
        ];
        self.raised(&mut it, now);
        it
    }

    /// The batch card's facts, in the closed order of O6.
    fn facts(&self, st: &ReadState) -> Vec<Fact> {
        let canon = st.materialized(&Scope::Canon).ok();
        let docs: BTreeSet<&BlockId> = self
            .touched
            .iter()
            .filter_map(|id| {
                self.next
                    .doc_of(id)
                    .or_else(|| canon.as_ref().and_then(|c| c.doc_of(id)))
                    .map(|d| &d.id)
            })
            .collect();
        let decisions = if self.decisions.is_empty() {
            "none".to_string()
        } else {
            let each: Vec<String> = self
                .decisions
                .iter()
                .map(|d| format!("{} \"{}\"", d.block.id, d.title()))
                .collect();
            format!("{} new: {}", self.decisions.len(), each.join(", "))
        };
        let supersedes: BTreeSet<String> = self
            .decisions
            .iter()
            .flat_map(|d| d.block.links.get(SUPERSEDES).into_iter().flatten())
            .map(|l| l.target.to_string())
            .collect();
        let supersedes = if supersedes.is_empty() {
            "none".to_string()
        } else {
            supersedes.into_iter().collect::<Vec<_>>().join(", ")
        };
        let (blocking, advisory) =
            self.tensions
                .iter()
                .fold((0, 0), |(b, a), t| match t.severity {
                    TensionSeverity::Blocking => (b + 1, a),
                    TensionSeverity::Advisory => (b, a + 1),
                });
        let mut tensions = format!("{blocking} blocking, {advisory} advisory");
        if advisory > 0 {
            tensions.push_str(", answers not recorded yet");
        }
        let exceptions = self
            .decisions
            .iter()
            .filter(|d| !d.kinds.is_empty())
            .count();
        let base = match self.p.canon_at {
            Some(h) => h.short(),
            None => "the start".to_string(),
        };
        let moved = if self.p.canon_at == st.canon_head() {
            "current"
        } else {
            "moved"
        };
        let candidate = st.candidate_coverage(&self.built, &self.p.changes);
        let fact = |name: &'static str, value: String| Fact { name, value };
        vec![
            fact(
                "Changes",
                format!(
                    "{}, {}",
                    count(docs.len(), "doc"),
                    count(self.touched.len(), "block")
                ),
            ),
            fact("Decisions", decisions),
            fact("Supersedes", supersedes),
            fact("Reversibility", self.reversibility()),
            fact("Consequences", worked_out(self.consequences(st))),
            fact("Tensions", tensions),
            fact(
                "Coverage",
                worked_out(self.coverage(st, candidate.as_ref())),
            ),
            fact(
                "Confidence",
                worked_out(self.confidence(candidate.as_ref())),
            ),
            fact("Base", format!("canon at {base}, {moved}")),
            fact(
                "Exceptions",
                match exceptions {
                    0 => "none".to_string(),
                    n => n.to_string(),
                },
            ),
        ]
    }

    /// The reversibility each decision declares: one value for all, or `mixed` with each.
    fn reversibility(&self) -> String {
        const UNDECLARED: &str = "not declared";
        let each: Vec<(&BlockId, &str)> = self
            .decisions
            .iter()
            .map(|d| {
                (
                    &d.block.id,
                    text(&d.block, "reversibility").unwrap_or(UNDECLARED),
                )
            })
            .collect();
        let values: BTreeSet<&str> = each.iter().map(|(_, v)| *v).collect();
        match values.into_iter().collect::<Vec<_>>()[..] {
            [] => "none".to_string(),
            [UNDECLARED] => UNDECLARED.to_string(),
            [v] => format!("{v}, declared by the proposer"),
            _ => {
                let list: Vec<String> = each.iter().map(|(id, v)| format!("{id} {v}")).collect();
                format!("mixed: {}, declared by the proposer", list.join(", "))
            }
        }
    }

    /// D-NJ2SP: the consequences of landing the version, computed on its final state.
    fn consequences(&self, st: &ReadState) -> Result<String, RdyError> {
        let c = st.landing_consequences(&self.pref.version, None)?;
        Ok(format!("{} returned", c.items.len()))
    }

    /// D-C6HHV: canon's coverage against that of the canon the version would give
    /// (`candidate`). Fix round 1: the version's, so a later edit of its view, or a change of
    /// the view the version left out, does not move it. Ruling final: the propose reply gives
    /// the same numbers, from the same function ([`candidate_change`]).
    fn coverage(
        &self,
        st: &ReadState,
        candidate: Result<&CandidateCoverage, &RdyError>,
    ) -> Result<String, RdyError> {
        let c = candidate_change(
            st,
            &self.built,
            candidate.map_err(Clone::clone)?,
            &self.touched,
        )?;
        Ok(format!("{}% \u{2192} {}%", c.before, c.after))
    }

    /// D-XZXCE: the confidence of the blocks the version writes, read in the canon the version
    /// would give (`candidate`; fix round 1), with the first reason.
    fn confidence(
        &self,
        candidate: Result<&CandidateCoverage, &RdyError>,
    ) -> Result<String, RdyError> {
        let issues = &candidate.map_err(Clone::clone)?.issues;
        let mut paths = BTreeMap::new();
        for id in &self.touched {
            if let Some(e) = issues.blocks.get(id)
                && !e.issues.is_empty()
                && let Some(p) = e.doc.as_ref().and_then(|d| self.next.path_of(d))
            {
                paths.insert(id.to_string(), p);
            }
        }
        let c = confidence_in(|b| issues.blocks.get(b), &self.touched, None, &paths);
        let level = name_of(&c.confidence);
        Ok(match c.reasons.first() {
            None => level,
            Some(r) => format!("{level}: {} on {}", name_of(&r.code), r.block),
        })
    }

    /// The `confirm` items of the version's exceptions, by ref, answered or not.
    fn exception_items(&self, now: i64) -> Vec<Item> {
        let r = self.r#ref();
        let mut out: Vec<Item> = self
            .decisions
            .iter()
            .filter(|d| !d.kinds.is_empty())
            .map(|d| {
                let mut it = item(ItemKind::Confirm, &d.r#ref, &d.title());
                it.why = text(&d.block, "why").unwrap_or_default().to_string();
                it.batch = Some(r.clone());
                it.exception = Some(d.kinds[0].0);
                it.facts = d
                    .kinds
                    .iter()
                    .map(|(k, detail)| Fact {
                        name: fact_name(*k),
                        value: detail.clone(),
                    })
                    .collect();
                let reason = text(&d.block, "recommend_reason")
                    .unwrap_or_default()
                    .to_string();
                let reject = d.block.field_str("recommend") == Some("reject");
                it.options = vec![
                    option(
                        "confirm",
                        "Confirm",
                        "Lets the batch land with this decision.",
                        (!reject).then(|| reason.clone()),
                    ),
                    option(
                        "reject",
                        "Reject",
                        "The batch waits until its author drops or revises this decision.",
                        reject.then_some(reason),
                    ),
                    later(),
                ];
                self.raised(&mut it, now);
                it
            })
            .collect();
        out.sort_by(|a, b| a.r#ref.cmp(&b.r#ref));
        out
    }
}

/// The open tensions of version `p`: its landing form and the in-flight forms of its changes,
/// as `proposal_tensions` gathers them (a kind judged on the net change only in its landing
/// form), not dismissed, by id.
pub(super) fn open_tensions(st: &ReadState, p: &Proposal) -> Result<Vec<Arc<Tension>>, RdyError> {
    let net = |t: &Tension| crate::tensions::judged_on_the_net_change(t.kind);
    let landing = st.landing_tensions(&p.view, &p.changes)?;
    let mine = st.tensions_of_changes(&p.changes)?;
    let mut seen = BTreeSet::new();
    let mut out: Vec<Arc<Tension>> = landing
        .into_iter()
        .chain(mine.into_iter().filter(|t| !net(t)))
        .filter(|t| seen.insert(t.id) && st.tension_status(t) == TensionStatus::Open)
        .collect();
    out.sort_by_key(|t| t.id);
    Ok(out)
}

/// The exception kinds decision `b` carries, in the closed order, each with its detail. `live`
/// says whether a block is a decision live in canon; `unapplied` whether no content would apply
/// `b` once it lands.
fn exception_kinds(
    b: &Block,
    live: &impl Fn(&BlockId) -> bool,
    tensions: &[Arc<Tension>],
    unapplied: impl FnOnce() -> bool,
) -> Vec<(ExceptionKind, String)> {
    let mut kinds = vec![];
    let mut against: Vec<String> = vec![];
    for (link, word) in [(SUPERSEDES, "supersedes"), (REVOKES, "revokes")] {
        for l in b.links.get(link).into_iter().flatten() {
            if live(&l.target) {
                against.push(format!("{word} {}", l.target));
            }
        }
    }
    let mut sides: BTreeSet<BlockId> = BTreeSet::new();
    for t in tensions.iter().filter(|t| t.kind == TensionKind::Semantic) {
        let blocks = t.blocks();
        if blocks.contains(&b.id) {
            sides.extend(blocks.into_iter().filter(|o| *o != b.id && live(o)));
        }
    }
    against.extend(sides.iter().map(|o| format!("a tension with {o}")));
    if !against.is_empty() {
        kinds.push((ExceptionKind::Against, against.join("; ")));
    }
    if b.field_str("reversibility") == Some("one_way") {
        kinds.push((
            ExceptionKind::OneWay,
            "one_way, declared by the proposer".to_string(),
        ));
    }
    if unapplied() {
        kinds.push((
            ExceptionKind::Unapplied,
            "no content applies it once it lands".to_string(),
        ));
    }
    if b.field_str("recommend") == Some("reject") {
        kinds.push((
            ExceptionKind::NotRecommended,
            "the proposer recommends reject".to_string(),
        ));
    }
    let blocking: Vec<String> = tensions
        .iter()
        .filter(|t| t.severity == TensionSeverity::Blocking && t.blocks().contains(&b.id))
        .map(|t| tension_label(&t.id))
        .collect();
    if !blocking.is_empty() {
        kinds.push((ExceptionKind::BlockingTension, blocking.join(", ")));
    }
    kinds
}

/// The fact that names an exception kind on its `confirm` card.
fn fact_name(k: ExceptionKind) -> &'static str {
    match k {
        ExceptionKind::Against => "Goes against",
        ExceptionKind::OneWay => "Reversibility",
        ExceptionKind::Unapplied => "Applied by",
        ExceptionKind::NotRecommended => "Recommendation",
        ExceptionKind::BlockingTension => "Blocking",
    }
}

/// An item of `kind` with no fields beyond its ref and title.
pub(super) fn item(kind: ItemKind, r#ref: &str, title: &str) -> Item {
    Item {
        r#ref: r#ref.to_string(),
        kind,
        title: title.to_string(),
        why: String::new(),
        blocking: false,
        summary: None,
        summary_by: None,
        facts: vec![],
        members: vec![],
        options: vec![],
        raised_by: String::new(),
        raised_at: String::new(),
        age: String::new(),
        batch: None,
        exception: None,
        card: String::new(),
        card_hash: String::new(),
        removal: None,
        removal_reports: BTreeMap::new(),
    }
}

/// One option with one consequence line (O25).
pub(super) fn option(
    id: &str,
    label: &str,
    consequence: &str,
    recommended: Option<String>,
) -> ItemOption {
    ItemOption {
        id: id.to_string(),
        label: label.to_string(),
        consequences: vec![consequence.to_string()],
        recommended,
    }
}

/// The `later` option every item has (F4).
pub(super) fn later() -> ItemOption {
    option(
        "later",
        "Later",
        "Hides this card until the chat session changes.",
        None,
    )
}

/// `n` and `word`, plural unless `n` is 1.
fn count(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// A computed fact, or the code of the error that kept it from being worked out.
fn worked_out(r: Result<String, RdyError>) -> String {
    r.unwrap_or_else(|e| format!("not worked out ({})", e.code))
}

/// The wire name of a unit enum value.
fn name_of<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pending::card::render;
    use crate::{DecisionInput, Rdy, Session, WriteRequest};
    use rdy_model::{Answer, Role};
    use serde_json::json;

    fn writer() -> Session {
        Session::human("human:qa".into(), Role::Writer)
    }

    /// A local confirm or reject of `item` by the person.
    fn answer(item: &str, act: AnswerAct) -> Answer {
        Answer {
            item: item.into(),
            act,
            option: None,
            label: None,
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

    #[test]
    fn di5_open_exceptions_are_the_exceptions_with_no_confirm() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let w = r
            .write(
                &writer(),
                WriteRequest {
                    view: "work".into(),
                    ops: serde_json::from_value(json!([
                        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
                        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
                        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C holds."}
                    ]))
                    .unwrap(),
                    justification: None,
                    message: String::new(),
                },
            )
            .unwrap();
        let decide = |r: &mut Rdy, statement: &str, reversibility: &str| {
            let d = r
                .decide(
                    &writer(),
                    "work",
                    DecisionInput {
                        parent: w.created["$s"].clone(),
                        statement: statement.into(),
                        reversibility: Some(reversibility.into()),
                        affects: vec![w.created["$c"].clone()],
                        ..Default::default()
                    },
                )
                .unwrap();
            BlockId::parse(&d.created["$decision"]).unwrap()
        };
        decide(&mut r, "Keep C", "two_way");
        let one_way = decide(&mut r, "Never drop C", "one_way");
        let p = r
            .propose_subset(&writer(), "work", "work", None, Some("Adds C."))
            .unwrap();
        let pref = r.resolve_proposal(&p.r#ref).unwrap();
        let open = approval_holds(&r.read(), &pref).unwrap().open;
        assert_eq!(open.len(), 1, "{open:#?}");
        let ex = &open[0];
        assert!(ex.r#ref.starts_with(&format!("{one_way}@")), "{}", ex.r#ref);
        assert_eq!(
            (ex.batch.as_deref(), ex.exception),
            (Some(p.r#ref.as_str()), Some(ExceptionKind::OneWay))
        );
        assert_eq!(ex.card, render(ex, CARD_DEFAULT_TOKENS));
        assert_eq!(ex.card_hash, card_hash(ex).to_string());
        let reopened = |mut r: Rdy, a: &Answer| {
            r.store_mut().put_answer(a).unwrap();
            r.close().unwrap();
            Rdy::open(dir.path()).unwrap()
        };
        // a confirm of the decision's current version closes it
        let r = reopened(r, &answer(&ex.r#ref, AnswerAct::Confirm));
        assert!(approval_holds(&r.read(), &pref).unwrap().open.is_empty());
        // a later reject opens it again: a rejected decision has no confirm (F1, F2)
        let r = reopened(r, &answer(&ex.r#ref, AnswerAct::RejectDecision));
        let holds = approval_holds(&r.read(), &pref).unwrap();
        let open = holds.open;
        assert_eq!(holds.rejected.len(), 1, "{:#?}", holds.rejected);
        assert_eq!(
            open.iter().map(|i| i.r#ref.as_str()).collect::<Vec<_>>(),
            vec![ex.r#ref.as_str()]
        );
    }
}
