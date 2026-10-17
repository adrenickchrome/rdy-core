//! The invariant checker (spec §3): what must always hold in canon or a view.
//!
//! A block whose content an approved removal erased reads as its authorized projection (its id,
//! type, parent, order and recorded identity; body `Content removed.`; no fields, links or pins).
//! The checker keeps every structure rule for it (parent, cycle, depth, placement, order key,
//! references) and checks that its absence has exact active authorization; it never validates
//! the stub's missing fields as content (D-600CW, D-3M81W).
use rdy_engine::{Rdy, ReadState, Scope};
use rdy_model::redaction::RedactedBlockIdentity;
use rdy_model::{
    Block, BlockId, FieldValue, Hash, LandingRecord, StateSnapshot, decision, tree, valid_order_key,
};
use rdy_schema::{Severity, validate_block, validate_prefix};
use rdy_store::{Conflict, StoreReader, ViewOpKind, kinds};
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

/// The checker's rules on `scope`, all read from one snapshot of `r` (D-XH24M), then `r.fsck()`.
pub fn check(r: &Rdy, scope: &Scope) -> Result<(), Vec<Violation>> {
    let st = r.read();
    let mut out = match check_in(r, &st, scope) {
        Ok(()) => vec![],
        Err(vs) => vs,
    };
    if let Err(e) = r.fsck() {
        out.push(v("fsck", None, e.to_string()));
    }
    if out.is_empty() { Ok(()) } else { Err(out) }
}

/// `check` on canon, then on every open view, all from one snapshot of `r`, then `r.fsck()`
/// once and the `logged` rule (`reindex_check::logged`): every violation together.
pub fn check_all(r: &Rdy) -> Result<(), Vec<Violation>> {
    let st = r.read();
    let mut out = vec![];
    let mut scopes = vec![Scope::Canon];
    for name in st.view_names() {
        if st.view_head(&name).is_some_and(|h| !h.closed) {
            scopes.push(Scope::View(name));
        }
    }
    for scope in &scopes {
        if let Err(vs) = check_in(r, &st, scope) {
            out.extend(vs);
        }
    }
    match r.fsck() {
        Ok(f) => out.extend(crate::reindex_check::logged_in(&st, &f)),
        Err(e) => out.push(v("fsck", None, e.to_string())),
    }
    if out.is_empty() { Ok(()) } else { Err(out) }
}

/// Every rule of `check` but `fsck`, read from the snapshot `st`; `frozen` reads `r`'s store.
fn check_in(r: &Rdy, st: &ReadState, scope: &Scope) -> Result<(), Vec<Violation>> {
    let mut out = vec![];
    let m = match st.materialized(scope) {
        Ok(m) => m,
        Err(e) => {
            return Err(vec![v(
                "fsck",
                None,
                format!("cannot materialize {scope:?}: {e}"),
            )]);
        }
    };
    let pack = st.pack();
    for b in m.iter() {
        // an authorized projection: its absence must have exact active authorization
        if let Some(ident) = &b.read_identity
            && let Err(d) = authorized(st.reader(), &b.id, ident)
        {
            out.push(v("redaction", Some(&b.id), d));
        }
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
        // schema, of content only: a removed block's stub holds no fields to validate
        if b.is_redacted() {
            continue;
        }
        // schema; nameless M1 docs are validated with their effective name, as the gate does
        let mut checked: Block = b.clone();
        if b.block_type == tree::DOC
            && !b.fields.contains_key("name")
            && let Some(n) = m.name_of(&b.id)
        {
            checked.fields.insert("name".into(), FieldValue::Str(n));
        }
        for i in validate_prefix(pack, &checked)
            .into_iter()
            .chain(validate_block(pack, &checked, parent))
        {
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
        if let Err(vs) = check_views_in(st) {
            out.extend(vs);
        }
    }
    if out.is_empty() { Ok(()) } else { Err(out) }
}

/// Whether the removed version `ident` of block `id` has exact active authorization: the store's
/// active mask names the stub's own request for that BLOCK version, and that request's manifest
/// selects exactly that id and version. The reason it has not, otherwise.
fn authorized(
    reader: &StoreReader,
    id: &BlockId,
    ident: &RedactedBlockIdentity,
) -> Result<(), String> {
    match reader.redaction_of(kinds::BLOCK, &ident.version) {
        Ok(Some(q)) if q == ident.request => {}
        Ok(other) => {
            return Err(format!(
                "removed version {} reads as removed by {}, but the active removal is {other:?}",
                ident.version, ident.request
            ));
        }
        Err(e) => {
            return Err(format!(
                "cannot read the removal of version {}: {e}",
                ident.version
            ));
        }
    }
    let req = reader
        .redaction_request(&ident.request)
        .map_err(|e| format!("cannot read request {}: {e}", ident.request))?;
    if !req
        .manifest
        .versions
        .iter()
        .any(|(i, h)| i == id && *h == ident.version)
    {
        return Err(format!(
            "request {} does not select version {} of {id}",
            ident.request, ident.version
        ));
    }
    Ok(())
}

/// A frozen decision whose content comparison is unavailable: a compared version (its first
/// frozen version, a later one, or both) was erased by an authorized removal. The checker
/// verified each erased version's exact active authorization instead; it never counts this as
/// a successful comparison.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Unavailable {
    pub block: BlockId,
    /// The first frozen version.
    pub first: Hash,
    /// The later version that differs from it.
    pub later: Hash,
    /// The requests that removed the compared versions, sorted.
    pub requests: Vec<Hash>,
}

/// What the frozen rule found: its violations, and the comparisons a removal made unavailable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrozenReport {
    pub violations: Vec<Violation>,
    pub unavailable: Vec<Unavailable>,
}

/// Accepted and rejected decisions never change once landed, except in the pack's tracking
/// fields (trk1): each later canon version equals the first frozen one, or differs from it only
/// in tracking fields.
fn frozen(r: &Rdy) -> Vec<Violation> {
    frozen_report(r).violations
}

/// The frozen rule, read from `r`'s landings oldest first. A decision is frozen from the first
/// snapshot whose version is accepted or rejected; a removed version answers with its stub's
/// recorded frozen fact. Each later version that differs is compared in content with the first
/// frozen version: it must equal it or differ only in tracking fields.
///
/// Where an authorized removal erased a compared version, that comparison is unavailable: each
/// erased version must have exact active authorization (else a `frozen` violation), and the
/// case is listed in `unavailable`, never counted as a successful comparison. When the first
/// frozen version is erased, the first later version that reads in full becomes the base the
/// next versions are compared with, so a later unremoved change still fails.
pub fn frozen_report(r: &Rdy) -> FrozenReport {
    let mut out = FrozenReport::default();
    let Ok(mut chain) = r.store().landings() else {
        out.violations
            .push(v("frozen", None, "cannot read landings"));
        return out;
    };
    chain.reverse(); // oldest first
    let store = r.store();
    let reader: &StoreReader = store;
    let tracking = store.tracking();
    let changed = |id: &BlockId| {
        v(
            "frozen",
            Some(id),
            "a frozen decision changed after it landed",
        )
    };
    // per frozen decision: its first frozen version, that block, and the base later versions
    // are compared with (the first block itself, unless a removal erased it)
    let mut fixed: BTreeMap<BlockId, (Hash, Block, Option<Block>)> = BTreeMap::new();
    let mut unavailable: BTreeSet<Unavailable> = BTreeSet::new();
    for (_, rec) in chain {
        let rec: LandingRecord = rec;
        let Ok(snap) = store.get::<StateSnapshot>(&rec.state) else {
            continue;
        };
        for (id, (h, first, base)) in fixed.iter_mut() {
            let Some(now) = snap.blocks.get(id) else {
                continue;
            };
            if now == h {
                continue;
            }
            let Ok(later) = store.get_block(now) else {
                out.violations.push(changed(id));
                continue;
            };
            if let Some(b) = base.as_ref()
                && !later.is_redacted()
            {
                if later != *b && !decision::only_tracking_changed(b, &later, tracking) {
                    out.violations.push(changed(id));
                }
                continue;
            }
            // the comparison needs erased fields: verify each erased version's coverage
            let mut requests = vec![];
            let mut covered = true;
            for b in [&*first, &later] {
                let Some(ident) = &b.read_identity else {
                    continue;
                };
                match authorized(reader, id, ident) {
                    Ok(()) => requests.push(ident.request),
                    Err(d) => {
                        covered = false;
                        out.violations.push(v(
                            "frozen",
                            Some(id),
                            format!("a frozen decision's version is missing: {d}"),
                        ));
                    }
                }
            }
            if covered {
                requests.sort();
                requests.dedup();
                unavailable.insert(Unavailable {
                    block: id.clone(),
                    first: *h,
                    later: *now,
                    requests,
                });
                if base.is_none() && !later.is_redacted() {
                    *base = Some(later);
                }
            }
        }
        for (id, h) in &snap.blocks {
            if fixed.contains_key(id) {
                continue;
            }
            if let Ok(b) = store.get_block(h)
                && decision::is_frozen(&b)
            {
                let base = (!b.is_redacted()).then(|| b.clone());
                fixed.insert(id.clone(), (*h, b, base));
            }
        }
    }
    out.unavailable = unavailable.into_iter().collect();
    out
}

/// Spec §15.3 invariants 1-4 for views: every view's set is closed under deps (`closed`), every
/// live unpinned view reads on canon's head (`rebased`), every recorded or derived conflict side
/// names a stored change and block (`conflict_sides`), and the store's typed objects, view chains
/// and dependency DAG hold (`chains`). The frame walk of `Rdy::fsck` is left to the caller.
pub fn check_views(r: &Rdy) -> Result<(), Vec<Violation>> {
    check_views_in(&r.read())
}

/// `check_views` on the snapshot `st`.
fn check_views_in(st: &ReadState) -> Result<(), Vec<Violation>> {
    let mut out = vec![];
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
