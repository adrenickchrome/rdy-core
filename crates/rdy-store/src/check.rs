//! Structural checks of the typed objects (fsck2, fsck3): view chains and heads, the view-op
//! index, canon's landings against the change -> landing index, the relay and evidence rules of
//! a landing, recorded conflicts, undo chains, the dependency DAG and closed included sets.
use crate::StoreReader;
use crate::error::StoreError;
use crate::kinds;
use crate::view::{ViewHead, ViewOpKind, resolve};
use rdy_model::{Assurance, Change, EvidenceKind, Hash, LandingRecord};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

/// What `Store::check_objects` counted in a store that checked clean.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreCheck {
    pub views: usize,
    /// The ops of every view's chain, walked in full.
    pub view_ops: usize,
    /// The sum of `changes` over canon's landings.
    pub landed_changes: usize,
    /// Canon's landings that carry a relay.
    pub relayed: usize,
    /// The conflicts recorded by the `Rebase` ops of every view's chain.
    pub conflicts_recorded: usize,
}

/// What `check_view` found out about one view.
struct ViewCheck {
    ops: usize,
    conflicts: usize,
    included: Arc<Vec<Hash>>,
}

fn bad(m: String) -> StoreError {
    StoreError::Decode(m)
}

fn variant(k: &ViewOpKind) -> &'static str {
    match k {
        ViewOpKind::Create { .. } => "Create",
        ViewOpKind::Fork { .. } => "Fork",
        ViewOpKind::Commit { .. } => "Commit",
        ViewOpKind::Include { .. } => "Include",
        ViewOpKind::Exclude { .. } => "Exclude",
        ViewOpKind::Rebase { .. } => "Rebase",
        ViewOpKind::Undo { .. } => "Undo",
        ViewOpKind::Close { .. } => "Close",
        ViewOpKind::Own { .. } => "Own",
    }
}

impl StoreReader {
    /// Checks the view chains, their heads and index entries, canon's landings (relay and evidence
    /// rules included), recorded conflicts, undo chains, the dependency DAG and the closure of
    /// every view's included set; the first failure found is a `StoreError::Decode`.
    pub fn check_objects(&self) -> Result<StoreCheck, StoreError> {
        let mut views = 0;
        let mut view_ops = 0;
        let mut conflicts_recorded = 0;
        let mut sets: Vec<(String, ViewHead, Arc<Vec<Hash>>)> = vec![];
        for (key, bytes) in self.refs_with_prefix("view/")? {
            let name = &key["view/".len()..];
            let head: ViewHead = postcard::from_bytes(&bytes)
                .map_err(|_| bad(format!("view {name:?}: its ref is not a view head")))?;
            let c = self.check_view(name, &head)?;
            view_ops += c.ops;
            conflicts_recorded += c.conflicts;
            sets.push((name.to_string(), head, c.included));
            views += 1;
        }
        let landings = self.landings()?;
        let mut landed_changes = 0;
        let mut relayed = 0;
        for (l, rec) in &landings {
            self.check_landing_fields(l, rec)?;
            if rec.relayed_by.is_some() {
                relayed += 1;
            }
            for c in &rec.changes {
                if !self.has_kind(kinds::CHANGE, c)? {
                    return Err(bad(format!("landing {l}: change {c} is missing")));
                }
                match self.landing_of(c)? {
                    Some(o) if o == *l => {}
                    other => {
                        return Err(bad(format!(
                            "landing {l}: the index has {other:?} as the landing of change {c}; run rdy reindex"
                        )));
                    }
                }
            }
            landed_changes += rec.changes.len();
        }
        self.check_dag(&landings, &sets)?;
        Ok(StoreCheck {
            views,
            view_ops,
            landed_changes,
            relayed,
            conflicts_recorded,
        })
    }

    /// The relay and evidence rules of one landing, and its references to other objects.
    fn check_landing_fields(&self, l: &Hash, rec: &LandingRecord) -> Result<(), StoreError> {
        if rec.relayed_by.is_some() != rec.relay_evidence.is_some() {
            return Err(bad(format!(
                "landing {l}: relayed_by and relay_evidence must both be set or both unset"
            )));
        }
        if (rec.assurance == Assurance::Relayed) != rec.relayed_by.is_some() {
            return Err(bad(format!(
                "landing {l}: assurance is {} but the relay fields are {}",
                rec.assurance.as_str(),
                if rec.relayed_by.is_some() {
                    "set"
                } else {
                    "unset"
                }
            )));
        }
        match &rec.evidence {
            Some(e) => {
                let kind = e.kind.as_str();
                match e.kind {
                    EvidenceKind::Elicited if rec.assurance != Assurance::Local => {
                        return Err(bad(format!(
                            "landing {l}: evidence kind {kind} needs assurance local"
                        )));
                    }
                    EvidenceKind::Card | EvidenceKind::Text
                        if rec.assurance != Assurance::Relayed =>
                    {
                        return Err(bad(format!(
                            "landing {l}: evidence kind {kind} needs assurance relayed"
                        )));
                    }
                    _ => {}
                }
            }
            None if rec.assurance == Assurance::Relayed => {
                return Err(bad(format!(
                    "landing {l}: a relayed landing names its evidence kind (card or text)"
                )));
            }
            None => {}
        }
        if let Some(e) = &rec.evidence
            && e.kind == EvidenceKind::Card
            && e.card_hash.is_none()
        {
            return Err(bad(format!("landing {l}: card evidence needs card_hash")));
        }
        for h in &rec.rebased_from {
            if !self.has_kind(kinds::LANDING, h)? {
                return Err(bad(format!(
                    "landing {l}: rebased_from names {h}, which is not a landing"
                )));
            }
        }
        if let Some(h) = &rec.approved_version
            && !self.has_kind(kinds::PROPOSAL, h)?
        {
            return Err(bad(format!(
                "landing {l}: approved_version {h} is not a proposal"
            )));
        }
        Ok(())
    }

    /// A change that the check follows, or None when it is not stored (its absence is reported
    /// where a change of the checked sets names it).
    fn change_for_dag(
        &self,
        cache: &mut HashMap<Hash, Option<Arc<Change>>>,
        c: &Hash,
    ) -> Result<Option<Arc<Change>>, StoreError> {
        if let Some(v) = cache.get(c) {
            return Ok(v.clone());
        }
        let v = match self.change(c) {
            Ok(ch) => Some(Arc::new(ch)),
            Err(StoreError::NotFound(_)) => None,
            Err(e) => return Err(e),
        };
        cache.insert(*c, v.clone());
        Ok(v)
    }

    /// Every dep of every change of every view's set and every landing exists; the deps have no
    /// cycle; and each view's included changes have their deps included or landed at or before the
    /// view's base.
    fn check_dag(
        &self,
        landings: &[(Hash, LandingRecord)],
        sets: &[(String, ViewHead, Arc<Vec<Hash>>)],
    ) -> Result<(), StoreError> {
        let mut cache: HashMap<Hash, Option<Arc<Change>>> = HashMap::new();
        // position in canon's chain, newest = 0, and the landing each landed change is in
        let mut position: HashMap<Hash, usize> = HashMap::new();
        let mut landed_at: HashMap<Hash, usize> = HashMap::new();
        for (i, (l, rec)) in landings.iter().enumerate() {
            position.insert(*l, i);
            for c in &rec.changes {
                landed_at.entry(*c).or_insert(i);
            }
        }
        let mut d: BTreeSet<Hash> = landed_at.keys().copied().collect();
        for (_, _, inc) in sets {
            d.extend(inc.iter().copied());
        }
        for c in &d {
            let Some(ch) = self.change_for_dag(&mut cache, c)? else {
                // the sets' own checks report a missing change; a landed one is checked above
                continue;
            };
            for dep in &ch.deps {
                if !self.has_kind(kinds::CHANGE, dep)? {
                    return Err(bad(format!("change {c}: dep {dep} is missing")));
                }
            }
        }
        self.check_acyclic(&d, &mut cache)?;
        for (name, head, inc) in sets {
            let base_pos = head.base.and_then(|b| position.get(&b).copied());
            let included: HashSet<&Hash> = inc.iter().collect();
            for c in inc.iter() {
                let Some(ch) = self.change_for_dag(&mut cache, c)? else {
                    continue;
                };
                for dep in &ch.deps {
                    let in_base = match (landed_at.get(dep), base_pos) {
                        (Some(at), Some(base)) => *at >= base,
                        _ => false,
                    };
                    if !included.contains(dep) && !in_base {
                        return Err(bad(format!(
                            "view {name:?}: change {c} depends on {dep}, which is neither included nor in the view's base"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// An iterative depth-first search over the deps of `roots`, following into deps outside them.
    fn check_acyclic(
        &self,
        roots: &BTreeSet<Hash>,
        cache: &mut HashMap<Hash, Option<Arc<Change>>>,
    ) -> Result<(), StoreError> {
        let mut done: HashSet<Hash> = HashSet::new();
        let mut on_path: HashSet<Hash> = HashSet::new();
        for root in roots {
            if done.contains(root) {
                continue;
            }
            // (change, index of the next dep to visit)
            let mut stack: Vec<(Hash, usize)> = vec![(*root, 0)];
            on_path.insert(*root);
            while let Some((c, i)) = stack.last().copied() {
                let deps: Vec<Hash> = match self.change_for_dag(cache, &c)? {
                    Some(ch) => ch.deps.clone(),
                    None => vec![],
                };
                match deps.get(i) {
                    Some(dep) => {
                        stack.last_mut().expect("the stack is not empty").1 += 1;
                        if on_path.contains(dep) {
                            return Err(bad(format!("dependency cycle through {dep}")));
                        }
                        if !done.contains(dep) {
                            on_path.insert(*dep);
                            stack.push((*dep, 0));
                        }
                    }
                    None => {
                        stack.pop();
                        on_path.remove(&c);
                        done.insert(c);
                    }
                }
            }
        }
        Ok(())
    }

    /// Checks one view: its chain, head, index entries, recorded conflicts and undo chains.
    fn check_view(&self, name: &str, head: &ViewHead) -> Result<ViewCheck, StoreError> {
        let mut chain: BTreeSet<Hash> = BTreeSet::new();
        let mut checked: Vec<(Hash, ViewOpKind)> = vec![];
        let mut cur = Some(head.head);
        let mut last: Option<(Hash, ViewOpKind)> = None;
        while let Some(h) = cur {
            let op = match self.view_op(&h) {
                Ok(op) => op,
                Err(StoreError::NotFound(_)) => {
                    return Err(bad(format!("view {name:?}: op {h} is missing")));
                }
                Err(e) => return Err(e),
            };
            if op.view != name {
                return Err(bad(format!(
                    "view {name:?}: op {h} belongs to view {:?}",
                    op.view
                )));
            }
            if !chain.insert(h) {
                return Err(bad(format!("view {name:?}: op {h} is on its chain twice")));
            }
            cur = op.prev;
            if matches!(op.op, ViewOpKind::Rebase { .. } | ViewOpKind::Undo { .. }) {
                checked.push((h, op.op.clone()));
            }
            last = Some((h, op.op));
        }
        if let Some((h, kind)) = last
            && !matches!(kind, ViewOpKind::Create { .. } | ViewOpKind::Fork { .. })
        {
            return Err(bad(format!(
                "view {name:?}: its chain starts with {} {h}, not Create or Fork",
                variant(&kind)
            )));
        }
        let included = self.included_at(name, head)?;
        if included.len() as u64 != head.len {
            return Err(bad(format!(
                "view {name:?}: the head says {} changes, the chain gives {}",
                head.len,
                included.len()
            )));
        }
        if let Some(b) = head.base
            && !self.has_kind(kinds::LANDING, &b)?
        {
            return Err(bad(format!("view {name:?}: base {b} is not a landing")));
        }
        for c in included.iter() {
            if !self.has_kind(kinds::CHANGE, c)? {
                return Err(bad(format!("view {name:?}: change {c} is missing")));
            }
        }
        let indexed: BTreeSet<Hash> = self.view_op_ids(name)?.into_iter().collect();
        let newer = if indexed.is_subset(&chain) {
            BTreeSet::new()
        } else {
            self.ops_after(name, &chain)?
        };
        if let Some(h) = indexed.difference(&chain).find(|h| !newer.contains(*h)) {
            return Err(bad(format!(
                "view {name:?}: op {h} is indexed but not on its chain"
            )));
        }
        if let Some(h) = chain.difference(&indexed).next() {
            return Err(bad(format!(
                "view {name:?}: op {h} is on its chain but not indexed; run rdy reindex"
            )));
        }
        let mut conflicts = 0;
        for (h, kind) in &checked {
            match kind {
                ViewOpKind::Rebase { conflicts: cs, .. } => {
                    for c in cs.iter().flat_map(|c| c.sides.iter()) {
                        if let Some(c) = &c.change
                            && !self.has_kind(kinds::CHANGE, c)?
                        {
                            return Err(bad(format!(
                                "view {name:?}: rebase {h} records a conflict side {c} that is not a change"
                            )));
                        }
                    }
                    conflicts += cs.len();
                }
                ViewOpKind::Undo { .. } => {
                    resolve(self, name, h, 0)?;
                }
                _ => {}
            }
        }
        Ok(ViewCheck {
            ops: chain.len(),
            conflicts,
            included,
        })
    }

    /// The ops a writer has put on view `name`'s chain since `chain` was walked: those reached
    /// from the view's current head before the walk meets `chain`. Called after the index was
    /// read, so every op that read saw is on the current head's chain (an index apply moves the
    /// ref and indexes its ops in one commit); this keeps a check that runs beside a live writer
    /// from reporting an op the writer appended after the head it checked.
    fn ops_after(&self, name: &str, chain: &BTreeSet<Hash>) -> Result<BTreeSet<Hash>, StoreError> {
        let mut newer = BTreeSet::new();
        let mut cur = self.view_head(name)?.map(|h| h.head);
        while let Some(h) = cur {
            if chain.contains(&h) || !newer.insert(h) {
                break;
            }
            cur = match self.view_op(&h) {
                Ok(op) if op.view == name => op.prev,
                // a broken newer op is the next check's to report
                _ => None,
            };
        }
        Ok(newer)
    }
}
