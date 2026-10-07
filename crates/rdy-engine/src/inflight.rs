//! infl1: work in flight on a briefing's blocks (FW-E1): the other open views whose included
//! changes write a covered block, and the open proposals whose latest version does. What is in
//! flight is advice for the writer; it is not part of the briefing's token.
use crate::error::*;
use crate::gate::open_proposals;
use crate::session::rfc3339;
use crate::views::OwnerInfo;
use crate::{ReadState, Scope};
use rdy_model::{BlockId, Hash, Op};
use serde::Serialize;
use std::collections::BTreeSet;
use std::sync::Arc;

/// The work in flight on a briefing's covered blocks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Inflight {
    /// Other open views that change a covered block, by view name.
    pub views: Vec<InflightView>,
    /// Open proposals whose latest version changes a covered block, by ref.
    pub proposals: Vec<InflightProposal>,
}

/// An open view that changes covered blocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InflightView {
    pub view: String,
    pub owner: Option<OwnerInfo>,
    /// The view's last write, RFC 3339 UTC.
    pub last_write: String,
    pub last_write_ms: i64,
    /// The covered blocks the view's included changes write, in id order.
    pub blocks: Vec<String>,
}

/// An open proposal (at its latest version) that changes covered blocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InflightProposal {
    /// `P-<stable short>@<version>`.
    pub r#ref: String,
    pub view: String,
    pub title: String,
    /// The covered blocks the version's changes write, in id order.
    pub blocks: Vec<String>,
}

/// The ids in both sets, in id order, walking the smaller set.
fn common(ids: &BTreeSet<BlockId>, touched: &BTreeSet<BlockId>) -> Vec<String> {
    let (small, large) = if touched.len() < ids.len() {
        (touched, ids)
    } else {
        (ids, touched)
    };
    small
        .iter()
        .filter(|id| large.contains(*id))
        .map(ToString::to_string)
        .collect()
}

impl ReadState {
    /// The block ids that the ops of `changes` write.
    fn ids_written(&self, changes: &[Hash]) -> Result<BTreeSet<BlockId>, RdyError> {
        let mut ids = BTreeSet::new();
        for h in changes {
            let change = self.reader.change(h)?;
            ids.extend(change.ops.iter().map(Op::block_id).cloned());
        }
        Ok(ids)
    }

    /// The block ids that view `name`'s included changes write (empty for a view with no ref),
    /// memoised in its entry: a publish that changes the view makes a new entry.
    pub(crate) fn view_touched(&self, name: &str) -> Result<Arc<BTreeSet<BlockId>>, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let Some(entry) = self.views.get(name) else {
            return Ok(Arc::default());
        };
        if let Some(t) = entry.touched.get() {
            return Ok(Arc::clone(t));
        }
        let ids = Arc::new(self.ids_written(&entry.included)?);
        Ok(Arc::clone(entry.touched.get_or_init(|| ids)))
    }

    /// The work in flight on `ids`, briefed in `scope`: every open view other than the scope's
    /// own that includes changes writing one of them, and every open proposal whose latest
    /// version writes one of them.
    pub fn inflight(&self, scope: &Scope, ids: &BTreeSet<BlockId>) -> Result<Inflight, RdyError> {
        let own = match scope {
            Scope::View(v) => Some(v.as_str()),
            Scope::Canon | Scope::At(_) => None,
        };
        let mut views = vec![];
        for name in self.view_names() {
            if own == Some(name.as_str()) {
                continue;
            }
            let Some(head) = self.view_head(&name) else {
                continue;
            };
            if head.closed || self.included(&name).is_empty() {
                continue;
            }
            let touched = self.view_touched(&name)?;
            let blocks = common(ids, &touched);
            if blocks.is_empty() {
                continue;
            }
            views.push(InflightView {
                owner: head.owner.as_ref().map(OwnerInfo::from),
                last_write: rfc3339(head.last_write_ms),
                last_write_ms: head.last_write_ms,
                view: name,
                blocks,
            });
        }
        let mut proposals = vec![];
        for (pref, p) in open_proposals(self)? {
            let blocks = common(ids, &self.ids_written(&p.changes)?);
            if blocks.is_empty() {
                continue;
            }
            proposals.push(InflightProposal {
                r#ref: pref.to_string(),
                view: p.view,
                title: p.title,
                blocks,
            });
        }
        Ok(Inflight { views, proposals })
    }
}
