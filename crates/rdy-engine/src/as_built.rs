//! As-built status (D-WJ0SJ, D-YAY1G): what a content block says about the release that ships
//! it, derived at the scope being read.
//!
//! The status of a content block comes from the first of these that applies: its own
//! `planned: true`; its own `since`; `planned: true` on its nearest section or doc; the status
//! its direct `decided_by` and `implements` decisions derive; unversioned. A decision derives
//! nothing from `none`, an absent `built`, a missing target or a redacted one. Any `planned`
//! target makes the derivation planned; else the latest version wins.
//!
//! [`Resolver`] is pure: it reads one [`Pack`] and one [`Materialized`], so one snapshot gives
//! one answer. It keeps what it worked out (the decisions it parsed and the parent walks it
//! made) only for its own life, never by a content hash: a `built` edit is a tracking edit and
//! leaves that hash unchanged.
//!
//! [`ReadState::as_built_warnings`] is the one warning function. Write, read, context and brief
//! call it.

use crate::error::W_AS_BUILT_OVERRIDE;
use crate::materialize::Materialized;
use crate::{RdyError, ReadState, Scope};
use rdy_graph::{EdgeKind, Graph};
use rdy_model::{Block, BlockId, FieldValue, decision, tree};
use rdy_schema::as_built::{
    BUILT_FIELD, Built, PLANNED_FIELD, SINCE_FIELD, is_content_type, parse_built, version_of,
};
use rdy_schema::{Issue, Pack, Severity};
use semver::Version;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};

/// The most as-built warnings one reply carries. The report arrays of a publication check have
/// the same cap.
pub const MAX_AS_BUILT_WARNINGS: usize = 200;

/// The fix of `W_AS_BUILT_OVERRIDE` when no open plan item asks about the override (D-YAY1G).
pub const AS_BUILT_FIX_NO_ITEM: &str =
    "Remove the override, or change it in a view and confirm its as-built plan item.";

/// The section type: its `planned: true` covers the blocks under it, as a doc's does.
const SECTION: &str = "section";

/// One status word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Planned,
    Since,
    Unversioned,
}

/// A status and its version: `since` holds the normalized version for `Status::Since` only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Derived {
    pub status: Status,
    pub since: Option<String>,
}

impl Derived {
    /// The status as source tools print it: `planned`, `since <v>` or `unversioned`.
    pub fn label(&self) -> String {
        label(self.status, self.since.as_deref())
    }
}

/// The override that set the effective status: the block that holds it and its field
/// (`planned` or `since`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Override {
    pub block: String,
    pub field: String,
}

/// A content block's as-built status: the effective one, the one its direct decisions derive,
/// and the winning override, own or inherited (null when none applies).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AsBuilt {
    pub status: Status,
    pub since: Option<String>,
    pub derived: Derived,
    #[serde(rename = "override")]
    pub owner: Option<Override>,
}

impl AsBuilt {
    /// The effective status as source tools print it: `planned`, `since <v>` or `unversioned`.
    pub fn label(&self) -> String {
        label(self.status, self.since.as_deref())
    }

    /// D-YAY1G: whether the override warns. It warns only when an override applies, the
    /// derivation is planned or since a version, and the effective status differs from it.
    pub fn mismatch(&self) -> bool {
        self.owner.is_some()
            && self.derived.status != Status::Unversioned
            && (self.status, self.since.as_deref())
                != (self.derived.status, self.derived.since.as_deref())
    }
}

fn label(status: Status, since: Option<&str>) -> String {
    match status {
        Status::Planned => "planned".to_string(),
        Status::Since => format!("since {}", since.unwrap_or_default()),
        Status::Unversioned => "unversioned".to_string(),
    }
}

/// The as-built status of the blocks of one state. Each call reads only the block, its direct
/// decision targets and its parent chain; the parses and walks are kept for the resolver's life.
pub struct Resolver<'a> {
    pack: &'a Pack,
    m: &'a Materialized,
    /// What each decision's `built` says; `None` for no build fact.
    built: RefCell<HashMap<BlockId, Option<Built>>>,
    /// The nearest section or doc at or above each walked block that holds `planned: true`.
    owner: RefCell<HashMap<BlockId, Option<BlockId>>>,
    /// Each status worked out, `None` for a block that is no content block.
    status: RefCell<HashMap<BlockId, Option<AsBuilt>>>,
}

/// `b`'s authored fields; `None` for a stub of removed content, whose fields are not read.
fn fields_of(b: &Block) -> Option<&std::collections::BTreeMap<String, FieldValue>> {
    (!b.is_redacted()).then_some(&b.fields)
}

fn own_planned(b: &Block) -> bool {
    fields_of(b).is_some_and(|f| f.get(PLANNED_FIELD) == Some(&FieldValue::Bool(true)))
}

fn own_since(b: &Block) -> Option<Version> {
    match fields_of(b)?.get(SINCE_FIELD)? {
        FieldValue::Str(text) => version_of(text),
        _ => None,
    }
}

impl<'a> Resolver<'a> {
    pub fn new(pack: &'a Pack, m: &'a Materialized) -> Resolver<'a> {
        Resolver {
            pack,
            m,
            built: RefCell::new(HashMap::new()),
            owner: RefCell::new(HashMap::new()),
            status: RefCell::new(HashMap::new()),
        }
    }

    /// The status of `id`; `None` when the state lacks it or it is no content block (a doc, a
    /// folder, a decision or another supporting block).
    pub fn status(&self, id: &BlockId) -> Option<AsBuilt> {
        if let Some(known) = self.status.borrow().get(id) {
            return known.clone();
        }
        let found = self.work_out(id);
        self.status.borrow_mut().insert(id.clone(), found.clone());
        found
    }

    fn work_out(&self, id: &BlockId) -> Option<AsBuilt> {
        let b = self.m.get(id)?;
        if !is_content_type(self.pack, &b.block_type) {
            return None;
        }
        let derived = self.derive(b);
        let own = |field: &str| {
            Some(Override {
                block: b.id.to_string(),
                field: field.to_string(),
            })
        };
        let (status, since, owner) = if own_planned(b) {
            (Status::Planned, None, own(PLANNED_FIELD))
        } else if let Some(v) = own_since(b) {
            (Status::Since, Some(v.to_string()), own(SINCE_FIELD))
        } else if let Some(holder) = self.planned_owner(b.parent.as_ref()) {
            let owner = Override {
                block: holder.to_string(),
                field: PLANNED_FIELD.to_string(),
            };
            (Status::Planned, None, Some(owner))
        } else {
            (derived.status, derived.since.clone(), None)
        };
        Some(AsBuilt {
            status,
            since,
            derived,
            owner,
        })
    }

    /// The status `b`'s direct `decided_by` and `implements` targets derive, each target once.
    fn derive(&self, b: &Block) -> Derived {
        let mut planned = false;
        let mut latest: Option<Version> = None;
        let mut seen = BTreeSet::new();
        for kind in [decision::DECIDED_BY, decision::IMPLEMENTS] {
            for link in b.links.get(kind).into_iter().flatten() {
                if !seen.insert(link.target.clone()) {
                    continue;
                }
                match self.built_of(&link.target) {
                    Some(Built::Planned) => planned = true,
                    Some(Built::Version(v)) => {
                        if latest.as_ref().is_none_or(|l| v > *l) {
                            latest = Some(v);
                        }
                    }
                    Some(Built::None) | None => {}
                }
            }
        }
        if planned {
            Derived {
                status: Status::Planned,
                since: None,
            }
        } else if let Some(v) = latest {
            Derived {
                status: Status::Since,
                since: Some(v.to_string()),
            }
        } else {
            Derived {
                status: Status::Unversioned,
                since: None,
            }
        }
    }

    /// What `target`'s `built` says, when it is a decision of this state whose content stays.
    fn built_of(&self, target: &BlockId) -> Option<Built> {
        if let Some(known) = self.built.borrow().get(target) {
            return known.clone();
        }
        let found = self
            .m
            .get(target)
            .filter(|t| t.block_type == decision::DECISION)
            .and_then(fields_of)
            .and_then(|f| match f.get(BUILT_FIELD)? {
                FieldValue::Str(text) => parse_built(text),
                _ => None,
            });
        self.built
            .borrow_mut()
            .insert(target.clone(), found.clone());
        found
    }

    /// The nearest section or doc at or above `start` that holds `planned: true`. The walk is
    /// iterative, keeps the ids it visits and stops at a block it saw before, so a damaged
    /// parent chain ends it.
    fn planned_owner(&self, start: Option<&BlockId>) -> Option<BlockId> {
        let mut path = vec![];
        let mut visited = HashSet::new();
        let mut cur = start.cloned();
        let mut found = None;
        while let Some(id) = cur {
            if let Some(known) = self.owner.borrow().get(&id) {
                found = known.clone();
                break;
            }
            if !visited.insert(id.clone()) {
                break;
            }
            let Some(b) = self.m.get(&id) else { break };
            let container = b.block_type == SECTION || b.block_type == tree::DOC;
            path.push(id.clone());
            if container && own_planned(b) {
                found = Some(id);
                break;
            }
            cur = b.parent.clone();
        }
        let mut memo = self.owner.borrow_mut();
        for id in path {
            memo.insert(id, found.clone());
        }
        found
    }
}

/// The blocks whose as-built status a write may move: each written block, each block that cites
/// a written decision through `decided_by` or `implements`, and each block under a written
/// section or doc. In id order.
pub(crate) fn affected(m: &Materialized, g: &Graph, written: &BTreeSet<BlockId>) -> Vec<BlockId> {
    let mut out = BTreeSet::new();
    for id in written {
        let Some(b) = m.get(id) else { continue };
        out.insert(id.clone());
        if b.block_type == decision::DECISION {
            for e in g.incoming(id) {
                if matches!(&e.kind, EdgeKind::Link(k) if k == decision::DECIDED_BY || k == decision::IMPLEMENTS)
                {
                    out.insert(e.from.clone());
                }
            }
        }
        if b.block_type == SECTION || b.block_type == tree::DOC {
            let mut todo = vec![id.clone()];
            let mut seen = HashSet::from([id.clone()]);
            while let Some(cur) = todo.pop() {
                for c in m.children(Some(&cur)) {
                    if seen.insert(c.id.clone()) {
                        out.insert(c.id.clone());
                        todo.push(c.id.clone());
                    }
                }
            }
        }
    }
    out.into_iter().collect()
}

impl ReadState {
    /// The as-built status of each of `ids` in `scope` that is a content block, in the order
    /// given.
    pub fn as_built<'i>(
        &self,
        scope: &Scope,
        ids: impl IntoIterator<Item = &'i BlockId>,
    ) -> Result<Vec<(BlockId, AsBuilt)>, RdyError> {
        let m = self.materialized(scope)?;
        let r = Resolver::new(self.pack(), &m);
        Ok(ids
            .into_iter()
            .filter_map(|id| r.status(id).map(|s| (id.clone(), s)))
            .collect())
    }

    /// The one as-built warning function (D-YAY1G): a `W_AS_BUILT_OVERRIDE` for each of `rows`
    /// whose override disagrees with its decisions, in the order given, at most
    /// [`MAX_AS_BUILT_WARNINGS`]. `rows` are statuses in `scope`.
    pub fn as_built_warnings(
        &self,
        _scope: &Scope,
        rows: &[(BlockId, AsBuilt)],
    ) -> Result<Vec<Issue>, RdyError> {
        Ok(rows
            .iter()
            .filter(|(_, s)| s.mismatch())
            .take(MAX_AS_BUILT_WARNINGS)
            .map(|(id, s)| Issue {
                severity: Severity::Warning,
                code: W_AS_BUILT_OVERRIDE,
                block: Some(id.clone()),
                message: format!(
                    "Block {id} has override status {}, but its decisions derive {}.",
                    s.label(),
                    s.derived.label()
                ),
                fix: AS_BUILT_FIX_NO_ITEM.to_string(),
            })
            .collect())
    }
}
