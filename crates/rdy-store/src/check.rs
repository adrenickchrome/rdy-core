//! Structural checks of the typed objects (fsck2): view chains and heads, the view-op index, and
//! canon's landings against the change -> landing index.
use crate::Store;
use crate::error::StoreError;
use crate::kinds;
use crate::view::{ViewHead, ViewOpKind};
use rdy_model::Hash;
use std::collections::BTreeSet;

/// What `Store::check_objects` counted in a store that checked clean.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreCheck {
    pub views: usize,
    /// The ops of every view's chain, walked in full.
    pub view_ops: usize,
    /// The sum of `changes` over canon's landings.
    pub landed_changes: usize,
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

impl Store {
    /// Checks the view chains, their heads and index entries, and canon's landings; the first
    /// failure found is a `StoreError::Decode`. Landing fields that a later rdy interprets
    /// (assurance, relay, approved version, rebased-from) are not looked at.
    pub fn check_objects(&self) -> Result<StoreCheck, StoreError> {
        let mut views = 0;
        let mut view_ops = 0;
        for (key, bytes) in self.refs_with_prefix("view/")? {
            let name = &key["view/".len()..];
            let head: ViewHead = postcard::from_bytes(&bytes)
                .map_err(|_| bad(format!("view {name:?}: its ref is not a view head")))?;
            view_ops += self.check_view(name, &head)?;
            views += 1;
        }
        let mut landed_changes = 0;
        for (l, rec) in self.landings()? {
            for c in &rec.changes {
                if !self.has_kind(kinds::CHANGE, c)? {
                    return Err(bad(format!("landing {l}: change {c} is missing")));
                }
                match self.landing_of(c)? {
                    Some(o) if o == l => {}
                    other => {
                        return Err(bad(format!(
                            "landing {l}: the index has {other:?} as the landing of change {c}; run rdy reindex"
                        )));
                    }
                }
            }
            landed_changes += rec.changes.len();
        }
        Ok(StoreCheck {
            views,
            view_ops,
            landed_changes,
        })
    }

    /// Checks one view; returns the number of ops on its chain.
    fn check_view(&self, name: &str, head: &ViewHead) -> Result<usize, StoreError> {
        let mut chain: BTreeSet<Hash> = BTreeSet::new();
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
        let included = self.included(name)?;
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
        if let Some(h) = indexed.difference(&chain).next() {
            return Err(bad(format!(
                "view {name:?}: op {h} is indexed but not on its chain"
            )));
        }
        if let Some(h) = chain.difference(&indexed).next() {
            return Err(bad(format!(
                "view {name:?}: op {h} is on its chain but not indexed; run rdy reindex"
            )));
        }
        Ok(chain.len())
    }
}
