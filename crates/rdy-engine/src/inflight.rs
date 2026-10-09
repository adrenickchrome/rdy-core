//! infl1: work in flight on a briefing's blocks (FW-E1): the other open views whose included
//! changes write a covered block, the open proposals whose latest version does, and (ibrf1) the
//! open intents of other views on a covered block or its area. What is in flight is advice for the writer; it is not part of the briefing's token.
use crate::coord_graph::intent_label;
use crate::error::*;
use crate::gate::open_proposals;
use crate::intent::status_open_at;
use crate::session::{now_ms, rfc3339};
use crate::tensions::Tension;
use crate::tensions::info::TensionInfo;
use crate::views::OwnerInfo;
use crate::{ReadState, Scope};
use rdy_model::{BlockId, Hash, Op};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

/// How many proposal versions `ProposalTouched` holds before it starts again.
const PROPOSAL_TOUCHED_ENTRIES: usize = 1024;

/// The block ids each proposal version's changes write, by version hash. A version never
/// changes, so an entry never goes stale; shared by every published state, cleared when full.
pub(crate) type ProposalTouched = Arc<Mutex<HashMap<Hash, Arc<BTreeSet<BlockId>>>>>;

/// The work in flight on a briefing's covered blocks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Inflight {
    /// Other open views that change a covered block, by view name.
    pub views: Vec<InflightView>,
    /// Open proposals whose latest version changes a covered block, by ref.
    pub proposals: Vec<InflightProposal>,
    /// Open, unexpired intents of other views on a covered block or its area, by id.
    pub intents: Vec<InflightIntent>,
    /// Tensions that involve a covered block, open or dismissed, blocking first, then by kind,
    /// then by id: the event-derived set (all-canon tensions included) and the read-time
    /// overlaps. A cut briefing pages them like any list, after the Q15 rule (FW Q15, ruling
    /// on item 54): their sides go first (`tensions_detail_cut`), then advisory ones collapse
    /// into `tensions_collapsed`; a blocking one is never collapsed.
    pub tensions: Vec<TensionInfo>,
    /// The cut left out every listed tension's `sides`; `rdy_tensions {involving: <block>}`
    /// has them.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub tensions_detail_cut: bool,
    /// Advisory tensions this page leaves out, by kind (also in `omitted`); a later page lists
    /// them, or collapses them again.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tensions_collapsed: BTreeMap<String, usize>,
    /// Why the tensions could not be read (the rest of the briefing stands).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tensions_error: Option<String>,
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

/// An open intent that covers covered blocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InflightIntent {
    /// `in-<stable short>`.
    pub id: String,
    pub actor: String,
    pub run: Option<String>,
    pub view: String,
    pub angle: String,
    pub area: Option<String>,
    /// The covered blocks the intent covers (named, or in its area), in id order.
    pub blocks: Vec<String>,
    /// RFC 3339 UTC.
    pub expires_at: String,
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

    /// The block ids that proposal version `version` (whose changes are `changes`) writes,
    /// memoised in the cache every state shares.
    fn proposal_writes(
        &self,
        version: &Hash,
        changes: &[Hash],
    ) -> Result<Arc<BTreeSet<BlockId>>, RdyError> {
        let lock = || {
            self.proposal_touched
                .lock()
                .unwrap_or_else(|e| e.into_inner())
        };
        if let Some(hit) = lock().get(version) {
            return Ok(Arc::clone(hit));
        }
        let ids = Arc::new(self.ids_written(changes)?);
        let mut cache = lock();
        if cache.len() >= PROPOSAL_TOUCHED_ENTRIES {
            cache.clear();
        }
        cache.insert(*version, Arc::clone(&ids));
        Ok(ids)
    }

    /// Test hook: how many proposal versions' written blocks are memoised (the cache is shared
    /// by every state).
    #[doc(hidden)]
    pub fn proposal_touched_cache_len(&self) -> usize {
        self.proposal_touched
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// The work in flight on `ids`, briefed in `scope`: every open view other than the scope's
    /// own that includes changes writing one of them, and every open proposal whose latest
    /// version writes one of them.
    pub fn inflight(&self, scope: &Scope, ids: &BTreeSet<BlockId>) -> Result<Inflight, RdyError> {
        let own = match scope {
            Scope::View(v) => Some(v.as_str()),
            Scope::Canon | Scope::At(_) | Scope::Inflight => None,
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
            let touched = self.proposal_writes(&pref.version, &p.changes)?;
            let blocks = common(ids, &touched);
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
        let intents = self.inflight_intents(scope, own, ids)?;
        // the tensions never fail a briefing (a briefing gates writes): an error is reported
        let (tensions, tensions_error) = match self.inflight_tensions(ids) {
            Ok(t) => (t, None),
            Err(e) => (vec![], Some(format!("{}: {}", e.code, e.message))),
        };
        Ok(Inflight {
            views,
            proposals,
            intents,
            tensions,
            tensions_error,
            ..Inflight::default()
        })
    }

    /// The tensions that involve any of `ids`, open or dismissed: the event-derived set and the
    /// read-time overlaps as of now, each once, blocking first, then by kind, then by id.
    fn inflight_tensions(&self, ids: &BTreeSet<BlockId>) -> Result<Vec<TensionInfo>, RdyError> {
        let set = self.tensions()?;
        let mut found: BTreeMap<Hash, Arc<Tension>> = BTreeMap::new();
        for id in ids {
            for t in set.involving_block(id) {
                found.entry(t.id).or_insert(t);
            }
        }
        for t in self.intent_tensions(now_ms())? {
            if t.blocks().iter().any(|b| ids.contains(b)) {
                found.entry(t.id).or_insert_with(|| Arc::new(t));
            }
        }
        let mut found: Vec<Arc<Tension>> = found.into_values().collect();
        found.sort_by_key(|t| (t.severity, t.kind, t.id));
        Ok(found.iter().map(|t| self.tension_info(t)).collect())
    }

    /// The open intents (as of now) of views other than `own` that cover any of `ids` in
    /// `scope`, each with the ids it covers; by id.
    fn inflight_intents(
        &self,
        scope: &Scope,
        own: Option<&str>,
        ids: &BTreeSet<BlockId>,
    ) -> Result<Vec<InflightIntent>, RdyError> {
        let now = now_ms();
        let mut out = vec![];
        for (&stable, open) in &self.coord_heads.open_intents {
            let it = &open.intent;
            if !status_open_at(it, now) || own == Some(it.view.as_str()) {
                continue;
            }
            let mut blocks = vec![];
            for id in ids {
                if self.intent_covers(scope, it, id)? {
                    blocks.push(id.to_string());
                }
            }
            if blocks.is_empty() {
                continue;
            }
            out.push(InflightIntent {
                id: intent_label(&stable),
                actor: it.actor.clone(),
                run: it.run.clone(),
                view: it.view.clone(),
                angle: it.angle.clone(),
                area: it.area.clone(),
                blocks,
                expires_at: rfc3339(it.expires_at_ms),
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }
}
