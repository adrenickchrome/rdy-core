//! Organization and team locks (repo links, D-8VJAZ, D-MEMMQ, D-4W1ZR, rpl23).
//!
//! # Lock force
//!
//! A cited instruction is locked here when a live, unrevoked repo link pins it in its instruction
//! baseline at a version whose `locked` is true, and the link's last adopt answer served an
//! organization or team set: the same foreign identity and adopted baseline that
//! `rdy_instructions` reads ([`crate::foreign_instructions`]). A lock that a delivery only offers
//! has no force until an adopt answer puts it in the baseline, and a no-set cited lock never has
//! force. Of two links that pin one instruction, a link with a set gives the lock its force; the
//! first such link in alias order names it.
//!
//! # Refusals
//!
//! | Where | Condition | Fix |
//! |---|---|---|
//! | a write or decision ([`check_deviation`]), the landing ([`check_landing`]) | a decision that departs from a locked instruction | [`LOCKED_FIX`] |
//! | an adopt answer ([`check_adopt`]) | the adopted baseline locks an instruction a live deviation departs from | `revoke the deviation <id> first, then answer the adopt item again` |
//!
//! Each is `E_INSTRUCTION_LOCKED`, `<alias>:<id> is locked by its <organization | team <name>> set`.
//! A rejected decision departs from nothing, so it is never refused.
//!
//! # Facts for the hub
//!
//! Pure reads of one captured state: [`ReadState::lock_facts`] (new deviations from locked
//! instructions in some changes), [`ReadState::lock_changes`] (a set owner's changes to locks or
//! locked instructions) and [`ReadState::unadopted_locked`] (each delivery's lock changes against
//! the adopted baseline). The hub owns deadlines.
use crate::error::{E_INSTRUCTION_LOCKED, RdyError};
use crate::foreign_refs::{RepoLinks, instruction_baseline};
use crate::pending::adopt::adopted_set;
use crate::{Materialized, ReadState, Scope};
use rdy_model::decision::{self, DecisionStatus};
use rdy_model::foreign::{ForeignEvent, ForeignId, ForeignPin, ForeignSet, ForeignSource};
use rdy_model::{Block, BlockId, FieldValue, Hash, Op, RepoId};
use rdy_schema::DEVIATES_FROM;
use rdy_schema::instruction::{INSTRUCTION_PREFIX, LOCKED};
use rdy_store::foreign::{MAX_CHAIN, SourceView};
use std::collections::{BTreeMap, BTreeSet};

/// The fix of a deviation from a locked instruction.
pub const LOCKED_FIX: &str = "add an instruction on top of it, or ask the set's owners to change it there: rdy_write a new instruction / rdy new instruction";

/// The words of a set in a lock refusal: `organization` or `team <name>`.
fn set_words(set: &ForeignSet) -> String {
    match set {
        ForeignSet::Organization => "organization".to_string(),
        ForeignSet::Team(n) => format!("team {n}"),
    }
}

/// `E_INSTRUCTION_LOCKED`: `<alias>:<id> is locked by its <set> set`, with `fix`.
fn locked(alias: &str, id: &BlockId, set: &ForeignSet, fix: String) -> RdyError {
    RdyError::new(
        E_INSTRUCTION_LOCKED,
        format!("{alias}:{id} is locked by its {} set", set_words(set)),
        fix,
    )
}

fn is_instruction(id: &BlockId) -> bool {
    id.prefix() == INSTRUCTION_PREFIX
}

/// Whether `b` is locked: its `locked` field is true.
fn is_locked(b: &Block) -> bool {
    b.fields.get(LOCKED) == Some(&FieldValue::Bool(true))
}

/// One locked instruction in force: the link that pins it, the alias that shows it, the set that
/// gives the lock its force and the pinned version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockedRule {
    pub link: BlockId,
    pub alias: String,
    pub target: ForeignId,
    pub set: ForeignSet,
    pub version: Hash,
}

/// The locked instructions in force in the state's canon, by cited identity (see the module
/// docs).
fn locked_in_force(st: &ReadState) -> Result<BTreeMap<ForeignId, LockedRule>, RdyError> {
    let mut out = BTreeMap::new();
    let foreign = st.foreign()?;
    if foreign.is_empty() {
        return Ok(out);
    }
    let m = st.materialized(&Scope::Canon)?;
    let links = RepoLinks::of_view(&m);
    for (alias, row) in links.rows() {
        let Some(view) = foreign.link_source(&row.link) else {
            continue;
        };
        if view.revoked {
            continue;
        }
        let Some(link) = m.get(&row.link) else {
            continue;
        };
        let Some(set) = adopted_set(st, &row.repo, view.head)? else {
            continue;
        };
        for (target, pin) in instruction_baseline(link) {
            if out.contains_key(target) {
                continue;
            }
            let Some(version) = st.foreign_held_pin(target, pin)? else {
                continue;
            };
            if st
                .foreign_held(target, &version)?
                .is_some_and(|b| is_locked(&b))
            {
                out.insert(
                    target.clone(),
                    LockedRule {
                        link: row.link.clone(),
                        alias: alias.to_string(),
                        target: target.clone(),
                        set: set.clone(),
                        version,
                    },
                );
            }
        }
    }
    Ok(out)
}

/// The cited targets `b` departs from, when `b` is a decision that is not rejected.
fn departs_from(b: &Block) -> Vec<&ForeignId> {
    if b.block_type != decision::DECISION || decision::status(b) == Some(DecisionStatus::Rejected) {
        return vec![];
    }
    b.foreign
        .links
        .get(DEVIATES_FROM)
        .into_iter()
        .flatten()
        .map(|l| &l.target)
        .collect()
}

/// Refuses `block` when it is a decision that departs from a locked instruction (see the module
/// docs). The write and decision paths call it on each block they create or update.
pub(crate) fn check_deviation(st: &ReadState, block: &Block) -> Result<(), RdyError> {
    let targets = departs_from(block);
    if targets.is_empty() {
        return Ok(());
    }
    let locks = locked_in_force(st)?;
    for t in targets {
        if let Some(l) = locks.get(t) {
            return Err(locked(&l.alias, &t.id, &l.set, LOCKED_FIX.to_string()));
        }
    }
    Ok(())
}

/// Refuses the landing of `ops` when a block it writes is a decision that departs from a
/// locked instruction: a deviation written before its target's lock was adopted.
pub(crate) fn check_landing<'a>(
    st: &ReadState,
    ops: impl IntoIterator<Item = &'a Op>,
) -> Result<(), RdyError> {
    let mut locks: Option<BTreeMap<ForeignId, LockedRule>> = None;
    for op in ops {
        let (Op::Create { block } | Op::Update { block, .. }) = op else {
            continue;
        };
        let targets = departs_from(block);
        if targets.is_empty() {
            continue;
        }
        if locks.is_none() {
            locks = Some(locked_in_force(st)?);
        }
        let locks = locks.as_ref().expect("read above");
        for t in targets {
            if let Some(l) = locks.get(t) {
                return Err(locked(&l.alias, &t.id, &l.set, LOCKED_FIX.to_string()));
            }
        }
    }
    Ok(())
}

/// Refuses the adopt answer of the delivery `entry` from `repo` into link `link` when the
/// baseline it leaves (`pins`, the link block's foreign pins after the adoption) locks an
/// instruction that a live deviation of canon departs from. The set is the delivery's: the one
/// the answer will have served.
pub(crate) fn check_adopt<'a>(
    st: &ReadState,
    link: &BlockId,
    repo: &RepoId,
    entry: &Hash,
    pins: impl IntoIterator<Item = (&'a ForeignId, &'a ForeignPin)>,
) -> Result<(), RdyError> {
    let ForeignEvent::Delivery { set: Some(set), .. } = st.reader().foreign_entry(entry)?.event
    else {
        return Ok(());
    };
    let live: Vec<_> = st
        .deviations(&Scope::Canon)?
        .into_iter()
        .filter(|d| d.live && d.target.repo == *repo)
        .collect();
    if live.is_empty() {
        return Ok(());
    }
    let m = st.materialized(&Scope::Canon)?;
    let links = RepoLinks::of_view(&m);
    let alias = links
        .rows()
        .find(|(_, r)| r.link == *link)
        .map(|(a, _)| a.to_string())
        .unwrap_or_else(|| links.shown(repo));
    for (target, pin) in pins {
        if target.repo != *repo || !is_instruction(&target.id) {
            continue;
        }
        let Some(d) = live.iter().find(|d| d.target == *target) else {
            continue;
        };
        let Some(version) = st.foreign_held_pin(target, pin)? else {
            continue;
        };
        if st
            .foreign_held(target, &version)?
            .is_some_and(|b| is_locked(&b))
        {
            return Err(locked(
                &alias,
                &target.id,
                &set,
                format!(
                    "revoke the deviation {} first, then answer the adopt item again",
                    d.decision
                ),
            ));
        }
    }
    Ok(())
}

/// What a change does to a lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LockEdit {
    /// A locked instruction appears, or an instruction becomes locked.
    Locked,
    /// A locked instruction stays locked and its content changes.
    Changed,
    /// A locked instruction becomes unlocked, or loses the set that gave its lock force.
    Unlocked,
    /// A locked instruction is deleted, or no longer delivered.
    Removed,
}

/// One new deviation from a locked instruction in force ([`ReadState::lock_facts`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockFact {
    /// The change that writes the deviation.
    pub change: Hash,
    /// The deviating decision.
    pub decision: BlockId,
    /// The locked instruction it departs from.
    pub target: ForeignId,
    /// The repo link whose baseline holds the lock, and its alias.
    pub link: BlockId,
    pub alias: String,
    /// The set that gives the lock its force.
    pub set: ForeignSet,
    /// The locked version in force.
    pub version: Hash,
    /// The FOREIGN entry that first delivered that version, when one did.
    pub entry: Option<Hash>,
}

/// One change of a set owner to a lock or a locked instruction ([`ReadState::lock_changes`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockChange {
    /// The change that makes it.
    pub change: Hash,
    /// The instruction.
    pub id: BlockId,
    pub edit: LockEdit,
    /// The instruction's version before and after the change (`None`: absent).
    pub before: Option<Hash>,
    pub after: Option<Hash>,
}

/// One delivered lock change that the adopted baseline does not hold
/// ([`ReadState::unadopted_locked`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnadoptedLock {
    /// The repo link, its alias and the cited repo.
    pub link: BlockId,
    pub alias: String,
    pub repo: RepoId,
    /// The instruction.
    pub id: BlockId,
    /// `Locked` (a lock the baseline lacks), `Changed` (a locked version the baseline does not
    /// pin), `Unlocked` (a baseline lock the delivery clears) or `Removed` (a baseline lock the
    /// delivery no longer selects).
    pub change: LockEdit,
    /// The baseline's locked version, when the baseline locks it.
    pub adopted: Option<Hash>,
    /// The delivered version, when the delivery selects the instruction.
    pub delivered: Option<Hash>,
    /// The delivery's set.
    pub set: Option<ForeignSet>,
    /// The FOREIGN entry that made the change, and its delivery time (0 for a source with none).
    pub entry: Hash,
    pub at_ms: i64,
}

/// The delivery time of `entry`: its link source's `at_ms`, else 0.
fn at_ms(st: &ReadState, entry: &Hash) -> Result<i64, RdyError> {
    Ok(match st.reader().foreign_entry(entry)?.source {
        ForeignSource::Link { at_ms, .. } => at_ms,
        ForeignSource::Adr { .. } => 0,
    })
}

/// The newest entry of the source whose newest entry is `head` that lists `id` as removed, else
/// `head`.
fn removed_at(st: &ReadState, head: Hash, id: &BlockId) -> Result<Hash, RdyError> {
    let mut next = Some(head);
    let mut steps = 0usize;
    while let Some(h) = next {
        steps += 1;
        if steps > MAX_CHAIN {
            break;
        }
        let ForeignEvent::Delivery { prev, removed, .. } = st.reader().foreign_entry(&h)?.event
        else {
            break;
        };
        if removed.contains(id) {
            return Ok(h);
        }
        next = prev;
    }
    Ok(head)
}

/// The oldest entry of the unbroken run, back from `head`, whose deliveries serve `head`'s set:
/// the entry where the source's set took its current value.
fn set_since(st: &ReadState, head: Hash) -> Result<Hash, RdyError> {
    let reader = st.reader();
    let ForeignEvent::Delivery { set, prev, .. } = reader.foreign_entry(&head)?.event else {
        return Ok(head);
    };
    let mut since = head;
    let mut next = prev;
    let mut steps = 0usize;
    while let Some(h) = next {
        steps += 1;
        if steps > MAX_CHAIN {
            break;
        }
        let ForeignEvent::Delivery {
            set: older, prev, ..
        } = reader.foreign_entry(&h)?.event
        else {
            break;
        };
        if older != set {
            break;
        }
        since = h;
        next = prev;
    }
    Ok(since)
}

/// The locked instructions of `members` with their versions: only when `set` gives locks force.
fn delivered_locks(st: &ReadState, view: &SourceView) -> Result<BTreeMap<BlockId, Hash>, RdyError> {
    let mut out = BTreeMap::new();
    if view.set.is_none() {
        return Ok(out);
    }
    for (id, held) in view.members.iter().filter(|(id, _)| is_instruction(id)) {
        let target = ForeignId::new(view.repo, id.clone());
        if st
            .foreign_held(&target, &held.version.version)?
            .is_some_and(|b| is_locked(&b))
        {
            out.insert(id.clone(), held.version.version);
        }
    }
    Ok(out)
}

impl ReadState {
    /// The new deviations from locked instructions in force that the changes `changes` write,
    /// against this state's canon and foreign cut: for each decision a change creates or updates
    /// (not rejected), each `deviates_from` target its base version did not depart from and that
    /// is locked in force. In change order, then decision and target order. A pure read.
    pub fn lock_facts(&self, changes: &[Hash]) -> Result<Vec<LockFact>, RdyError> {
        let mut out = vec![];
        let mut locks: Option<BTreeMap<ForeignId, LockedRule>> = None;
        for h in changes {
            let ch = self.reader().change(h)?;
            let mut rows = vec![];
            for op in &ch.ops {
                let (block, base) = match op {
                    Op::Create { block } => (block, None),
                    Op::Update { block, base } => (block, Some(base)),
                    Op::Delete { .. } => continue,
                };
                let targets = departs_from(block);
                if targets.is_empty() {
                    continue;
                }
                let before: BTreeSet<ForeignId> = match base {
                    Some(v) => {
                        let old = self.reader().get_block(v)?;
                        departs_from(&old).into_iter().cloned().collect()
                    }
                    None => BTreeSet::new(),
                };
                if locks.is_none() {
                    locks = Some(locked_in_force(self)?);
                }
                let locks = locks.as_ref().expect("read above");
                for t in targets.into_iter().filter(|t| !before.contains(*t)) {
                    let Some(l) = locks.get(t) else { continue };
                    rows.push(LockFact {
                        change: *h,
                        decision: block.id.clone(),
                        target: t.clone(),
                        link: l.link.clone(),
                        alias: l.alias.clone(),
                        set: l.set.clone(),
                        version: l.version,
                        entry: self.foreign_carrier(t, &l.version)?.map(|(_, e)| e),
                    });
                }
            }
            rows.sort_by(|a, b| (&a.decision, &a.target).cmp(&(&b.decision, &b.target)));
            rows.dedup_by(|a, b| (&a.decision, &a.target) == (&b.decision, &b.target));
            out.extend(rows);
        }
        Ok(out)
    }

    /// The changes to locks or locked instructions that the changes `changes` make, as the set
    /// owner's project records them: for each instruction a change creates, updates or deletes,
    /// read against its base version, `Locked` (created locked, or unlocked to locked),
    /// `Unlocked` (locked to unlocked), `Changed` (locked before and after, with other content)
    /// or `Removed` (a locked instruction deleted). In change order, then op order. A pure read.
    pub fn lock_changes(&self, changes: &[Hash]) -> Result<Vec<LockChange>, RdyError> {
        let mut out = vec![];
        for h in changes {
            let ch = self.reader().change(h)?;
            for op in &ch.ops {
                if !is_instruction(op.block_id()) {
                    continue;
                }
                let old = match op.base() {
                    Some(v) => Some(self.reader().get_block(&v)?),
                    None => None,
                };
                let new = match op {
                    Op::Create { block } | Op::Update { block, .. } => Some(block),
                    Op::Delete { .. } => None,
                };
                let was = old.as_ref().is_some_and(is_locked);
                let is = new.is_some_and(is_locked);
                let edit = match (&old, new) {
                    (_, None) if was => LockEdit::Removed,
                    (_, Some(_)) if !was && is => LockEdit::Locked,
                    (_, Some(_)) if was && !is => LockEdit::Unlocked,
                    (Some(o), Some(n)) if was && is && o.content_hash() != n.content_hash() => {
                        LockEdit::Changed
                    }
                    _ => continue,
                };
                out.push(LockChange {
                    change: *h,
                    id: op.block_id().clone(),
                    edit,
                    before: op.base(),
                    after: new.map(Block::version_hash),
                });
            }
        }
        Ok(out)
    }

    /// The lock changes that each live, unrevoked repo link's newest delivery makes against its
    /// adopted baseline (D-MEMMQ): the instructions its block pins, under the set of its last
    /// adopt answer. A lock counts only where a set gives it force. A first delivery's locks
    /// count against the empty baseline, with that delivery's time. The entry of a row is the
    /// one that delivered the version, removed the instruction, or gave the source its set. In
    /// alias order, then instruction order. A pure read of canon and the foreign cut.
    pub fn unadopted_locked(&self) -> Result<Vec<UnadoptedLock>, RdyError> {
        let mut out = vec![];
        let foreign = self.foreign()?;
        if foreign.is_empty() {
            return Ok(out);
        }
        let m: std::sync::Arc<Materialized> = self.materialized(&Scope::Canon)?;
        let links = RepoLinks::of_view(&m);
        for (alias, row) in links.rows() {
            let Some(view) = foreign.link_source(&row.link) else {
                continue;
            };
            if view.revoked {
                continue;
            }
            let adopted = adopted_set(self, &row.repo, view.head)?;
            let mut baseline: BTreeMap<BlockId, Hash> = BTreeMap::new();
            if adopted.is_some()
                && let Some(link) = m.get(&row.link)
            {
                for (target, pin) in instruction_baseline(link) {
                    if let Some(v) = self.foreign_held_pin(target, pin)?
                        && self
                            .foreign_held(target, &v)?
                            .is_some_and(|b| is_locked(&b))
                    {
                        baseline.insert(target.id.clone(), v);
                    }
                }
            }
            let delivered = delivered_locks(self, view)?;
            let set_moved = adopted != view.set;
            let mut push = |id: &BlockId, change: LockEdit, entry: Hash| -> Result<(), RdyError> {
                out.push(UnadoptedLock {
                    link: row.link.clone(),
                    alias: alias.to_string(),
                    repo: row.repo,
                    id: id.clone(),
                    change,
                    adopted: baseline.get(id).copied(),
                    delivered: view.members.get(id).map(|h| h.version.version),
                    set: view.set.clone(),
                    entry,
                    at_ms: at_ms(self, &entry)?,
                });
                Ok(())
            };
            let ids: BTreeSet<&BlockId> = delivered.keys().chain(baseline.keys()).collect();
            for id in ids {
                let entry_of = |id: &BlockId| view.members.get(id).map(|h| h.entry);
                match (baseline.get(id), delivered.get(id)) {
                    (None, Some(_)) => {
                        push(id, LockEdit::Locked, entry_of(id).unwrap_or(view.head))?;
                    }
                    (Some(old), Some(new)) if old != new => {
                        push(id, LockEdit::Changed, entry_of(id).unwrap_or(view.head))?;
                    }
                    (Some(_), Some(_)) if set_moved => {
                        push(id, LockEdit::Changed, set_since(self, view.head)?)?;
                    }
                    (Some(old), None) => match view.members.get(id) {
                        None => push(id, LockEdit::Removed, removed_at(self, view.head, id)?)?,
                        // the same version, delivered with no set: the set took the force
                        Some(h) if h.version.version == *old => {
                            push(id, LockEdit::Unlocked, set_since(self, view.head)?)?
                        }
                        Some(h) => push(id, LockEdit::Unlocked, h.entry)?,
                    },
                    _ => {}
                }
            }
        }
        Ok(out)
    }
}
