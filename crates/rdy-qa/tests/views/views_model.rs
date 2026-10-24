//! vmodel1: a reference model of view sets and conflicts. Random command sequences (write,
//! include, exclude, undo, close, reopen, land, create, fork) run against the engine and against a
//! small model over three blocks and up to four views, whose versions are counters. The model's
//! merge is spec §5.6's rule applied to the counters and its landing rebases as §6.5 says; after
//! every command each view's included set, conflicts, effective base and block versions must
//! agree, and canon and every view pass the invariant checker. Every refusal is predicted by the
//! model and must be the engine's.
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestCaseError, TestRunner};
use rdy_engine::views::UndoResult;
use rdy_engine::*;
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::*;
use rdy_qa::intake::TEST_SUMMARY;
use rdy_qa::invariants::check;
use serde_json::json;
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};

/// At most this many views, `v0` to `v3`.
const VIEWS: usize = 4;
/// The views every case starts with (unpinned, empty).
const START_VIEWS: usize = 2;
/// The blocks every write edits: three seeded claims.
const BLOCKS: usize = 3;

#[derive(Clone, Debug)]
enum Cmd {
    /// An update of one block's body, from the version the view reads.
    Write {
        view: u8,
        block: u8,
    },
    /// The newest change of `from` that `to` does not include and that has not landed.
    Include {
        from: u8,
        to: u8,
    },
    /// One included change, with its dependents (`cascade: true`).
    Exclude {
        view: u8,
        which: u8,
    },
    /// A target-less undo, without cascade.
    Undo {
        view: u8,
    },
    Close {
        view: u8,
    },
    /// The undo of the close that closed the view.
    Reopen {
        view: u8,
    },
    /// Propose and approve at once.
    Land {
        view: u8,
    },
    /// The next view, floating or pinned at canon's head.
    Create {
        pinned: bool,
    },
    /// The next view as a fork of `from`, open or closed: a copy of its base, pin and included
    /// set. An unpinned, non-empty fork behind canon starts on canon's head without the changes
    /// that landed.
    Fork {
        from: u8,
    },
}

fn cmd() -> impl Strategy<Value = Cmd> {
    let v = 0u8..VIEWS as u8;
    prop_oneof![
        10 => (v.clone(), 0u8..BLOCKS as u8).prop_map(|(view, block)| Cmd::Write { view, block }),
        5 => (v.clone(), v.clone()).prop_map(|(from, to)| Cmd::Include { from, to }),
        2 => (v.clone(), any::<u8>()).prop_map(|(view, which)| Cmd::Exclude { view, which }),
        2 => v.clone().prop_map(|view| Cmd::Undo { view }),
        1 => v.clone().prop_map(|view| Cmd::Close { view }),
        2 => v.clone().prop_map(|view| Cmd::Reopen { view }),
        3 => v.clone().prop_map(|view| Cmd::Land { view }),
        1 => any::<bool>().prop_map(|pinned| Cmd::Create { pinned }),
        5 => v.prop_map(|from| Cmd::Fork { from }),
    ]
}

// ---------------------------------------------------------------------------------------------
// The model

/// One change: one op on one block, from version `base` to `result`.
#[derive(Clone, Debug)]
struct MChange {
    block: BlockId,
    base: Option<u32>,
    result: Option<u32>,
    deps: BTreeSet<usize>,
    /// The engine's id. The merge orders deps first, then by change id (§5.6), so the model
    /// breaks ties as the engine does.
    id: Hash,
    /// The landing (index) that carried it into canon.
    landed: Option<usize>,
}

/// An op of a view's chain that a target-less undo can pick: the session's commits, includes,
/// excludes and closes.
#[derive(Clone, Debug)]
enum MOp {
    Commit(usize),
    Include(Vec<usize>),
    Exclude(Vec<usize>),
    Close,
}

#[derive(Clone, Debug)]
struct MView {
    /// The stored base (a landing index). An empty unpinned view reads canon instead.
    base_landing: usize,
    included: Vec<usize>,
    pinned: bool,
    closed: bool,
    /// The undoable ops, oldest first, each with whether it is undone.
    ops: Vec<(MOp, bool)>,
}

/// A conflict as `(block, base version, sorted side versions)`.
type MConflict = (BlockId, Option<u32>, Vec<Option<u32>>);

/// A side of a conflict as the merge holds it: (the change that wrote it, its version).
type MSide = (Option<usize>, Option<u32>);

enum Slot {
    Clean {
        version: Option<u32>,
        owner: Option<usize>,
    },
    Conflicted {
        base: Option<u32>,
        sides: Vec<MSide>,
    },
}

/// What a target-less undo picks and does.
enum MUndo {
    /// Exclude `taken_out` (the target's changes still included) from the view.
    Exclude {
        op: usize,
        taken_out: Vec<usize>,
    },
    /// Put `put_back` back, with the deps they need (`missing`), recorded as an `Include` op.
    Include {
        op: usize,
        put_back: Vec<usize>,
        missing: Vec<usize>,
    },
    Reopen {
        op: usize,
    },
}

impl MUndo {
    fn op(&self) -> usize {
        match self {
            MUndo::Exclude { op, .. } | MUndo::Include { op, .. } | MUndo::Reopen { op } => *op,
        }
    }
}

#[derive(Default)]
struct Model {
    canon: BTreeMap<BlockId, u32>,
    changes: Vec<MChange>,
    views: BTreeMap<String, MView>,
    landings: Vec<BTreeMap<BlockId, u32>>,
}

impl Model {
    fn canon_at(&self) -> usize {
        self.landings.len() - 1
    }

    /// The change that wrote version `v` of `block` (every version has one: the seed's, or a
    /// write's).
    fn producer(&self, block: &BlockId, v: u32) -> usize {
        self.changes
            .iter()
            .position(|c| c.block == *block && c.result == Some(v))
            .expect("every version has a producer")
    }

    fn floats(&self, view: &str) -> bool {
        let v = &self.views[view];
        !v.pinned && v.included.is_empty()
    }

    /// The landing the view reads on: canon's head for an empty unpinned view (it floats).
    fn effective_base(&self, view: &str) -> usize {
        if self.floats(view) {
            self.canon_at()
        } else {
            self.views[view].base_landing
        }
    }

    /// Whether `c` landed at or before landing `base`.
    fn in_base(&self, base: usize, c: usize) -> bool {
        self.changes[c].landed.is_some_and(|l| l <= base)
    }

    /// Deps first, ties by change id (Kahn's algorithm over the distinct changes of `set`).
    fn order(&self, set: &[usize]) -> Vec<usize> {
        let mut nodes = vec![];
        let mut seen = BTreeSet::new();
        for &c in set {
            if seen.insert(c) {
                nodes.push(c);
            }
        }
        let mut indegree: BTreeMap<usize, usize> = nodes.iter().map(|&c| (c, 0)).collect();
        let mut succ: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for &c in &nodes {
            for &d in &self.changes[c].deps {
                if d != c && seen.contains(&d) {
                    succ.entry(d).or_default().push(c);
                    *indegree.get_mut(&c).expect("node") += 1;
                }
            }
        }
        let mut ready: BinaryHeap<Reverse<(Hash, usize)>> = indegree
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(&c, _)| Reverse((self.changes[c].id, c)))
            .collect();
        let mut out = vec![];
        while let Some(Reverse((_, c))) = ready.pop() {
            out.push(c);
            for &s in succ.get(&c).into_iter().flatten() {
                let n = indegree.get_mut(&s).expect("node");
                *n -= 1;
                if *n == 0 {
                    ready.push(Reverse((self.changes[s].id, s)));
                }
            }
        }
        assert_eq!(out.len(), nodes.len(), "the model's deps form a cycle");
        out
    }

    /// Spec §5.6 on version counters: changes in deps-first order; an op whose base is not the
    /// block's current version records a conflict `(block, base, sides)`; a change that depends
    /// on every side resolves it. A conflicted block reads as its conflict's base.
    fn merge_onto(
        &self,
        base: &BTreeMap<BlockId, u32>,
        set: &[usize],
    ) -> (BTreeMap<BlockId, u32>, Vec<MConflict>) {
        let mut slots: BTreeMap<BlockId, Slot> = BTreeMap::new();
        for c in self.order(set) {
            let ch = &self.changes[c];
            let slot = slots.entry(ch.block.clone()).or_insert_with(|| {
                let version = base.get(&ch.block).copied();
                Slot::Clean {
                    version,
                    owner: version.map(|v| self.producer(&ch.block, v)),
                }
            });
            let covered = |s: &MSide| s.0.is_some_and(|x| ch.deps.contains(&x));
            let mine = (Some(c), ch.result);
            match slot {
                Slot::Clean { version, owner } if *version == ch.base => {
                    *version = ch.result;
                    *owner = Some(c);
                }
                Slot::Clean { version, .. } if *version == ch.result => {}
                Slot::Clean { version, owner } => {
                    let theirs = (*owner, *version);
                    *slot = Slot::Conflicted {
                        base: ch.base,
                        sides: vec![theirs, mine],
                    };
                }
                Slot::Conflicted { sides, .. }
                    if sides.iter().all(|s| s.0.is_none() || covered(s)) =>
                {
                    *slot = Slot::Clean {
                        version: ch.result,
                        owner: Some(c),
                    };
                }
                Slot::Conflicted { sides, .. } => {
                    sides.retain(|s| !covered(s));
                    sides.push(mine);
                }
            }
        }
        let mut state = base.clone();
        let mut conflicts = vec![];
        for (block, slot) in slots {
            let version = match slot {
                Slot::Clean { version, .. } => version,
                Slot::Conflicted { base, sides } => {
                    let mut versions: Vec<Option<u32>> = sides.iter().map(|s| s.1).collect();
                    versions.sort();
                    conflicts.push((block.clone(), base, versions));
                    base
                }
            };
            match version {
                Some(v) => {
                    state.insert(block, v);
                }
                None => {
                    state.remove(&block);
                }
            }
        }
        (state, conflicts)
    }

    /// The view's included changes merged onto its effective base, with the sides of each
    /// conflict as (change, version) for the deps a resolving write takes.
    fn merge(&self, view: &str) -> (BTreeMap<BlockId, u32>, Vec<MConflict>) {
        let v = &self.views[view];
        self.merge_onto(&self.landings[self.effective_base(view)], &v.included)
    }

    /// The sides' writers of the conflict on `block` in `view`, if it has one.
    fn side_changes(&self, view: &str, block: &BlockId) -> Vec<usize> {
        let (_, conflicts) = self.merge(view);
        let Some((_, _, versions)) = conflicts.iter().find(|c| c.0 == *block) else {
            return vec![];
        };
        versions
            .iter()
            .map(|v| self.producer(block, v.expect("no deletes in the model")))
            .collect()
    }

    /// The base and deps of a write of `block` in `view`: the version the view reads (a
    /// conflicted block reads its base), its producer, and every side of a conflict on it.
    fn plan_write(&self, view: &str, block: &BlockId) -> (u32, BTreeSet<usize>) {
        let (state, _) = self.merge(view);
        let base = state[block];
        let mut deps = BTreeSet::from([self.producer(block, base)]);
        deps.extend(self.side_changes(view, block));
        (base, deps)
    }

    /// The float catch-up: the first change into an empty unpinned view behind canon moves its
    /// stored base to canon's head first. Returns whether it fired.
    fn catch_up(&mut self, view: &str) -> bool {
        let canon = self.canon_at();
        let v = self.views.get_mut(view).expect("view");
        if !v.pinned && v.included.is_empty() && v.base_landing != canon {
            v.base_landing = canon;
            return true;
        }
        false
    }

    /// The deps `added` needs that `view` neither includes nor has in its base, breadth first.
    fn missing_deps(&self, view: &str, added: &[usize]) -> Vec<usize> {
        let base = self.effective_base(view);
        let included: BTreeSet<usize> = self.views[view].included.iter().copied().collect();
        let added_set: BTreeSet<usize> = added.iter().copied().collect();
        let mut found = vec![];
        let mut queue: VecDeque<usize> = added.iter().copied().collect();
        while let Some(c) = queue.pop_front() {
            for &d in &self.changes[c].deps {
                if included.contains(&d)
                    || added_set.contains(&d)
                    || found.contains(&d)
                    || self.in_base(base, d)
                {
                    continue;
                }
                found.push(d);
                queue.push_back(d);
            }
        }
        found
    }

    /// The included changes of `view`, not in `gone`, whose deps meet `gone` transitively.
    fn dependents(&self, view: &str, gone: &BTreeSet<usize>) -> Vec<usize> {
        let included = &self.views[view].included;
        let mut out = gone.clone();
        loop {
            let mut grew = false;
            for &c in included {
                if !out.contains(&c) && self.changes[c].deps.iter().any(|d| out.contains(d)) {
                    out.insert(c);
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        included
            .iter()
            .filter(|c| out.contains(c) && !gone.contains(c))
            .copied()
            .collect()
    }

    /// Appends the changes not yet included, in order.
    fn include(&mut self, view: &str, changes: &[usize]) {
        let v = self.views.get_mut(view).expect("view");
        for &c in changes {
            if !v.included.contains(&c) {
                v.included.push(c);
            }
        }
    }

    fn exclude(&mut self, view: &str, changes: &[usize]) {
        let v = self.views.get_mut(view).expect("view");
        v.included.retain(|c| !changes.contains(c));
    }

    /// A target-less undo: the newest undoable op not undone whose inverse brings back or
    /// removes no landed work only; `E_UNDO_BLOCKED` when its exclusion would orphan dependents.
    fn plan_undo(&self, view: &str) -> Result<MUndo, &'static str> {
        let v = &self.views[view];
        let landed = |c: &usize| self.changes[*c].landed.is_some();
        for (i, (op, undone)) in v.ops.iter().enumerate().rev() {
            if *undone {
                continue;
            }
            match op {
                MOp::Commit(_) | MOp::Include(_) => {
                    let cs = match op {
                        MOp::Commit(c) => vec![*c],
                        MOp::Include(cs) => cs.clone(),
                        _ => unreachable!(),
                    };
                    let any_included = cs.iter().any(|c| v.included.contains(c));
                    if !any_included && cs.iter().all(landed) {
                        continue;
                    }
                    let taken_out: Vec<usize> = cs
                        .iter()
                        .filter(|c| v.included.contains(c))
                        .copied()
                        .collect();
                    let gone: BTreeSet<usize> = taken_out.iter().copied().collect();
                    if !self.dependents(view, &gone).is_empty() {
                        return Err(E_UNDO_BLOCKED);
                    }
                    return Ok(MUndo::Exclude { op: i, taken_out });
                }
                MOp::Exclude(cs) => {
                    if cs.iter().any(landed) {
                        continue;
                    }
                    let mut put_back = vec![];
                    for c in cs {
                        if !v.included.contains(c) && !put_back.contains(c) {
                            put_back.push(*c);
                        }
                    }
                    let missing = self.missing_deps(view, &put_back);
                    // a re-include never pulls a landed dep back in (msw3)
                    if missing.iter().any(landed) {
                        continue;
                    }
                    return Ok(MUndo::Include {
                        op: i,
                        put_back,
                        missing,
                    });
                }
                MOp::Close => return Ok(MUndo::Reopen { op: i }),
            }
        }
        Err(E_NOTHING_TO_UNDO)
    }

    /// Reopens `view` (the undo of its close at `op`): it is open again, and then it follows the
    /// open-view rule (`catch_up_open`). Returns whether it rebased.
    fn reopen(&mut self, view: &str, op: usize) -> bool {
        let v = self.views.get_mut(view).expect("view");
        v.ops[op].1 = true;
        v.closed = false;
        self.catch_up_open(view)
    }

    /// Adds `to`, a fork of `from`: a new open view with the source's set, base and pin and no
    /// ops of its own, which then follows the open-view rule (`catch_up_open`). The model states
    /// the fork as "a copy that is an open view", not as the store's conditions: an open view
    /// behind canon is one a landing would have rebased, and a fork of a closed view is the one
    /// case that can be. Returns whether it rebased.
    fn fork(&mut self, from: &str, to: &str) -> bool {
        let mut fork = self.views[from].clone();
        fork.closed = false;
        fork.ops = vec![];
        self.views.insert(to.to_string(), fork);
        self.catch_up_open(to)
    }

    /// The open-view rule, which `land` applies to every open view: an open, unpinned view with
    /// work behind canon starts on canon's head and drops the changes that landed. Returns
    /// whether it moved.
    fn catch_up_open(&mut self, view: &str) -> bool {
        let canon = self.canon_at();
        let landed: Vec<bool> = self.changes.iter().map(|c| c.landed.is_some()).collect();
        let v = self.views.get_mut(view).expect("view");
        if !v.closed && !v.pinned && !v.included.is_empty() && v.base_landing != canon {
            v.included.retain(|&c| !landed[c]);
            v.base_landing = canon;
            return true;
        }
        false
    }

    /// The outcome of proposing and approving `view` at once: the new canon, or the refusal.
    fn plan_land(&self, view: &str) -> Result<BTreeMap<BlockId, u32>, &'static str> {
        let v = &self.views[view];
        // propose: a closed view takes no proposal (D-K0SS1, R2)
        if v.closed {
            return Err(E_VIEW_CLOSED);
        }
        // propose: an empty view has nothing to propose
        if v.included.is_empty() {
            return Err(E_BAD_INPUT);
        }
        // the version is the view's whole set: its blocking conflicts are the view's
        let blocking = !self.merge(view).1.is_empty();
        // propose validates a version with no blocking conflict against canon: a pinned or
        // closed view behind canon collides with it
        let (state, conflicts) = self.merge_onto(&self.canon, &v.included);
        if !blocking && !conflicts.is_empty() {
            return Err(E_STALE_BASE);
        }
        // approve: a version holding a landed change (a pinned or closed view keeps one that
        // landed from another view) is refused first, then one with a blocking conflict, which
        // is not ready: the conflict is its first readiness issue (D-ME2HD)
        if v.included.iter().any(|&c| self.changes[c].landed.is_some()) {
            return Err(E_ALREADY_LANDED);
        }
        if blocking {
            return Err(E_PROPOSAL_NOT_READY);
        }
        Ok(state)
    }

    /// Lands `view` onto `state`: its changes land; it rebases (always), and so does every other
    /// open, unpinned, non-empty view. Returns the rebased views.
    fn land(&mut self, view: &str, state: BTreeMap<BlockId, u32>) -> BTreeSet<String> {
        self.landings.push(state.clone());
        self.canon = state;
        let at = self.canon_at();
        let landed: Vec<usize> = self.views[view].included.clone();
        for &c in &landed {
            self.changes[c].landed = Some(at);
        }
        let mut rebased = BTreeSet::new();
        for (name, v) in self.views.iter_mut() {
            if name != view && (v.closed || v.pinned || v.included.is_empty()) {
                continue;
            }
            v.included.retain(|c| !landed.contains(c));
            v.base_landing = at;
            rebased.insert(name.clone());
        }
        rebased
    }
}

// ---------------------------------------------------------------------------------------------
// The driver

#[derive(Default, Debug)]
struct Stats {
    /// commands applied (not skipped), by kind
    applied: BTreeMap<&'static str, usize>,
    /// refusals predicted and observed, by code
    refused: BTreeMap<&'static str, usize>,
    cases_with_a_conflict: usize,
    cascade_excludes: usize,
    reopen_rebases: usize,
    landings: usize,
    landings_rebasing_3_views: usize,
    conflict_unresolved: usize,
    float_catch_ups: usize,
    forks: usize,
    forks_rebasing: usize,
}

struct Driver<'a> {
    r: Rdy,
    _dir: tempfile::TempDir,
    model: Model,
    /// change index → engine id
    ids: Vec<String>,
    /// engine version hash → counter
    versions: BTreeMap<String, u32>,
    /// (block, counter) → engine version hash
    hashes: BTreeMap<(BlockId, u32), String>,
    /// landing index → engine landing
    landing_ids: Vec<Hash>,
    blocks: Vec<BlockId>,
    /// view → the op id of the close that closed it
    close_ops: BTreeMap<String, String>,
    saw_conflict: bool,
    stats: &'a RefCell<Stats>,
}

fn fail(msg: String) -> TestCaseError {
    TestCaseError::fail(msg)
}

fn name(i: usize) -> String {
    format!("v{i}")
}

/// Each view is written, included into, closed and reopened only by its own session.
fn session(view: &str) -> Session {
    as_agent(&format!("agent:{view}"), "r")
}

fn kind(c: &Cmd) -> &'static str {
    match c {
        Cmd::Write { .. } => "write",
        Cmd::Include { .. } => "include",
        Cmd::Exclude { .. } => "exclude",
        Cmd::Undo { .. } => "undo",
        Cmd::Close { .. } => "close",
        Cmd::Reopen { .. } => "reopen",
        Cmd::Land { .. } => "land",
        Cmd::Create { .. } => "create",
        Cmd::Fork { .. } => "fork",
    }
}

fn set_of(list: &[String]) -> BTreeSet<String> {
    list.iter().cloned().collect()
}

impl<'a> Driver<'a> {
    /// A project with one doc whose section holds three claims, landed; views `v0`, `v1`.
    fn new(stats: &'a RefCell<Stats>) -> Driver<'a> {
        let (dir, mut r) = project();
        let res = write(&mut r, "seed", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "model", "title": "Model", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
            {"op": "create", "type": "claim", "id": "$b0", "parent": "$s", "body": "b0"},
            {"op": "create", "type": "claim", "id": "$b1", "parent": "$s", "body": "b1"},
            {"op": "create", "type": "claim", "id": "$b2", "parent": "$s", "body": "b2"}
        ]))
        .expect("seed");
        let seed: Hash = res.change.parse().unwrap();
        let landing: Hash = land(&mut r, "seed").expect("seed lands").parse().unwrap();
        let blocks: Vec<BlockId> = (0..BLOCKS)
            .map(|i| BlockId::parse(&res.created[&format!("$b{i}")]).unwrap())
            .collect();
        let mut d = Driver {
            r,
            _dir: dir,
            model: Model::default(),
            ids: vec![],
            versions: BTreeMap::new(),
            hashes: BTreeMap::new(),
            landing_ids: vec![landing],
            blocks: blocks.clone(),
            close_ops: BTreeMap::new(),
            saw_conflict: false,
            stats,
        };
        // the seed: one pseudo-change per block (the engine's one seed change), version 0, landed
        // in landing 0
        for b in &blocks {
            let v = version_of(&d.r, &Scope::Canon, &b.to_string());
            d.versions.insert(v.clone(), 0);
            d.hashes.insert((b.clone(), 0), v);
            d.model.canon.insert(b.clone(), 0);
            d.model.changes.push(MChange {
                block: b.clone(),
                base: None,
                result: Some(0),
                deps: BTreeSet::new(),
                id: seed,
                landed: Some(0),
            });
            d.ids.push(seed.to_string());
        }
        d.model.landings.push(d.model.canon.clone());
        for i in 0..START_VIEWS {
            d.create(i, false).expect("start views");
        }
        d
    }

    fn view_count(&self) -> usize {
        self.model.views.len()
    }

    fn view(&self, i: u8) -> String {
        name(i as usize % self.view_count())
    }

    /// The engine's answer against the model's: the same success, or the same refusal.
    fn expect<T>(
        &self,
        what: &str,
        want: Result<(), &'static str>,
        got: Result<T, RdyError>,
    ) -> Result<Option<T>, TestCaseError> {
        match (want, got) {
            (Ok(()), Ok(t)) => Ok(Some(t)),
            (Err(code), Err(e)) if e.code == code => {
                *self.stats.borrow_mut().refused.entry(code).or_default() += 1;
                Ok(None)
            }
            (Ok(()), Err(e)) => Err(fail(format!(
                "{what}: the model predicts success, the engine refused: {e:?}"
            ))),
            (Err(code), Ok(_)) => Err(fail(format!(
                "{what}: the model predicts {code}, the engine succeeded"
            ))),
            (Err(code), Err(e)) => Err(fail(format!(
                "{what}: the model predicts {code}, the engine refused with {}: {e:?}",
                e.code
            ))),
        }
    }

    fn id_of(&self, c: usize) -> String {
        self.ids[c].clone()
    }

    fn ids_of(&self, cs: &[usize]) -> BTreeSet<String> {
        cs.iter().map(|&c| self.id_of(c)).collect()
    }

    fn counter(&self, h: &Hash) -> Result<u32, String> {
        self.versions
            .get(&h.to_string())
            .copied()
            .ok_or_else(|| format!("version {h} is not one the driver wrote"))
    }

    fn landing_index(&self, h: Option<Hash>) -> Result<usize, String> {
        let h = h.ok_or("no effective base")?;
        self.landing_ids
            .iter()
            .position(|l| *l == h)
            .ok_or_else(|| format!("{h} is not a landing the driver made"))
    }

    fn create(&mut self, i: usize, pinned: bool) -> Result<(), TestCaseError> {
        let v = name(i);
        self.r
            .view_create(&session(&v), &v, None, pinned)
            .map_err(|e| fail(format!("creating {v}: {e:?}")))?;
        self.model.views.insert(
            v,
            MView {
                base_landing: self.model.canon_at(),
                included: vec![],
                pinned,
                closed: false,
                ops: vec![],
            },
        );
        Ok(())
    }

    fn count_catch_up(&self, fired: bool) {
        if fired {
            self.stats.borrow_mut().float_catch_ups += 1;
        }
    }

    /// Applies `c` to the engine and the model; returns whether it ran (a command that does not
    /// apply to the state is skipped).
    fn apply(&mut self, c: &Cmd) -> Result<bool, TestCaseError> {
        let what = format!("{c:?}");
        match *c {
            Cmd::Create { pinned } => {
                if self.view_count() == VIEWS {
                    return Ok(false);
                }
                self.create(self.view_count(), pinned)?;
            }
            Cmd::Fork { from } => {
                if self.view_count() == VIEWS {
                    return Ok(false);
                }
                let src = self.view(from);
                let to = name(self.view_count());
                self.r
                    .view_fork(&session(&to), &src, &to)
                    .map_err(|e| fail(format!("{what}: {e:?}")))?;
                let rebases = self.model.fork(&src, &to);
                let mut s = self.stats.borrow_mut();
                s.forks += 1;
                if rebases {
                    s.forks_rebasing += 1;
                }
            }
            Cmd::Write { view, block } => {
                let v = self.view(view);
                let b = self.blocks[block as usize].clone();
                let (base, deps) = self.model.plan_write(&v, &b);
                let body = format!("w{} in {v}", self.model.changes.len());
                let ops = json!([{"op": "update", "id": b.to_string(), "base": self.hashes[&(b.clone(), base)], "body": body}]);
                let want = if self.model.views[&v].closed {
                    Err(E_VIEW_CLOSED)
                } else {
                    Ok(())
                };
                let got = write_as(&mut self.r, &session(&v), &v, ops);
                let Some(res) = self.expect(&what, want, got)? else {
                    return Ok(true);
                };
                // the version the view now reads is the write's
                let h = version_of(&self.r, &Scope::View(v.clone()), &b.to_string());
                if self.versions.contains_key(&h) {
                    return Err(fail(format!(
                        "{what}: the view reads {h}, not a new version"
                    )));
                }
                let counter = self
                    .model
                    .changes
                    .iter()
                    .filter(|c| c.block == b)
                    .filter_map(|c| c.result)
                    .max()
                    .unwrap_or(0)
                    + 1;
                self.versions.insert(h.clone(), counter);
                self.hashes.insert((b.clone(), counter), h);
                let id: Hash = res.change.parse().unwrap();
                // the engine's deps are the model's
                let got: BTreeSet<String> = self
                    .r
                    .read()
                    .reader()
                    .change(&id)
                    .unwrap()
                    .deps
                    .iter()
                    .map(|h| h.to_string())
                    .collect();
                let want: BTreeSet<String> = deps.iter().map(|&d| self.id_of(d)).collect();
                if got != want {
                    return Err(fail(format!(
                        "{what}: the change's deps are {got:?}, the model's {want:?}"
                    )));
                }
                let fired = self.model.catch_up(&v);
                self.count_catch_up(fired);
                let idx = self.model.changes.len();
                self.model.changes.push(MChange {
                    block: b,
                    base: Some(base),
                    result: Some(counter),
                    deps,
                    id,
                    landed: None,
                });
                self.ids.push(res.change);
                self.model.include(&v, &[idx]);
                self.model
                    .views
                    .get_mut(&v)
                    .unwrap()
                    .ops
                    .push((MOp::Commit(idx), false));
            }
            Cmd::Include { from, to } => {
                let (from, to) = (self.view(from), self.view(to));
                if from == to {
                    return Ok(false);
                }
                let into = &self.model.views[&to].included;
                let Some(pick) = self.model.views[&from]
                    .included
                    .iter()
                    .copied()
                    .filter(|c| !into.contains(c) && self.model.changes[*c].landed.is_none())
                    .max()
                else {
                    return Ok(false);
                };
                let closed = self.model.views[&to].closed;
                let missing = self.model.missing_deps(&to, &[pick]);
                let want = if closed { Err(E_VIEW_CLOSED) } else { Ok(()) };
                let got = self.r.view_include(&session(&to), &to, &[self.id_of(pick)]);
                let Some(res) = self.expect(&what, want, got)? else {
                    return Ok(true);
                };
                if set_of(&res.added) != self.ids_of(&[pick])
                    || set_of(&res.added_deps) != self.ids_of(&missing)
                {
                    return Err(fail(format!(
                        "{what}: the engine added {:?} with deps {:?}, the model {:?} with {:?}",
                        res.added,
                        res.added_deps,
                        self.ids_of(&[pick]),
                        self.ids_of(&missing)
                    )));
                }
                let fired = self.model.catch_up(&to);
                self.count_catch_up(fired);
                let all: Vec<usize> = std::iter::once(pick).chain(missing).collect();
                self.model.include(&to, &all);
                self.model
                    .views
                    .get_mut(&to)
                    .unwrap()
                    .ops
                    .push((MOp::Include(all), false));
            }
            Cmd::Exclude { view, which } => {
                let v = self.view(view);
                let included = self.model.views[&v].included.clone();
                if included.is_empty() {
                    return Ok(false);
                }
                let pick = included[which as usize % included.len()];
                let deps = self.model.dependents(&v, &BTreeSet::from([pick]));
                let want = if self.model.views[&v].closed {
                    Err(E_VIEW_CLOSED)
                } else {
                    Ok(())
                };
                let got = self
                    .r
                    .view_exclude(&session(&v), &v, &[self.id_of(pick)], true);
                let Some(res) = self.expect(&what, want, got)? else {
                    return Ok(true);
                };
                if set_of(&res.removed) != self.ids_of(&[pick])
                    || set_of(&res.cascaded) != self.ids_of(&deps)
                {
                    return Err(fail(format!(
                        "{what}: the engine removed {:?} cascading {:?}, the model {:?} cascading {:?}",
                        res.removed,
                        res.cascaded,
                        self.ids_of(&[pick]),
                        self.ids_of(&deps)
                    )));
                }
                if !deps.is_empty() {
                    self.stats.borrow_mut().cascade_excludes += 1;
                }
                let all: Vec<usize> = std::iter::once(pick).chain(deps).collect();
                self.model.exclude(&v, &all);
                self.model
                    .views
                    .get_mut(&v)
                    .unwrap()
                    .ops
                    .push((MOp::Exclude(all), false));
            }
            Cmd::Undo { view } => {
                let v = self.view(view);
                let plan = self.model.plan_undo(&v);
                let want = plan.as_ref().map(|_| ()).map_err(|e| *e);
                let got = self.r.undo(&session(&v), &v, None, false);
                let Some(res) = self.expect(&what, want, got)? else {
                    return Ok(true);
                };
                let plan = plan.expect("predicted a success");
                let op = plan.op();
                let undone_kind = match &self.model.views[&v].ops[op].0 {
                    MOp::Commit(_) => "commit",
                    MOp::Include(_) => "include",
                    MOp::Exclude(_) => "exclude",
                    MOp::Close => "close",
                };
                let (included, excluded) = match &plan {
                    MUndo::Exclude { taken_out, .. } => (vec![], taken_out.clone()),
                    MUndo::Include {
                        put_back, missing, ..
                    } => (put_back.iter().chain(missing).copied().collect(), vec![]),
                    MUndo::Reopen { .. } => (vec![], vec![]),
                };
                if res.kind != undone_kind
                    || set_of(&res.included) != self.ids_of(&included)
                    || set_of(&res.excluded) != self.ids_of(&excluded)
                {
                    return Err(fail(format!(
                        "{what}: the engine undid a {} including {:?} excluding {:?}; the model a {undone_kind} including {:?} excluding {:?}",
                        res.kind,
                        res.included,
                        res.excluded,
                        self.ids_of(&included),
                        self.ids_of(&excluded)
                    )));
                }
                match plan {
                    MUndo::Exclude { op, taken_out } => {
                        self.model.exclude(&v, &taken_out);
                        self.model.views.get_mut(&v).unwrap().ops[op].1 = true;
                    }
                    MUndo::Include {
                        op,
                        put_back,
                        missing,
                    } => {
                        let fired = self.model.catch_up(&v);
                        self.count_catch_up(fired);
                        self.model.include(&v, &put_back);
                        self.model.include(&v, &missing);
                        let mv = self.model.views.get_mut(&v).unwrap();
                        mv.ops[op].1 = true;
                        // the deps an undo re-includes are an `Include` op of this session
                        if !missing.is_empty() {
                            mv.ops.push((MOp::Include(missing), false));
                        }
                    }
                    MUndo::Reopen { op } => {
                        let rebased = self.model.reopen(&v, op);
                        self.check_reopen(&what, &res, rebased)?;
                    }
                }
            }
            Cmd::Close { view } => {
                let v = self.view(view);
                if self.model.views[&v].closed {
                    return Ok(false);
                }
                self.r
                    .view_close(
                        &session(&v),
                        std::slice::from_ref(&v),
                        "the model closes it",
                    )
                    .map_err(|e| fail(format!("{what}: {e:?}")))?;
                let head = self.r.read().view_head(&v).unwrap().head.to_string();
                self.close_ops.insert(v.clone(), head);
                let mv = self.model.views.get_mut(&v).unwrap();
                mv.closed = true;
                mv.ops.push((MOp::Close, false));
            }
            Cmd::Reopen { view } => {
                let v = self.view(view);
                if !self.model.views[&v].closed {
                    return Ok(false);
                }
                let op = self.model.views[&v]
                    .ops
                    .iter()
                    .rposition(|(o, undone)| matches!(o, MOp::Close) && !undone)
                    .expect("a closed view has a live close");
                let target = self.close_ops[&v].clone();
                let res = self
                    .r
                    .undo(&session(&v), &v, Some(&target), false)
                    .map_err(|e| fail(format!("{what}: {e:?}")))?;
                let rebased = self.model.reopen(&v, op);
                self.check_reopen(&what, &res, rebased)?;
            }
            Cmd::Land { view } => {
                let v = self.view(view);
                let plan = self.model.plan_land(&v);
                let want = plan.as_ref().map(|_| ()).map_err(|e| *e);
                let got = self
                    .r
                    .propose(&session(&v), &v, "model", Some(TEST_SUMMARY))
                    .and_then(|p| self.r.land_report(&approver(), &p.id));
                // the predicted conflict is the refusal's first issue
                if want == Err(E_PROPOSAL_NOT_READY)
                    && let Err(e) = &got
                    && e.code == E_PROPOSAL_NOT_READY
                    && e.issues.first().map(|i| i.code) != Some(E_CONFLICT_UNRESOLVED)
                {
                    return Err(fail(format!(
                        "{what}: the model predicts a conflict, the engine's first issue is another: {e:?}"
                    )));
                }
                let Some(info) = self.expect(&what, want, got)? else {
                    if want == Err(E_PROPOSAL_NOT_READY) {
                        self.stats.borrow_mut().conflict_unresolved += 1;
                    }
                    return Ok(true);
                };
                let state = plan.expect("predicted a landing");
                self.landing_ids.push(info.landing.parse().unwrap());
                let rebased = self.model.land(&v, state);
                let got: BTreeSet<String> = info.rebased.iter().map(|r| r.view.clone()).collect();
                if got != rebased {
                    return Err(fail(format!(
                        "{what}: the landing rebased {got:?}, the model {rebased:?}"
                    )));
                }
                let mut s = self.stats.borrow_mut();
                s.landings += 1;
                if rebased.len() >= 3 {
                    s.landings_rebasing_3_views += 1;
                }
            }
        }
        Ok(true)
    }

    fn check_reopen(
        &self,
        what: &str,
        res: &UndoResult,
        rebased: bool,
    ) -> Result<(), TestCaseError> {
        if !res.reopened || res.rebased != rebased {
            return Err(fail(format!(
                "{what}: the engine reopened={} rebased={}, the model rebased={rebased}",
                res.reopened, res.rebased
            )));
        }
        if rebased {
            self.stats.borrow_mut().reopen_rebases += 1;
        }
        Ok(())
    }

    /// The engine's view agrees with the model's: flags, included set, effective base,
    /// conflicts and the version of every block.
    fn assert_same(&self, view: &str) -> Result<(), String> {
        let model = &self.model;
        let mv = &model.views[view];
        let st = self.r.read();
        let head = st.view_head(view).ok_or("no view head")?;
        if (head.pinned, head.closed) != (mv.pinned, mv.closed) {
            return Err(format!(
                "pinned/closed: engine ({}, {}), model ({}, {})",
                head.pinned, head.closed, mv.pinned, mv.closed
            ));
        }
        let got: BTreeSet<String> = st.included(view).iter().map(|h| h.to_string()).collect();
        let want = self.ids_of(&mv.included);
        if got != want {
            return Err(format!("included: engine {got:?}, model {want:?}"));
        }
        let base = self.landing_index(st.effective_base(view))?;
        if base != model.effective_base(view) {
            return Err(format!(
                "effective base: engine landing {base}, model landing {}",
                model.effective_base(view)
            ));
        }
        let scope = Scope::View(view.to_string());
        let (state, conflicts) = model.merge(view);
        let mut got = vec![];
        for c in st.conflicts(&scope).map_err(|e| e.to_string())?.iter() {
            let base = c.base.map(|h| self.counter(&h)).transpose()?;
            let mut sides = c
                .sides
                .iter()
                .map(|s| s.version.map(|h| self.counter(&h)).transpose())
                .collect::<Result<Vec<_>, _>>()?;
            sides.sort();
            got.push((c.block.clone(), base, sides));
        }
        if got != conflicts {
            return Err(format!("conflicts: engine {got:?}, model {conflicts:?}"));
        }
        let m = st.materialized(&scope).map_err(|e| e.to_string())?;
        for b in &self.blocks {
            let v = m.version_of(b).ok_or_else(|| format!("{b} is missing"))?;
            let v = self.counter(&v)?;
            if Some(&v) != state.get(b) {
                return Err(format!(
                    "{b} reads version {v} in the engine, {:?} in the model",
                    state.get(b)
                ));
            }
        }
        Ok(())
    }

    /// After every command: every view agrees with the model and passes the checker, and so
    /// does canon.
    fn compare(&mut self, at: usize, c: &Cmd) -> Result<(), TestCaseError> {
        if let Err(x) = check(&self.r, &Scope::Canon) {
            return Err(fail(format!(
                "after command {at} ({c:?}), canon broke: {x:?}"
            )));
        }
        let views: Vec<String> = self.model.views.keys().cloned().collect();
        for v in views {
            if let Err(why) = self.assert_same(&v) {
                return Err(fail(format!(
                    "after command {at} ({c:?}), view {v} differs from the model: {why}"
                )));
            }
            if let Err(x) = check(&self.r, &Scope::View(v.clone())) {
                return Err(fail(format!(
                    "after command {at} ({c:?}), view {v} broke: {x:?}"
                )));
            }
            if !self.model.merge(&v).1.is_empty() {
                self.saw_conflict = true;
            }
        }
        Ok(())
    }
}

/// Seeds this thread's block ids for one case; unseeds them on drop, even if the case fails.
struct SeededIds;

impl SeededIds {
    fn new(key: &str) -> SeededIds {
        use std::hash::{Hash as _, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        rdy_model::seed_ids_for_tests(h.finish());
        SeededIds
    }
}

impl Drop for SeededIds {
    fn drop(&mut self) {
        rdy_model::unseed_ids_for_tests();
    }
}

#[test]
fn vmodel1_views_match_the_reference_model() {
    let stats = RefCell::new(Stats::default());
    let config = Config {
        cases: cases(40),
        rng_seed: RngSeed::Fixed(0x7d0d_e11e),
        failure_persistence: None,
        max_shrink_iters: 4096,
        ..Config::default()
    };
    let mut runner = TestRunner::new(config);
    let result = runner.run(&proptest::collection::vec(cmd(), 60..=200), |cmds| {
        let _ids = SeededIds::new(&format!("{cmds:?}"));
        let mut d = Driver::new(&stats);
        d.compare(0, &Cmd::Create { pinned: false })?;
        for (at, c) in cmds.iter().enumerate() {
            if d.apply(c)? {
                *stats.borrow_mut().applied.entry(kind(c)).or_default() += 1;
            }
            d.compare(at + 1, c)?;
        }
        if d.saw_conflict {
            stats.borrow_mut().cases_with_a_conflict += 1;
        }
        Ok(())
    });
    if let Err(e) = result {
        panic!("{e}");
    }
    let s = stats.into_inner();
    eprintln!("views model coverage: {s:#?}");
    let floors = [
        ("cases with a conflict recorded", s.cases_with_a_conflict),
        ("cascade excludes", s.cascade_excludes),
        ("reopens with a rebase", s.reopen_rebases),
        ("landings rebasing 3 views", s.landings_rebasing_3_views),
        (
            "E_CONFLICT_UNRESOLVED predicted and observed",
            s.conflict_unresolved,
        ),
        ("float catch-ups", s.float_catch_ups),
        ("forks", s.forks),
    ];
    for (what, got) in floors {
        assert!(got >= 1, "{what}: none seen: {s:#?}");
    }
}

/// sg2: a fixed sequence over fork. Write in v0, fork v0 as v2, include v2's change into a new
/// v3, land v0. After each command every view agrees with the model, and the checker passes.
#[test]
fn sg2_a_fork_then_include_matches_the_model() {
    let stats = RefCell::new(Stats::default());
    let _ids = SeededIds::new("sg2_a_fork_then_include_matches_the_model");
    let mut d = Driver::new(&stats);
    let steps = [
        Cmd::Write { view: 0, block: 0 },
        Cmd::Fork { from: 0 },
        Cmd::Create { pinned: false },
        Cmd::Include { from: 2, to: 3 },
        Cmd::Land { view: 0 },
    ];
    for (at, c) in steps.iter().enumerate() {
        assert!(
            d.apply(c).unwrap_or_else(|e| panic!("{c:?}: {e}")),
            "{c:?} was skipped"
        );
        d.compare(at + 1, c).unwrap_or_else(|e| panic!("{e}"));
        if matches!(c, Cmd::Fork { .. }) {
            let st = d.r.read();
            assert_eq!(
                st.included("v2"),
                st.included("v0"),
                "the fork holds a copy of the source's set"
            );
            assert_eq!(st.included("v2").len(), 1);
        }
    }
    let st = d.r.read();
    assert!(
        st.included("v2").is_empty() && st.included("v3").is_empty(),
        "the landing took the change out of the fork and of the view that included it"
    );
    assert_eq!(stats.borrow().forks, 1);
}

/// sg2: a fork of a closed view behind canon starts on canon's head and drops the changes that
/// landed from another view (the one case where a fork rebases: open views are rebased at every
/// landing). v0 writes two blocks; v1 includes the second change and lands; v0 was closed first,
/// so it keeps a landed change; its fork keeps only the first.
#[test]
fn sg2_a_fork_of_a_closed_view_behind_canon_drops_what_landed() {
    let stats = RefCell::new(Stats::default());
    let _ids = SeededIds::new("sg2_a_fork_of_a_closed_view_behind_canon_drops_what_landed");
    let mut d = Driver::new(&stats);
    let steps = [
        Cmd::Write { view: 0, block: 0 },
        Cmd::Write { view: 0, block: 1 },
        Cmd::Include { from: 0, to: 1 },
        Cmd::Close { view: 0 },
        Cmd::Land { view: 1 },
        Cmd::Fork { from: 0 },
    ];
    for (at, c) in steps.iter().enumerate() {
        assert!(
            d.apply(c).unwrap_or_else(|e| panic!("{c:?}: {e}")),
            "{c:?} was skipped"
        );
        d.compare(at + 1, c).unwrap_or_else(|e| panic!("{e}"));
    }
    let st = d.r.read();
    assert_eq!(st.included("v0").len(), 2, "the closed view keeps its two");
    assert_eq!(st.included("v2").len(), 1, "the fork drops the landed one");
    assert_eq!(stats.borrow().forks_rebasing, 1);
}

fn update(id: &str, base: &str, body: &str) -> serde_json::Value {
    json!([{"op": "update", "id": id, "base": base, "body": body}])
}

/// The ids of view `v`'s conflicts.
fn conflict_ids(r: &Rdy, v: &str) -> Vec<Hash> {
    r.read()
        .conflicts(&Scope::View(v.into()))
        .unwrap()
        .iter()
        .map(|c| c.id)
        .collect()
}

#[test]
fn vmodel1_conflict_ids_are_order_independent() {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let ca = write_as(
        &mut r,
        &as_agent("agent:a", "r"),
        "a",
        update(&c, &v, "a's"),
    )
    .unwrap()
    .change;
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r"),
        "b",
        update(&c, &v, "b's"),
    )
    .unwrap()
    .change;
    // x includes a's change then b's; y includes b's then a's
    let (x, y) = (as_agent("agent:x", "r"), as_agent("agent:y", "r"));
    r.view_create(&x, "x", None, false).unwrap();
    r.view_create(&y, "y", None, false).unwrap();
    r.view_include(&x, "x", std::slice::from_ref(&ca)).unwrap();
    let rx = r.view_include(&x, "x", std::slice::from_ref(&cb)).unwrap();
    r.view_include(&y, "y", std::slice::from_ref(&cb)).unwrap();
    let ry = r.view_include(&y, "y", std::slice::from_ref(&ca)).unwrap();
    let ids = conflict_ids(&r, "x");
    assert_eq!(ids.len(), 1, "the two changes collide");
    assert_eq!(
        ids,
        conflict_ids(&r, "y"),
        "the include order does not matter"
    );
    assert_eq!(rx.conflicts.len(), 1);
    assert_eq!(
        rx.conflicts[0].id, ry.conflicts[0].id,
        "include reports the same id"
    );
    assert_eq!(rx.conflicts[0].id, ids[0].to_string());
    // and the views each wrote into, which hold one side and include the other
    r.view_include(&as_agent("agent:a", "r"), "a", std::slice::from_ref(&cb))
        .unwrap();
    r.view_include(&as_agent("agent:b", "r"), "b", std::slice::from_ref(&ca))
        .unwrap();
    assert_eq!(conflict_ids(&r, "a"), ids);
    assert_eq!(conflict_ids(&r, "b"), ids);
    // a rebuilt index reads the same ids
    r.reindex().unwrap();
    for v in ["x", "y", "a", "b"] {
        assert_eq!(conflict_ids(&r, v), ids, "after reindex, view {v}");
    }
}
