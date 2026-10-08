//! Structural checks of the typed objects (fsck2, fsck3): view chains and heads, the view-op
//! index, canon's landings against the change -> landing index, the relay and evidence rules of
//! a landing, recorded conflicts, undo chains, the dependency DAG and closed included sets, and
//! the version chains of intents and alternative sets and the links that name them (cfmt1).
use crate::StoreReader;
use crate::coord::{ALT_REF, INTENT_REF, key_hash};
use crate::error::StoreError;
use crate::kinds;
use crate::view::{ViewHead, ViewOpKind, resolve};
use rdy_model::{
    AltChoice, AltSet, AltStatus, Assurance, Change, EvidenceKind, Hash, Intent, LandingRecord,
};
use serde::de::DeserializeOwned;
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
    /// The `intent/` refs, each with its version chain checked.
    pub intents: usize,
    /// The `alt/` refs, each with its version chain checked.
    pub alt_sets: usize,
}

/// What `check_view` found out about one view.
struct ViewCheck {
    ops: usize,
    conflicts: usize,
    included: Arc<Vec<Hash>>,
}

/// The stable ids of the coordination objects, which the links of changes must name.
struct CoordKeys {
    intents: BTreeSet<Hash>,
    alt_sets: BTreeSet<Hash>,
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
    /// rules included), recorded conflicts, undo chains, the dependency DAG, the closure of every
    /// view's included set, the version chains of intents and alternative sets, and the links that
    /// name them; the first failure found is a `StoreError::Decode`.
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
        let intents = self
            .check_chains::<Intent>(kinds::INTENT, INTENT_REF, "intent", |i| (i.stable, i.prev))?;
        let alt_sets =
            self.check_chains::<AltSet>(kinds::ALT_SET, ALT_REF, "alternative set", |s| {
                (s.stable, s.prev)
            })?;
        self.check_alt_sets(&alt_sets, &sets, &landings)?;
        let keys = CoordKeys {
            intents: intents.iter().map(|(k, _)| *k).collect(),
            alt_sets: alt_sets.iter().map(|(k, _)| *k).collect(),
        };
        self.check_dag(&landings, &sets, &keys)?;
        Ok(StoreCheck {
            views,
            view_ops,
            landed_changes,
            relayed,
            conflicts_recorded,
            intents: intents.len(),
            alt_sets: alt_sets.len(),
        })
    }

    /// Every ref under `prefix` names an object of `kind`, and walking `prev` from it reaches a
    /// first version (no `stable`, no `prev`) whose hash is the ref's key; every version on the way
    /// has `stable == Some(key)`. Returns (stable id, latest version) per ref, in key order.
    fn check_chains<T: DeserializeOwned>(
        &self,
        kind: u8,
        prefix: &str,
        what: &str,
        chain: impl Fn(&T) -> (Option<Hash>, Option<Hash>),
    ) -> Result<Vec<(Hash, T)>, StoreError> {
        let mut out = vec![];
        for (key, value) in self.refs_with_prefix(prefix)? {
            let stable = key_hash(&key, prefix, what)?;
            let latest = crate::index::hash_from(&value, &format!("{what} ref {stable}"))?;
            let read = |h: &Hash| -> Result<T, StoreError> {
                match self.get_typed(kind, h) {
                    Err(StoreError::NotFound(_)) => Err(bad(format!(
                        "{what} {stable}: version {h} is not a stored {what}"
                    ))),
                    r => r,
                }
            };
            let head = read(&latest)?;
            let mut cur = (latest, chain(&head));
            loop {
                let (h, (s, prev)) = cur;
                match (s, prev) {
                    (None, None) if h == stable => break,
                    (None, None) => {
                        return Err(bad(format!(
                            "{what} {stable}: its chain starts at {h}, not at its stable id"
                        )));
                    }
                    (Some(s), Some(p)) if s == stable => cur = (p, chain(&read(&p)?)),
                    _ => {
                        return Err(bad(format!(
                            "{what} {stable}: version {h} has stable {s:?} and prev {prev:?}"
                        )));
                    }
                }
            }
            out.push((stable, head));
        }
        Ok(out)
    }

    /// The latest version of each set names member views that exist, and a chosen set names a
    /// landing of canon that records the choice; every landing of canon that records a choice
    /// names a set whose latest version is chosen by that landing.
    fn check_alt_sets(
        &self,
        sets: &[(Hash, AltSet)],
        views: &[(String, ViewHead, Arc<Vec<Hash>>)],
        landings: &[(Hash, LandingRecord)],
    ) -> Result<(), StoreError> {
        let names: HashSet<&str> = views.iter().map(|(n, _, _)| n.as_str()).collect();
        let canon: HashMap<&Hash, &LandingRecord> = landings.iter().map(|(h, r)| (h, r)).collect();
        for (key, set) in sets {
            for m in &set.members {
                if !names.contains(m.view.as_str()) {
                    return Err(bad(format!(
                        "alternative set {key}: member view {} does not exist",
                        m.view
                    )));
                }
            }
            if let AltStatus::Chosen { member, landing } = &set.status {
                let want = AltChoice {
                    set: *key,
                    member: member.clone(),
                };
                match canon.get(landing) {
                    None => {
                        return Err(bad(format!(
                            "alternative set {key}: chosen by landing {landing}, which is not in canon's chain"
                        )));
                    }
                    Some(rec) if rec.chosen.as_ref() != Some(&want) => {
                        return Err(bad(format!(
                            "alternative set {key}: landing {landing} does not record choosing {member}"
                        )));
                    }
                    Some(_) => {}
                }
            }
        }
        let latest: HashMap<&Hash, &AltSet> = sets.iter().map(|(k, s)| (k, s)).collect();
        for (l, rec) in landings {
            let Some(c) = &rec.chosen else { continue };
            match latest.get(&c.set).map(|s| &s.status) {
                None => {
                    return Err(bad(format!(
                        "landing {l}: chooses from alternative set {}, which does not exist",
                        c.set
                    )));
                }
                Some(AltStatus::Chosen { landing, .. }) if landing == l => {}
                Some(_) => {
                    return Err(bad(format!(
                        "landing {l}: alternative set {} does not record this landing as its choice",
                        c.set
                    )));
                }
            }
        }
        Ok(())
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
    /// cycle; each view's included changes have their deps included or landed at or before the
    /// view's base; and each of those changes links only to intents and sets that exist.
    fn check_dag(
        &self,
        landings: &[(Hash, LandingRecord)],
        sets: &[(String, ViewHead, Arc<Vec<Hash>>)],
        keys: &CoordKeys,
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
            if let Some(h) = &ch.links.intent
                && !keys.intents.contains(h)
            {
                return Err(bad(format!("change {c}: intent {h} does not exist")));
            }
            if let Some(a) = &ch.links.alternative
                && !keys.alt_sets.contains(&a.set)
            {
                return Err(bad(format!(
                    "change {c}: alternative set {} does not exist",
                    a.set
                )));
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
