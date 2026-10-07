//! A block's history: the versions it has had, each with the change that wrote it, who wrote it,
//! why, and the landing that carried it into canon (hist1).
use crate::error::*;
use crate::{Materialized, ReadState, Scope};
use rdy_model::{
    BlockId, Change, EditorialClass, Hash, Justification, LandingRecord, Op, Provenance, Role,
};
use rdy_store::StoreReader;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// The most versions one history call returns.
pub const HISTORY_MAX: usize = 1000;

/// Who wrote a version, as the change's provenance records it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Author {
    pub actor: String,
    pub run: Option<String>,
    /// `reader`, `writer`, `reviewer` or `approver`.
    pub role: String,
    pub at_ms: i64,
}

/// Why a change was made: the decision it implements, or an editorial class and reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JustificationInfo {
    Implements(String),
    Editorial { class: String, reason: String },
}

impl JustificationInfo {
    /// `implements <id>` or `editorial <class>`.
    pub fn label(&self) -> String {
        match self {
            JustificationInfo::Implements(id) => format!("implements {id}"),
            JustificationInfo::Editorial { class, .. } => format!("editorial {class}"),
        }
    }
}

/// The landing that carried a change into canon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LandingRef {
    /// The landing's full hash.
    pub id: String,
    pub approved_by: String,
    pub at_ms: i64,
    pub assurance: String,
}

/// One version of a block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HistoryEntry {
    /// The version's full hash.
    pub version: String,
    /// `create` or `update`.
    pub op: String,
    /// The change that wrote it (full hash).
    pub change: String,
    pub author: Author,
    pub message: String,
    pub justification: Option<JustificationInfo>,
    pub landing: Option<LandingRef>,
}

/// A block's versions, newest first. `more` is true when older versions were left out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct History {
    pub entries: Vec<HistoryEntry>,
    pub more: bool,
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::Reader => "reader",
        Role::Writer => "writer",
        Role::Reviewer => "reviewer",
        Role::Approver => "approver",
    }
}

fn class_str(c: EditorialClass) -> &'static str {
    match c {
        EditorialClass::Typo => "typo",
        EditorialClass::Restructure => "restructure",
        EditorialClass::Clarify => "clarify",
    }
}

fn author(p: &Provenance) -> Author {
    Author {
        actor: p.actor.clone(),
        run: p.run.clone(),
        role: role_str(p.role).into(),
        at_ms: p.at_ms,
    }
}

fn justification(j: &Justification) -> JustificationInfo {
    match j {
        Justification::Implements(id) => JustificationInfo::Implements(id.to_string()),
        Justification::Editorial { class, reason } => JustificationInfo::Editorial {
            class: class_str(*class).into(),
            reason: reason.clone(),
        },
    }
}

/// What one call has already decoded: changes by hash, landing refs by change and by landing
/// hash, the view's included set, and the landing the scope reads as of. Never kept across
/// calls, so a landing shows on the first read after it.
struct Memo<'a> {
    reader: &'a StoreReader,
    included: Option<HashSet<Hash>>,
    head: Option<Hash>,
    changes: HashMap<Hash, Arc<Change>>,
    landed: HashMap<Hash, Option<Hash>>,
    landings: HashMap<Hash, LandingRef>,
}

fn no_writer(version: &Hash, id: &BlockId) -> RdyError {
    RdyError::new(
        E_IO,
        format!("no change wrote version {} of {id}", version.short()),
        "run rdy fsck; rdy reindex rebuilds the producer index",
    )
}

impl<'a> Memo<'a> {
    fn new(state: &'a ReadState, scope: &Scope) -> Self {
        let (included, head) = match scope {
            Scope::View(name) => (
                Some(state.included(name).iter().copied().collect()),
                state.effective_base(name),
            ),
            Scope::Canon => (None, state.canon_head()),
            Scope::At(h) => (None, Some(*h)),
        };
        Memo {
            reader: state.reader(),
            included,
            head,
            changes: HashMap::new(),
            landed: HashMap::new(),
            landings: HashMap::new(),
        }
    }

    /// The change that wrote `version` in this scope, never one in `used` (ruling 4): of the
    /// version's other producers, the one the view includes that was written last; else the one
    /// landed last at or before the scope's landing; else the one written first. `None` when every producer is
    /// in `used`; a version with no producer at all is index damage (`E_IO`).
    fn writer_of(
        &self,
        version: &Hash,
        id: &BlockId,
        used: &HashSet<Hash>,
    ) -> Result<Option<Hash>, RdyError> {
        let all = self.reader.producers(version)?;
        if all.is_empty() {
            return Err(no_writer(version, id));
        }
        let free: Vec<Hash> = all.into_iter().filter(|c| !used.contains(c)).collect();
        if let Some(set) = &self.included {
            let own: Vec<Hash> = free.iter().copied().filter(|c| set.contains(c)).collect();
            if !own.is_empty() {
                return Ok(self.reader.written_last(&own)?);
            }
        }
        Ok(self.reader.producer_at(&free, self.head.as_ref())?)
    }

    fn change(&mut self, c: &Hash) -> Result<Arc<Change>, RdyError> {
        if let Some(ch) = self.changes.get(c) {
            return Ok(Arc::clone(ch));
        }
        let ch = Arc::new(self.reader.change(c)?);
        self.changes.insert(*c, Arc::clone(&ch));
        Ok(ch)
    }

    fn landing(&mut self, c: &Hash) -> Result<Option<LandingRef>, RdyError> {
        let h = match self.landed.get(c) {
            Some(h) => *h,
            None => {
                let h = self.reader.landing_of(c)?;
                self.landed.insert(*c, h);
                h
            }
        };
        let Some(h) = h else { return Ok(None) };
        if let Some(l) = self.landings.get(&h) {
            return Ok(Some(l.clone()));
        }
        let rec: LandingRecord = self.reader.landing(&h)?;
        let l = LandingRef {
            id: h.to_string(),
            approved_by: rec.approved_by.actor.clone(),
            at_ms: rec.approved_by.at_ms,
            assurance: rec.assurance.as_str().into(),
        };
        self.landings.insert(h, l.clone());
        Ok(Some(l))
    }

    /// The entry change `c` gives for `version` of `id`, and the version before it (None after a
    /// create).
    fn entry(
        &mut self,
        c: Hash,
        version: &Hash,
        id: &BlockId,
    ) -> Result<(HistoryEntry, Option<Hash>), RdyError> {
        let change = self.change(&c)?;
        let (op, prev) = change
            .ops
            .iter()
            .rev()
            .find_map(|op| match op {
                Op::Create { block } if block.id == *id && block.version_hash() == *version => {
                    Some(("create", None))
                }
                Op::Update { base, block }
                    if block.id == *id && block.version_hash() == *version =>
                {
                    Some(("update", Some(*base)))
                }
                _ => None,
            })
            .ok_or_else(|| {
                RdyError::new(
                    E_IO,
                    format!(
                        "change {} does not write version {} of {id}",
                        c.short(),
                        version.short()
                    ),
                    "run rdy fsck",
                )
            })?;
        let landing = self.landing(&c)?;
        let entry = HistoryEntry {
            version: version.to_string(),
            op: op.into(),
            change: c.to_string(),
            author: author(&change.provenance),
            message: change.message.clone(),
            justification: change.justification.as_ref().map(justification),
            landing,
        };
        Ok((entry, prev))
    }

    /// The newest entry for `version` of `id`.
    fn last(&mut self, version: &Hash, id: &BlockId) -> Result<HistoryEntry, RdyError> {
        let c = self
            .writer_of(version, id, &HashSet::new())?
            .ok_or_else(|| no_writer(version, id))?;
        Ok(self.entry(c, version, id)?.0)
    }

    /// Follows each update's base from `start`, never taking a change twice: a block written back
    /// to an earlier text has that text's version again, and the walk goes on past it to the
    /// create instead of cycling. A version whose producers are all taken ends the walk.
    fn walk(&mut self, start: Hash, id: &BlockId, limit: usize) -> Result<History, RdyError> {
        let mut entries = Vec::new();
        let mut used = HashSet::new();
        let mut version = start;
        let mut c = self
            .writer_of(&version, id, &used)?
            .ok_or_else(|| no_writer(&version, id))?;
        loop {
            used.insert(c);
            let (entry, prev) = self.entry(c, &version, id)?;
            entries.push(entry);
            let next = match prev {
                Some(base) => self.writer_of(&base, id, &used)?.map(|n| (base, n)),
                None => None,
            };
            match next {
                None => {
                    return Ok(History {
                        entries,
                        more: false,
                    });
                }
                Some(_) if entries.len() == limit => {
                    return Ok(History {
                        entries,
                        more: true,
                    });
                }
                Some((base, n)) => (version, c) = (base, n),
            }
        }
    }
}

/// The version `scope` holds of `id`, from the materialization's version map (no clone, no hash).
fn version_in(m: &Materialized, id: &BlockId) -> Result<Hash, RdyError> {
    m.version_of(id).ok_or_else(|| {
        RdyError::new(
            E_UNKNOWN_BLOCK,
            format!("no block {id}"),
            "check the id with rdy_read",
        )
    })
}

impl ReadState {
    /// `id`'s versions in `scope`, newest first, at most `limit` (1..=`HISTORY_MAX`). The walk
    /// follows each update's base and ends at the block's create.
    pub fn history(&self, scope: &Scope, id: &BlockId, limit: usize) -> Result<History, RdyError> {
        if !(1..=HISTORY_MAX).contains(&limit) {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("history limit {limit} is outside 1..=1000"),
                "ask for 1 to 1000 versions",
            ));
        }
        let version = version_in(&*self.materialized(scope)?, id)?;
        Memo::new(self, scope).walk(version, id, limit)
    }

    /// For each distinct id, the entry `history(scope, id, 1)` gives, sharing one memo.
    pub fn last_changes(
        &self,
        scope: &Scope,
        ids: &[BlockId],
    ) -> Result<BTreeMap<BlockId, HistoryEntry>, RdyError> {
        let m = self.materialized(scope)?;
        let mut memo = Memo::new(self, scope);
        let mut out = BTreeMap::new();
        for id in ids {
            if out.contains_key(id) {
                continue;
            }
            let version = version_in(&m, id)?;
            out.insert(id.clone(), memo.last(&version, id)?);
        }
        Ok(out)
    }
}
