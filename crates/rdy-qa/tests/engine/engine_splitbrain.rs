//! Split-brain model-based property test: random command sequences across four views forked
//! from one canon. After every successful write the view passes the invariant checker; after
//! every successful landing canon does. The generator is biased toward conflicts (a small shared
//! pool of targets, clashing names, folder and section moves that can form cycles, deep sibling
//! keys that give `rebalance` real work), and the run counts outcomes and asserts coverage
//! minimums so the test cannot silently decay into one that never conflicts. Views also include
//! each other's changes (sb2), so conflicts are recorded in views and refuse their landings.
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestCaseError, TestRunner};
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
enum Cmd {
    Create {
        view: u8,
        kind: u8,
        parent: u16,
        name: u8,
    },
    /// 1–40 claims, each placed `after` the parent's first child, in one write or several.
    InsertAfterFirst {
        view: u8,
        parent: u16,
        burst: u8,
        split: bool,
    },
    /// A claim or section moved under a section or doc.
    Move {
        view: u8,
        pick: u16,
        to: u16,
    },
    /// A folder or doc moved under a folder: views doing this crosswise can form cycles.
    MoveTree {
        view: u8,
        pick: u16,
        to: u16,
    },
    /// A section moved under another section: crosswise moves can form cycles.
    MoveSection {
        view: u8,
        pick: u16,
        to: u16,
    },
    /// View `a` moves x under y, view `b` moves y under x (folders, or sections), then `a`
    /// lands and `b` lands: the second landing must be refused as a cycle unless something else
    /// refuses it first.
    Crosswise {
        a: u8,
        b: u8,
        pick: u16,
        to: u16,
        tree: bool,
    },
    Delete {
        view: u8,
        pick: u16,
    },
    Edit {
        view: u8,
        pick: u16,
    },
    Rename {
        view: u8,
        pick: u16,
        name: u8,
    },
    Rebalance {
        view: u8,
        parent: u16,
    },
    /// A proposed decision under a pool section; it is accepted (frozen) when its view lands.
    Decide {
        view: u8,
        parent: u16,
    },
    Land {
        view: u8,
    },
    /// sb2: the newest change of `from` touching `pick`'s block (one of the blocks `from`'s
    /// changes touch), included into `to` by `to`'s writer.
    Include {
        from: u8,
        to: u8,
        pick: u16,
    },
    /// An approver tags canon's head as the next version, if canon moved since the last tag.
    Tag,
}

const NAMES: [&str; 4] = ["alpha", "Alpha", "beta", "gamma"];
/// Picks index a small shared pool so views touch the same blocks.
const POOL: u16 = 5;
/// Tree and section moves pick from an even smaller pool of canon's blocks, so two views often
/// move a pair crosswise (a under b here, b under a there).
const MOVE_POOL: u16 = 3;

fn name() -> impl Strategy<Value = u8> {
    // weighted toward the case-clashing pair
    prop_oneof![4 => Just(0u8), 4 => Just(1u8), 1 => Just(2u8), 1 => Just(3u8)]
}

fn cmd() -> impl Strategy<Value = Cmd> {
    let v = 0u8..4;
    let p = 0u16..POOL;
    let mp = 0u16..MOVE_POOL;
    prop_oneof![
        3 => (v.clone(), 0u8..4, p.clone(), name()).prop_map(|(view, kind, parent, name)| Cmd::Create { view, kind, parent, name }),
        3 => (v.clone(), p.clone(), 1u8..=40, any::<bool>()).prop_map(|(view, parent, burst, split)| Cmd::InsertAfterFirst { view, parent, burst, split }),
        1 => (v.clone(), p.clone(), p.clone()).prop_map(|(view, pick, to)| Cmd::Move { view, pick, to }),
        2 => (v.clone(), mp.clone(), mp.clone()).prop_map(|(view, pick, to)| Cmd::MoveTree { view, pick, to }),
        2 => (v.clone(), mp.clone(), mp.clone()).prop_map(|(view, pick, to)| Cmd::MoveSection { view, pick, to }),
        1 => (v.clone(), 1u8..4, mp.clone(), mp, any::<bool>()).prop_map(|(a, d, pick, to, tree)| Cmd::Crosswise { a, b: (a + d) % 4, pick, to, tree }),
        1 => (v.clone(), any::<u16>()).prop_map(|(view, pick)| Cmd::Delete { view, pick }),
        2 => (v.clone(), p.clone()).prop_map(|(view, pick)| Cmd::Edit { view, pick }),
        4 => (v.clone(), p.clone(), name()).prop_map(|(view, pick, name)| Cmd::Rename { view, pick, name }),
        3 => (v.clone(), p.clone()).prop_map(|(view, parent)| Cmd::Rebalance { view, parent }),
        2 => (v.clone(), p.clone()).prop_map(|(view, parent)| Cmd::Decide { view, parent }),
        3 => v.clone().prop_map(|view| Cmd::Land { view }),
        3 => (v, 1u8..4, any::<u16>()).prop_map(|(from, d, pick)| Cmd::Include { from, to: (from + d) % 4, pick }),
        1 => Just(Cmd::Tag),
    ]
}

/// Ids in `scope` of the given types, sorted, so picks are deterministic.
fn ids(r: &mut Rdy, scope: &Scope, types: &[&str]) -> Vec<String> {
    let m = r.read().materialized(scope).unwrap();
    m.iter()
        .filter(|b| types.contains(&b.block_type.as_str()))
        .map(|b| b.id.to_string())
        .collect()
}

fn pick(list: &[String], n: u16) -> Option<String> {
    (!list.is_empty()).then(|| list[n as usize % list.len()].clone())
}

fn base(r: &mut Rdy) {
    write(r, "seed", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "area"}},
        {"op": "create", "type": "folder", "id": "$g", "fields": {"name": "zone"}},
        {"op": "create", "type": "folder", "id": "$h", "parent": "$f", "fields": {"name": "inner"}},
        {"op": "create", "type": "doc", "id": "$d1", "parent": "$f", "fields": {"title": "One", "doc_type": "page"}},
        {"op": "create", "type": "doc", "id": "$d2", "fields": {"title": "Two", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s1", "parent": "$d1", "fields": {"title": "Terms"}},
        {"op": "create", "type": "section", "id": "$s2", "parent": "$d2", "fields": {"title": "Terms"}},
        {"op": "create", "type": "section", "id": "$s3", "parent": "$s1", "fields": {"title": "More"}},
        {"op": "create", "type": "claim", "parent": "$s1", "body": "c1"},
        {"op": "create", "type": "claim", "parent": "$s2", "body": "c2"},
        {"op": "create", "type": "claim", "parent": "$s3", "body": "c3"}
    ])).unwrap();
    land(r, "seed").unwrap();
}

#[derive(Default, Debug)]
struct Stats {
    /// successful writes by command kind
    writes_ok: BTreeMap<&'static str, usize>,
    lands_ok: usize,
    effective_rebalances: usize,
    /// refused landings by code; gate refusals as `E_VALIDATION/<first error issue>`
    land_refused: BTreeMap<String, usize>,
    write_refused: BTreeMap<String, usize>,
    siblings_rekeyed: usize,
    tags: usize,
    /// sb2: includes that added a change, refused includes by code, conflicts recorded in a view
    /// (by an include, or by a landing's rebase), and landings refused for an unresolved conflict
    included: usize,
    include_refused: BTreeMap<String, usize>,
    conflict_recorded: usize,
    conflict_unresolved_at_land: usize,
}

struct Sim<'a> {
    r: Rdy,
    /// a slot whose landing was refused is abandoned: it continues under a fresh view name
    generation: [u32; 4],
    stats: &'a RefCell<Stats>,
    /// versions tagged so far: (name, snapshot id at tagging time)
    tags: Vec<(String, rdy_model::Hash)>,
    tagged_head: Option<rdy_model::Hash>,
}

/// Codes that are defects whenever they appear: a caught panic, or a malformed order key in a
/// project only the engine has written. (`E_ORDER_DEPTH` is a legitimate refusal.)
fn unexpected(e: &RdyError) -> bool {
    [E_INTERNAL, "E_ORDER_KEY"].contains(&e.code)
        || e.issues.iter().any(|i| i.code == "E_ORDER_KEY")
}

fn kind(c: &Cmd) -> &'static str {
    match c {
        Cmd::Create { .. } => "create",
        Cmd::InsertAfterFirst { .. } => "insert_after_first",
        Cmd::Move { .. } => "move",
        Cmd::MoveTree { .. } => "move_tree",
        Cmd::MoveSection { .. } => "move_section",
        Cmd::Crosswise { .. } => "crosswise",
        Cmd::Delete { .. } => "delete",
        Cmd::Edit { .. } => "edit",
        Cmd::Rename { .. } => "rename",
        Cmd::Rebalance { .. } => "rebalance",
        Cmd::Decide { .. } => "decide",
        Cmd::Land { .. } => "land",
        Cmd::Include { .. } => "include",
        Cmd::Tag => "tag",
    }
}

/// `scope`'s violations now (none when it passes the checker).
fn violations(r: &Rdy, scope: &Scope) -> Vec<rdy_qa::invariants::Violation> {
    check(r, scope).err().unwrap_or_default()
}

/// The violations `scope` has now that it did not have `before`. A landing rebases every open
/// unpinned view with work (reb1), so a view can carry a clash that canon's landing brought in
/// (two blocks given one name, say); a write must not add one, and such a view is refused when
/// it lands, as before.
fn new_violations(
    before: &[rdy_qa::invariants::Violation],
    r: &Rdy,
    scope: &Scope,
) -> Vec<rdy_qa::invariants::Violation> {
    violations(r, scope)
        .into_iter()
        .filter(|x| !before.contains(x))
        .collect()
}

fn refusal(e: &RdyError) -> String {
    match e
        .issues
        .iter()
        .find(|i| i.severity == rdy_schema::Severity::Error)
    {
        Some(i) if e.code == E_VALIDATION => format!("{}/{}", e.code, i.code),
        _ => e.code.to_string(),
    }
}

impl Sim<'_> {
    fn view(&self, slot: u8) -> String {
        format!("v{slot}g{}", self.generation[slot as usize])
    }

    fn scope(&self, slot: u8) -> Scope {
        Scope::View(self.view(slot))
    }

    /// One write; on success the view must gain no violation from it.
    fn write(&mut self, slot: u8, ops: serde_json::Value, c: &Cmd) -> Result<bool, TestCaseError> {
        let view = self.view(slot);
        let rebalance = ops[0]["op"] == "rebalance";
        let before = violations(&self.r, &Scope::View(view.clone()));
        match write(&mut self.r, &view, ops) {
            Ok(_) => {
                let mut s = self.stats.borrow_mut();
                *s.writes_ok.entry(kind(c)).or_default() += 1;
                if rebalance {
                    s.effective_rebalances += 1;
                }
                drop(s);
                let x = new_violations(&before, &self.r, &Scope::View(view.clone()));
                if !x.is_empty() {
                    return Err(TestCaseError::fail(format!(
                        "after {c:?}, view {view} broke: {x:?}"
                    )));
                }
                Ok(true)
            }
            Err(e) if unexpected(&e) => Err(TestCaseError::fail(format!(
                "after {c:?}, a write to {view} failed unexpectedly: {e:?}"
            ))),
            Err(e) => {
                *self
                    .stats
                    .borrow_mut()
                    .write_refused
                    .entry(refusal(&e))
                    .or_default() += 1;
                Ok(false)
            }
        }
    }

    fn land(&mut self, slot: u8, c: &Cmd) -> Result<(), TestCaseError> {
        let view = self.view(slot);
        let res = match self.r.propose(&writer(), &view, &view) {
            // an empty view has nothing to propose: not a conflict
            Err(e) if e.code == E_BAD_INPUT => return Ok(()),
            Err(e) => Err(e),
            Ok(p) => {
                let warned = p.warnings.iter().any(|i| i.code == "W_SIBLINGS_REKEYED");
                self.r.land_report(&approver(), &p.id).map(|l| {
                    (
                        warned || l.warnings.iter().any(|i| i.code == "W_SIBLINGS_REKEYED"),
                        l.rebased.iter().filter(|v| v.conflicts > 0).count(),
                    )
                })
            }
        };
        match res {
            Ok((rekeyed, conflicted)) => {
                let mut s = self.stats.borrow_mut();
                s.lands_ok += 1;
                s.siblings_rekeyed += usize::from(rekeyed);
                s.conflict_recorded += conflicted;
                drop(s);
                if let Err(x) = check(&self.r, &Scope::Canon) {
                    return Err(TestCaseError::fail(format!(
                        "after {c:?} ({view}), canon broke: {x:?}"
                    )));
                }
                self.check_tags(c)?;
            }
            // a freshly made proposal is never bad input to land
            Err(e) if unexpected(&e) || e.code == E_BAD_INPUT => {
                return Err(TestCaseError::fail(format!(
                    "after {c:?}, landing {view} failed unexpectedly: {e:?}"
                )));
            }
            Err(e) => {
                let mut s = self.stats.borrow_mut();
                *s.land_refused.entry(refusal(&e)).or_default() += 1;
                if e.code == E_CONFLICT_UNRESOLVED {
                    s.conflict_unresolved_at_land += 1;
                }
                drop(s);
                self.generation[slot as usize] += 1;
                if let Err(x) = check(&self.r, &Scope::Canon) {
                    return Err(TestCaseError::fail(format!(
                        "after refused {c:?} ({view}: {}), canon broke: {x:?}",
                        e.code
                    )));
                }
            }
        }
        Ok(())
    }

    /// Every earlier version still reads exactly as it did when tagged, and the newest passes
    /// the checker.
    fn check_tags(&mut self, c: &Cmd) -> Result<(), TestCaseError> {
        for (name, snap) in self.tags.clone() {
            let scope = self
                .r
                .read()
                .resolve_scope(&format!("tag:{name}"))
                .map_err(|e| TestCaseError::fail(format!("after {c:?}, tag {name}: {e}")))?;
            let now = self.r.read().materialized(&scope).unwrap().snapshot_id();
            if now != snap {
                return Err(TestCaseError::fail(format!(
                    "after {c:?}, version {name} changed: tagged {snap}, now {now}"
                )));
            }
        }
        if let Some((name, _)) = self.tags.last().cloned() {
            let scope = self.r.read().resolve_scope(&format!("tag:{name}")).unwrap();
            if let Err(x) = check(&self.r, &scope) {
                return Err(TestCaseError::fail(format!(
                    "after {c:?}, version {name} broke: {x:?}"
                )));
            }
        }
        Ok(())
    }

    /// sb2: includes into `to` the newest change of `from` that touches the picked block and that
    /// `to` does not include yet. The included view may now hold a conflict, or a clash two
    /// clean changes make together (it is refused when it lands); later writes to it must not
    /// add a violation of their own.
    fn include(&mut self, from: u8, to: u8, pick: u16, c: &Cmd) -> Result<(), TestCaseError> {
        let (fv, tv) = (self.view(from), self.view(to));
        let st = self.r.read();
        let into = st.included(&tv);
        let mut changes = vec![];
        for id in st.included(&fv).iter() {
            let ops = st.reader().change(id).unwrap().ops;
            let blocks: Vec<BlockId> = ops.iter().map(|op| op.block_id().clone()).collect();
            changes.push((*id, blocks));
        }
        let mut blocks: Vec<BlockId> = changes.iter().flat_map(|(_, b)| b.clone()).collect();
        blocks.sort();
        blocks.dedup();
        if blocks.is_empty() {
            return Ok(());
        }
        let block = &blocks[pick as usize % blocks.len()];
        let Some(change) = changes
            .iter()
            .rev()
            .find(|(id, b)| b.contains(block) && !into.contains(id))
            .map(|(id, _)| id.to_string())
        else {
            return Ok(());
        };
        drop(st);
        match self.r.view_include(&writer(), &tv, &[change]) {
            Ok(res) => {
                let mut s = self.stats.borrow_mut();
                s.included += 1;
                s.conflict_recorded += usize::from(!res.conflicts.is_empty());
            }
            Err(e) if unexpected(&e) => {
                return Err(TestCaseError::fail(format!(
                    "after {c:?}, including into {tv} failed unexpectedly: {e:?}"
                )));
            }
            Err(e) => {
                *self
                    .stats
                    .borrow_mut()
                    .include_refused
                    .entry(refusal(&e))
                    .or_default() += 1;
            }
        }
        Ok(())
    }

    fn tag(&mut self, c: &Cmd) -> Result<(), TestCaseError> {
        let head = self.r.store().canon_head().unwrap();
        if head.is_none() || head == self.tagged_head {
            return Ok(());
        }
        let name = format!("v0.{}.0", self.tags.len() + 1);
        if let Err(e) = self.r.tag(&approver(), &name, None, None) {
            return Err(TestCaseError::fail(format!(
                "after {c:?}, tagging {name} failed: {e:?}"
            )));
        }
        let scope = self.r.read().resolve_scope(&format!("tag:{name}")).unwrap();
        let snap = self.r.read().materialized(&scope).unwrap().snapshot_id();
        self.tags.push((name, snap));
        self.tagged_head = head;
        self.stats.borrow_mut().tags += 1;
        Ok(())
    }

    fn apply(&mut self, c: &Cmd) -> Result<(), TestCaseError> {
        let (slot, op) = match c {
            Cmd::Create {
                view: vi,
                kind,
                parent,
                name,
            } => {
                let sc = self.scope(*vi);
                let r = &mut self.r;
                let nm = NAMES[*name as usize];
                let op = match kind {
                    0 => pick(&ids(r, &sc, &["section"]), *parent).map(|p| json!({"op": "create", "type": "claim", "parent": p, "body": "new claim"})),
                    1 => pick(&ids(r, &sc, &["doc", "section"]), *parent).map(|p| json!({"op": "create", "type": "section", "parent": p, "fields": {"title": "S"}})),
                    2 => Some(match pick(&ids(r, &sc, &["folder"]), *parent) {
                        Some(p) if parent % 2 == 0 => json!({"op": "create", "type": "doc", "parent": p, "fields": {"name": nm, "title": "D", "doc_type": "page"}}),
                        _ => json!({"op": "create", "type": "doc", "fields": {"name": nm, "title": "D", "doc_type": "page"}}),
                    }),
                    _ => Some(match pick(&ids(r, &sc, &["folder"]), *parent) {
                        Some(p) if parent % 2 == 0 => json!({"op": "create", "type": "folder", "parent": p, "fields": {"name": nm}}),
                        _ => json!({"op": "create", "type": "folder", "fields": {"name": nm}}),
                    }),
                };
                (*vi, op)
            }
            Cmd::InsertAfterFirst {
                view: vi,
                parent,
                burst,
                split,
            } => {
                let sc = self.scope(*vi);
                let Some(p) = pick(&ids(&mut self.r, &sc, &["section"]), *parent) else {
                    return Ok(());
                };
                let pid = BlockId::parse(&p).unwrap();
                let first = self
                    .r
                    .read()
                    .materialized(&sc)
                    .unwrap()
                    .children(Some(&pid))
                    .first()
                    .map(|b| b.id.to_string());
                let first = match first {
                    Some(f) => f,
                    None => {
                        let op = json!([{"op": "create", "type": "claim", "parent": p, "body": "first"}]);
                        if !self.write(*vi, op, c)? {
                            return Ok(());
                        }
                        let m = self.r.read().materialized(&sc).unwrap();
                        m.children(Some(&pid))[0].id.to_string()
                    }
                };
                let ops: Vec<_> = (0..*burst)
                    .map(|i| json!({"op": "create", "type": "claim", "parent": p, "after": first, "body": format!("ins {i}")}))
                    .collect();
                let chunk = if *split { 5 } else { ops.len() };
                for part in ops.chunks(chunk) {
                    if !self.write(*vi, json!(part), c)? {
                        break;
                    }
                }
                return Ok(());
            }
            Cmd::Move {
                view: vi,
                pick: p,
                to,
            } => {
                let sc = self.scope(*vi);
                let all = ids(&mut self.r, &sc, &["claim", "decision", "section"]);
                let parents = ids(&mut self.r, &sc, &["section", "doc"]);
                (
                    *vi,
                    pick(&all, *p)
                        .zip(pick(&parents, *to))
                        .map(|(id, parent)| json!({"op": "move", "id": id, "parent": parent})),
                )
            }
            Cmd::MoveTree {
                view: vi,
                pick: p,
                to,
            } => {
                // canon's blocks are the pool every view shares: crosswise moves form cycles
                let all = ids(&mut self.r, &Scope::Canon, &["folder", "doc"]);
                let parents = ids(&mut self.r, &Scope::Canon, &["folder"]);
                (
                    *vi,
                    pick(&all, *p)
                        .zip(pick(&parents, *to))
                        .map(|(id, parent)| json!({"op": "move", "id": id, "parent": parent})),
                )
            }
            Cmd::MoveSection {
                view: vi,
                pick: p,
                to,
            } => {
                let all = ids(&mut self.r, &Scope::Canon, &["section"]);
                (
                    *vi,
                    pick(&all, *p)
                        .zip(pick(&all, *to))
                        .map(|(id, parent)| json!({"op": "move", "id": id, "parent": parent})),
                )
            }
            Cmd::Crosswise {
                a,
                b,
                pick: p,
                to,
                tree,
            } => {
                let types: &[&str] = if *tree { &["folder"] } else { &["section"] };
                let all = ids(&mut self.r, &Scope::Canon, types);
                if let Some((x, y)) = pick(&all, *p).zip(pick(&all, *to))
                    && x != y
                {
                    self.write(*a, json!([{"op": "move", "id": x, "parent": y}]), c)?;
                    self.write(*b, json!([{"op": "move", "id": y, "parent": x}]), c)?;
                    self.land(*a, c)?;
                    self.land(*b, c)?;
                }
                return Ok(());
            }
            Cmd::Delete { view: vi, pick: p } => {
                let sc = self.scope(*vi);
                (
                    *vi,
                    pick(
                        &ids(
                            &mut self.r,
                            &sc,
                            &["claim", "decision", "section", "doc", "folder"],
                        ),
                        *p,
                    )
                    .map(|id| json!({"op": "delete", "id": id})),
                )
            }
            Cmd::Edit { view: vi, pick: p } => {
                let sc = self.scope(*vi);
                let body = format!("edited in {}", self.view(*vi));
                (
                    *vi,
                    pick(&ids(&mut self.r, &sc, &["claim", "decision"]), *p)
                        .map(|id| json!({"op": "update", "id": id, "body": body})),
                )
            }
            Cmd::Rename {
                view: vi,
                pick: p,
                name,
            } => {
                let sc = self.scope(*vi);
                (*vi, pick(&ids(&mut self.r, &sc, &["doc", "folder"]), *p).map(|id| json!({"op": "update", "id": id, "fields": {"name": NAMES[*name as usize]}})))
            }
            Cmd::Rebalance { view: vi, parent } => {
                let sc = self.scope(*vi);
                (
                    *vi,
                    pick(&ids(&mut self.r, &sc, &["section"]), *parent)
                        .map(|p| json!({"op": "rebalance", "parent": p})),
                )
            }
            Cmd::Decide { view: vi, parent } => {
                let sc = self.scope(*vi);
                let Some(p) = pick(&ids(&mut self.r, &sc, &["section"]), *parent) else {
                    return Ok(());
                };
                let view = self.view(*vi);
                let d = DecisionInput {
                    parent: p,
                    statement: format!("decided in {view}"),
                    ..Default::default()
                };
                let before = violations(&self.r, &sc);
                match self.r.decide(&writer(), &view, d) {
                    Ok(_) => {
                        *self
                            .stats
                            .borrow_mut()
                            .writes_ok
                            .entry("decide")
                            .or_default() += 1;
                        let x = new_violations(&before, &self.r, &sc);
                        if !x.is_empty() {
                            return Err(TestCaseError::fail(format!(
                                "after {c:?}, view {view} broke: {x:?}"
                            )));
                        }
                    }
                    Err(e) if unexpected(&e) => {
                        return Err(TestCaseError::fail(format!(
                            "after {c:?}, a decision in {view} failed unexpectedly: {e:?}"
                        )));
                    }
                    Err(e) => {
                        *self
                            .stats
                            .borrow_mut()
                            .write_refused
                            .entry(refusal(&e))
                            .or_default() += 1;
                    }
                }
                return Ok(());
            }
            Cmd::Land { view: vi } => return self.land(*vi, c),
            Cmd::Include { from, to, pick } => return self.include(*from, *to, *pick, c),
            Cmd::Tag => return self.tag(c),
        };
        if let Some(op) = op {
            self.write(slot, json!([op]), c)?;
        }
        Ok(())
    }
}

/// Seeds this thread's block ids for one case; unseeds them on drop, even if the case fails.
struct SeededIds;

impl SeededIds {
    fn new(key: &str) -> SeededIds {
        use std::hash::{Hash, Hasher};
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
fn engine_split_brain_histories_keep_every_invariant() {
    let stats = RefCell::new(Stats::default());
    let config = Config {
        cases: cases(32),
        rng_seed: RngSeed::Fixed(0x5b11_7b2a),
        failure_persistence: None,
        ..Config::default()
    };
    let mut runner = TestRunner::new(config);
    let result = runner.run(&proptest::collection::vec(cmd(), 1..64), |cmds| {
        // block ids come from a generator seeded by the case itself, so every case (and every
        // shrink step) replays identically and the coverage counters are deterministic
        let _ids = SeededIds::new(&format!("{cmds:?}"));
        let (_d, mut r) = project();
        base(&mut r);
        let mut sim = Sim {
            r,
            generation: [0; 4],
            stats: &stats,
            tags: vec![],
            tagged_head: None,
        };
        for c in &cmds {
            sim.apply(c)?;
        }
        Ok(())
    });
    if let Err(e) = result {
        panic!("{e}");
    }

    // Coverage floors. Block ids are seeded per case, so the fast run's counters are exact and
    // repeatable; the floors sit at about 70% of the seeded fast run (shown in brackets), so a
    // generator change that loses coverage fails here. Deep mode sees about ten times as much.
    // sb2: the generator gained `Include`, which reseeds every case; the brackets show the new
    // seeded run and the floors are unchanged, so three of them (refused landings,
    // E_CONFLICT_UNRESOLVED and E_PATH_CLASH landings) now sit at or just under their counts.
    // Final review fix round 1: an include brings the change that created a parent it needs, so
    // the seeded run's one E_MISSING_PARENT landing is gone and the history after it moved; the
    // brackets show that run, and the E_CONFLICT_UNRESOLVED floor (10, its old count) is now 6,
    // about 70% of the new count like the others.
    let s = stats.into_inner();
    eprintln!("split-brain coverage: {s:#?}");
    let land = |code: &str| s.land_refused.get(code).copied().unwrap_or(0);
    let floors = [
        ("effective rebalances [32]", s.effective_rebalances, 16),
        // gconf1: a collision a landing recorded as a view conflict is refused as
        // E_CONFLICT_UNRESOLVED, no longer E_STALE_BASE; the seeded run's 15 E_STALE_BASE
        // refusals (the bracket said 19) are now 15 E_CONFLICT_UNRESOLVED, and the refused
        // landings were 28 before and after (the bracket said 36)
        ("refused landings [21]", s.land_refused.values().sum(), 19),
        ("refused-landing codes [4]", s.land_refused.len(), 3),
        (
            "E_CONFLICT_UNRESOLVED landings [9]",
            land("E_CONFLICT_UNRESOLVED"),
            6,
        ),
        (
            "E_PARENT_CYCLE landings [6]",
            land("E_VALIDATION/E_PARENT_CYCLE"),
            4,
        ),
        // vcreate1: an empty unpinned view reads canon (the floating rule), so writes into it no
        // longer read a stale base; the seeded run's path clashes at landing fell from 5 to 4
        (
            "E_PATH_CLASH landings [5]",
            land("E_VALIDATION/E_PATH_CLASH"),
            3,
        ),
        (
            "E_FROZEN writes [7]",
            s.write_refused.get(E_FROZEN).copied().unwrap_or(0),
            3,
        ),
        ("versions tagged [26]", s.tags, 19),
    ];
    for (what, got, min) in floors {
        assert!(got >= min, "{what}: {got} < floor {min}: {s:#?}");
    }
    // sb2: views include each other's changes, record conflicts, and a landing with one is
    // refused as unresolved; each at least once per run
    let sb2 = [
        ("included [65]", s.included),
        ("conflict_recorded [33]", s.conflict_recorded),
        (
            "conflict_unresolved_at_land [9]",
            s.conflict_unresolved_at_land,
        ),
    ];
    for (what, got) in sb2 {
        assert!(got >= 1, "{what}: none seen: {s:#?}");
    }
    // class 5: a landing's collision with canon is recorded as a view conflict
    // (E_CONFLICT_UNRESOLVED); E_STALE_BASE at landing comes only from pinned or closed views,
    // and this helper makes none
    assert_eq!(land("E_STALE_BASE"), 0, "{s:#?}");
    // reb1: the four views are unpinned, so every landing rebases them onto the re-keyed
    // siblings and W_SIBLINGS_REKEYED (a pinned view's warning) is never raised here
    assert_eq!(s.siblings_rekeyed, 0, "{s:#?}");
}

/// sb2 (final review fix round 1): the helper's include across views, as one named case, so the
/// gate lists the id. View v1 includes only v0's claim under a section v0 created; the include
/// brings the section's creation along, v1 passes the checker and lands, and canon does too (the
/// helper's run once saw this landing refused as `E_MISSING_PARENT`).
#[test]
fn sb2_an_include_across_views_brings_the_parents_creation_and_lands() {
    let (_d, mut r) = project();
    base(&mut r);
    let doc = ids(&mut r, &Scope::Canon, &["doc"])[0].clone();
    let c1 = write(
        &mut r,
        "v0",
        json!([{"op": "create", "type": "section", "parent": doc, "fields": {"title": "Fresh"}}]),
    )
    .unwrap();
    let s = c1.created.values().next().unwrap().clone();
    let c2 = write(
        &mut r,
        "v0",
        json!([{"op": "create", "type": "claim", "parent": s, "body": "under fresh"}]),
    )
    .unwrap();
    let res = r
        .view_include(&writer(), "v1", std::slice::from_ref(&c2.change))
        .unwrap();
    assert_eq!(
        (res.added, res.added_deps),
        (vec![c2.change.clone()], vec![c1.change.clone()])
    );
    check(&r, &Scope::View("v1".into())).unwrap();
    land(&mut r, "v1").unwrap();
    check(&r, &Scope::Canon).unwrap();
    let claim = c2.created.values().next().unwrap().clone();
    assert_eq!(body_of(&r, &Scope::Canon, &claim), "under fresh");
}
