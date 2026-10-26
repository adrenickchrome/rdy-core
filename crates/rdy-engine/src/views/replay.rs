//! Replay: selected changes, from any view and landed or not, become new changes in an owned
//! open view, one new change per source change, in one view op. A replay brings none of its
//! sources' other dependencies. Each op keeps its source base: a base the target does not hold
//! is a recorded conflict, unless the replayed result equals the target's version, which the op
//! then already satisfies. Undo of the one `Include` removes the whole replay.
use super::{
    Action, ConflictInfo, check_view_op, conflict_infos, no_view, owner_of, parse_change, strings,
};
use crate::error::*;
use crate::read_state::ReadState;
use crate::{Delta, Rdy, Scope, Session};
use rdy_model::{BlockId, Change, ChangeLinks, Hash, Op, Provenance};
use rdy_store::kinds;
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap, HashSet};

/// The most source changes one replay takes, counted before duplicates collapse.
pub const MAX_REPLAY_CHANGES: usize = 1_000;
/// The most source ops one replay takes, over all its sources.
pub const MAX_REPLAY_OPS: usize = 10_000;
/// The most encoded bytes of new changes one replay writes, over all of them.
pub const MAX_REPLAY_BYTES: usize = 16_777_216;

/// One replayed change: its source and the new change that replays it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReplayedChange {
    pub source: String,
    pub change: String,
}

/// What a replay did to the view. `head` is the one `Include` op (the undo target); `added`
/// holds the new changes in replay order, and `replayed` maps each source to its new change in
/// that order. A replay never adds, removes or cascades other changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReplayResult {
    pub view: String,
    pub head: String,
    pub changes: usize,
    pub replayed: Vec<ReplayedChange>,
    pub added: Vec<String>,
    pub added_deps: Vec<String>,
    pub removed: Vec<String>,
    pub cascaded: Vec<String>,
    pub conflicts: Vec<ConflictInfo>,
}

fn limit() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!(
            "replay accepts at most {MAX_REPLAY_CHANGES} changes, {MAX_REPLAY_OPS} ops and \
             {MAX_REPLAY_BYTES} encoded bytes"
        ),
        "replay a smaller set of changes",
    )
}

fn cycle() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "the selected replay changes contain a dependency cycle",
        "choose changes with an acyclic dependency order",
    )
}

fn not_owned() -> RdyError {
    RdyError::new(
        E_VIEW_OWNER,
        "replay needs a view owned by this session",
        "use your own view, or ask the owner to hand it to you",
    )
}

/// The replay order of `nodes` (each a source and its deps): Kahn's algorithm over the graph
/// the selected sources induce (a dep outside them is no edge), its ready set in ascending hash
/// order. A cycle among them is refused.
fn replay_order(nodes: &[(Hash, Vec<Hash>)]) -> Result<Vec<usize>, RdyError> {
    let index: HashMap<Hash, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, (h, _))| (*h, i))
        .collect();
    let mut indegree = vec![0usize; nodes.len()];
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (i, (_, deps)) in nodes.iter().enumerate() {
        let mut seen = HashSet::new();
        for d in deps {
            if let Some(&j) = index.get(d)
                && seen.insert(j)
            {
                if j == i {
                    return Err(cycle());
                }
                succ[j].push(i);
                indegree[i] += 1;
            }
        }
    }
    let mut ready: BinaryHeap<Reverse<(Hash, usize)>> = indegree
        .iter()
        .enumerate()
        .filter(|(_, n)| **n == 0)
        .map(|(i, _)| Reverse((nodes[i].0, i)))
        .collect();
    let mut out = Vec::with_capacity(nodes.len());
    while let Some(Reverse((_, i))) = ready.pop() {
        out.push(i);
        for &s in &succ[i] {
            indegree[s] -= 1;
            if indegree[s] == 0 {
                ready.push(Reverse((nodes[s].0, s)));
            }
        }
    }
    if out.len() < nodes.len() {
        return Err(cycle());
    }
    Ok(out)
}

/// Test hook: the replay order of `nodes` (each a source and its deps), as hashes, or the cycle
/// refusal. A content-addressed log holds no dependency cycle, so only this hook reaches it.
#[doc(hidden)]
pub fn replay_order_for_tests(nodes: &[(Hash, Vec<Hash>)]) -> Result<Vec<Hash>, RdyError> {
    Ok(replay_order(nodes)?
        .into_iter()
        .map(|i| nodes[i].0)
        .collect())
}

/// A block as the preview reads it: clean at a version (None: absent), or conflicted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Clean(Option<Hash>),
    Conflicted,
}

/// The target as each replay leaves it, in replay order: the target's materialized versions and
/// recorded conflicts, then each replayed op applied (an equal base), agreeing (an equal result)
/// or conflicted (another base and another result, or a block already conflicted).
struct Preview<'a> {
    versions: &'a dyn Fn(&BlockId) -> Option<Hash>,
    conflicted: HashSet<BlockId>,
    slots: HashMap<BlockId, Slot>,
}

impl Preview<'_> {
    fn get(&self, id: &BlockId) -> Slot {
        match self.slots.get(id) {
            Some(s) => *s,
            None if self.conflicted.contains(id) => Slot::Conflicted,
            None => Slot::Clean((self.versions)(id)),
        }
    }

    /// Whether `id` still holds the target's own version (no earlier replay of this call wrote
    /// it) and that version is clean.
    fn targets_own(&self, id: &BlockId) -> bool {
        !self.slots.contains_key(id) && !self.conflicted.contains(id)
    }
}

/// The changes `view` includes that wrote the target's version `v` of block `id`: of `v`'s
/// producers, the ones `view` includes; for an absent block, the included changes that delete
/// it. `src` never counts: a replay never depends on its own source.
fn view_writers(
    st: &ReadState,
    included: &HashSet<Hash>,
    src: &Hash,
    id: &BlockId,
    v: Option<Hash>,
) -> Result<Vec<Hash>, RdyError> {
    let reader = st.reader();
    let all = match v {
        Some(v) => reader.producers(&v)?,
        None => reader.deleters(id)?,
    };
    Ok(all
        .into_iter()
        .filter(|w| w != src && included.contains(w))
        .collect())
}

/// The new change for `source` (its hash `src`), replayed into `view` at ordinal `n`, with
/// `deps` and the caller's provenance `by`.
fn replayed(
    source: &Change,
    src: Hash,
    view: &str,
    head: &str,
    n: usize,
    deps: Vec<Hash>,
    by: &Provenance,
) -> Change {
    Change {
        view: view.to_string(),
        ops: source.ops.clone(),
        deps,
        provenance: by.clone(),
        justification: source.justification.clone(),
        message: source.message.clone(),
        links: ChangeLinks {
            intent: None,
            alternative: source.links.alternative.clone(),
            client_nonce: Some(format!("replay:{head}:{n}")),
            imported_by: None,
            replayed_from: Some(src),
            briefing: source.links.briefing,
        },
        read_identity: None,
    }
}

/// Whether `op` puts its block under a parent it must find: a create under a parent, or an
/// update whose parent differs from its base version's. Returns that parent.
fn placed_under(st: &ReadState, op: &Op) -> Result<Option<BlockId>, RdyError> {
    Ok(match op {
        Op::Create { block } => block.parent.clone(),
        Op::Update { base, block } => match &block.parent {
            Some(p) if st.reader().get_block(base)?.parent.as_ref() != Some(p) => Some(p.clone()),
            _ => None,
        },
        Op::Delete { .. } => None,
    })
}

/// The blocks `changes` move: an update whose parent differs from its base version's.
fn moved_blocks(st: &ReadState, changes: &[Change]) -> Result<Vec<BlockId>, RdyError> {
    let mut out = vec![];
    for c in changes {
        for op in &c.ops {
            if let Op::Update { base, block } = op
                && st.reader().get_block(base)?.parent != block.parent
            {
                out.push(block.id.clone());
            }
        }
    }
    Ok(out)
}

impl Rdy {
    /// Replays `changes` into `view`, an existing open view the session owns: one new change
    /// per distinct source, in dependency order, stored with one `Include` in one write. Each new
    /// change copies its source's ops (and their bases), justification, message, alternative and
    /// briefing, and depends on the replays of its selected deps and on the latest earlier
    /// replay that writes one of its blocks. An op whose base is the target's version applies,
    /// and the change then depends on what the view's ordinary write would (the version's
    /// producer, or a re-created id's delete), so it merges after it (ruling task-7 item 3: an
    /// ordinary write on that version records the same dep, and no source or source dep enters).
    /// An op whose result is the target's version agrees with it, whatever its base (ruling
    /// task-7-round1), and the change depends on the view's own writers of that version other
    /// than its source, so it merges after them in every hash order. Any other op (another base
    /// and another result) is a recorded conflict. A block the call creates or moves under a
    /// parent needs that parent clean once the whole call is applied: held by the target, or
    /// created by a selected source whose replay is clean (ruling task-7 item 1, as include's
    /// parent rule counts every id the call creates). Nothing is written when any check fails.
    pub fn view_replay(
        &mut self,
        s: &Session,
        view: &str,
        changes: &[String],
    ) -> Result<ReplayResult, RdyError> {
        // 1. the rules: an existing open view this session owns
        let st = self.read();
        check_view_op(&st, s, view, Action::Replay)?;
        let target = st.view_head(view).ok_or_else(|| no_view(view))?;
        if target.owner.as_ref() != Some(&owner_of(s)) {
            return Err(not_owned());
        }
        let head = target.head.to_string();
        // 2. the input, counted before duplicates collapse
        if changes.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "name at least one change to replay",
                "call rdy_view with op replay and changes, or rdy view replay <view> <change>...",
            ));
        }
        if changes.len() > MAX_REPLAY_CHANGES {
            return Err(limit());
        }
        let mut requested: Vec<Hash> = Vec::new();
        requested
            .try_reserve_exact(changes.len())
            .map_err(|_| limit())?;
        let mut seen = HashSet::new();
        for text in changes {
            let h = parse_change(text)?;
            if seen.insert(h) {
                requested.push(h);
            }
        }
        // 3. each source read once: present, not a removed projection; the ops counted
        let reader = st.reader();
        let mut sources: Vec<(Hash, Change)> = Vec::new();
        sources
            .try_reserve_exact(requested.len())
            .map_err(|_| limit())?;
        let mut ops = 0usize;
        for c in &requested {
            if !reader.has_kind(kinds::CHANGE, c)? {
                return Err(RdyError::new(
                    E_NOT_FOUND,
                    format!("no change {}", c.short()),
                    "check the id with rdy view show <view>",
                ));
            }
            let change = reader.change(c)?;
            reader.refuse_copy_of(&change)?;
            ops = ops
                .checked_add(change.ops.len())
                .filter(|n| *n <= MAX_REPLAY_OPS)
                .ok_or_else(limit)?;
            sources.push((*c, change));
        }
        // 4. the order: the graph the selected sources induce
        let nodes: Vec<(Hash, Vec<Hash>)> =
            sources.iter().map(|(h, c)| (*h, c.deps.clone())).collect();
        let order = replay_order(&nodes)?;
        drop(nodes);
        // 5. the preview and the new changes, in replay order
        let m = st.materialized(&Scope::View(view.to_string()))?;
        let target_conflicts = st.conflicts(&Scope::View(view.to_string()))?;
        let versions = |id: &BlockId| m.version_of(id);
        let mut preview = Preview {
            versions: &versions,
            conflicted: target_conflicts.iter().map(|c| c.block.clone()).collect(),
            slots: HashMap::new(),
        };
        let included: HashSet<Hash> = st.included(view).iter().copied().collect();
        let by = s.provenance();
        let mut new_of: HashMap<Hash, Hash> = HashMap::new();
        let mut last_writer: HashMap<BlockId, Hash> = HashMap::new();
        let mut prepared: Vec<Change> = Vec::new();
        prepared
            .try_reserve_exact(order.len())
            .map_err(|_| limit())?;
        let mut replayed_list: Vec<(Hash, Hash)> = Vec::new();
        replayed_list
            .try_reserve_exact(order.len())
            .map_err(|_| limit())?;
        let mut bytes = 0usize;
        // each parent a create or a move puts a block under, in replay order
        let mut placed: Vec<BlockId> = Vec::new();
        for (n, &i) in order.iter().enumerate() {
            let (src, source) = &sources[i];
            let mut deps: BTreeSet<Hash> = source
                .deps
                .iter()
                .filter_map(|d| new_of.get(d).copied())
                .collect();
            // the ops whose base is the target's own version: they merge as the view's ordinary
            // write would, after what produced that version
            let mut ordinary: Vec<Op> = vec![];
            let mut mine: HashSet<&BlockId> = HashSet::new();
            for op in &source.ops {
                let id = op.block_id();
                let earlier = last_writer.get(id).copied();
                if let Some(e) = earlier {
                    deps.insert(e);
                }
                let new = match op {
                    Op::Create { block } | Op::Update { block, .. } => Some(block.version_hash()),
                    Op::Delete { .. } => None,
                };
                let slot = match preview.get(id) {
                    Slot::Clean(v) if v == op.base() => {
                        if earlier.is_none() && !mine.contains(id) {
                            ordinary.push(op.clone());
                        }
                        Slot::Clean(new)
                    }
                    // the result is the target's version: already satisfied, whatever the base.
                    // It merges after the view's writers of that version
                    Slot::Clean(v) if v == new => {
                        if preview.targets_own(id) {
                            deps.extend(view_writers(&st, &included, src, id, v)?);
                        }
                        Slot::Clean(new)
                    }
                    _ => Slot::Conflicted,
                };
                preview.slots.insert(id.clone(), slot);
                mine.insert(id);
            }
            for op in &source.ops {
                if let Some(p) = placed_under(&st, op)? {
                    placed.push(p);
                }
            }
            if !ordinary.is_empty() {
                let probe = Change {
                    ops: ordinary,
                    ..replayed(source, *src, view, &head, n, vec![], &by)
                };
                deps.extend(reader.change_deps(&probe, Some(view))?);
            }
            let change = replayed(
                source,
                *src,
                view,
                &head,
                n,
                deps.into_iter().collect(),
                &by,
            );
            let (_, payload) = rdy_store::codec::item(kinds::CHANGE, &change)?;
            bytes = bytes
                .checked_add(payload.len())
                .filter(|b| *b <= MAX_REPLAY_BYTES)
                .ok_or_else(limit)?;
            let id = Hash::of(&payload);
            drop(payload);
            for op in &source.ops {
                last_writer.insert(op.block_id().clone(), id);
            }
            new_of.insert(*src, id);
            replayed_list.push((*src, id));
            prepared.push(change);
        }
        // the parent rule, over the whole call: a parent the target holds clean once every
        // replay is applied (the target's own, or one a selected source creates cleanly)
        if let Some(p) = placed
            .iter()
            .find(|p| !matches!(preview.get(p), Slot::Clean(Some(_))))
        {
            return Err(RdyError::new(
                E_DEPS_MISSING,
                format!("replay needs parent {p} in view {view:?}"),
                "create the parent in the target view, then replay again",
            ));
        }
        drop(placed);
        // 6. the candidate graph: no parent loop with what the view holds
        let moved = moved_blocks(&st, &prepared)?;
        if !moved.is_empty() {
            let base = st.effective_base(view);
            let mut list: Vec<Change> = st
                .included(view)
                .iter()
                .map(|h| reader.change(h))
                .collect::<Result<_, _>>()?;
            list.extend(prepared.iter().cloned());
            let merged = reader.merge_onto(base, &reader.state_of(base)?, &list)?;
            if let Some((a, b)) = reader.parent_cycle_in(&merged.state, moved)? {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("replay would make {a} and {b} each other's ancestors"),
                    "move one block out of the other, then replay again",
                ));
            }
        }
        // 7. the one write, then publish and report
        drop(m);
        drop(st);
        let (include, ids) = self.store.view_replay_prepared(view, &prepared, &by)?;
        debug_assert_eq!(
            ids,
            replayed_list.iter().map(|(_, n)| *n).collect::<Vec<_>>()
        );
        self.publish(Delta::View {
            view: view.to_string(),
        })?;
        let st = self.read();
        Ok(ReplayResult {
            view: view.to_string(),
            head: include.to_string(),
            changes: st.included(view).len(),
            replayed: replayed_list
                .iter()
                .map(|(src, new)| ReplayedChange {
                    source: src.to_string(),
                    change: new.to_string(),
                })
                .collect(),
            added: strings(&ids),
            added_deps: vec![],
            removed: vec![],
            cascaded: vec![],
            conflicts: conflict_infos(&st, &st.conflicts(&Scope::View(view.to_string()))?)?,
        })
    }
}
