//! The invariant checker (spec §3): what must always hold in canon or a view.
use rdy_engine::{Rdy, Scope};
use rdy_model::{
    Block, BlockId, FieldValue, LandingRecord, StateSnapshot, decision, tree, valid_order_key,
};
use rdy_schema::{Severity, validate_block};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub detail: String,
}

fn v(rule: &'static str, block: Option<&BlockId>, detail: impl Into<String>) -> Violation {
    Violation {
        rule,
        block: block.cloned(),
        detail: detail.into(),
    }
}

pub fn check(r: &Rdy, scope: &Scope) -> Result<(), Vec<Violation>> {
    let mut out = vec![];
    let m = match r.read().materialized(scope) {
        Ok(m) => m.clone(),
        Err(e) => {
            return Err(vec![v(
                "fsck",
                None,
                format!("cannot materialize {scope:?}: {e}"),
            )]);
        }
    };
    let pack = r.pack().clone();
    for b in m.iter() {
        // parent exists; walk the chain for cycles and folder depth
        let parent = b.parent.as_ref().and_then(|p| m.get(p));
        if let Some(p) = &b.parent
            && parent.is_none()
        {
            out.push(v(
                "parent",
                Some(&b.id),
                format!("parent {p} does not exist"),
            ));
        }
        let (mut cur, mut steps, mut folders) = (b.parent.clone(), 0usize, 0usize);
        while let Some(c) = cur {
            if c == b.id {
                out.push(v("cycle", Some(&b.id), "its parent chain loops back to it"));
                break;
            }
            steps += 1;
            if steps > m.len() {
                break;
            }
            let Some(pb) = m.get(&c) else { break };
            if pb.block_type == tree::FOLDER {
                folders += 1;
            }
            cur = pb.parent.clone();
        }
        let own = usize::from(b.block_type == tree::FOLDER);
        if folders + own > tree::MAX_FOLDER_DEPTH {
            out.push(v(
                "depth",
                Some(&b.id),
                format!("{} folders deep", folders + own),
            ));
        }
        // placement: docs and folders only at the top or under a folder; others under something
        if tree::is_tree_type(&b.block_type) {
            if let Some(p) = parent
                && p.block_type != tree::FOLDER
            {
                out.push(v(
                    "placement",
                    Some(&b.id),
                    format!("a {} under a {}", b.block_type, p.block_type),
                ));
            }
        } else if b.parent.is_none() {
            out.push(v(
                "placement",
                Some(&b.id),
                format!("a {} at the top level", b.block_type),
            ));
        }
        if !valid_order_key(&b.order) {
            out.push(v(
                "order-key",
                Some(&b.id),
                format!("order key {:?}", b.order),
            ));
        }
        for tok in b.ref_tokens() {
            if BlockId::parse(&tok)
                .ok()
                .and_then(|id| m.get(&id))
                .is_none()
            {
                out.push(v(
                    "ref",
                    Some(&b.id),
                    format!("{{{{{tok}}}}} does not resolve"),
                ));
            }
        }
        for l in b.links.values().flatten() {
            if m.get(&l.target).is_none() {
                out.push(v(
                    "ref",
                    Some(&b.id),
                    format!("link to {} does not resolve", l.target),
                ));
            }
        }
        // schema; nameless M1 docs are validated with their effective name, as the gate does
        let mut checked: Block = b.clone();
        if b.block_type == tree::DOC
            && !b.fields.contains_key("name")
            && let Some(n) = m.name_of(&b.id)
        {
            checked.fields.insert("name".into(), FieldValue::Str(n));
        }
        for i in validate_block(&pack, &checked, parent) {
            if i.severity == Severity::Error && i.code != "E_ORDER_KEY" {
                out.push(v(
                    "schema",
                    Some(&b.id),
                    format!("{}: {}", i.code, i.message),
                ));
            }
        }
    }
    // sibling names, ignoring case
    let parents: BTreeSet<Option<BlockId>> = m
        .iter()
        .filter(|b| tree::is_tree_type(&b.block_type))
        .map(|b| b.parent.clone())
        .collect();
    for p in parents {
        let mut seen: BTreeMap<String, BlockId> = BTreeMap::new();
        for (id, name) in m.names_under(p.as_ref()) {
            if let Some(other) = seen.insert(name.to_ascii_lowercase(), id.clone()) {
                out.push(v(
                    "name",
                    Some(&id),
                    format!("{name:?} clashes with {other}"),
                ));
            }
        }
    }
    if *scope == Scope::Canon {
        out.extend(frozen(r));
    }
    if let Err(e) = r.fsck() {
        out.push(v("fsck", None, e.to_string()));
    }
    if out.is_empty() { Ok(()) } else { Err(out) }
}

/// Accepted and rejected decisions never change once landed.
fn frozen(r: &Rdy) -> Vec<Violation> {
    let mut out = vec![];
    let Ok(mut chain) = r.store().landings() else {
        return vec![v("frozen", None, "cannot read landings")];
    };
    chain.reverse(); // oldest first
    let mut fixed: BTreeMap<BlockId, rdy_model::Hash> = BTreeMap::new();
    for (_, rec) in chain {
        let rec: LandingRecord = rec;
        let Ok(snap) = r.store().get::<StateSnapshot>(&rec.state) else {
            continue;
        };
        for (id, h) in &fixed {
            if let Some(now) = snap.blocks.get(id)
                && now != h
            {
                out.push(v(
                    "frozen",
                    Some(id),
                    "a frozen decision changed after it landed",
                ));
            }
        }
        for (id, h) in &snap.blocks {
            if let Ok(b) = r.store().get_block(h)
                && decision::is_frozen(&b)
            {
                fixed.entry(id.clone()).or_insert(*h);
            }
        }
    }
    out
}
