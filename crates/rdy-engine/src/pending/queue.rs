//! The queue (D-JV6Z8): what waits for a person, computed from the published state. Nothing is
//! stored for it, so it never falls out of step with the records (p-7X6QY).
//!
//! # Batches (D-YFDG3)
//!
//! Each open proposal's latest version that is ready is one `approve` item, ref `P-x@n`. Its
//! card leads with the author's summary (D-4PG9Y), lists the decisions it confirms (`members`),
//! and gives the computed facts in the closed order of O6: Changes, Justification (rf9, TASK-507
//! (3): the claims of the changes' justifications), Decisions, Supersedes,
//! Re-affirms (D-GV465: each decision it re-affirms that is not `one_way`, `<id> (after <cause
//! list>)`), Reversibility, Consequences (D-NJ2SP), Change plan (D-4JMPH: `<open> open,
//! <closed> closed, <deferred> deferred`, code items included), Tensions (D-45RG6, C7),
//! Coverage (D-C6HHV), Confidence (D-XZXCE), Base and Exceptions. Consequences, Coverage and
//! Confidence are worked out on the canon the version's landing would give, never on its view,
//! which may have moved on. No fact reads an answer. The approve option's reason reads one
//! answer only: while the deferral card has no acceptance, it says the landing waits for each
//! card beside it (p-T69HT); an acceptance gives the reason of the holds that remain.
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
//! A decision the version re-affirms (D-GV465; its `reaffirmed` metadata differs from canon's,
//! read from the block delta, never a message) whose `reversibility` is `one_way` stands beside
//! the batch too, as a `confirm` item of kind `reaffirm_one_way`, ref `<D-id>@<version short>`.
//! Its confirm (`It still holds`) is recommended with the reason this version wrote, or each
//! cause with its reason when they differ; its reject (`Drop the re-affirmation`) records
//! `reject_reaffirm` and makes the version not ready.
//!
//! # Deferrals (D-AAQ9Z)
//!
//! A version whose bound plan defers non-code items stands beside its batch with one `defer`
//! item, ref `defer:P-x@n` ([`super::plan`]), after its exceptions. An `accept_deferrals` answer
//! bound to its current card takes it out of the queue and lets the version land; a
//! `refuse_deferrals` answer makes the version not ready. Every approval waits for it
//! ([`approval_holds`]), after the exception holds. A version with no bound plan (O19) has no
//! such card.
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
//! # Routing (D-TF78H, O10, O11)
//!
//! Under a live policy whose fields read, each item gets its required policy role where it is
//! made ([`crate::policy::roles::required_role`]). An item bound to a version routes by that
//! version's candidate, so the version's approve item and its cards name one role. The queue and
//! its counts for a person leave out each item whose role does not hold that person, after a
//! snooze. An answer still binds every item ([`Eligible::role_of`]), and it refuses a person
//! outside the role. With no policy, or none that applies, nothing is left out.
//!
//! # Readiness (D-3JZP4, D-ME2HD; closed)
//!
//! The queue runs the one readiness ([`crate::readiness`]) on each latest open version, read
//! once with its prospective canon: the landing's checks (no conflict on a block it writes, no
//! blocking tension it would put into canon, no error in the canon it would give;
//! `gate::landing_readiness`), each change that touches stable content records a briefing
//! (check 1, O3), it carries a summary, its bound plan can be read, its change plan has no open
//! non-code item on stable content and no unanswered tension item (`E_PLAN_OPEN`, D-FQ7AK,
//! D-45RG6), its reach is within its bound (O9), and no decision or one_way re-affirmation of it
//! was rejected and no deferral card of it refused (the later refusals: F1, D-AAQ9Z). A version
//! that is not ready is no item: it is listed under `not_ready`, one issue per failed check, for
//! the agent and never on a card. Every approval path (a direct approval, `rdy answer`,
//! `rdy_answer` and `all_recommended`) refuses a version that is not ready with
//! `E_PROPOSAL_NOT_READY`, whose issues are these, in this order ([`approval_readiness`],
//! D-ME2HD).
//!
//! # The approve card (D-ME2HD, D-45RG6, O16)
//!
//! A ready version's `approve` item takes its facts from the assessment propose stored, so a
//! later change to a live fact (the base moving, say) leaves the card as the person was shown it.
//! Its options stay live. A ready version with no stored assessment is not ready: it is listed
//! under `not_ready` with the one issue of O10. The Tensions fact lists each advisory tension
//! with the answer of the bound plan (`(confirm: <reason>)`, `(dismiss: <reason>)` or `(no
//! answer)`).
//!
//! # Answers (O23)
//!
//! The queue reads the answers (`answer/<item>`): a `reject_proposal` answer to `P-x@n` takes the
//! version and everything beside it out of the queue; a `confirm` answer to `<D-id>@<version>`
//! takes that item out; a `reject_decision` answer makes the batch not ready, and every approval
//! path refuses it with `E_PROPOSAL_NOT_READY`, whether the decision is an exception now or not
//! ([`approval_readiness`]); an
//! `answer_question` answer to `<Q-id>@<version>` takes the question out (its decision waits for
//! a landing).
//!
//! # Order (O5)
//!
//! Blocking items first; then batches, each with its items directly after it (the `approve`
//! item, its exceptions by ref, its deferral card, then its advice); then the re-affirmation
//! cards ([`super::reaffirm`], D-CQMA4), each a group of its own; then the other items that stand
//! alone (questions);
//! each kind by `raised_at`, oldest first; then by ref. At most [`PENDING_ITEMS_MAX`] items are
//! listed, whole groups only; `omitted` counts the rest.
//!
//! # Re-affirmation cards (D-CQMA4)
//!
//! Each cause that made accepted decisions of canon suspect is one `reaffirm` item
//! ([`super::reaffirm`]). The latest open versions read here (a version a `reject_proposal`
//! answer took out aside) name the decisions they re-affirm or fully retire, which no card
//! lists.
use super::card::{
    CARD_DEFAULT_TOKENS, CARD_MAX_TOKENS, REMOVAL_IRREVERSIBLE, card_hash, render_checked,
};
use super::item::{ExceptionKind, Fact, Item, ItemKind, ItemOption, Recommended, RemovalCard, age};
use super::plan::{PlanCardBinding, VersionPlan, defer_ref, deferral_item, version_plan};
use super::questions::question_items;
use super::reaffirm::{OpenWork, reaffirm_items};
use super::snooze::hide_snoozed;
use super::tensions::tension_items;
use crate::coverage::change::{candidate_build, candidate_change};
use crate::coverage::{CandidateCoverage, confidence_in, lands_unapplied};
use crate::error::{E_VALIDATION, RdyError, redaction_limit};
use crate::gate::{ProposalRef, changes_of, open_proposals};
use crate::plan::lifecycle::PlanSummary;
use crate::policy::roles::{ItemFacts, RequiredRole, holds, required_role};
use crate::policy_verdict::{ChangeFacts, VerdictInputs, WrittenBlock, WrittenDecision};
use crate::read_state::Built;
use crate::readiness::assessment::cut_text;
use crate::readiness::{Candidate, PlanSource, PolicyChange, Readiness};
use crate::session::{now_ms, rfc3339};
use crate::tensions::{Tension, TensionKind, TensionSeverity, TensionStatus, tension_label};
use crate::{Materialized, ReadState, Scope};
use rdy_graph::{DerivedStatus, Graph};
use rdy_model::assessment::{
    AssessedPolicy, Assessment, AssessmentCoverage, AssessmentExceptionKind, AssessmentIssue,
    AssessmentPage, AssessmentScoreChange, MAX_TEXT_BYTES,
};
use rdy_model::decision::{self, DEPENDS_ON, DecisionStatus, REVOKES, SUPERSEDES};
use rdy_model::engine_fields::reaffirmed_causes;
use rdy_model::redaction::record_buffer;
use rdy_model::{AnswerAct, Block, BlockId, Change, Hash, Justification, Op, Proposal};
use rdy_schema::policy::{JustificationKind, POLICY_TYPE, PolicyFields};
use rdy_schema::{Issue, Severity, supporting};
use rdy_store::redaction::limit;
use serde::Serialize;
use std::cell::OnceCell;
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
/// too, less those a snooze hides and those routed to other people (final review fix round 1,
/// R6: the session's summary counts the queue, not its first page).
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
    /// The policy role each item needs, by [`role_key`] (O10): only the items whose role is
    /// `Some`, and none with no live policy or one whose fields do not read.
    roles: Roles,
}

/// The policy role of each item of a queue that needs one, by [`role_key`].
type Roles = BTreeMap<(String, Option<String>), RequiredRole>;

/// An item's key in [`Roles`]: its ref and its batch. A decision version beside two batches has
/// two items of one ref, each with its own batch.
fn role_key(i: &Item) -> (String, Option<String>) {
    (i.r#ref.clone(), i.batch.clone())
}

/// Whether `person` may see item `i` (D-TF78H): its required role holds the person, or it needs
/// none.
fn routed(roles: &Roles, i: &Item, person: &str) -> bool {
    roles
        .get(&role_key(i))
        .is_none_or(|role| holds(role, person))
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

    /// The policy role item `i` of this queue needs (O10), if any.
    pub(crate) fn role_of(&self, i: &Item) -> Option<&RequiredRole> {
        self.roles.get(&role_key(i))
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
            // D-TF78H: the items whose required policy role does not hold the person are not
            // theirs
            let shown: Vec<Item> = shown
                .into_iter()
                .filter(|i| routed(&e.roles, i, &q.person))
                .collect();
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
    /// past the cap too, less what a snooze hides and what is routed to other people
    /// ([`PendingCounts`]). It renders no card.
    pub fn pending_counts(&self, q: &PendingQuery) -> Result<PendingCounts, RdyError> {
        let e = self.eligible(q)?;
        let mut c = PendingCounts::default();
        for g in e.groups {
            let (shown, _) = hide_snoozed(self, q, g.items);
            for i in shown.iter().filter(|i| routed(&e.roles, i, &q.person)) {
                c.items += 1;
                c.blocking += usize::from(i.blocking);
            }
        }
        Ok(c)
    }

    /// Every item of the queue for `q`, in order and in its groups, before a snooze or the cap
    /// ([`Eligible`]). The open proposals are read once, and the tension items share them.
    pub(crate) fn eligible(&self, q: &PendingQuery) -> Result<Eligible, RdyError> {
        let now = now_ms();
        let mut groups: Vec<Group> = vec![];
        let mut not_ready = vec![];
        let mut work = OpenWork::default();
        let mut routing = Routing::read(self)?;
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
            v.open_work(&mut work);
            let ready = v.readiness_with_card(self, now)?;
            let Some(mut card) = ready.card else {
                not_ready.push(NotReady {
                    r#ref: r,
                    issues: ready.issues,
                });
                continue;
            };
            // D-ME2HD, O16: the approve card is drawn from the stored assessment, its facts as
            // propose showed them, except Base, which stays live (I-d); a ready version with no
            // stored assessment is not ready (O10, C2)
            match stored_facts(self, &v)? {
                Some(facts) => card.facts = facts,
                None => {
                    not_ready.push(NotReady {
                        issues: vec![crate::gate::no_stored_assessment(pref)],
                        r#ref: r,
                    });
                    continue;
                }
            }
            if let Some(rt) = routing.as_mut() {
                rt.note(self, &card, Some(&v.next), v.approve_facts());
            }
            let mut items = vec![card];
            for item in v.exception_items(now) {
                if !answered(self, &item.r#ref, AnswerAct::Confirm)? {
                    if let Some(rt) = routing.as_mut() {
                        rt.note(self, &item, Some(&v.next), v.confirm_facts(&item));
                    }
                    items.push(item);
                }
            }
            // D-AAQ9Z: the deferral card follows the exceptions; an accepted one is no item
            if let Some(item) = v.deferral_item(now)
                && accepted(self, &item)?.is_none()
            {
                if let Some(rt) = routing.as_mut() {
                    rt.note(self, &item, Some(&v.next), v.defer_facts());
                }
                items.push(item);
            }
            if let Some(rt) = routing.as_mut() {
                rt.batches
                    .insert(r.clone(), (v.writes_decision(), Arc::clone(&v.next)));
            }
            groups.push(Group { items });
        }
        // D-CQMA4: each re-affirmation card is a group of its own
        for item in reaffirm_items(self, &work, now)? {
            if let Some(rt) = routing.as_mut() {
                let facts = rt.reaffirm_facts(&item);
                rt.note(self, &item, None, facts);
            }
            groups.push(Group { items: vec![item] });
        }
        // the items that stand beside a batch go into its group, after its exceptions; the
        // others are groups of one
        let batches: Vec<Item> = groups.iter().map(|g| g.items[0].clone()).collect();
        let questions = question_items(self, q)?.into_iter().map(|i| {
            // O10: an answer item covers its question block, which its ref names
            let block = i
                .r#ref
                .split_once('@')
                .and_then(|(id, _)| BlockId::parse(id).ok());
            (i, block.into_iter().collect())
        });
        let others = tension_items(self, &open, &batches)?
            .into_iter()
            .chain(questions);
        for (item, blocks) in others {
            if let Some(rt) = routing.as_mut() {
                let facts = rt.other_facts(&item, blocks);
                let candidate = rt.candidate_of(&item);
                rt.note(self, &item, candidate.as_deref(), facts);
            }
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
        Ok(Eligible {
            groups,
            not_ready,
            roles: routing.map(|rt| rt.roles).unwrap_or_default(),
        })
    }
}

/// The live policy's routing of one queue (O10, D-TF78H): the policy's fields, canon at its head
/// (where the policy lives), the role of each item noted so far, and for each ready batch
/// whether its version writes a decision and the canon it would give.
struct Routing {
    canon: Arc<Materialized>,
    fields: PolicyFields,
    roles: Roles,
    batches: BTreeMap<String, (bool, Arc<Materialized>)>,
}

impl Routing {
    /// The routing under the live policy (the policy block of canon at its head); none with no
    /// policy, or one whose fields do not read: then nothing routes.
    fn read(st: &ReadState) -> Result<Option<Routing>, RdyError> {
        policy_of(st).map(|p| {
            p.map(|(canon, fields)| Routing {
                canon,
                fields,
                roles: Roles::new(),
                batches: BTreeMap::new(),
            })
        })
    }

    /// Notes the role of item `i`, whose facts are `facts` (O10), when it needs one. `prospective`:
    /// the version's candidate, for every item bound to a version ([`required_role`]).
    fn note(
        &mut self,
        st: &ReadState,
        i: &Item,
        prospective: Option<&Materialized>,
        facts: ItemFacts,
    ) {
        if let Some(role) = required_role(st, &self.canon, prospective, &self.fields, &facts) {
            self.roles.insert(role_key(i), role);
        }
    }

    /// The facts of re-affirmation card `i` (O10): its member decisions; `one_way` when one of
    /// them is declared `one_way` in canon. It belongs to no version.
    fn reaffirm_facts(&self, i: &Item) -> ItemFacts {
        let blocks: Vec<BlockId> = match &i.plan_binding {
            Some(PlanCardBinding::Reaffirm { members, .. }) => {
                members.iter().map(|m| m.id.clone()).collect()
            }
            _ => vec![],
        };
        ItemFacts {
            kind: i.kind,
            one_way: one_way_among(&self.canon, &blocks),
            blocks,
            decision: false,
        }
    }

    /// The candidate of the ready version item `i` stands beside, if any: an advice item's.
    fn candidate_of(&self, i: &Item) -> Option<Arc<Materialized>> {
        let (_, next) = self.batches.get(i.batch.as_ref()?)?;
        Some(Arc::clone(next))
    }

    /// The facts of a `resolve` or `answer` item `i` that covers `blocks` (O10). An advice item
    /// belongs to its batch's version: `one_way` reads the canon that version would give, and
    /// `decision` whether it writes a decision. Any other belongs to no version: `one_way` reads
    /// canon.
    fn other_facts(&self, i: &Item, blocks: Vec<BlockId>) -> ItemFacts {
        let (decision, m) = match i.batch.as_ref().and_then(|b| self.batches.get(b)) {
            Some((decision, next)) => (*decision, &**next),
            None => (false, &*self.canon),
        };
        ItemFacts {
            kind: i.kind,
            one_way: one_way_among(m, &blocks),
            blocks,
            decision,
        }
    }
}

/// The live policy's fields and canon at its head: none with no policy block in canon, or one
/// whose fields do not read (then no role applies).
fn policy_of(st: &ReadState) -> Result<Option<(Arc<Materialized>, PolicyFields)>, RdyError> {
    let canon = st.materialized(&Scope::Canon)?;
    let fields = crate::policy::live_policy(st, &canon).and_then(|l| l.fields);
    Ok(fields.map(|f| (canon, f)))
}

/// Whether one of `blocks` is a decision of `m` declared `one_way`.
fn one_way_among(m: &Materialized, blocks: &[BlockId]) -> bool {
    blocks.iter().any(|id| {
        m.get(id).is_some_and(|b| {
            b.block_type == decision::DECISION && b.field_str("reversibility") == Some("one_way")
        })
    })
}

/// The policy role the `approve` item of version `pref` (record `p`) needs (O10, D-TF78H), under
/// the live policy: none with no policy, or none that applies. Every approval of the version
/// meets it in its landing.
pub(crate) fn approve_role(
    st: &ReadState,
    pref: &ProposalRef,
    p: &Proposal,
) -> Result<Option<RequiredRole>, RdyError> {
    let Some((canon, fields)) = policy_of(st)? else {
        return Ok(None);
    };
    let v = Version::read_for(st, &Candidate::stored(st, *pref, p.clone())?, Removal::Skip)?;
    Ok(required_role(
        st,
        &canon,
        Some(&v.next),
        &fields,
        &v.approve_facts(),
    ))
}

/// The policy role a deferral card `i` needs (O10), when the card is bound outside the queue
/// ([`deferral_card`]): none with no policy, or none that applies.
pub(crate) fn deferral_role(st: &ReadState, i: &Item) -> Result<Option<RequiredRole>, RdyError> {
    let Some((canon, fields)) = policy_of(st)? else {
        return Ok(None);
    };
    let Some(batch) = i.batch.as_deref() else {
        return Ok(None);
    };
    let pref = crate::gate::resolve_in(st, batch)?;
    let p = st.reader().proposal(&pref.version)?;
    let v = Version::read_for(st, &Candidate::stored(st, pref, p)?, Removal::Skip)?;
    Ok(required_role(
        st,
        &canon,
        Some(&v.next),
        &fields,
        &v.defer_facts(),
    ))
}

/// What holds version `pref` back from landing (F2, D-AAQ9Z), read once for the landing. A
/// rejected decision or re-affirmation and refused deferrals are no hold: readiness refuses them
/// first ([`approval_readiness`], D-ME2HD).
pub(crate) struct Holds {
    /// The exception items that have no `confirm` answer bound to the decision's current
    /// version (F2). A rejected re-affirmation's item is not among them: readiness refuses its
    /// version. Each card is cut to [`CARD_DEFAULT_TOKENS`].
    pub open: Vec<Item>,
    /// The version's deferral card while no acceptance is bound to it (D-AAQ9Z), cut to
    /// [`CARD_DEFAULT_TOKENS`], and how many deferrals it lists.
    pub deferral: Option<(Item, usize)>,
    /// The `AcceptDeferrals` ANSWER bound to the version's current deferral card, if any.
    pub accepted: Option<Hash>,
}

/// What holds version `pref` back from landing ([`Holds`]). It does not ask whether the version
/// is ready; the landing asks [`approval_readiness`] first. `land_with` calls it (Task 17), so
/// every approval path waits. Only a version that writes a decision (`decides`) can carry an
/// exception, so only it reads the version whole; any bound version reads its change plan for
/// the deferral card. A bound version whose change plan cannot be read is that error: its
/// deferrals cannot be known (fail closed). Readiness reports such a plan first, so this return
/// stays only as a guard.
pub(crate) fn approval_holds(
    st: &ReadState,
    pref: &ProposalRef,
    decides: bool,
) -> Result<Holds, RdyError> {
    let p = st.reader().proposal(&pref.version)?;
    let mut open = vec![];
    // the clock is read only where a card is built: a version that writes no decision and has
    // no deferral card reads it never, as before D-AAQ9Z, so a landing's stamps (and with them
    // every later id and hash of a pinned clock) do not move
    let card = if decides {
        let v = Version::read(st, *pref, p)?;
        let now = now_ms();
        if v.plan_bound.is_some()
            && let Err(e) = &v.plan
        {
            return Err(e.clone());
        }
        for item in v.exception_items(now) {
            if answered(st, &item.r#ref, AnswerAct::Confirm)? {
                continue;
            }
            // a rejected re-affirmation has its answer: readiness refuses its version with the
            // issue the queue gives, never as an open card
            if item.exception == Some(ExceptionKind::ReaffirmOneWay)
                && answered(st, &item.r#ref, AnswerAct::RejectReaffirm)?
            {
                continue;
            }
            open.push(finish(item, CARD_DEFAULT_TOKENS)?);
        }
        v.deferral_item(now)
    } else {
        bound_deferral_item(st, pref, &p, now_ms)?.flatten()
    };
    let (mut deferral, mut accepted_by) = (None, None);
    if let Some(item) = card {
        match accepted(st, &item)? {
            Some(h) => accepted_by = Some(h),
            None => {
                let n = item.facts.len();
                deferral = Some((finish(item, CARD_DEFAULT_TOKENS)?, n));
            }
        }
    }
    Ok(Holds {
        open,
        deferral,
        accepted: accepted_by,
    })
}

/// The facts of the approve card version `v` stored in its assessment (O16): each line
/// `<name>: <value>` split at its first `: `, in the card's order; none when the version has no
/// stored assessment. The Base fact is the exception: it stays live (I-d), read now against the
/// canon head, so a landing after propose shows `moved` and moves the card's hash.
fn stored_facts(st: &ReadState, v: &Version) -> Result<Option<Vec<Fact>>, RdyError> {
    let reader = st.reader();
    let Some(h) = reader.assessment_of_proposal(&v.pref.version, st.seq())? else {
        return Ok(None);
    };
    let live_base = v.base_fact(st);
    let facts = reader
        .assessment(&h)?
        .card
        .facts
        .into_iter()
        .map(|line| match line.split_once(": ") {
            Some((name, _)) if name == BASE_FACT => live_base.clone(),
            Some((name, value)) => Fact {
                name: name.to_string().into(),
                value: value.to_string(),
            },
            None => Fact {
                name: line.into(),
                value: String::new(),
            },
        })
        .collect();
    Ok(Some(facts))
}

/// The readiness issues of version `pref` (record `p`), in the queue's order: the issues the
/// queue lists under `not_ready` for it. Empty when the version is ready. Every approval path
/// refuses a version that is not ready with them (D-ME2HD, [`crate::gate::proposal_not_ready`]).
/// It is the queue's one readiness ([`Version::readiness`]) on the same published state. The
/// version is read without its removal cards: readiness never reads them, and the landing's own
/// freshness check computes each removal report once.
#[cfg(test)]
pub(crate) fn approval_readiness(
    st: &ReadState,
    pref: &ProposalRef,
    p: Proposal,
) -> Result<Vec<Issue>, RdyError> {
    let c = Candidate::stored(st, *pref, p)?;
    Version::read_for(st, &c, Removal::Skip)?.readiness(st)
}

/// The readiness of candidate `c` ([`crate::readiness::readiness`]): the version read once with
/// its removal cards, the checks, and its `approve` item when it is ready, as the queue lists it.
pub(crate) fn readiness_of(st: &ReadState, c: &Candidate) -> Result<Readiness, RdyError> {
    Version::read_for(st, c, Removal::Read)?.readiness_with_card(st, now_ms())
}

/// The readiness of candidate `c` and, when it is ready, its assessment
/// ([`crate::readiness::assess`]): the version read once with its removal cards, the checks, and
/// for a ready version its `approve` item and the record built from what that read gave
/// (D-SDSV2). A record the store would refuse (O9) is the version's one issue: it is not ready.
pub(crate) fn assessed(
    st: &ReadState,
    c: &Candidate,
    gate: &crate::readiness::assessment::GateConfig,
) -> Result<(Readiness, Option<Assessment>), RdyError> {
    let v = Version::read_for(st, c, Removal::Read)?;
    let issues = v.readiness(st)?;
    let summary = v
        .plan
        .as_ref()
        .map(|p| p.summary.clone())
        .unwrap_or_default();
    let (warnings, checks_omitted) = v.rule_warnings(st)?;
    if !issues.is_empty() {
        let r = Readiness {
            issues,
            summary,
            card: None,
            policy: None,
            warnings,
            checks_omitted,
        };
        return Ok((r, None));
    }
    // the verdict comes before the card, which shows it as its `Policy` fact
    let policy = v.policy(st)?;
    let candidate = v.candidate(st);
    let consequences = v.consequence_count(st);
    let card = v.batch_item_with(
        st,
        now_ms(),
        candidate,
        consequences.as_ref().copied(),
        Some(&policy),
    );
    let built = v.assessment(
        st,
        c,
        &card,
        &summary,
        candidate.map_err(Clone::clone)?,
        consequences?,
        gate,
        &policy,
    )?;
    Ok(match built {
        Ok(a) => (
            Readiness {
                issues,
                summary,
                card: Some(card),
                policy: Some(policy),
                warnings,
                checks_omitted,
            },
            Some(a),
        ),
        Err(issue) => (
            Readiness {
                issues: vec![issue],
                summary,
                card: None,
                policy: None,
                warnings,
                checks_omitted,
            },
            None,
        ),
    })
}

/// The gate score (O17) of page `doc` in `m`, its blocks scored by `entry`; none when no block of
/// the page is scored.
fn page_gate_score<'e>(
    pack: &rdy_schema::Pack,
    m: &Materialized,
    doc: &BlockId,
    entry: impl Fn(&BlockId) -> Option<&'e crate::coverage::BlockEntry>,
) -> Option<u32> {
    let blocks = crate::coverage::page_blocks(m, doc);
    let entries: Vec<(&BlockId, &crate::coverage::BlockEntry)> = blocks
        .iter()
        .filter_map(|id| Some((id, entry(id)?)))
        .collect();
    (!entries.is_empty()).then(|| crate::coverage::gate_rollup(pack, m, entries.into_iter()).score)
}

/// O19: each `authority_policy` block that `changes` (whose hashes are `hashes`, in the version's
/// order) create, update or delete, in id order, with the last change that writes it and whether
/// that change is `implements` with at least one decision of `next`, the prospective canon. A
/// deleted block's type is read in `canon`.
fn policy_changes(
    hashes: &[Hash],
    changes: &[Change],
    canon: &Materialized,
    next: &Materialized,
) -> Vec<PolicyChange> {
    let mut last: BTreeMap<BlockId, (Hash, &Change)> = BTreeMap::new();
    for (h, c) in hashes.iter().zip(changes) {
        for op in &c.ops {
            let policy = match op {
                Op::Create { block } | Op::Update { block, .. } => block.block_type == POLICY_TYPE,
                Op::Delete { id, .. } => canon.get(id).is_some_and(|b| b.block_type == POLICY_TYPE),
            };
            if policy {
                last.insert(op.block_id().clone(), (*h, c));
            }
        }
    }
    last.into_iter()
        .map(|(block, (h, c))| PolicyChange {
            block,
            short: h.short(),
            implements: matches!(&c.justification, Some(Justification::Implements(ids))
            if ids.iter().any(|d| {
                next.get(d).is_some_and(|b| b.block_type == decision::DECISION)
            })),
        })
        .collect()
}

/// The record kind of an exception kind.
fn exception_record(k: ExceptionKind) -> AssessmentExceptionKind {
    match k {
        ExceptionKind::Against => AssessmentExceptionKind::Against,
        ExceptionKind::OneWay => AssessmentExceptionKind::OneWay,
        ExceptionKind::Unapplied => AssessmentExceptionKind::Unapplied,
        ExceptionKind::NotRecommended => AssessmentExceptionKind::NotRecommended,
        ExceptionKind::BlockingTension => AssessmentExceptionKind::BlockingTension,
        ExceptionKind::ReaffirmOneWay => AssessmentExceptionKind::ReaffirmOneWay,
    }
}

/// Tests only ([`crate::readiness::reach_for_tests`]): the reach readiness's check 6 reads for
/// candidate `c`.
pub(crate) fn reach_of(st: &ReadState, c: &Candidate) -> Result<BTreeSet<BlockId>, RdyError> {
    let v = Version::read_for(st, c, Removal::Skip)?;
    let conflicted: BTreeSet<BlockId> =
        crate::gate::blocking_conflicts(st, &v.p.view, &v.p.changes)?
            .iter()
            .map(|c| c.block.clone())
            .collect();
    let checks = v.checks(st)?;
    crate::readiness::reach(st, &v.checked(checks), &conflicted)?
        .map_err(|i| RdyError::new(i.code, i.message, i.fix))
}

/// The deferral card of version `pref` (whose record is `p`), answered or not, read from its
/// bound plan alone: `None` for a version with no bound plan (O19), `Some(None)` for a bound
/// version with no deferred non-code item. A bound version whose plan cannot be read is that
/// error (fail closed). The card is the one the queue builds ([`Version::deferral_item`]). `now`
/// reads the clock for the card's age, only when there is a card.
fn bound_deferral_item(
    st: &ReadState,
    pref: &ProposalRef,
    p: &Proposal,
    now: impl FnOnce() -> i64,
) -> Result<Option<Option<Item>>, RdyError> {
    let Some(bound) = st.reader().plan_of_proposal(&pref.version, st.seq())? else {
        return Ok(None);
    };
    let plan = version_plan(st, &pref.version, p, &PlanSource::Bound(Some(bound)))?;
    Ok(Some(deferral_item(pref, &plan).map(|mut it| {
        raised_from(p, &mut it, now());
        it
    })))
}

/// The deferral card the explicit ref `r` (`defer:P-x@n`) names, its card whole, whether or not
/// its version is ready and whether or not the card has an answer (D-AAQ9Z). A
/// refusal makes the version not ready, so its group leaves the queue, yet the person's later
/// answer to the same card replaces it: the latest answer wins. The ref must name, as the queue
/// writes it, an open proposal's latest version that no `reject_proposal` answer took out; any
/// other ref, or a version with no deferral card, gives `None`. The answer that binds it makes
/// every check it makes on a queue card: the card hash, the person and the relay.
pub(crate) fn deferral_card(st: &ReadState, r: &str) -> Result<Option<Item>, RdyError> {
    let Some(want) = r.strip_prefix("defer:") else {
        return Ok(None);
    };
    let Some((pref, p)) = open_proposals(st)?
        .into_iter()
        .find(|(pref, _)| pref.to_string() == want)
    else {
        return Ok(None);
    };
    if answered(st, want, AnswerAct::RejectProposal)? {
        return Ok(None);
    }
    bound_deferral_item(st, &pref, &p, now_ms)?
        .flatten()
        .map(|i| finish(i, CARD_MAX_TOKENS))
        .transpose()
}

/// The issue of a deferral card of `pref` the person refused (D-AAQ9Z), if its latest answer
/// refuses it: the version is not ready, and a refusal is never an acceptance.
pub(crate) fn deferrals_refused(
    st: &ReadState,
    pref: &ProposalRef,
) -> Result<Option<Issue>, RdyError> {
    if !answered(st, &defer_ref(pref), AnswerAct::RefuseDeferrals)? {
        return Ok(None);
    }
    Ok(Some(Issue {
        severity: Severity::Error,
        code: E_VALIDATION,
        block: None,
        message: format!("the deferrals of {pref} were refused"),
        fix: "close the items and propose again".into(),
    }))
}

/// The `AcceptDeferrals` ANSWER bound to deferral card `item`: the latest answer to its ref,
/// when its act accepts and it names the card's current complete hash. Any other latest answer
/// (none, a refusal, a stale card) binds none.
fn accepted(st: &ReadState, item: &Item) -> Result<Option<Hash>, RdyError> {
    let Some((h, a)) = st.reader().answer_of(&item.r#ref)? else {
        return Ok(None);
    };
    let bound = a.act == AnswerAct::AcceptDeferrals && a.card_hash == Some(card_hash(item));
    Ok(bound.then_some(h))
}

/// The `approve` item of version `pref` (whose record is `p`) with its card whole at
/// [`CARD_MAX_TOKENS`], when a change of the version carries a removal request; `None` when none
/// does (D-CC90E). It reads that version alone: neither readiness, an answer, a snooze nor the
/// queue's cap hides it, and no other version's card is rendered, so another card's budget never
/// refuses it. The item is the one the queue builds; a version that is not ready says why in its
/// `why` line instead of `ready to land`. A card whose mandatory removal lines do not fit is
/// `E_REDACTION_BUDGET` ([`render_checked`]). A direct approval shows this card before it lands. Its
/// facts are the stored assessment's, as the queue's are (O16).
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
    let mut item = v.batch_item(st, now_ms(), None);
    // the item the queue builds draws its facts from the stored assessment (O16); a version with
    // none (O10) shows the facts as read now
    if let Some(facts) = stored_facts(st, &v)? {
        item.facts = facts;
    }
    let issues = v.readiness(st)?;
    if !issues.is_empty() {
        let why: Vec<&str> = issues.iter().map(|i| i.message.as_str()).collect();
        item.why = format!("not ready to land: {}", why.join("; "));
        // D-D8FAB: the approve reason says its checks pass; a version that is not ready has no
        // recommended option.
        for o in &mut item.options {
            o.recommended = None;
        }
    }
    finish(item, CARD_MAX_TOKENS).map(Some)
}

/// Whether the latest answer to `item` is of act `act`.
pub(crate) fn answered(st: &ReadState, item: &str, act: AnswerAct) -> Result<bool, RdyError> {
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

/// Who raised the items of version `p`, when, and how long ago.
fn raised_from(p: &Proposal, it: &mut Item, now: i64) {
    let at = p.provenance.at_ms;
    it.raised_by = p.provenance.actor.clone();
    it.raised_at = rfc3339(at);
    it.age = age(at, now);
}

/// One batch and the items beside it, or one item on its own.
struct Group {
    items: Vec<Item>,
}

impl Group {
    /// O5: blocking first, then batches, then re-affirmation cards (D-CQMA4), then the other
    /// items, each by `raised_at`, then by ref (of the first item).
    fn key(&self) -> (bool, u8, String, String) {
        let head = &self.items[0];
        let rank = match head.kind {
            ItemKind::Approve => 0,
            ItemKind::Reaffirm => 1,
            _ => 2,
        };
        (
            !head.blocking,
            rank,
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

/// A decision the version re-affirms (D-GV465): its block in the prospective canon, its item
/// ref, whether it is `one_way`, and each cause whose re-affirmation the version writes, with
/// the reason it wrote, in cause-id order. Read from the block delta, never from a message.
struct Reaffirmed {
    block: Block,
    r#ref: String,
    one_way: bool,
    causes: Vec<(BlockId, String)>,
}

impl Reaffirmed {
    /// The causes, joined.
    fn cause_list(&self) -> String {
        let ids: Vec<String> = self.causes.iter().map(|(c, _)| c.to_string()).collect();
        ids.join(", ")
    }

    /// The reason its confirm is recommended with: the newly written reason, or, when the
    /// version writes different reasons for its causes, `<cause>: <reason>` each, in cause-id
    /// order, joined with `; ` ([`Recommended::new`] cuts it).
    fn reason(&self) -> String {
        let first = self.causes.first().map(|(_, r)| r.as_str()).unwrap_or("");
        if self.causes.iter().all(|(_, r)| r == first) {
            return first.to_string();
        }
        let each: Vec<String> = self
            .causes
            .iter()
            .map(|(c, r)| format!("{c}: {r}"))
            .collect();
        each.join("; ")
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
    /// The decisions the version re-affirms, by id.
    reaffirms: Vec<Reaffirmed>,
    /// The change plan the version's record binds (none before store 17, O19).
    plan_bound: Option<Hash>,
    /// The version's change plan, as its cards and its readiness read it.
    plan: Result<VersionPlan, RdyError>,
    /// The plan blocks of a version whose change plan cannot be read: none.
    no_blocks: BTreeSet<BlockId>,
    /// O19: each authority_policy block the version writes, with the last change that writes it.
    policy_changes: Vec<PolicyChange>,
    /// The coverage of the canon the version would give, worked out once on first use.
    candidate: OnceCell<Result<CandidateCoverage, RdyError>>,
    /// The landing projection's facts and the pack rules' scan, worked out once on first use.
    checks: OnceCell<Result<Checks, RdyError>>,
}

/// The derived sets a version's rules read: suspects, tension blocks, coverage scores.
type RuleSets = (BTreeSet<BlockId>, BTreeSet<BlockId>, BTreeMap<BlockId, u32>);

/// Tests only ([`crate::readiness::rule_inputs_for_tests`]): the derived sets the pack rules of
/// candidate `c` read, in its landing projection.
pub(crate) fn rule_inputs_of(
    st: &ReadState,
    c: &Candidate,
) -> Result<crate::readiness::RuleInputs, RdyError> {
    let v = Version::read_for(st, c, Removal::Skip)?;
    let compiled = crate::check::rules::compiled(st.pack())?;
    let projection = crate::check::landing_projection(st, &v.pref, &v.p, &v.next)?;
    let built = v.built.after(&projection.updates, st.pack());
    let graph = Arc::clone(built.graph.get_or_init(|| {
        Arc::new(Graph::build_with(
            built.m.iter(),
            Arc::clone(st.reader().tracking()),
        ))
    }));
    let all = compiled
        .into_iter()
        .map(|mut r| {
            // every set, whatever the pack's rules need
            r.require = rdy_schema::filter::parse(
                "suspect = true or tension = true or coverage >= 0",
                st.pack(),
            )
            .expect("the test filter parses");
            r
        })
        .collect::<Vec<_>>();
    let (suspects, tensions, coverage) = v.rule_sets(st, &built, &graph, &all)?;
    Ok(crate::readiness::RuleInputs {
        suspects,
        tensions,
        coverage,
    })
}

/// What a version's landing projection and its pack rules give readiness (D-JQD5X), at
/// propose: item 6's issues, the advisory rows, the rows left out, the reach the rules add and
/// the considered options of a choice.
struct Checks {
    /// Readiness item 6: the projection's refusal (`E_ALT_DECISION`, or a view that moved on),
    /// then each kept `E_RULE_CHECK` row.
    issues: Vec<Issue>,
    /// The kept `W_RULE_CHECK` rows, in rule then block order.
    warnings: Vec<crate::check::RuleIssue>,
    /// The blocking and advisory rows left out past the cut.
    omitted: (usize, usize),
    /// The reach the rules add (TASK-946, D-DSPMW); Err: the "too large to assess" issue.
    reach: Result<BTreeSet<BlockId>, Issue>,
    considered: Option<crate::check::ConsideredOptions>,
}

impl Checks {
    /// A version with no rule and no choice: nothing to add.
    fn none() -> Checks {
        Checks {
            issues: vec![],
            warnings: vec![],
            omitted: (0, 0),
            reach: Ok(BTreeSet::new()),
            considered: None,
        }
    }
}

/// Whether [`Version::read_for`] reads the removal cards of a version.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Removal {
    /// Reads them, with the freshness check of each request (`removal_cards`): the cards need
    /// them.
    Read,
    /// Leaves them out (`removal` none): readiness never reads them.
    Skip,
}

impl Version {
    fn read(st: &ReadState, pref: ProposalRef, p: Proposal) -> Result<Version, RdyError> {
        Version::read_for(st, &Candidate::stored(st, pref, p)?, Removal::Read)
    }

    /// Candidate `c` read once: its changes, its prospective canon, its decisions and
    /// re-affirmations, its tensions, and its change plan from `c.plan` (the store's bound plan,
    /// or one in hand that the store does not hold yet).
    fn read_for(st: &ReadState, c: &Candidate, removal: Removal) -> Result<Version, RdyError> {
        let (pref, p) = (c.pref, c.p.clone());
        let changes = changes_of(st, &p.changes)?;
        let touched: BTreeSet<BlockId> = changes
            .iter()
            .flat_map(|c| c.ops.iter().map(|o| o.block_id().clone()))
            .collect();
        let tensions = open_tensions(st, &p)?;
        let (removal, removal_reports) = match removal {
            Removal::Read => removal_cards(st, &p.changes)?,
            Removal::Skip => (None, BTreeMap::new()),
        };
        // canon's graph first, so the prospective canon carries it instead of building another
        let canon_g = st.graph(&Scope::Canon)?;
        crate::readiness::count_candidate_build();
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
        let mut reaffirms = vec![];
        let canon = st.materialized(&Scope::Canon)?;
        for id in ids {
            let Some(b) = next.get(&id) else { continue };
            if decision::status(b) != Some(DecisionStatus::Proposed) {
                let causes = reaffirmed_causes(canon.get(&id), b);
                if !causes.is_empty() {
                    reaffirms.push(Reaffirmed {
                        r#ref: format!("{id}@{}", b.version_hash().short()),
                        one_way: b.field_str("reversibility") == Some("one_way"),
                        block: b.clone(),
                        causes,
                    });
                }
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
        let plan_bound = c.plan.hash();
        let plan = version_plan(st, &pref.version, &p, &c.plan);
        let policy_changes = policy_changes(&p.changes, &changes, &canon, &next);
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
            reaffirms,
            plan_bound,
            plan,
            no_blocks: BTreeSet::new(),
            policy_changes,
            candidate: OnceCell::new(),
            checks: OnceCell::new(),
        })
    }

    fn r#ref(&self) -> String {
        self.pref.to_string()
    }

    /// Adds to `work` the decisions this version re-affirms (from its block delta) and the ones
    /// its proposed decisions fully retire (a whole supersede, or a revoke).
    fn open_work(&self, work: &mut OpenWork) {
        work.reaffirms
            .extend(self.reaffirms.iter().map(|d| d.block.id.clone()));
        for d in &self.decisions {
            for l in d.block.links.get(SUPERSEDES).into_iter().flatten() {
                if l.part.is_none() {
                    work.retires.insert(l.target.clone());
                }
            }
            for l in d.block.links.get(REVOKES).into_iter().flatten() {
                work.retires.insert(l.target.clone());
            }
        }
    }

    /// The landing projection's facts and the rule scan of this version, worked out once
    /// ([`Version::compute_checks`]).
    fn checks(&self, st: &ReadState) -> Result<&Checks, RdyError> {
        self.checks
            .get_or_init(|| self.compute_checks(st))
            .as_ref()
            .map_err(Clone::clone)
    }

    /// D-JQD5X, review I1: with no pack rule and no choice, nothing. Otherwise the version's
    /// landing projection ([`crate::check::landing_projection`]); its refusal is item 6's one
    /// issue. Then every rule on every block of the projected canon, with the graph, areas,
    /// derived status, suspects, tensions and intrinsic coverage of that canon (only the sets a
    /// rule needs), and the reach the rules add from canon to it.
    fn compute_checks(&self, st: &ReadState) -> Result<Checks, RdyError> {
        use crate::check::{Stage, evaluate_rules, rules};
        let pack = st.pack();
        let compiled = rules::compiled(pack)?;
        if compiled.is_empty() && self.p.alternative.is_none() {
            return Ok(Checks::none());
        }
        let projection = match crate::check::landing_projection(st, &self.pref, &self.p, &self.next)
        {
            Ok(p) => p,
            Err(e) => {
                return Ok(Checks {
                    issues: vec![Issue {
                        severity: Severity::Error,
                        code: e.code,
                        block: None,
                        message: e.message,
                        fix: e.fix,
                    }],
                    ..Checks::none()
                });
            }
        };
        // the projected blocks meet the pack's field rules and bounds, as the landing's dry run
        // will judge them; their versions are temporary and in no store
        let mut issues: Vec<Issue> = vec![];
        for op in &projection.updates {
            let rdy_model::Op::Update { block, .. } = op else {
                continue;
            };
            let parent = block
                .parent
                .as_ref()
                .and_then(|p| projection.candidate.get(p));
            issues.extend(
                rdy_schema::validate_read_block(pack, block, parent)
                    .into_iter()
                    .filter(|i| i.severity == Severity::Error),
            );
        }
        if compiled.is_empty() {
            return Ok(Checks {
                issues,
                considered: projection.considered,
                ..Checks::none()
            });
        }
        let built = self.built.after(&projection.updates, pack);
        let graph = Arc::clone(built.graph.get_or_init(|| {
            Arc::new(Graph::build_with(
                built.m.iter(),
                Arc::clone(st.reader().tracking()),
            ))
        }));
        let (suspects, tensions, scores) = self.rule_sets(st, &built, &graph, &compiled)?;
        let inputs = crate::filter::FilterInputs {
            pack,
            m: &built.m,
            graph: &graph,
            areas: st.area_matcher(),
            suspects: &suspects,
            tensions: &tensions,
            coverage: &scores,
        };
        let scan = evaluate_rules(pack, inputs, built.m.ids().cloned(), Stage::Propose)?;
        let (canon, canon_g) = (st.materialized(&Scope::Canon)?, st.graph(&Scope::Canon)?);
        let reach = rules::rule_reach(
            &self.pref,
            &compiled,
            (&canon, &canon_g),
            (&built.m, &graph),
            &self.touched,
            &scan.read_blocks,
        );
        issues.extend(scan.blocking.iter().map(|r| r.issue()));
        Ok(Checks {
            issues,
            warnings: scan.warnings,
            omitted: (scan.blocking_omitted, scan.warnings_omitted),
            reach,
            considered: projection.considered,
        })
    }

    /// The derived sets the rules of `compiled` read in the projected canon `built` (graph
    /// `graph`), only those a rule needs (`Filter::needs`): its suspects (presentational sources
    /// left out, as a scope's suspects are), the blocks of the candidate's open tensions
    /// ([`Version::candidate_tension_blocks`]), and each block's intrinsic coverage score. No
    /// machine-local source fact enters them.
    fn rule_sets(
        &self,
        st: &ReadState,
        built: &Built,
        graph: &Graph,
        compiled: &[rdy_schema::CompiledRule],
    ) -> Result<RuleSets, RdyError> {
        let (mut suspect, mut tension, mut coverage) = (false, false, false);
        for f in compiled.iter().flat_map(|r| [&r.select, &r.require]) {
            let n = f.needs();
            suspect |= n.suspect;
            tension |= n.tension;
            coverage |= n.coverage;
        }
        let suspects: BTreeSet<BlockId> = if suspect {
            graph
                .suspects()
                .into_iter()
                .filter(|s| !st.is_presentational_source(&built.m, s))
                .map(|s| s.block)
                .collect()
        } else {
            BTreeSet::new()
        };
        let tensions: BTreeSet<BlockId> = if tension {
            self.candidate_tension_blocks(st)?
        } else {
            BTreeSet::new()
        };
        let scores = if coverage {
            crate::filter::coverage_scores(
                st.pack(),
                &st.candidate_coverage(built, &self.p.changes)?.issues,
            )
        } else {
            BTreeMap::new()
        };
        Ok((suspects, tensions, scores))
    }

    /// The blocks of the projected candidate's open tensions (review fix L6/I7), what a rule's
    /// `tension` reads at propose: each open all-canon tension that involves no seed of the
    /// version's changes (a block they write, or a target their decisions supersede or revoke),
    /// which the landing leaves standing, and the version's own open tensions, whose landing form
    /// re-judges every tension around a seed. Readiness's own tension list stays
    /// [`Version::tensions`], the version's alone.
    fn candidate_tension_blocks(&self, st: &ReadState) -> Result<BTreeSet<BlockId>, RdyError> {
        let mut seeds = BTreeSet::new();
        for c in &self.p.changes {
            seeds.extend(crate::tensions::set::seeds_of(st, c)?);
        }
        let mut out: BTreeSet<BlockId> = self.tensions.iter().flat_map(|t| t.blocks()).collect();
        for t in st.canon_tensions()? {
            let blocks = t.blocks();
            if st.tension_status(&t) == TensionStatus::Open
                && !blocks.iter().any(|b| seeds.contains(b))
            {
                out.extend(blocks);
            }
        }
        Ok(out)
    }

    /// What the readiness checks read of this version, with its landing projection's facts and
    /// rule scan `checks`.
    fn checked<'a>(&'a self, checks: &'a Checks) -> crate::readiness::Read<'a> {
        let (summary, plan_blocks) = match &self.plan {
            Ok(plan) => (Ok(&plan.summary), &plan.blocks),
            Err(e) => (Err(e), &self.no_blocks),
        };
        crate::readiness::Read {
            pref: &self.pref,
            p: &self.p,
            changes: &self.changes,
            plan_bound: self.plan_bound,
            plan: summary,
            plan_blocks,
            written: &self.touched,
            decisions: self
                .decisions
                .iter()
                .map(|d| (d.block.id.clone(), d.r#ref.clone()))
                .collect(),
            reaffirms: self
                .reaffirms
                .iter()
                .map(|d| (d.block.id.clone(), d.r#ref.clone(), d.one_way))
                .collect(),
            policy_changes: &self.policy_changes,
            item6: &checks.issues,
            rule_reach: checks.reach.as_ref(),
        }
    }

    /// Every failed readiness check, one issue each, in the closed order of
    /// [`crate::readiness`]: the one readiness, on this version as read.
    fn readiness(&self, st: &ReadState) -> Result<Vec<Issue>, RdyError> {
        let checks = self.checks(st)?;
        crate::readiness::issues(st, &self.checked(checks), || self.lowered(st))
    }

    /// The rule warnings of this version, as `rdy_propose`'s warnings give them, and the rows
    /// left out (blocking, advisory).
    fn rule_warnings(&self, st: &ReadState) -> Result<(Vec<Issue>, (usize, usize)), RdyError> {
        let checks = self.checks(st)?;
        Ok((
            checks.warnings.iter().map(|r| r.issue()).collect(),
            checks.omitted,
        ))
    }

    /// [`Version::readiness`] with the version's change plan summary and, when it is ready, the
    /// policy's verdict and its `approve` item ([`Readiness`]).
    fn readiness_with_card(&self, st: &ReadState, now: i64) -> Result<Readiness, RdyError> {
        let issues = self.readiness(st)?;
        let policy = if issues.is_empty() {
            Some(self.policy(st)?)
        } else {
            None
        };
        let card = policy.as_ref().map(|v| self.batch_item(st, now, Some(v)));
        let (warnings, checks_omitted) = self.rule_warnings(st)?;
        Ok(Readiness {
            issues,
            summary: self
                .plan
                .as_ref()
                .map(|p| p.summary.clone())
                .unwrap_or_default(),
            card,
            policy,
            warnings,
            checks_omitted,
        })
    }

    /// The coverage of the canon the version would give, worked out once.
    fn candidate(&self, st: &ReadState) -> Result<&CandidateCoverage, &RdyError> {
        self.candidate
            .get_or_init(|| st.candidate_coverage(&self.built, &self.p.changes))
            .as_ref()
    }

    /// Readiness check 9 (O17, D-ZWP7Q, D-WB22B, D-T5S9A): under `coverage.gate: no_lower`, one
    /// `E_COVERAGE_LOWERED` issue for each page whose gate score the version lowers, in doc id
    /// order, then one for the corpus. A page is each doc the version touches that has blocks in
    /// canon, scored before (canon) and after (the canon the version would give); a new page
    /// counts in the corpus only. The canon the version would give holds its proposed decisions
    /// as proposed; a landing accepts them, so each one no content carries (its `unapplied`
    /// exception) counts after as the landing gives it: with its Governed check and its
    /// `unapplied_decision` issue (D-T5S9A: an opted-in project cannot land a decision that no
    /// content carries). A coverage read error is the one issue: the gate cannot pass what it
    /// cannot read.
    fn lowered(&self, st: &ReadState) -> Vec<Issue> {
        let read_error = |e: &RdyError| Issue {
            severity: Severity::Error,
            code: e.code,
            block: None,
            message: e.message.clone(),
            fix: e.fix.clone(),
        };
        let candidate = match self.candidate(st) {
            Ok(c) => c,
            Err(e) => return vec![read_error(e)],
        };
        let (canon, before) = match (
            st.materialized(&Scope::Canon),
            st.coverage_issues(&Scope::Canon),
        ) {
            (Ok(m), Ok(i)) => (m, i),
            (Err(e), _) | (_, Err(e)) => return vec![read_error(&e)],
        };
        let pack = st.pack();
        let landed = self.landed_unapplied(candidate);
        let mut out = vec![];
        for d in self.docs(Some(&canon)) {
            let Some(was) = page_gate_score(pack, &canon, &d, |id| before.blocks.get(id)) else {
                continue;
            };
            let now = page_gate_score(pack, &self.next, &d, |id| {
                landed.get(id).or_else(|| candidate.issues.blocks.get(id))
            })
            .unwrap_or(was);
            if now < was {
                let path = self
                    .next
                    .path_of(&d)
                    .or_else(|| canon.path_of(&d))
                    .unwrap_or_else(|| d.to_string());
                out.push(crate::readiness::coverage_lowered(
                    &self.pref,
                    Some(d),
                    &path,
                    was,
                    now,
                ));
            }
        }
        let was = crate::coverage::gate_rollup(pack, &canon, before.blocks.iter()).score;
        let entries = candidate
            .issues
            .blocks
            .iter()
            .filter(|(id, _)| !landed.contains_key(*id))
            .chain(landed.iter());
        let now = crate::coverage::gate_rollup(pack, &self.next, entries).score;
        if now < was {
            out.push(crate::readiness::coverage_lowered(
                &self.pref,
                None,
                "the corpus",
                was,
                now,
            ));
        }
        out
    }

    /// D-T5S9A: the coverage entry, as the landing gives it, of each proposed decision of the
    /// version that no content carries (its `unapplied` exception, `lands_unapplied`): its
    /// entry in `candidate` with one Governed check and its `unapplied_decision` issue.
    fn landed_unapplied(
        &self,
        candidate: &CandidateCoverage,
    ) -> BTreeMap<BlockId, crate::coverage::BlockEntry> {
        use crate::coverage::{BlockEntry, CoverageIssue, Dimension, IssueCode};
        let governed = Dimension::ALL
            .iter()
            .position(|d| *d == Dimension::Governed)
            .expect("Governed is a dimension");
        self.decisions
            .iter()
            .filter(|d| d.kinds.iter().any(|(k, _)| *k == ExceptionKind::Unapplied))
            .map(|d| {
                let id = &d.block.id;
                let mut e =
                    candidate
                        .issues
                        .blocks
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| BlockEntry {
                            doc: self.next.doc_of(id).map(|b| b.id.clone()),
                            checks: [0; 6],
                            issues: vec![],
                        });
                if !e
                    .issues
                    .iter()
                    .any(|i| i.code == IssueCode::UnappliedDecision)
                {
                    e.checks[governed] = e.checks[governed].max(1);
                    e.issues.push(CoverageIssue::new(
                        id,
                        IssueCode::UnappliedDecision,
                        None,
                        crate::coverage::fix_unapplied_decision(id),
                    ));
                }
                (id.clone(), e)
            })
            .collect()
    }

    /// The authority policy's verdict on this version (D-ZWP7Q, O8): `policy_verdict::verdict`
    /// under the live policy, the policy block in canon at its head, with the required role of
    /// the version's `approve` item (O10) when the policy's fields read.
    fn policy(&self, st: &ReadState) -> Result<AssessedPolicy, RdyError> {
        let canon = st.materialized(&Scope::Canon)?;
        let live = crate::policy::live_policy(st, &canon);
        let inputs = self.verdict_inputs(st, &canon)?;
        let role = live
            .as_ref()
            .and_then(|l| l.fields.as_ref())
            .and_then(|f| {
                crate::policy::roles::required_role(
                    st,
                    &canon,
                    Some(&self.next),
                    f,
                    &self.approve_facts(),
                )
            })
            .map(|r| r.label());
        let reference = live.as_ref().map(|l| l.reference());
        let policy = live
            .as_ref()
            .zip(reference.as_ref())
            .map(|(l, r)| (r, l.fields.as_ref()));
        Ok(crate::policy_verdict::verdict(policy, &inputs, role))
    }

    /// What the verdict reads of this version (closed): its proposer, its changes in
    /// order, each block it writes (a deleted one read in canon) with its areas and whether it is
    /// in stable content in canon or the prospective canon, each decision it writes (O7), its
    /// exceptions in the facts' order, whether a change carries a removal request, whether it
    /// writes the policy, its deferred non-code items, the docs it makes stable, and its change
    /// plan's items.
    fn verdict_inputs(
        &self,
        st: &ReadState,
        canon: &Materialized,
    ) -> Result<VerdictInputs, RdyError> {
        let changes = self
            .p
            .changes
            .iter()
            .zip(&self.changes)
            .map(|(h, c)| ChangeFacts {
                short: h.short(),
                author: c.provenance.actor.clone(),
                justification: c.justification.as_ref().map(|j| match j {
                    Justification::Implements(_) => JustificationKind::Implements,
                    Justification::Editorial { .. } => JustificationKind::Editorial,
                    Justification::Reflects { .. } => JustificationKind::Reflects,
                }),
            })
            .collect();
        let blocks = self
            .touched
            .iter()
            .map(|id| {
                let (m, b) = match self.next.get(id) {
                    Some(b) => (&*self.next, Some(b)),
                    None => (canon, canon.get(id)),
                };
                WrittenBlock {
                    id: id.clone(),
                    areas: b.map(|b| st.areas_in(m, b)).unwrap_or_default(),
                    stable: crate::brief::effective_stable(canon, Some(id))
                        || crate::brief::effective_stable(&self.next, Some(id)),
                }
            })
            .collect();
        let decisions = self
            .written_decisions()
            .map(|b| WrittenDecision {
                id: b.id.clone(),
                basis: b.field_str("authority_basis").map(str::to_string),
                reversibility: b.field_str("reversibility").map(str::to_string),
                attested_by: b.field_str("attested_by").map(str::to_string),
                areas: st.areas_in(&self.next, b),
            })
            .collect();
        let mut exceptions: Vec<(ExceptionKind, String)> = self
            .decisions
            .iter()
            .flat_map(|d| d.kinds.iter().map(|(k, _)| (*k, d.r#ref.clone())))
            .collect();
        exceptions.extend(
            self.reaffirms
                .iter()
                .filter(|d| d.one_way)
                .map(|d| (ExceptionKind::ReaffirmOneWay, d.r#ref.clone())),
        );
        let redaction = !crate::redaction::requests_carried(st, &self.p.changes)?.is_empty();
        let (noncode_deferrals, plan_items) = match &self.plan {
            Ok(plan) => (
                if plan.bound.is_some() {
                    plan.deferrals.len()
                } else {
                    0
                },
                plan.counts.open + plan.counts.closed + plan.counts.deferred,
            ),
            Err(_) => (0, 0),
        };
        let stable_docs = self
            .docs(Some(canon))
            .into_iter()
            .filter(|d| {
                crate::brief::effective_stable(&self.next, Some(d))
                    && !crate::brief::effective_stable(canon, Some(d))
            })
            .collect();
        Ok(VerdictInputs {
            proposer: self.p.provenance.actor.clone(),
            changes,
            blocks,
            decisions,
            exceptions,
            redaction,
            changes_policy: !self.policy_changes.is_empty(),
            noncode_deferrals,
            stable_docs,
            plan_items,
        })
    }

    /// O7: every decision block the version creates or updates, read in the prospective canon,
    /// in id order. A tracking edit counts.
    fn written_decisions(&self) -> impl Iterator<Item = &Block> {
        self.touched
            .iter()
            .filter_map(|id| self.next.get(id))
            .filter(|b| b.block_type == decision::DECISION)
    }

    /// The facts of the version's `approve` item that the policy's `require` entries match on
    /// (O10): the blocks it writes, whether a decision it writes or re-affirms is `one_way`, and
    /// whether it writes a decision (O7, [`Version::written_decisions`]).
    fn approve_facts(&self) -> ItemFacts {
        let one_way = self
            .written_decisions()
            .any(|b| b.field_str("reversibility") == Some("one_way"))
            || self.reaffirms.iter().any(|d| d.one_way);
        ItemFacts {
            kind: ItemKind::Approve,
            blocks: self.touched.iter().cloned().collect(),
            one_way,
            decision: self.writes_decision(),
        }
    }

    /// Whether the version writes a decision (O7).
    fn writes_decision(&self) -> bool {
        self.written_decisions().next().is_some()
    }

    /// The facts of the version's `confirm` item `i` (O10): the blocks the version writes;
    /// `one_way` when the decision the item confirms is declared `one_way` in the canon the
    /// version would give; the version writes a decision.
    fn confirm_facts(&self, i: &Item) -> ItemFacts {
        let decided: Vec<BlockId> = i
            .r#ref
            .split_once('@')
            .and_then(|(id, _)| BlockId::parse(id).ok())
            .into_iter()
            .collect();
        ItemFacts {
            kind: ItemKind::Confirm,
            blocks: self.touched.iter().cloned().collect(),
            one_way: one_way_among(&self.next, &decided),
            decision: self.writes_decision(),
        }
    }

    /// The facts of the version's deferral card (O10): the subjects of the deferred items that
    /// are blocks; `one_way` when one of them is a decision declared `one_way` in the canon the
    /// version would give; whether the version writes a decision.
    fn defer_facts(&self) -> ItemFacts {
        let blocks: Vec<BlockId> = match &self.plan {
            Ok(plan) => plan
                .deferrals
                .iter()
                .filter_map(|d| BlockId::parse(&d.subject).ok())
                .collect(),
            Err(_) => vec![],
        };
        ItemFacts {
            kind: ItemKind::Defer,
            one_way: one_way_among(&self.next, &blocks),
            blocks,
            decision: self.writes_decision(),
        }
    }

    /// The assessment of this version, which is ready (D-SDSV2, O16 to O19), from what its read
    /// gave: `card` its `approve` item, `summary` its change plan, `candidate` the coverage of the
    /// canon it would give and `consequences` their count. Err in the inner result: the O9 issue
    /// of a record too large to store.
    #[allow(clippy::too_many_arguments)]
    fn assessment(
        &self,
        st: &ReadState,
        c: &Candidate,
        card: &Item,
        summary: &PlanSummary,
        candidate: &CandidateCoverage,
        consequences: usize,
        gate: &crate::readiness::assessment::GateConfig,
        policy: &AssessedPolicy,
    ) -> Result<Result<Assessment, Issue>, RdyError> {
        use crate::readiness::assessment::{
            self as record, MAX_TENSION_ROWS, confidence_record, issue_of, scores_of,
        };
        let pack = st.pack();
        // O17: the tension rows the version lists, open and dismissed, the first 1,000; a ready
        // version has no blocking conflict, so none is left out for one
        let fixed = crate::plan::compute::fixed_tensions(st, &self.pref.version)?;
        let listed = st.listed_tensions(
            &self.p.view,
            &self.p.changes,
            &BTreeSet::new(),
            fixed.as_ref().map(|f| &f.ids),
        )?;
        let mut cache = crate::tensions::info::InfoCache::default();
        let rows: Vec<_> = listed
            .iter()
            .take(MAX_TENSION_ROWS)
            .map(|(refuses, t)| {
                let mut info = st.tension_info_with(t, &mut cache);
                info.refuses = Some(*refuses);
                (info, t.vkey, st.tension_status(t))
            })
            .collect();
        let none = (vec![], vec![], BTreeSet::new());
        let (answers, items, plan_blocks) = match &self.plan {
            Ok(p) => (
                p.tension_answers.clone(),
                p.suspect_items.clone(),
                p.blocks.clone(),
            ),
            Err(_) => none,
        };
        // the facts: every decision's supersede targets, every re-affirmed decision (one_way
        // ones too, ruling I-c), each decision's reversibility, and each exception
        let supersedes: Vec<BlockId> = self
            .decisions
            .iter()
            .flat_map(|d| d.block.links.get(SUPERSEDES).into_iter().flatten())
            .map(|l| l.target.clone())
            .collect();
        let reaffirms: Vec<BlockId> = self.reaffirms.iter().map(|d| d.block.id.clone()).collect();
        let mut reversibility: Vec<(BlockId, bool)> = self
            .decisions
            .iter()
            .map(|d| {
                let one_way = d.block.field_str("reversibility") == Some("one_way");
                (d.block.id.clone(), one_way)
            })
            .collect();
        reversibility.extend(
            self.reaffirms
                .iter()
                .map(|d| (d.block.id.clone(), d.one_way)),
        );
        let mut exceptions: Vec<(AssessmentExceptionKind, String)> = self
            .decisions
            .iter()
            .flat_map(|d| {
                d.kinds
                    .iter()
                    .map(|(k, _)| (exception_record(*k), d.r#ref.clone()))
            })
            .collect();
        exceptions.extend(
            self.reaffirms
                .iter()
                .filter(|d| d.one_way)
                .map(|d| (AssessmentExceptionKind::ReaffirmOneWay, d.r#ref.clone())),
        );
        // O18: the suspects among the reaffirm and confirm items, read in the canon the version
        // would give; the scope: the blocks of each doc it touches there, and each block it
        // writes that is in no doc
        let suspect = |b: &BlockId| {
            candidate.issues.blocks.get(b).is_some_and(|e| {
                e.issues
                    .iter()
                    .any(|i| i.code == crate::coverage::IssueCode::OpenSuspect)
            })
        };
        let suspects: Vec<(BlockId, BlockId)> =
            items.into_iter().filter(|(b, _)| suspect(b)).collect();
        let canon = st.materialized(&Scope::Canon)?;
        let docs = self.docs(Some(&canon));
        let mut scope_blocks: BTreeSet<BlockId> = docs
            .iter()
            .flat_map(|d| crate::coverage::page_blocks(&self.next, d))
            .collect();
        scope_blocks.extend(
            self.touched
                .iter()
                .filter(|id| self.next.doc_of(id).is_none() && canon.doc_of(id).is_none())
                .cloned(),
        );
        // O19: the pages before and after, the corpus, the confidence and the scope's issues
        let doc_refs: BTreeSet<&BlockId> = docs.iter().collect();
        let before = st.doc_rollups(&Scope::Canon, &doc_refs)?;
        let after = crate::coverage::rollups_of(pack, &self.next, &candidate.issues, &doc_refs);
        let pages = docs
            .iter()
            .map(|d| AssessmentPage {
                doc: d.clone(),
                before: scores_of(before.get(d)),
                after: scores_of(after.get(d)),
            })
            .collect();
        let corpus = AssessmentScoreChange {
            before: scores_of(Some(&*st.scope_rollup(&Scope::Canon)?)),
            after: scores_of(Some(&crate::coverage::rollup(
                pack,
                candidate.issues.blocks.values(),
            ))),
        };
        let paths = crate::coverage::issue_paths(
            &self.next,
            self.touched
                .iter()
                .filter_map(|id| Some((id, candidate.issues.blocks.get(id)?))),
        );
        let confidence = confidence_in(
            |b| candidate.issues.blocks.get(b),
            &self.touched,
            None,
            &paths,
        );
        let issues: Vec<AssessmentIssue> = scope_blocks
            .iter()
            .filter_map(|b| candidate.issues.blocks.get(b))
            .flat_map(|e| e.issues.iter().map(issue_of))
            .collect();
        Ok(record::build(record::Inputs {
            pref: self.pref,
            base: self.p.canon_at,
            pack,
            gate: *gate,
            changes: self.p.changes.len(),
            consequences,
            plan: summary,
            plan_hash: c.plan.hash(),
            supersedes,
            reaffirms,
            reversibility,
            exceptions,
            tensions: &rows,
            tensions_omitted: listed.len().saturating_sub(MAX_TENSION_ROWS),
            answers: &answers,
            carried: fixed.as_ref().map_or(&[], |f| f.rows.as_slice()),
            suspects,
            written: self.touched.clone(),
            plan_blocks,
            rule_reach: self.checks(st)?.reach.clone().unwrap_or_default(),
            scope_blocks,
            coverage: AssessmentCoverage { pages, corpus },
            confidence: confidence_record(&confidence),
            issues,
            card,
            policy: Some(policy.clone()),
        }))
    }

    /// The version's deferral card (D-AAQ9Z), answered or not, with who raised it; none for a
    /// version with no bound plan, no deferred non-code item, or a plan that cannot be read.
    fn deferral_item(&self, now: i64) -> Option<Item> {
        let plan = self.plan.as_ref().ok()?;
        let mut it = deferral_item(&self.pref, plan)?;
        self.raised(&mut it, now);
        Some(it)
    }

    /// Whether the version's deferral card stands with no valid acceptance (D-AAQ9Z, p-T69HT):
    /// it has a card, and the latest answer to it is not an `accept_deferrals` bound to its
    /// current card. An answer that cannot be read counts as none, so the card stays open.
    fn deferral_open(&self, st: &ReadState, now: i64) -> bool {
        self.deferral_item(now)
            .is_some_and(|it| !matches!(accepted(st, &it), Ok(Some(_))))
    }

    /// Who raised the version's items, when, and how long ago.
    fn raised(&self, it: &mut Item, now: i64) {
        raised_from(&self.p, it, now);
    }

    /// The version's `approve` item, with the policy's verdict `policy` as its `Policy` fact
    /// when one is given (a ready version).
    fn batch_item(&self, st: &ReadState, now: i64, policy: Option<&AssessedPolicy>) -> Item {
        let candidate = self.candidate(st);
        let consequences = self.consequence_count(st);
        self.batch_item_with(st, now, candidate, consequences.as_ref().copied(), policy)
    }

    /// [`Version::batch_item`] with the coverage of the canon the version would give
    /// (`candidate`) and the count of its consequences, each worked out once by the caller.
    fn batch_item_with(
        &self,
        st: &ReadState,
        now: i64,
        candidate: Result<&CandidateCoverage, &RdyError>,
        consequences: Result<usize, &RdyError>,
        policy: Option<&AssessedPolicy>,
    ) -> Item {
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
        it.summary = summary;
        it.summary_by = Some(self.p.provenance.actor.clone());
        it.facts = self.facts(st, candidate, consequences, policy);
        it.members = members
            .iter()
            .map(|d| format!("{}  {}", d.block.id, d.title()))
            .collect();
        let mut approve = option(
            "approve",
            &approve,
            "Lands the proposal in canon.",
            Some(approve_reason(
                self.exception_count(),
                self.deferral_open(st, now),
            )),
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

    /// The docs the version touches: for each block it writes, its doc in the canon it would give
    /// and its doc in canon. A move between pages touches both pages (O17: the source page's
    /// gate score is compared too); a deleted block has only its doc in canon.
    fn docs(&self, canon: Option<&Materialized>) -> BTreeSet<BlockId> {
        self.touched
            .iter()
            .flat_map(|id| {
                let after = self.next.doc_of(id).map(|d| d.id.clone());
                let before = canon.and_then(|c| c.doc_of(id)).map(|d| d.id.clone());
                after.into_iter().chain(before)
            })
            .collect()
    }

    /// The batch card's facts, in the closed order of O6, with the coverage of the canon the
    /// version would give (`candidate`) and the count of its consequences. A ready version's
    /// verdict (`policy`) follows Exceptions as the fact `Policy` (D-ZWP7Q, O9).
    fn facts(
        &self,
        st: &ReadState,
        candidate: Result<&CandidateCoverage, &RdyError>,
        consequences: Result<usize, &RdyError>,
        policy: Option<&AssessedPolicy>,
    ) -> Vec<Fact> {
        let canon = st.materialized(&Scope::Canon).ok();
        let docs = self.docs(canon.as_deref());
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
        // D-GV465: the decisions it re-affirms that need no card of their own
        let reaffirms: Vec<String> = self
            .reaffirms
            .iter()
            .filter(|d| !d.one_way)
            .map(|d| format!("{} (after {})", d.block.id, d.cause_list()))
            .collect();
        let reaffirms = if reaffirms.is_empty() {
            "none".to_string()
        } else {
            reaffirms.join(REAFFIRMS_SEPARATOR)
        };
        let plan = match &self.plan {
            Ok(p) => Ok(format!(
                "{} open, {} closed, {} deferred",
                p.counts.open, p.counts.closed, p.counts.deferred
            )),
            Err(e) => Err(e.clone()),
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
            tensions.push_str(": ");
            tensions.push_str(&self.advisory_rows());
        }
        let exceptions = self.exception_count();
        let fact = |name: &'static str, value: String| Fact {
            name: name.into(),
            value,
        };
        let mut facts = vec![
            fact(
                "Changes",
                format!(
                    "{}, {}",
                    count(docs.len(), "doc"),
                    count(self.touched.len(), "block")
                ),
            ),
            fact(JUSTIFICATION_FACT, self.justification()),
            fact("Decisions", decisions),
        ];
        // D-8GR43: the options the landing writes for a choice, and the set version they bind
        let checks = self.checks(st).ok();
        if let Some(c) = checks.and_then(|c| c.considered.as_ref()) {
            facts.push(fact(CONSIDERED_OPTIONS_FACT, c.fact_value()));
            facts.push(fact(ALTERNATIVE_SET_FACT, c.set_value()));
        }
        facts.extend([
            fact("Supersedes", supersedes),
            fact(REAFFIRMS_FACT, reaffirms),
            fact("Reversibility", self.reversibility()),
            fact(
                "Consequences",
                worked_out(
                    consequences
                        .map(|n| format!("{n} returned"))
                        .map_err(Clone::clone),
                ),
            ),
            fact("Change plan", worked_out(plan)),
            fact("Tensions", tensions),
            fact("Coverage", worked_out(self.coverage(st, candidate))),
            fact("Confidence", worked_out(self.confidence(candidate))),
            self.base_fact(st),
            fact(
                "Exceptions",
                match exceptions {
                    0 => "none".to_string(),
                    n => n.to_string(),
                },
            ),
        ]);
        // D-JQD5X: the rule warnings propose found, then the rows it left out, before Policy
        if let Some(c) = checks {
            for row in &c.warnings {
                facts.push(fact(
                    RULE_CHECK_FACT,
                    format!("{}; fix: {}", row.message, row.fix),
                ));
            }
            let (blocking, advisory) = c.omitted;
            if blocking > 0 || advisory > 0 {
                facts.push(fact(
                    RULE_CHECKS_OMITTED_FACT,
                    format!("{blocking} blocking, {advisory} advisory"),
                ));
            }
        }
        if let Some(policy) = policy {
            facts.push(fact(POLICY_FACT, crate::policy_verdict::fact_value(policy)));
        }
        facts
    }

    /// The Base fact: the canon the version was proposed on, and whether canon is still there.
    /// It is always read live, on the approve card and in the stored facts (I-d).
    fn base_fact(&self, st: &ReadState) -> Fact {
        let base = match self.p.canon_at {
            Some(h) => h.short(),
            None => "the start".to_string(),
        };
        let moved = if self.p.canon_at == st.canon_head() {
            "current"
        } else {
            "moved"
        };
        Fact {
            name: BASE_FACT.into(),
            value: format!("canon at {base}, {moved}"),
        }
    }

    /// D-45RG6: one row for each advisory tension of the version, in tension id order, joined
    /// by `; `: `<tn-id> <kind>: <summary> (<confirm|dismiss>: <reason>)`, or `(no answer)`.
    /// The answer is the bound plan's answer to the tension's item; a plan that cannot be read
    /// answers nothing. The rows stop once the fact passes [`MAX_TEXT_BYTES`] and the text is cut
    /// as O9 says, so a version with many tensions builds no unbounded string.
    fn advisory_rows(&self) -> String {
        let acts = self.plan.as_ref().ok().map(|p| &p.tension_acts);
        let answer_of = |t: &Tension| -> Option<String> {
            let (_, act, reason) = acts?.iter().find(|(id, _, _)| *id == t.id)?;
            Some(format!(
                "{}: {}",
                act.as_str(),
                reason.as_deref().unwrap_or("")
            ))
        };
        let mut out = String::new();
        for t in self
            .tensions
            .iter()
            .filter(|t| t.severity == TensionSeverity::Advisory)
        {
            if out.len() > MAX_TEXT_BYTES {
                break;
            }
            if !out.is_empty() {
                out.push_str("; ");
            }
            let answer = answer_of(t).unwrap_or_else(|| "no answer".to_string());
            out.push_str(&format!(
                "{} {}: {} ({answer})",
                tension_label(&t.id),
                t.kind.name(),
                t.summary
            ));
        }
        cut_text(out)
    }

    /// rf9 (TASK-507 (3)): the claim of each justification of the version's changes, in the
    /// changes' stored order, a repeated claim kept at its first place, joined with `; `; `none`
    /// when no change makes one. Implements is `implements <ids>` in stored order; Editorial is
    /// `editorial <class>: <reason>`; Reflects is `reflects <titles>[, code]` with the sources
    /// in id order (a source with no title in the prospective canon by its id) and `code` for
    /// the code state; code alone is `reflects code`.
    fn justification(&self) -> String {
        let title = |id: &BlockId| {
            self.next
                .get(id)
                .and_then(|b| text(b, "title"))
                .map_or_else(|| id.to_string(), str::to_string)
        };
        let mut claims: Vec<String> = vec![];
        for c in &self.changes {
            let claim = match &c.justification {
                Some(Justification::Implements(ids)) if !ids.is_empty() => {
                    let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
                    format!("implements {}", ids.join(", "))
                }
                Some(Justification::Editorial { class, reason }) => {
                    format!("editorial {}: {reason}", crate::history::class_str(*class))
                }
                Some(Justification::Reflects { sources, code }) => {
                    let mut ids: Vec<&BlockId> = sources.iter().collect();
                    ids.sort();
                    let mut parts: Vec<String> = ids.into_iter().map(title).collect();
                    if *code {
                        parts.push("code".to_string());
                    }
                    if parts.is_empty() {
                        continue;
                    }
                    format!("reflects {}", parts.join(", "))
                }
                _ => continue,
            };
            if !claims.contains(&claim) {
                claims.push(claim);
            }
        }
        if claims.is_empty() {
            "none".to_string()
        } else {
            claims.join("; ")
        }
    }

    /// rf9 (TASK-507 (3)): the value of the Findings fact of a superseding decision's card: the
    /// held finding targets of its direct `depends_on` links, as `<id>: <statement>` in id
    /// order joined with `; `, or `none`. A `conditional_on` link and a target that is not a
    /// finding add nothing.
    fn findings(&self, d: &Block) -> String {
        let targets: BTreeSet<&BlockId> = d
            .links
            .get(DEPENDS_ON)
            .into_iter()
            .flatten()
            .map(|l| &l.target)
            .collect();
        let each: Vec<String> = targets
            .into_iter()
            .filter_map(|t| {
                let b = self.next.get(t)?;
                (b.block_type == supporting::FINDING)
                    .then(|| format!("{t}: {}", text(b, "statement").unwrap_or_default()))
            })
            .collect();
        if each.is_empty() {
            "none".to_string()
        } else {
            each.join("; ")
        }
    }

    /// How many decisions of the version stand beside its batch as exceptions.
    fn exception_count(&self) -> usize {
        let decisions = self
            .decisions
            .iter()
            .filter(|d| !d.kinds.is_empty())
            .count();
        decisions + self.reaffirms.iter().filter(|d| d.one_way).count()
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

    /// D-NJ2SP: how many consequences landing the version returns, computed on its final state,
    /// from its record (a propose reads them before the store holds the version).
    fn consequence_count(&self, st: &ReadState) -> Result<usize, RdyError> {
        let c = st.landing_consequences_of(&self.pref.version, &self.p, None)?;
        Ok(c.items.len())
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
        let paths = crate::coverage::issue_paths(
            &self.next,
            self.touched
                .iter()
                .filter_map(|id| Some((id, issues.blocks.get(id)?))),
        );
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
                        name: fact_name(*k).into(),
                        value: detail.clone(),
                    })
                    .collect();
                // rf9: a superseding decision also names the findings it rests on
                if d.block.links.get(SUPERSEDES).is_some_and(|l| !l.is_empty()) {
                    it.facts.push(Fact {
                        name: FINDINGS_FACT.into(),
                        value: self.findings(&d.block),
                    });
                }
                let reason = text(&d.block, "recommend_reason")
                    .unwrap_or_default()
                    .to_string();
                let reject = d.block.field_str("recommend") == Some("reject");
                it.options = vec![
                    option(
                        "confirm",
                        "Confirm",
                        "Lets the batch land with this decision.",
                        (!reject).then_some(reason.as_str()),
                    ),
                    option(
                        "reject",
                        "Reject",
                        "The batch waits until its author drops or revises this decision.",
                        reject.then_some(reason.as_str()),
                    ),
                    later(),
                ];
                self.raised(&mut it, now);
                it
            })
            .collect();
        // D-GV465: a one_way decision the version re-affirms stands beside the batch too
        for d in self.reaffirms.iter().filter(|d| d.one_way) {
            let title = text(&d.block, "title")
                .or_else(|| text(&d.block, "statement"))
                .unwrap_or_default();
            let mut it = item(ItemKind::Confirm, &d.r#ref, title);
            it.why = text(&d.block, "why").unwrap_or_default().to_string();
            it.batch = Some(r.clone());
            it.exception = Some(ExceptionKind::ReaffirmOneWay);
            it.facts = vec![Fact {
                name: fact_name(ExceptionKind::ReaffirmOneWay).into(),
                value: d.cause_list(),
            }];
            it.options = vec![
                option(
                    "confirm",
                    "It still holds",
                    "Lets the batch land with this re-affirmation.",
                    Some(&d.reason()),
                ),
                option(
                    "reject",
                    "Drop the re-affirmation",
                    "The batch waits until its author drops the re-affirmation.",
                    None,
                ),
                later(),
            ];
            self.raised(&mut it, now);
            out.push(it);
        }
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
        ExceptionKind::ReaffirmOneWay => "Re-affirms",
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
        plan_binding: None,
    }
}

/// One option with one consequence line (O25), recommended with its reason when `reason` is
/// given (D-D8FAB).
pub(super) fn option(id: &str, label: &str, consequence: &str, reason: Option<&str>) -> ItemOption {
    ItemOption {
        id: id.to_string(),
        label: label.to_string(),
        consequences: vec![consequence.to_string()],
        recommended: reason.map(Recommended::new),
    }
}

/// The approve option's reason when no decision stands beside the batch (D-D8FAB).
pub const APPROVE_REASON: &str = "its checks pass and nothing blocks it";
/// The approve option's reason when decisions stand beside the batch on their own cards: the
/// landing waits for a confirm answer to each (D-D8FAB).
pub const APPROVE_REASON_BESIDE: &str =
    "its checks pass, and it lands after you confirm each decision beside it";

/// The approve option's reason while a deferral card with no acceptance stands beside the
/// batch, with or without exceptions (D-AAQ9Z): the landing waits for an answer to each card.
pub const APPROVE_REASON_CARDS: &str =
    "its checks pass, and it lands after you answer each card beside it";

/// The name of the batch fact that gives the claim of the version's justifications (rf9,
/// TASK-507 (3)); it follows Changes.
pub const JUSTIFICATION_FACT: &str = "Justification";
/// The name of the batch fact that gives the authority policy's verdict (D-ZWP7Q, O9); it
/// follows Exceptions.
pub const POLICY_FACT: &str = "Policy";
/// The fact of one rule warning (D-JQD5X), before Policy.
pub const RULE_CHECK_FACT: &str = "Rule check";
/// The fact of the rule rows a propose left out, before Policy.
pub const RULE_CHECKS_OMITTED_FACT: &str = "Rule checks omitted";
use crate::check::options::{ALTERNATIVE_SET_FACT, CONSIDERED_OPTIONS_FACT};
/// The name of the batch fact that gives the canon the version was proposed on; it is the one
/// fact the approve card reads live, not from the stored assessment (I-d).
const BASE_FACT: &str = "Base";
/// The name of the exception fact of a superseding decision that lists the findings it depends
/// on (rf9, TASK-507 (3)); it follows the exception-kind facts.
pub const FINDINGS_FACT: &str = "Findings";

/// The name of the batch fact that lists the decisions a version re-affirms with no card of
/// their own (D-GV465).
pub const REAFFIRMS_FACT: &str = "Re-affirms";
/// What joins the decisions of [`REAFFIRMS_FACT`]: each is `<id> (after <cause list>)`.
pub const REAFFIRMS_SEPARATOR: &str = "; ";

/// The approve option's reason: computed from the batch card's own state, never the summary
/// (D-D8FAB). The set is closed: [`APPROVE_REASON`], [`APPROVE_REASON_BESIDE`] or, while the
/// version's deferral card has no acceptance (`deferral`), [`APPROVE_REASON_CARDS`]. Only the
/// deferral acceptance moves it: an acceptance gives the batch card the reason of the holds
/// that remain, so an approval bound to the card shown before it is `E_PROPOSAL_MOVED`, with
/// the fresh card. A confirm of an exception never moves it.
fn approve_reason(exceptions: usize, deferral: bool) -> &'static str {
    match (exceptions, deferral) {
        (_, true) => APPROVE_REASON_CARDS,
        (0, false) => APPROVE_REASON,
        (_, false) => APPROVE_REASON_BESIDE,
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
        Session::human("human:qa".into(), Role::Writer).unwrap()
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
        let open = approval_holds(&r.read(), &pref, true).unwrap().open;
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
        assert!(
            approval_holds(&r.read(), &pref, true)
                .unwrap()
                .open
                .is_empty()
        );
        // a later reject opens it again: a rejected decision has no confirm (F1, F2); readiness
        // gives the rejection (D-ME2HD)
        let r = reopened(r, &answer(&ex.r#ref, AnswerAct::RejectDecision));
        let st = r.read();
        let open = approval_holds(&st, &pref, true).unwrap().open;
        let p = st.reader().proposal(&pref.version).unwrap();
        let rejected = approval_readiness(&st, &pref, p).unwrap();
        assert_eq!(rejected.len(), 1, "{rejected:#?}");
        assert_eq!(
            rejected[0].message,
            format!("{one_way} was rejected; drop or revise it, then propose again")
        );
        assert_eq!(
            open.iter().map(|i| i.r#ref.as_str()).collect::<Vec<_>>(),
            vec![ex.r#ref.as_str()]
        );
    }
}
