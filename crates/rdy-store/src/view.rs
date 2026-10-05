//! View chains (chain1): each view is a chain of `VIEW_OP` objects, newest first through `prev`,
//! whose head the ref `view/<name>` names in a `ViewHead`. A view's included change set is read by
//! walking back from the head to the latest checkpoint op and applying the ops after it.
use crate::StoreReader;
use crate::error::StoreError;
use crate::kinds;
use rdy_model::Hash;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

/// Who a view belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub actor: String,
    pub run: Option<String>,
}

/// What one op does to its view. The variant order is postcard's discriminant: it is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewOpKind {
    /// A checkpoint: the view starts empty.
    Create {
        base: Option<Hash>,
        owner: Option<Owner>,
        pinned: bool,
    },
    /// A checkpoint: the view starts with `changes`, copied from view `from` at `from_head`.
    Fork {
        from: String,
        from_head: Hash,
        base: Option<Hash>,
        changes: Vec<Hash>,
    },
    Commit {
        change: Hash,
    },
    Include {
        changes: Vec<Hash>,
    },
    Exclude {
        changes: Vec<Hash>,
    },
    /// A checkpoint: after landing `landing`, the view holds `remaining`.
    Rebase {
        landing: Hash,
        dropped: Vec<Hash>,
        remaining: Vec<Hash>,
    },
    /// Declared for the format; its semantics come with a newer rdy, so the walk refuses it.
    Undo {
        target: Hash,
    },
    Close {
        reason: String,
    },
    Own {
        owner: Option<Owner>,
    },
}

/// One op of view `view`'s chain (kind `VIEW_OP`). Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewOp {
    pub view: String,
    pub prev: Option<Hash>,
    pub op: ViewOpKind,
    pub actor: String,
    pub run: Option<String>,
    pub at_ms: i64,
}

/// The value of the ref `view/<name>`. `len` is the size of the included set after `head`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewHead {
    pub head: Hash,
    pub base: Option<Hash>,
    pub len: u64,
    pub owner: Option<Owner>,
    pub pinned: bool,
    pub closed: bool,
    pub last_write_ms: i64,
}

fn broken(name: &str, at: &Hash, why: impl std::fmt::Display) -> StoreError {
    StoreError::Decode(format!("view chain of {name:?} is broken at {at}: {why}"))
}

impl StoreReader {
    /// The head of view `name`'s chain, or None if the view has no ref.
    pub fn view_head(&self, name: &str) -> Result<Option<ViewHead>, StoreError> {
        self.get_ref(&format!("view/{name}"))?
            .map(|bytes| {
                postcard::from_bytes(&bytes).map_err(|e| {
                    StoreError::Decode(format!("view {name:?}: its ref is not a view head: {e}"))
                })
            })
            .transpose()
    }

    /// The view op with id `h`.
    pub fn view_op(&self, h: &Hash) -> Result<ViewOp, StoreError> {
        self.get_typed(kinds::VIEW_OP, h)
    }

    /// The ids of every op of view `name`'s chain that the index holds, in ascending hash order.
    pub fn view_op_ids(&self, name: &str) -> Result<Vec<Hash>, StoreError> {
        self.index().view_ops(name)
    }

    /// The landing that carried `change` into canon, if one has (lidx1).
    pub fn landing_of(&self, change: &Hash) -> Result<Option<Hash>, StoreError> {
        self.index().landing_of(change)
    }

    /// The changes view `name` includes, in order: the latest checkpoint's set with the ops after
    /// it applied. The set of the head last read is cached, and a later read walks back only to
    /// that head or to a checkpoint, whichever it meets first. A view with no ref is empty.
    pub fn included(&self, name: &str) -> Result<Arc<Vec<Hash>>, StoreError> {
        match self.view_head(name)? {
            Some(head) => self.included_at(name, &head),
            None => Ok(Arc::new(vec![])),
        }
    }

    /// The changes view `name` includes at `head`, a head the caller has read. Ops are
    /// content-addressed, so the answer depends on `head` alone, whatever the writer has done
    /// since: a read that needs a view's head and its set together reads the head once and passes
    /// it here, never `included`, which would read the head again.
    pub fn included_at(&self, name: &str, head: &ViewHead) -> Result<Arc<Vec<Hash>>, StoreError> {
        let cached = self.cached_set(name);
        if let Some((h, set)) = &cached
            && *h == head.head
        {
            return Ok(Arc::clone(set));
        }
        // newest first: the ops after the checkpoint (or the cached head), with their ids
        let mut pending: Vec<(Hash, ViewOpKind)> = vec![];
        let mut cur = head.head;
        let mut set: Vec<Hash> = loop {
            if let Some((h, set)) = &cached
                && *h == cur
            {
                break set.as_ref().clone();
            }
            let op = match self.view_op(&cur) {
                Ok(op) => op,
                Err(StoreError::NotFound(_)) => {
                    return Err(broken(name, &cur, "the op is missing"));
                }
                Err(e) => return Err(e),
            };
            if op.view != name {
                return Err(broken(
                    name,
                    &cur,
                    format!("the op belongs to view {:?}", op.view),
                ));
            }
            match op.op {
                ViewOpKind::Create { .. } => break vec![],
                ViewOpKind::Fork { changes, .. } => break changes,
                ViewOpKind::Rebase { remaining, .. } => break remaining,
                kind => {
                    pending.push((cur, kind));
                    match op.prev {
                        Some(prev) => cur = prev,
                        None => return Err(broken(name, &cur, "it ends without a checkpoint")),
                    }
                }
            }
        };
        let mut members: HashSet<Hash> = set.iter().copied().collect();
        for (id, kind) in pending.into_iter().rev() {
            match kind {
                ViewOpKind::Commit { change } => {
                    if members.insert(change) {
                        set.push(change);
                    }
                }
                ViewOpKind::Include { changes } => {
                    for c in changes {
                        if members.insert(c) {
                            set.push(c);
                        }
                    }
                }
                ViewOpKind::Exclude { changes } => {
                    let gone: HashSet<Hash> = changes.into_iter().collect();
                    set.retain(|c| !gone.contains(c));
                    members.retain(|c| !gone.contains(c));
                }
                ViewOpKind::Close { .. } | ViewOpKind::Own { .. } => {}
                ViewOpKind::Undo { .. } => {
                    return Err(broken(name, &id, "an undo op needs a newer rdy"));
                }
                ViewOpKind::Create { .. } | ViewOpKind::Fork { .. } | ViewOpKind::Rebase { .. } => {
                    unreachable!("a checkpoint ends the walk")
                }
            }
        }
        let set = Arc::new(set);
        self.cache_set(name, head.head, Arc::clone(&set));
        Ok(set)
    }
}
