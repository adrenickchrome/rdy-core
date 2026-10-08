//! The invariant checker (spec §3): what must always hold in canon or a view.
use rdy_engine::{Rdy, Scope};
use rdy_model::{
    Block, BlockId, FieldValue, LandingRecord, StateSnapshot, decision, tree, valid_order_key,
};
use rdy_schema::{Severity, validate_block};
use rdy_store::{Conflict, ViewOpKind, kinds};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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
        if let Err(vs) = check_views(r) {
            out.extend(vs);
        }
    }
    if let Err(e) = r.fsck() {
        out.push(v("fsck", None, e.to_string()));
    }
    if out.is_empty() { Ok(()) } else { Err(out) }
}

/// Accepted and rejected decisions never change once landed, except in the pack's tracking
/// fields (trk1): each later canon version equals the first frozen one, or differs from it only
/// in tracking fields.
fn frozen(r: &Rdy) -> Vec<Violation> {
    let mut out = vec![];
    let Ok(mut chain) = r.store().landings() else {
        return vec![v("frozen", None, "cannot read landings")];
    };
    chain.reverse(); // oldest first
    let tracking = r.store().tracking();
    let mut fixed: BTreeMap<BlockId, (rdy_model::Hash, Block)> = BTreeMap::new();
    for (_, rec) in chain {
        let rec: LandingRecord = rec;
        let Ok(snap) = r.store().get::<StateSnapshot>(&rec.state) else {
            continue;
        };
        for (id, (h, first)) in &fixed {
            if let Some(now) = snap.blocks.get(id)
                && now != h
                && !r.store().get_block(now).is_ok_and(|b| {
                    b == *first || decision::only_tracking_changed(first, &b, tracking)
                })
            {
                out.push(v(
                    "frozen",
                    Some(id),
                    "a frozen decision changed after it landed",
                ));
            }
        }
        for (id, h) in &snap.blocks {
            if fixed.contains_key(id) {
                continue;
            }
            if let Ok(b) = r.store().get_block(h)
                && decision::is_frozen(&b)
            {
                fixed.insert(id.clone(), (*h, b));
            }
        }
    }
    out
}

/// Spec §15.3 invariants 1-4 for views: every view's set is closed under deps (`closed`), every
/// live unpinned view reads on canon's head (`rebased`), every recorded or derived conflict side
/// names a stored change and block (`conflict_sides`), and the store's typed objects, view chains
/// and dependency DAG hold (`chains`). The frame walk of `Rdy::fsck` is left to the caller.
pub fn check_views(r: &Rdy) -> Result<(), Vec<Violation>> {
    let mut out = vec![];
    let st = r.read();
    let store = st.reader();
    let canon = st.canon_head();
    // landings, newest first: a landing's position is its distance from canon's head
    let landings = match store.landings() {
        Ok(l) => l,
        Err(e) => {
            return Err(vec![v(
                "chains",
                None,
                format!("cannot read landings: {e}"),
            )]);
        }
    };
    let position: HashMap<_, usize> = landings
        .iter()
        .enumerate()
        .map(|(i, (h, _))| (*h, i))
        .collect();
    for name in st.view_names() {
        let Some(head) = st.view_head(&name) else {
            continue;
        };
        let included = st.included(&name);
        // 1. closed under deps, relative to the view's base
        let set: HashSet<_> = included.iter().collect();
        let base_pos = head.base.and_then(|b| position.get(&b).copied());
        for c in included.iter() {
            let Ok(ch) = store.change(c) else {
                out.push(v(
                    "closed",
                    None,
                    format!("view {name:?}: change {c} is not stored"),
                ));
                continue;
            };
            for dep in &ch.deps {
                let landed_in_base = match (store.landing_of(dep), base_pos) {
                    (Ok(Some(l)), Some(b)) => position.get(&l).is_some_and(|at| *at >= b),
                    _ => false,
                };
                if !set.contains(dep) && !landed_in_base {
                    out.push(v(
                        "closed",
                        None,
                        format!(
                            "view {name:?}: change {c} depends on {dep}, which is neither included nor in the view's base"
                        ),
                    ));
                }
            }
        }
        // 2. a live unpinned view reads on canon's head
        if !head.closed && !head.pinned {
            if !included.is_empty() {
                if head.base != canon {
                    out.push(v(
                        "rebased",
                        None,
                        format!(
                            "view {name:?} has work on base {:?}, but canon's head is {canon:?}",
                            head.base
                        ),
                    ));
                }
            } else if st.effective_base(&name) != canon {
                out.push(v(
                    "rebased",
                    None,
                    format!("empty view {name:?} does not read on canon's head"),
                ));
            }
        }
        // 3. conflict sides: those a landing recorded, and those the view's merge derives
        let mut conflicts: Vec<Conflict> = vec![];
        match store.view_op_ids(&name) {
            Ok(ids) => {
                for h in ids {
                    match store.view_op(&h) {
                        Ok(op) => {
                            if let ViewOpKind::Rebase { conflicts: cs, .. } = op.op {
                                conflicts.extend(cs);
                            }
                        }
                        Err(e) => out.push(v(
                            "conflict_sides",
                            None,
                            format!("view {name:?}: cannot read op {h}: {e}"),
                        )),
                    }
                }
            }
            Err(e) => out.push(v(
                "conflict_sides",
                None,
                format!("view {name:?}: cannot list its ops: {e}"),
            )),
        }
        if !head.closed {
            match st.conflicts(&Scope::View(name.clone())) {
                Ok(cs) => conflicts.extend(cs.iter().cloned()),
                Err(e) => out.push(v(
                    "conflict_sides",
                    None,
                    format!("view {name:?}: cannot materialize it to derive its conflicts: {e}"),
                )),
            }
        }
        for c in &conflicts {
            for side in &c.sides {
                if let Some(ch) = &side.change
                    && !store.has_kind(kinds::CHANGE, ch).unwrap_or(false)
                {
                    out.push(v(
                        "conflict_sides",
                        Some(&c.block),
                        format!("view {name:?}: a conflict side names {ch}, which is not a change"),
                    ));
                }
                if let Some(ver) = &side.version
                    && !store.has_kind(kinds::BLOCK, ver).unwrap_or(false)
                {
                    out.push(v(
                        "conflict_sides",
                        Some(&c.block),
                        format!("view {name:?}: a conflict side holds {ver}, which is not a block"),
                    ));
                }
            }
        }
    }
    // 4. chains, heads, index entries, the dependency DAG
    if let Err(e) = store.check_objects() {
        out.push(v("chains", None, e.to_string()));
    }
    if out.is_empty() { Ok(()) } else { Err(out) }
}
