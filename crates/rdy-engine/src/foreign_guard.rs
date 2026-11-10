//! Read-only foreign content and deviations (repo links, D-8VJAZ).
//!
//! - A write that edits, moves, deletes, supersedes, refines or revokes a cited block or an
//!   external decision is `E_FOREIGN_READ_ONLY`. [`Guard`] reads each op's target and each
//!   supersedes, refines or revokes link target as written, before local-id parsing loses the
//!   foreign identity: the structured write and the decision paths both use it. A citation (a
//!   reference or another link kind) is no mutation. The redaction path takes no write op, so the
//!   guard never sees it.
//! - A decision departs from a cited decision, a cited instruction or an external decision with
//!   the engine link `deviates_from`, stored as a foreign link. Any other target is
//!   `E_VALIDATION`. Approval rules are the proposal's (a deviation is a CONFIRM exception).
//! - A deviation governs the local blocks that cite its target; its decision id is the waiver
//!   key ([`ReadState::deviations`]).
//! - A decision never departs from an instruction that an organization or team set locks in
//!   force: `E_INSTRUCTION_LOCKED` ([`crate::foreign_locks`]).
use crate::error::*;
use crate::foreign::ForeignState;
use crate::foreign_refs::{RepoLinks, unknown_alias};
use crate::write::OpInput;
use crate::{Materialized, RdyError, ReadState, Scope};
use rdy_graph::DerivedStatus;
use rdy_model::foreign::ForeignId;
use rdy_model::reference::{ForeignRef, foreign_ref};
use rdy_model::{Block, BlockId, RepoId, decision};
use rdy_schema::DEVIATES_FROM;
use rdy_schema::instruction::INSTRUCTION;
use rdy_store::foreign::{ADR_REF_PREFIX, SourceView};
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

/// The link kinds whose target a write changes: a foreign target is read-only.
const MUTATING_LINKS: [&str; 3] = [decision::SUPERSEDES, decision::REFINES, decision::REVOKES];

/// How both read-only fixes end.
const DEVIATE_TAIL: &str = "; to depart from it here, record a deviation: rdy_decide with deviates_from / a decision with deviates_from in rdy edit";

/// `E_FOREIGN_READ_ONLY`: `ref` is cited from `alias`.
fn read_only(r#ref: &str, alias: &str) -> RdyError {
    RdyError::new(
        E_FOREIGN_READ_ONLY,
        format!("{ref} is cited from {alias} and is read-only here"),
        format!("change it in its home project{DEVIATE_TAIL}"),
    )
}

/// `E_FOREIGN_READ_ONLY`: `key` is an external decision.
fn adr_read_only(key: &str) -> RdyError {
    RdyError::new(
        E_FOREIGN_READ_ONLY,
        format!("{key} is an external decision and is read-only here"),
        format!("change it in its home repo{DEVIATE_TAIL}"),
    )
}

/// `E_VALIDATION`: a `deviates_from` target that is not cited or external.
fn invalid_deviation(r#ref: &str) -> RdyError {
    RdyError::new(
        E_VALIDATION,
        format!("{DEVIATES_FROM} names {ref}, which is not cited or external"),
        "name a cited decision, a cited instruction or an external decision",
    )
}

/// The name and view of the ADR source that delivers `repo`, if one does: its blocks are
/// external decisions.
pub(crate) fn adr_source<'a>(
    foreign: &'a ForeignState,
    repo: &RepoId,
) -> Option<(&'a str, &'a SourceView)> {
    foreign.sources().find_map(|(name, view)| {
        let name = name.strip_prefix(ADR_REF_PREFIX)?;
        (view.repo == *repo).then_some((name, view))
    })
}

/// How an external decision shows: its key, as the source delivered it, else its id.
fn adr_key(st: &ReadState, view: &SourceView, id: &BlockId) -> Result<String, RdyError> {
    if let Some(held) = view.members.get(id) {
        let b = st.reader().get_block(&held.version.version)?;
        if let Some(k) = b.field_str("key") {
            return Ok(k.to_string());
        }
    }
    Ok(id.to_string())
}

/// The read-only check of one write or decision call: the view's live repo links (read only
/// once an op names a foreign block) and the state's foreign sources.
pub(crate) struct Guard<'a> {
    st: &'a ReadState,
    m: &'a Materialized,
    links: OnceCell<RepoLinks>,
}

impl<'a> Guard<'a> {
    /// The guard of a write to the view `m` materializes.
    pub(crate) fn new(st: &'a ReadState, m: &'a Materialized) -> Guard<'a> {
        Guard {
            st,
            m,
            links: OnceCell::new(),
        }
    }

    fn links(&self) -> &RepoLinks {
        self.links.get_or_init(|| RepoLinks::of_view(self.m))
    }

    /// The refusal of a change to the block `text` names, when it names a cited block or an
    /// external decision: `alias:id`, `alias:KEY` or the stored `<repo>:<id>`. An alias that no
    /// live link or ADR source has is `E_UNKNOWN_REF`. A local target passes.
    pub(crate) fn check(&self, text: &str) -> Result<(), RdyError> {
        let Some(target) = foreign_ref(text.trim()) else {
            return Ok(());
        };
        match target {
            ForeignRef::Alias { alias, target } => {
                if self.links().get(alias).is_some() {
                    return Err(read_only(&format!("{alias}:{target}"), alias));
                }
                let foreign = self.st.foreign()?;
                if foreign
                    .source(&format!("{ADR_REF_PREFIX}{alias}"))
                    .is_some()
                {
                    return Err(adr_read_only(target));
                }
                Err(unknown_alias(alias))
            }
            ForeignRef::Stored { repo, id } => {
                let foreign = self.st.foreign()?;
                if let Some((_, view)) = adr_source(foreign, &repo) {
                    return Err(adr_read_only(&adr_key(self.st, view, &id)?));
                }
                let shown = self.links().shown(&repo);
                Err(read_only(&format!("{shown}:{id}"), &shown))
            }
        }
    }

    /// Refuses the first op of `ops` that changes a foreign block: an edit, move, delete, repin
    /// or reaffirm of one, or a supersedes, refines or revokes link to one; and the first
    /// `deviates_from` target that names a local block.
    pub(crate) fn check_ops(&self, ops: &[OpInput]) -> Result<(), RdyError> {
        for op in ops {
            let (id, links) = match op {
                OpInput::Create { links, .. } => (None, Some(links)),
                OpInput::Update { id, links, .. } => (Some(id), links.as_ref()),
                OpInput::Move { id, .. }
                | OpInput::Delete { id, .. }
                | OpInput::Repin { id, .. } => (Some(id), None),
                OpInput::Reaffirm { id, .. } => (id.as_ref(), None),
                OpInput::Rebalance { .. } => (None, None),
            };
            if let Some(id) = id {
                self.check(id)?;
            }
            for (kind, targets) in links.into_iter().flatten() {
                if MUTATING_LINKS.contains(&kind.as_str()) {
                    for t in targets {
                        self.check(&t.target)?;
                    }
                }
                if kind == DEVIATES_FROM
                    && let Some(t) = targets
                        .iter()
                        .find(|t| foreign_ref(t.target.trim()).is_none())
                {
                    return Err(invalid_deviation(t.target.trim()));
                }
            }
        }
        Ok(())
    }
}

/// Refuses a `deviates_from` target of `block` that is cited but neither a decision nor an
/// instruction, and one that an organization or team set locks (`E_INSTRUCTION_LOCKED`,
/// [`crate::foreign_locks`]). An external decision always passes the type check. A target whose
/// pin no delivery holds is left to the held-pin check.
pub(crate) fn check_deviation_targets(
    st: &ReadState,
    links: &RepoLinks,
    block: &Block,
) -> Result<(), RdyError> {
    let Some(targets) = block.foreign.links.get(DEVIATES_FROM) else {
        return Ok(());
    };
    let foreign = st.foreign()?;
    for l in targets {
        let t = &l.target;
        if adr_source(foreign, &t.repo).is_some() {
            continue;
        }
        let Some(pin) = block.foreign.pins.get(t) else {
            continue;
        };
        let Some(version) = st.foreign_held_pin(t, pin)? else {
            continue;
        };
        let cited = st.reader().get_block(&version)?;
        if cited.block_type != decision::DECISION && cited.block_type != INSTRUCTION {
            return Err(invalid_deviation(&format!(
                "{}:{}",
                links.shown(&t.repo),
                t.id
            )));
        }
    }
    // a decision never departs from an instruction an organization or team set locks (rpl23)
    crate::foreign_locks::check_deviation(st, block)
}

/// One deviation: a decision that departs from a cited block or an external decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deviation {
    /// The deviating decision: the key a waiver records.
    pub decision: BlockId,
    /// The cited block or external decision it departs from.
    pub target: ForeignId,
    /// Whether the decision is in force (live or partly superseded); `false` while proposed.
    pub live: bool,
    /// The local blocks that cite the target, in id order: the deviation governs them. A block
    /// that departs from the same target is not among them.
    pub governs: Vec<BlockId>,
}

impl ReadState {
    /// The deviations in `scope`, by decision then target: each proposed, live or partly
    /// superseded decision with a `deviates_from` link, and the local blocks it governs. A
    /// rejected, superseded or revoked decision departs from nothing.
    pub fn deviations(&self, scope: &Scope) -> Result<Vec<Deviation>, RdyError> {
        let m = self.materialized(scope)?;
        let graph = self.graph(scope)?;
        let mut citing: BTreeMap<&ForeignId, BTreeSet<&BlockId>> = BTreeMap::new();
        let mut deviating: BTreeMap<&ForeignId, BTreeSet<&BlockId>> = BTreeMap::new();
        let mut decisions: Vec<(&BlockId, &ForeignId)> = vec![];
        for b in m.iter() {
            for target in b.foreign.pins.keys() {
                citing.entry(target).or_default().insert(&b.id);
            }
            for l in b.foreign.links.get(DEVIATES_FROM).into_iter().flatten() {
                deviating.entry(&l.target).or_default().insert(&b.id);
                decisions.push((&b.id, &l.target));
            }
        }
        decisions.sort();
        decisions.dedup();
        let mut out = vec![];
        for (id, target) in decisions {
            let live = match graph.status(id) {
                Some(DerivedStatus::Proposed) => false,
                Some(DerivedStatus::Live | DerivedStatus::PartiallySuperseded { .. }) => true,
                _ => continue,
            };
            let skip = deviating.get(target);
            let governs = citing
                .get(target)
                .into_iter()
                .flatten()
                .filter(|b| !skip.is_some_and(|s| s.contains(*b)))
                .map(|b| (*b).clone())
                .collect();
            out.push(Deviation {
                decision: id.clone(),
                target: target.clone(),
                live,
                governs,
            });
        }
        Ok(out)
    }
}
