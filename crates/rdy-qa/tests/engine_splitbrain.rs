use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;

#[derive(Clone, Debug)]
enum Cmd {
    Create {
        view: u8,
        kind: u8,
        parent: u16,
        name: u8,
    },
    Move {
        view: u8,
        pick: u16,
        to: u16,
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

fn cmd() -> impl Strategy<Value = Cmd> {
    let v = 0u8..4;
    prop_oneof![
        4 => (v.clone(), 0u8..4, any::<u16>(), 0u8..4).prop_map(|(view, kind, parent, name)| Cmd::Create { view, kind, parent, name }),
        2 => (v.clone(), any::<u16>(), any::<u16>()).prop_map(|(view, pick, to)| Cmd::Move { view, pick, to }),
        1 => (v.clone(), any::<u16>()).prop_map(|(view, pick)| Cmd::Delete { view, pick }),
        2 => (v.clone(), any::<u16>()).prop_map(|(view, pick)| Cmd::Edit { view, pick }),
        1 => (v.clone(), any::<u16>(), 0u8..4).prop_map(|(view, pick, name)| Cmd::Rename { view, pick, name }),
        1 => (v.clone(), any::<u16>()).prop_map(|(view, parent)| Cmd::Rebalance { view, parent }),
        2 => v.prop_map(|view| Cmd::Land { view }),
    ]
}

fn view(i: u8) -> String {
    format!("v{i}")
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
        {"op": "create", "type": "doc", "id": "$d1", "parent": "$f", "fields": {"title": "One", "doc_type": "glossary"}},
        {"op": "create", "type": "doc", "id": "$d2", "fields": {"title": "Two", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s1", "parent": "$d1", "fields": {"title": "Terms"}},
        {"op": "create", "type": "section", "id": "$s2", "parent": "$d2", "fields": {"title": "Terms"}},
        {"op": "create", "type": "claim", "parent": "$s1", "body": "c1"},
        {"op": "create", "type": "claim", "parent": "$s2", "body": "c2"}
    ])).unwrap();
    land(r, "seed").unwrap();
}

fn apply(r: &mut Rdy, c: &Cmd) -> Option<String> {
    let (v, ops) = match c {
        Cmd::Create {
            view: vi,
            kind,
            parent,
            name,
        } => {
            let sc = Scope::View(view(*vi));
            let op = match kind {
                0 => pick(&ids(r, &sc, &["section"]), *parent).map(|p| json!({"op": "create", "type": "claim", "parent": p, "body": "new claim"})),
                1 => pick(&ids(r, &sc, &["doc", "section"]), *parent).map(|p| json!({"op": "create", "type": "section", "parent": p, "fields": {"title": "S"}})),
                2 => Some(match pick(&ids(r, &sc, &["folder"]), *parent) {
                    Some(p) if parent % 2 == 0 => json!({"op": "create", "type": "doc", "parent": p, "fields": {"name": NAMES[*name as usize], "title": "D", "doc_type": "glossary"}}),
                    _ => json!({"op": "create", "type": "doc", "fields": {"name": NAMES[*name as usize], "title": "D", "doc_type": "glossary"}}),
                }),
                _ => Some(match pick(&ids(r, &sc, &["folder"]), *parent) {
                    Some(p) if parent % 2 == 0 => json!({"op": "create", "type": "folder", "parent": p, "fields": {"name": NAMES[*name as usize]}}),
                    _ => json!({"op": "create", "type": "folder", "fields": {"name": NAMES[*name as usize]}}),
                }),
            };
            (*vi, op)
        }
        Cmd::Move {
            view: vi,
            pick: p,
            to,
        } => {
            let sc = Scope::View(view(*vi));
            let all = ids(r, &sc, &["claim", "section", "doc", "folder"]);
            let parents = ids(r, &sc, &["section", "doc", "folder"]);
            (
                *vi,
                pick(&all, *p)
                    .zip(pick(&parents, *to))
                    .map(|(id, parent)| json!({"op": "move", "id": id, "parent": parent})),
            )
        }
        Cmd::Delete { view: vi, pick: p } => {
            let sc = Scope::View(view(*vi));
            (
                *vi,
                pick(&ids(r, &sc, &["claim", "section", "doc", "folder"]), *p)
                    .map(|id| json!({"op": "delete", "id": id})),
            )
        }
        Cmd::Edit { view: vi, pick: p } => {
            let sc = Scope::View(view(*vi));
            (*vi, pick(&ids(r, &sc, &["claim"]), *p).map(|id| json!({"op": "update", "id": id, "body": format!("edited in {}", view(*vi))})))
        }
        Cmd::Rename {
            view: vi,
            pick: p,
            name,
        } => {
            let sc = Scope::View(view(*vi));
            (*vi, pick(&ids(r, &sc, &["doc", "folder"]), *p).map(|id| json!({"op": "update", "id": id, "fields": {"name": NAMES[*name as usize]}})))
        }
        Cmd::Rebalance { view: vi, parent } => {
            let sc = Scope::View(view(*vi));
            (
                *vi,
                pick(&ids(r, &sc, &["section", "doc", "folder"]), *parent)
                    .map(|p| json!({"op": "rebalance", "parent": p})),
            )
        }
        Cmd::Land { view: vi } => {
            return land(r, &view(*vi)).ok().map(|_| "landed".into());
        }
    };
    let op = ops?;
    write(r, &view(v), json!([op])).ok().map(|_| view(v))
}

proptest! {
    #![proptest_config(Config { cases: cases(32), rng_seed: RngSeed::Fixed(0x5b11_7b2a), failure_persistence: None, ..Config::default() })]
    #[test]
    fn engine_split_brain_histories_keep_every_invariant(cmds in proptest::collection::vec(cmd(), 1..48)) {
        let (_d, mut r) = project();
        base(&mut r);
        for c in &cmds {
            match apply(&mut r, c) {
                Some(s) if s == "landed" => {
                    if let Err(v) = check(&mut r, &Scope::Canon) {
                        prop_assert!(false, "after {c:?}, canon broke: {v:?}");
                    }
                }
                Some(v) => {
                    if let Err(x) = check(&mut r, &Scope::View(v.clone())) {
                        prop_assert!(false, "after {c:?}, view {v} broke: {x:?}");
                    }
                }
                None => {}
            }
        }
    }
}
