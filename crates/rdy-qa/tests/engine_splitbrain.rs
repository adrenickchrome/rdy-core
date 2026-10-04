//! Split-brain model-based property test: random command sequences across four views forked
//! from one canon. After every successful write the view passes the invariant checker; after
//! every successful landing canon does. The generator is biased toward conflicts (a small shared
//! pool of targets, clashing names, folder and section moves that can form cycles, deep sibling
//! keys that give `rebalance` real work), and the run counts outcomes and asserts coverage
//! minimums so the test cannot silently decay into one that never conflicts.
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
    Land {
        view: u8,
    },
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
        2 => (v.clone(), p.clone(), name()).prop_map(|(view, pick, name)| Cmd::Rename { view, pick, name }),
        3 => (v.clone(), p.clone()).prop_map(|(view, parent)| Cmd::Rebalance { view, parent }),
        3 => v.prop_map(|view| Cmd::Land { view }),
    ]
}

/// Ids in `scope` of the given types, sorted, so picks are deterministic.
fn ids(r: &mut Rdy, scope: &Scope, types: &[&str]) -> Vec<String> {
    let m = r.materialized(scope).unwrap();
    m.blocks
        .values()
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
        {"op": "create", "type": "doc", "id": "$d1", "parent": "$f", "fields": {"title": "One", "doc_type": "glossary"}},
        {"op": "create", "type": "doc", "id": "$d2", "fields": {"title": "Two", "doc_type": "glossary"}},
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
}

struct Sim<'a> {
    r: Rdy,
    /// a slot whose landing was refused is abandoned: it continues under a fresh view name
    generation: [u32; 4],
    stats: &'a RefCell<Stats>,
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
        Cmd::Land { .. } => "land",
    }
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

    /// One write; on success the view must pass the checker.
    fn write(&mut self, slot: u8, ops: serde_json::Value, c: &Cmd) -> Result<bool, TestCaseError> {
        let view = self.view(slot);
        let rebalance = ops[0]["op"] == "rebalance";
        match write(&mut self.r, &view, ops) {
            Ok(_) => {
                let mut s = self.stats.borrow_mut();
                *s.writes_ok.entry(kind(c)).or_default() += 1;
                if rebalance {
                    s.effective_rebalances += 1;
                }
                drop(s);
                if let Err(x) = check(&mut self.r, &Scope::View(view.clone())) {
                    return Err(TestCaseError::fail(format!(
                        "after {c:?}, view {view} broke: {x:?}"
                    )));
                }
                Ok(true)
            }
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
        let res = self.r.propose(&writer(), &view, &view).and_then(|p| {
            let warned = p.warnings.iter().any(|i| i.code == "W_SIBLINGS_REKEYED");
            self.r
                .land_report(&approver(), &p.id)
                .map(|l| warned || l.warnings.iter().any(|i| i.code == "W_SIBLINGS_REKEYED"))
        });
        match res {
            Ok(rekeyed) => {
                let mut s = self.stats.borrow_mut();
                s.lands_ok += 1;
                s.siblings_rekeyed += usize::from(rekeyed);
                drop(s);
                if let Err(x) = check(&mut self.r, &Scope::Canon) {
                    return Err(TestCaseError::fail(format!(
                        "after {c:?} ({view}), canon broke: {x:?}"
                    )));
                }
            }
            // an empty view has nothing to propose: not a conflict
            Err(e) if e.code == E_BAD_INPUT => {}
            Err(e) => {
                *self
                    .stats
                    .borrow_mut()
                    .land_refused
                    .entry(refusal(&e))
                    .or_default() += 1;
                self.generation[slot as usize] += 1;
            }
        }
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
                        Some(p) if parent % 2 == 0 => json!({"op": "create", "type": "doc", "parent": p, "fields": {"name": nm, "title": "D", "doc_type": "glossary"}}),
                        _ => json!({"op": "create", "type": "doc", "fields": {"name": nm, "title": "D", "doc_type": "glossary"}}),
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
                        let m = self.r.materialized(&sc).unwrap();
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
                let all = ids(&mut self.r, &sc, &["claim", "section"]);
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
                        &ids(&mut self.r, &sc, &["claim", "section", "doc", "folder"]),
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
                    pick(&ids(&mut self.r, &sc, &["claim"]), *p)
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
            Cmd::Land { view: vi } => return self.land(*vi, c),
        };
        if let Some(op) = op {
            self.write(slot, json!([op]), c)?;
        }
        Ok(())
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
        let (_d, mut r) = project();
        base(&mut r);
        let mut sim = Sim {
            r,
            generation: [0; 4],
            stats: &stats,
        };
        for c in &cmds {
            sim.apply(c)?;
        }
        Ok(())
    });
    if let Err(e) = result {
        panic!("{e}");
    }

    // coverage minimums (fast-mode floors; deep mode sees about ten times as much)
    let s = stats.into_inner();
    eprintln!("split-brain coverage: {s:#?}");
    assert!(
        s.effective_rebalances >= 10,
        "too few effective rebalances: {s:#?}"
    );
    let refused: usize = s.land_refused.values().sum();
    assert!(refused >= 10, "too few refused landings: {s:#?}");
    assert!(
        s.land_refused.len() >= 3,
        "refused landings cover fewer than 3 codes: {s:#?}"
    );
    assert!(
        s.siblings_rekeyed >= 1,
        "no W_SIBLINGS_REKEYED seen: {s:#?}"
    );
}
