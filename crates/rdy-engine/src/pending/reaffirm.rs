//! Re-affirmation cards (D-CQMA4, D-EJBPD, D-GV465, D-KQ4CA, D-ZY24B; p-WQB0V, p-D3P76): the decisions a
//! changed, deleted or retired block made suspect, one card per cause, so the person can say that
//! they still hold or keep them suspect.
//!
//! # Membership
//!
//! The cards are read from canon's captured graph through the shared suspect projection
//! (`ReadState::suspects` with its accepted deferrals): each structural suspect row of a live or
//! partly superseded accepted decision joins the group of its target, the cause. A code row has
//! no target and never makes a card. Three exclusions, closed:
//! - a decision that a latest open proposal version re-affirms (its `reaffirmed` metadata or a
//!   pin of it moves) or fully retires (it supersedes it whole, or revokes it) is no member of any
//!   card: the open work answers it;
//! - a cause/member pair that an active accepted deferral covers (a sealed deferral or a
//!   KeepSuspect answer the canon has not cleared) is not in that cause's group; the member's
//!   other causes keep it;
//! - a member whose own landing made the cause's state (the landing's own edits, D-ZY24B): the landing
//!   that last changed, deleted or retired the cause also wrote the member's current version
//!   (edit order inside the view does not matter), or the member's landing came first and every
//!   landing that changed the cause after it came from the same view as the member's landing. A
//!   `one_way` member keeps its card in this second case. A cause change from any other landing
//!   opens the card. The member's suspect rows stay in every read.
//!
//! An empty group makes no card. The cause's version is its latest block object: its version in
//! canon, or the version its delete removed.
//!
//! # The card
//!
//! Ref `reaffirm:<cause>@<version short>` (the first 12 hex digits of the cause's version), title
//! `Re-affirm after <cause title>` (the cause id when it has none), why [`REAFFIRM_WHY`], summary
//! `Re-affirm the listed decisions after <cause>.`, members `<id>  <title>` in id order (the id
//! when a decision has no title), one fact `Cause`, and two options with no recommendation:
//! `o1` [`HOLD_LABEL`] and `o2` [`KEEP_LABEL`]. It stands alone (no batch, no exception), never
//! blocks, and was raised by the approver of the last landing that changed, deleted or retired
//! the cause, at that landing's time. A retirement's landing is the one that introduced it: a
//! later metadata-only version of the retiring decision does not move it.
//!
//! # The binding (O10)
//!
//! [`PlanCardBinding::Reaffirm`] binds the cause, its full version, its current retirement set
//! (partial superseders included) and every member with its full version and all its structural
//! suspects, sorted, also where the text cuts them. A card past [`MAX_CARD_MEMBERS`] members,
//! [`MAX_CARD_PAIRS`] member/suspect pairs or [`MAX_CARD_BYTES`] of encoded binding is refused
//! with `E_LIMIT` ([`check_card_bounds`]); the read writes nothing.
use super::item::{Fact, Item, ItemKind, age};
use super::plan::PlanCardBinding;
use super::queue::{item, option, text};
use crate::error::{E_LIMIT, RdyError};
use crate::session::rfc3339;
use crate::{ReadState, Scope};
use rdy_graph::DerivedStatus;
use rdy_model::decision::{self, DECISION, DecisionStatus};
use rdy_model::{Block, BlockId, Hash, Op};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// The why line of every re-affirmation card.
pub const REAFFIRM_WHY: &str = "these decisions are suspect after this block changed";
/// The label of the option that lands the re-affirmation of every member (`o1`).
pub const HOLD_LABEL: &str = rdy_store::REAFFIRM_HOLD_LABEL;
/// The label of the option that keeps every member suspect (`o2`).
pub const KEEP_LABEL: &str = "Keep them suspect";
/// The name of the card's one fact.
pub const CAUSE_FACT: &str = "Cause";
/// The most members one card binds.
pub const MAX_CARD_MEMBERS: usize = 100_000;
/// The most member/suspect pairs one card binds.
pub const MAX_CARD_PAIRS: usize = 100_000;
/// The most bytes one card's encoded binding takes.
pub const MAX_CARD_BYTES: usize = 16 * 1024 * 1024;
/// The prefix of a re-affirmation card's ref.
pub const REAFFIRM_REF_PREFIX: &str = "reaffirm:";

/// One member of a re-affirmation card as its hash binds it: the decision, its full version in
/// canon, and every structural suspect it has there, by target then reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReaffirmMember {
    pub id: BlockId,
    #[serde(with = "rdy_store::transition::hex::one")]
    pub version: Hash,
    pub suspects: Vec<ReaffirmSuspect>,
}

/// One structural suspect of a member: its target and the graph's reason.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ReaffirmSuspect {
    pub target: BlockId,
    pub reason: String,
}

/// What the latest open proposal versions do to accepted decisions: the ones they re-affirm and
/// the ones they fully retire. A card leaves them to that work.
#[derive(Clone, Debug, Default)]
pub(crate) struct OpenWork {
    pub(crate) reaffirms: BTreeSet<BlockId>,
    pub(crate) retires: BTreeSet<BlockId>,
}

impl OpenWork {
    fn excludes(&self, id: &BlockId) -> bool {
        self.reaffirms.contains(id) || self.retires.contains(id)
    }
}

/// The refusal of a card past its bounds.
pub fn card_limit_error() -> RdyError {
    RdyError::new(
        E_LIMIT,
        "re-affirmation card exceeds its record limit",
        "re-affirm smaller groups with rdy_write / rdy reaffirm",
    )
}

/// The bounds of one card: its members, its member/suspect pairs and its encoded binding's
/// bytes. The queue checks each once, where it builds the card: the members before any member
/// is read, the pairs as each member's suspects join, the bytes once the binding is complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardLimits {
    pub members: usize,
    pub pairs: usize,
    pub bytes: usize,
}

impl CardLimits {
    /// The product's bounds: [`MAX_CARD_MEMBERS`], [`MAX_CARD_PAIRS`] and [`MAX_CARD_BYTES`].
    pub const PRODUCT: CardLimits = CardLimits {
        members: MAX_CARD_MEMBERS,
        pairs: MAX_CARD_PAIRS,
        bytes: MAX_CARD_BYTES,
    };

    fn members(&self, n: usize) -> Result<(), RdyError> {
        if n > self.members {
            return Err(card_limit_error());
        }
        Ok(())
    }

    fn pairs(&self, n: usize) -> Result<(), RdyError> {
        if n > self.pairs {
            return Err(card_limit_error());
        }
        Ok(())
    }

    fn bytes(&self, n: usize) -> Result<(), RdyError> {
        if n > self.bytes {
            return Err(card_limit_error());
        }
        Ok(())
    }
}

thread_local! {
    /// This thread's card bounds: the product's, except inside
    /// [`with_card_limits_for_tests`].
    static LIMITS: std::cell::Cell<CardLimits> = const { std::cell::Cell::new(CardLimits::PRODUCT) };
}

/// Runs `f` with this thread's card bounds set to `limits`, then restores the product's: the
/// fault hook that lets a test reach each bound of the queue's own card construction with a
/// small project. The product never calls it.
pub fn with_card_limits_for_tests<T>(limits: CardLimits, f: impl FnOnce() -> T) -> T {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            LIMITS.with(|l| l.set(CardLimits::PRODUCT));
        }
    }
    LIMITS.with(|l| l.set(limits));
    let _restore = Restore;
    f()
}

/// Checks one card's bounds: at most [`MAX_CARD_MEMBERS`] members, [`MAX_CARD_PAIRS`]
/// member/suspect pairs and [`MAX_CARD_BYTES`] bytes of encoded binding; else
/// [`card_limit_error`]. The queue applies the same three checks of [`CardLimits`].
pub fn check_card_bounds(members: usize, pairs: usize, bytes: usize) -> Result<(), RdyError> {
    let l = CardLimits::PRODUCT;
    l.members(members)?;
    l.pairs(pairs)?;
    l.bytes(bytes)
}

/// The ref of the card of `cause` at its version `version`.
pub fn reaffirm_ref(cause: &BlockId, version: &Hash) -> String {
    format!("{REAFFIRM_REF_PREFIX}{cause}@{}", version.short())
}

/// The bytes of a LEB128 varint of `n`, as postcard writes a length or a variant index.
fn varint(n: usize) -> usize {
    let mut n = n as u64;
    let mut bytes = 1;
    while n >= 0x80 {
        n >>= 7;
        bytes += 1;
    }
    bytes
}

/// The encoded bytes of a string: its length, then its bytes.
fn str_len(s: &str) -> usize {
    varint(s.len()).saturating_add(s.len())
}

/// The bytes of [`PlanCardBinding::Reaffirm`]'s postcard encoding, computed without encoding it:
/// the variant index, the cause, the version's 32 bytes, the retirements and the members.
pub fn reaffirm_binding_len(
    cause: &BlockId,
    retirements: &[BlockId],
    members: &[ReaffirmMember],
) -> usize {
    let mut n = varint(1)
        .saturating_add(str_len(cause.as_str()))
        .saturating_add(32)
        .saturating_add(varint(retirements.len()));
    for r in retirements {
        n = n.saturating_add(str_len(r.as_str()));
    }
    n = n.saturating_add(varint(members.len()));
    for m in members {
        n = n
            .saturating_add(str_len(m.id.as_str()))
            .saturating_add(32)
            .saturating_add(varint(m.suspects.len()));
        for s in &m.suspects {
            n = n
                .saturating_add(str_len(s.target.as_str()))
                .saturating_add(str_len(&s.reason));
        }
    }
    n
}

/// Whether `id` is an accepted decision of canon that is live or partly superseded.
fn reaffirmable(m: &crate::Materialized, g: &rdy_graph::Graph, id: &BlockId) -> bool {
    m.get(id).is_some_and(|b| b.block_type == DECISION)
        && matches!(
            g.status(id),
            Some(DerivedStatus::Live | DerivedStatus::PartiallySuperseded { .. })
        )
}

/// The cause's latest block object: its version in canon, or the version the delete that removed
/// it from canon removed. None when neither is found.
fn cause_block(
    st: &ReadState,
    m: &crate::Materialized,
    cause: &BlockId,
) -> Result<Option<(Hash, Block, Option<Hash>)>, RdyError> {
    if let Some(b) = m.get(cause) {
        return Ok(Some((b.version_hash(), b.clone(), None)));
    }
    let reader = st.reader();
    let Some(deleter) = reader.deleter_at(cause, st.canon_head().as_ref())? else {
        return Ok(None);
    };
    for op in reader.change(&deleter)?.ops {
        if let Op::Delete { base, id } = op
            && id == *cause
        {
            return Ok(Some((base, reader.get_block(&base)?, Some(deleter))));
        }
    }
    Ok(None)
}

/// The landing that last wrote block version `v` into canon, if any.
pub(crate) fn landing_of_version(st: &ReadState, v: &Hash) -> Result<Option<Hash>, RdyError> {
    let reader = st.reader();
    let producers = reader.producers(v)?;
    let Some(change) = reader.producer_at(&producers, st.canon_head().as_ref())? else {
        return Ok(None);
    };
    Ok(reader.first_carrier(&change, Some(st.seq()))?)
}

/// Whether `b` is an accepted decision that supersedes or revokes `cause`: the retirement
/// relation [`rdy_graph::Graph::retiring_decisions`] reads.
fn retires(b: &Block, cause: &BlockId) -> bool {
    decision::status(b) == Some(DecisionStatus::Accepted)
        && [decision::SUPERSEDES, decision::REVOKES]
            .iter()
            .filter_map(|k| b.links.get(*k))
            .flatten()
            .any(|l| l.target == *cause)
}

/// The most retirement origins one [`RetirementOrigins`] keeps; past it, it starts again.
const MAX_RETIREMENT_ORIGINS: usize = 100_000;

/// What the card builds have learnt about where retirements began (final review F4): for one
/// occurrence of a retiring decision's version (the version and the change that wrote it there)
/// and the retired cause, the change that wrote the earliest version of its unbroken run of
/// retiring versions (None when that version has no producer). Every state a publish carries to
/// (all but a full publish's) shares it, so a metadata-only version of a retiring decision
/// costs one step past the occurrence whose origin is known, however long its history.
///
/// An entry is a fact of the log, the same for every reader: the walk steps from each change to
/// the change that wrote its base before it ([`rdy_store::StoreReader::producer_before`]), so a
/// version the decision took twice is two occurrences with their own histories (fix round 2,
/// N1), and no later landing changes what came before a change.
#[derive(Default)]
pub(crate) struct RetirementOrigins(
    std::sync::Mutex<std::collections::HashMap<OriginKey, Option<Hash>>>,
);

/// A retiring decision's version, the change that wrote it, and the retired cause.
type OriginKey = (Hash, Hash, BlockId);

impl RetirementOrigins {
    fn get(&self, k: &OriginKey) -> Option<Option<Hash>> {
        self.0.lock().ok()?.get(k).copied()
    }

    fn put(&self, keys: Vec<OriginKey>, origin: Option<Hash>) {
        let Ok(mut m) = self.0.lock() else { return };
        if m.len().saturating_add(keys.len()) > MAX_RETIREMENT_ORIGINS {
            m.clear();
        }
        for k in keys {
            m.insert(k, origin);
        }
    }
}

thread_local! {
    /// The retiring-decision versions this thread's card builds have stepped through (see
    /// `retirement_steps_for_tests`).
    static RETIREMENT_STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Tests only: how many versions of retiring decisions the card builds on this thread have
/// stepped through to find where a retirement began (each step reads a version's producers,
/// and on a miss its change and base block), so a test can check the count stays bounded as
/// metadata-only versions accumulate.
#[doc(hidden)]
pub fn retirement_steps_for_tests() -> u64 {
    RETIREMENT_STEPS.with(std::cell::Cell::get)
}

/// The landing that introduced the retirement of `cause` by `retirer` (its canon version): the
/// landing of the change that wrote the earliest version of the retirer's unbroken run of
/// versions back from canon that each retire `cause`. A later version that changes only the
/// retirer's metadata (a re-affirmation, say) keeps the retirement and is not its origin.
///
/// The walk follows the log, one occurrence at a time: canon's version as the change that last
/// wrote it at the state's head, then each base as the change that wrote it before the change
/// that read it ([`rdy_store::StoreReader::producer_before`]). So a retirer that returns to an
/// earlier version walks that version's later history, not its first one, and the walk ends:
/// each step goes back in the log. It stops at the first occurrence whose origin the state's
/// [`RetirementOrigins`] knows, and records the origin of every occurrence it stepped through.
fn retirement_landing(
    st: &ReadState,
    retirer: &Block,
    cause: &BlockId,
) -> Result<Option<Hash>, RdyError> {
    let reader = st.reader();
    let known = &st.retirement_origins;
    let mut v = retirer.version_hash();
    RETIREMENT_STEPS.with(|c| c.set(c.get() + 1));
    let Some(mut change) = reader.producer_at(&reader.producers(&v)?, st.canon_head().as_ref())?
    else {
        return Ok(None);
    };
    let mut walked: Vec<OriginKey> = vec![];
    let origin = loop {
        let key = (v, change, cause.clone());
        if let Some(origin) = known.get(&key) {
            break origin;
        }
        walked.push(key);
        let base = reader
            .change(&change)?
            .ops
            .into_iter()
            .find_map(|op| match op {
                Op::Update { base, block } if block.version_hash() == v => Some(base),
                _ => None,
            });
        let Some(base) = base else {
            break Some(change);
        };
        if !retires(&reader.get_block(&base)?, cause) {
            break Some(change);
        }
        RETIREMENT_STEPS.with(|c| c.set(c.get() + 1));
        let Some(before) = reader.producer_before(&reader.producers(&base)?, &change)? else {
            // the base came into canon with no landed change: the old walk's origin, a version
            // with no landing
            break None;
        };
        v = base;
        change = before;
    };
    known.put(walked, origin);
    match origin {
        Some(c) => Ok(reader.first_carrier(&c, Some(st.seq()))?),
        None => Ok(None),
    }
}

/// Who raised the card of a cause and when: the approver of the last landing that changed the
/// cause (wrote its canon version), deleted it (`deleter`) or retired it (introduced a retiring
/// decision's retirement of it, [`retirement_landing`]), by log order; else canon's head
/// landing.
fn raised_by(
    st: &ReadState,
    m: &crate::Materialized,
    cause: &BlockId,
    cause_in_canon: Option<Hash>,
    deleter: Option<Hash>,
    retirements: &[BlockId],
) -> Result<Raised, RdyError> {
    let reader = st.reader();
    let mut landings = vec![];
    if let Some(v) = cause_in_canon {
        landings.extend(landing_of_version(st, &v)?);
    }
    if let Some(d) = deleter {
        landings.extend(reader.first_carrier(&d, Some(st.seq()))?);
    }
    let mut retired = vec![];
    for r in retirements {
        if let Some(b) = m.get(r) {
            retired.extend(retirement_landing(st, b, cause)?);
        }
    }
    landings.extend(retired.iter().copied());
    let mut last: Option<(rdy_store::segment::Location, Hash)> = None;
    for l in landings {
        let key = (reader.landing_location(&l)?, l);
        last = Some(last.map_or(key, |k| k.max(key)));
    }
    let last = last.map(|(_, l)| l);
    let Some(l) = last.or(st.canon_head()) else {
        return Ok(Raised {
            last,
            retired,
            by: None,
        });
    };
    let rec = reader.landing(&l)?;
    Ok(Raised {
        last,
        retired,
        by: Some((rec.approved_by.actor, rec.approved_by.at_ms)),
    })
}

/// What [`raised_by`] finds for one cause.
struct Raised {
    /// The landing that last changed, deleted or retired the cause, if any is found.
    last: Option<Hash>,
    /// The landings that introduced its retirements.
    retired: Vec<Hash>,
    /// The approver of `last` (else of canon's head landing) and the time of that landing.
    by: Option<(String, i64)>,
}

/// The name of the view a landing came from: its proposal's view.
fn landing_view(st: &ReadState, landing: &Hash) -> Result<String, RdyError> {
    let reader = st.reader();
    let rec = reader.landing(landing)?;
    Ok(reader.proposal(&rec.proposal)?.view)
}

/// The landings that changed one cause, newest first, read back through its versions as far as a
/// caller asks: first the landing of the cause's current state, then, one step at a time, the
/// landing of each earlier version (a delete counts as the step that removed the cause). The
/// retirements of the cause join as landings of their own.
struct CauseHistory {
    cause: BlockId,
    /// The next step back: the change to read and where it sits.
    next: Option<Hash>,
    /// The landings read so far with where the log holds them, by log order, newest first.
    seen: Vec<(rdy_store::segment::Location, Hash)>,
    /// The landings that retired the cause.
    retired: Vec<(rdy_store::segment::Location, Hash)>,
}

impl CauseHistory {
    fn new(
        st: &ReadState,
        cause: &BlockId,
        version: &Hash,
        deleter: Option<Hash>,
        retired: &[Hash],
    ) -> Result<Self, RdyError> {
        let reader = st.reader();
        let next = match deleter {
            Some(d) => Some(d),
            None => reader.producer_at(&reader.producers(version)?, st.canon_head().as_ref())?,
        };
        let mut retired_at = vec![];
        for l in retired {
            retired_at.push((reader.landing_location(l)?, *l));
        }
        Ok(CauseHistory {
            cause: cause.clone(),
            next,
            seen: vec![],
            retired: retired_at,
        })
    }

    /// Reads back until the oldest landing read is at or before `bound`, or the history ends.
    fn read_to(
        &mut self,
        st: &ReadState,
        bound: rdy_store::segment::Location,
    ) -> Result<(), RdyError> {
        let reader = st.reader();
        while self.seen.last().is_none_or(|(at, _)| *at > bound) {
            let Some(change) = self.next.take() else {
                return Ok(());
            };
            let Some(landing) = reader.first_carrier(&change, Some(st.seq()))? else {
                return Ok(());
            };
            self.seen
                .push((reader.landing_location(&landing)?, landing));
            let base = reader
                .change(&change)?
                .ops
                .into_iter()
                .find_map(|op| match op {
                    Op::Update { base, block } if block.id == self.cause => Some(base),
                    Op::Delete { base, id } if id == self.cause => Some(base),
                    _ => None,
                });
            if let Some(base) = base {
                self.next = reader.producer_before(&reader.producers(&base)?, &change)?;
            }
        }
        Ok(())
    }

    /// The landings that changed or retired the cause after `bound`.
    fn since(
        &mut self,
        st: &ReadState,
        bound: rdy_store::segment::Location,
    ) -> Result<Vec<Hash>, RdyError> {
        self.read_to(st, bound)?;
        let mut out: Vec<Hash> = self
            .seen
            .iter()
            .chain(&self.retired)
            .filter(|(at, _)| *at > bound)
            .map(|(_, l)| *l)
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }
}

/// The view of `landing`, read once into `views`.
fn cached_view(
    st: &ReadState,
    views: &mut BTreeMap<Hash, String>,
    landing: Hash,
) -> Result<String, RdyError> {
    if let Some(v) = views.get(&landing) {
        return Ok(v.clone());
    }
    let v = landing_view(st, &landing)?;
    views.insert(landing, v.clone());
    Ok(v)
}

/// The members of one card that the landing's own edits left no reason to ask about (see the
/// module docs): `ids` less every member whose own landing also made the cause's state.
/// `last` is the landing that last changed, deleted or retired the cause.
fn without_own_landings<'a>(
    st: &ReadState,
    m: &crate::Materialized,
    history: &mut CauseHistory,
    last: Option<Hash>,
    ids: &BTreeSet<&'a BlockId>,
) -> Result<BTreeSet<&'a BlockId>, RdyError> {
    let Some(last) = last else {
        return Ok(ids.clone());
    };
    let reader = st.reader();
    let mut views: BTreeMap<Hash, String> = BTreeMap::new();
    let mut keep = BTreeSet::new();
    for id in ids {
        let Some(b) = m.get(id) else {
            keep.insert(*id);
            continue;
        };
        let Some(own) = landing_of_version(st, &b.version_hash())? else {
            keep.insert(*id);
            continue;
        };
        if own == last {
            continue;
        }
        if b.field_str("reversibility") == Some("one_way") {
            keep.insert(*id);
            continue;
        }
        let after = history.since(st, reader.landing_location(&own)?)?;
        if after.is_empty() {
            keep.insert(*id);
            continue;
        }
        let own_view = cached_view(st, &mut views, own)?;
        let mut same = true;
        for l in &after {
            if cached_view(st, &mut views, *l)? != own_view {
                same = false;
                break;
            }
        }
        if !same {
            keep.insert(*id);
        }
    }
    Ok(keep)
}

/// The re-affirmation cards of canon (see the module docs), by cause id, with who raised each.
/// `open` is what the latest open versions re-affirm or retire. A card past its bounds is
/// `E_LIMIT`.
pub(crate) fn reaffirm_items(
    st: &ReadState,
    open: &OpenWork,
    now: i64,
) -> Result<Vec<Item>, RdyError> {
    let structural = st.suspects(&Scope::Canon)?;
    if structural.is_empty() {
        return Ok(vec![]);
    }
    // the shared projection: each structural row with the deferral a person accepted for it,
    // and no code row
    let rows = st.suspect_rows(&Scope::Canon, structural, Some(&[]))?;
    let m = st.materialized(&Scope::Canon)?;
    let g = st.graph(&Scope::Canon)?;
    let mut suspects: BTreeMap<&BlockId, Vec<ReaffirmSuspect>> = BTreeMap::new();
    let mut groups: BTreeMap<&BlockId, BTreeSet<&BlockId>> = BTreeMap::new();
    for row in &rows {
        let Some(target) = row.target.as_ref().and_then(crate::RowTarget::block) else {
            continue;
        };
        suspects
            .entry(&row.block)
            .or_default()
            .push(ReaffirmSuspect {
                target: target.clone(),
                reason: row.reason.clone(),
            });
        if row.deferred.is_some() || open.excludes(&row.block) || !reaffirmable(&m, &g, &row.block)
        {
            continue;
        }
        groups.entry(target).or_default().insert(&row.block);
    }
    let limits = LIMITS.with(std::cell::Cell::get);
    let mut out = vec![];
    for (cause, ids) in groups {
        let Some((version, block, deleter)) = cause_block(st, &m, cause)? else {
            continue;
        };
        limits.members(ids.len())?;
        let retirements: Vec<BlockId> = g.retiring_decisions(cause).into_iter().collect();
        let in_canon = m.get(cause).map(|b| b.version_hash());
        let raised = raised_by(st, &m, cause, in_canon, deleter, &retirements)?;
        let mut history = CauseHistory::new(st, cause, &version, deleter, &raised.retired)?;
        let ids = without_own_landings(st, &m, &mut history, raised.last, &ids)?;
        if ids.is_empty() {
            continue;
        }
        let mut pairs = 0usize;
        let mut members = Vec::with_capacity(ids.len());
        for id in &ids {
            let mut own = suspects.get(id).cloned().unwrap_or_default();
            own.sort();
            own.dedup();
            pairs = pairs.saturating_add(own.len());
            limits.pairs(pairs)?;
            let version = m
                .get(id)
                .map(|b| b.version_hash())
                .expect("a member is a decision of canon");
            members.push(ReaffirmMember {
                id: (*id).clone(),
                version,
                suspects: own,
            });
        }
        limits.bytes(reaffirm_binding_len(cause, &retirements, &members))?;
        let title = match text(&block, "title") {
            Some(t) => format!("Re-affirm after {t}"),
            None => format!("Re-affirm after {cause}"),
        };
        let mut it = item(ItemKind::Reaffirm, &reaffirm_ref(cause, &version), &title);
        it.why = REAFFIRM_WHY.to_string();
        it.summary = Some(format!("Re-affirm the listed decisions after {cause}."));
        it.facts = vec![Fact {
            name: CAUSE_FACT.into(),
            value: cause.to_string(),
        }];
        it.members = ids
            .iter()
            .map(|id| {
                let title = m
                    .get(id)
                    .and_then(|b| text(b, "title"))
                    .map_or_else(|| id.to_string(), str::to_string);
                format!("{id}  {title}")
            })
            .collect();
        it.options = vec![
            option(
                "o1",
                HOLD_LABEL,
                "Records that each listed decision still holds, and lands that in canon.",
                None,
            ),
            option(
                "o2",
                KEEP_LABEL,
                "Each listed decision stays suspect, with your reason beside it.",
                None,
            ),
        ];
        if let Some((by, at)) = raised.by {
            it.raised_by = by;
            it.raised_at = rfc3339(at);
            it.age = age(at, now);
        }
        it.plan_binding = Some(PlanCardBinding::Reaffirm {
            cause: cause.clone(),
            version,
            retirements,
            members,
        });
        out.push(it);
    }
    Ok(out)
}
