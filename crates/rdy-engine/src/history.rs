//! A block's history: the versions it has had, each with the change that wrote it, who wrote it,
//! why, and the landing that carried it into canon (hist1).
use crate::error::*;
use crate::{ReadState, Scope};
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

/// What one call has already decoded: changes and landing refs by hash, and the view's included
/// set. Never kept across calls, so a landing shows on the first read after it.
struct Memo<'a> {
    reader: &'a StoreReader,
    included: Option<HashSet<Hash>>,
    changes: HashMap<Hash, Arc<Change>>,
    landings: HashMap<Hash, Option<LandingRef>>,
}

impl<'a> Memo<'a> {
    fn new(state: &'a ReadState, scope: &Scope) -> Self {
        let included = match scope {
            Scope::View(name) => Some(state.included(name).iter().copied().collect()),
            _ => None,
        };
        Memo {
            reader: state.reader(),
            included,
            changes: HashMap::new(),
            landings: HashMap::new(),
        }
    }

    /// The change that wrote `version` of `id` in this scope (ruling 4).
    fn writer_of(&self, version: &Hash, id: &BlockId) -> Result<Hash, RdyError> {
        let mut found = None;
        if let Some(set) = &self.included {
            found = self
                .reader
                .producers(version)?
                .into_iter()
                .find(|c| set.contains(c));
        }
        if found.is_none() {
            found = self.reader.producer(version)?;
        }
        found.ok_or_else(|| {
            RdyError::new(
                E_IO,
                format!("no change wrote version {} of {id}", version.short()),
                "run rdy fsck; rdy reindex rebuilds the producer index",
            )
        })
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
        if let Some(l) = self.landings.get(c) {
            return Ok(l.clone());
        }
        let l = match self.reader.landing_of(c)? {
            Some(h) => {
                let rec: LandingRecord = self.reader.landing(&h)?;
                Some(LandingRef {
                    id: h.to_string(),
                    approved_by: rec.approved_by.actor.clone(),
                    at_ms: rec.approved_by.at_ms,
                    assurance: rec.assurance.as_str().into(),
                })
            }
            None => None,
        };
        self.landings.insert(*c, l.clone());
        Ok(l)
    }

    /// The entry for `version` of `id`, and the version before it (None after a create).
    fn entry(
        &mut self,
        version: &Hash,
        id: &BlockId,
    ) -> Result<(HistoryEntry, Option<Hash>), RdyError> {
        let c = self.writer_of(version, id)?;
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

    fn walk(&mut self, start: Hash, id: &BlockId, limit: usize) -> Result<History, RdyError> {
        let mut entries = Vec::new();
        let mut version = start;
        loop {
            let (entry, prev) = self.entry(&version, id)?;
            entries.push(entry);
            match prev {
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
                Some(base) => version = base,
            }
        }
    }
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
        let version = self.block(scope, id)?.version_hash();
        Memo::new(self, scope).walk(version, id, limit)
    }

    /// For each distinct id, the entry `history(scope, id, 1)` gives, sharing one memo.
    pub fn last_changes(
        &self,
        scope: &Scope,
        ids: &[BlockId],
    ) -> Result<BTreeMap<BlockId, HistoryEntry>, RdyError> {
        let mut memo = Memo::new(self, scope);
        let mut out = BTreeMap::new();
        for id in ids {
            if out.contains_key(id) {
                continue;
            }
            let version = self.block(scope, id)?.version_hash();
            out.insert(id.clone(), memo.entry(&version, id)?.0);
        }
        Ok(out)
    }
}
