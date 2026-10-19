//! The store handle and its captured read snapshot.

use rdy_engine::{
    DumpSummary, E_INTERNAL, E_NOT_FOUND, Engine, FsckReport, Local, Rdy, RdyError, ReadState,
    parse_version,
};
use rdy_model::{Hash, RepoId, Tag};
use rdy_store::meta::Origin;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// Who a store is (D-DAGNB, D-C8C70): its repo id and, for a clone, where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub repo_id: RepoId,
    pub origin: Option<Origin>,
}

/// One open project store. It owns the project's `Local` (so the existing exclusive store lock,
/// sealing and recovery apply unchanged) and is not `Clone`.
pub struct StoreHandle {
    local: Local,
}

impl StoreHandle {
    /// Opens the project at `project`. A project that is open elsewhere is `E_LOCKED`; every
    /// other open error passes through unchanged.
    pub fn open(project: &Path) -> Result<Self, RdyError> {
        Ok(StoreHandle {
            local: Local::open(project)?,
        })
    }

    /// Creates the holder at `root` for `repo_id` (D-BPYN8) and holds it open: one durable batch
    /// with the store format, the repo id and the holder mode, and nothing else on disk (no
    /// config, pack directory or settings). A directory that already holds a store is refused
    /// and left as it was.
    pub fn create_holder(root: &Path, repo_id: RepoId) -> Result<Self, RdyError> {
        Ok(StoreHandle {
            local: Local::new(Rdy::create_holder(root, repo_id)?),
        })
    }

    /// Opens the holder at `root` through the existing exclusive lock. A project store there is
    /// refused (`this store is a project`); no config, pack or settings file is read.
    pub fn open_holder(root: &Path) -> Result<Self, RdyError> {
        Ok(StoreHandle {
            local: Local::new(Rdy::open_holder(root)?),
        })
    }

    /// Writes the store's current published state as a dump to `out`, through the lock this
    /// handle already holds: the store is not opened a second time. A project's dump carries
    /// its project files; a holder's carries none. `out` must not exist.
    pub fn dump(&self, out: &Path) -> Result<DumpSummary, RdyError> {
        // one captured state: the dump ends where that state ends, whatever is written meanwhile
        self.local.read().dump(out)
    }

    /// Checks the store with the engine's own `fsck`, by mode (a holder checks each tag's own
    /// history). It reads under the lock and emits no batch.
    pub fn fsck(&self) -> Result<FsckReport, RdyError> {
        self.local.inspect(|rdy| rdy.fsck())
    }

    /// A read snapshot: the published state, the identity, the canon head and every `tag/*` ref,
    /// all taken together under the store's lock, so no write falls between them.
    pub fn snapshot(&self) -> Result<Snapshot, RdyError> {
        self.local.inspect(|rdy| {
            let state = rdy.read();
            let reader = state.reader();
            let Some(repo_id) = reader.repo_id()? else {
                // a store with no repo id fails the meta check with its own text
                reader.check_meta()?;
                return Err(RdyError::new(
                    E_INTERNAL,
                    "the store has no repo id",
                    "report this as a bug in rdy",
                ));
            };
            let identity = Identity {
                repo_id,
                origin: reader.origin()?,
            };
            let tags = reader
                .tags()?
                .into_iter()
                .map(|(name, hash, tag)| (name, (hash, tag)))
                .collect();
            let canon = state.canon_head();
            Ok(Snapshot {
                state,
                identity,
                canon,
                tags,
            })
        })
    }

    /// The engine handle, so a sibling module's tests can write through it.
    #[cfg(test)]
    pub(crate) fn local(&self) -> &Local {
        &self.local
    }

    /// Closes the store cleanly, which releases its lock.
    pub fn close(self) -> Result<(), RdyError> {
        self.local.close()
    }
}

/// A read snapshot. Its tag refs are the ones captured with it: a later write never changes
/// what it answers (the reader's own refs are live, so no method here reads them).
pub struct Snapshot {
    state: Arc<ReadState>,
    identity: Identity,
    canon: Option<Hash>,
    /// Tag name (as stored) to the tag's hash and the tag, in name order.
    tags: BTreeMap<String, (Hash, Tag)>,
}

impl Snapshot {
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// The canon head when the snapshot was taken; None before the first landing.
    pub fn canon(&self) -> Option<Hash> {
        self.canon
    }

    /// The captured tags as (tag hash, tag), oldest version first (Semantic Versioning
    /// precedence, then name). A stored name that is not a version is its own error.
    pub fn tags(&self) -> Result<Vec<(Hash, Tag)>, RdyError> {
        let mut rows = Vec::with_capacity(self.tags.len());
        for (name, (hash, tag)) in &self.tags {
            rows.push((parse_version(name)?, *hash, tag.clone()));
        }
        rows.sort_by(|a, b| {
            a.0.cmp_precedence(&b.0)
                .then_with(|| a.2.name.cmp(&b.2.name))
        });
        Ok(rows.into_iter().map(|(_, hash, tag)| (hash, tag)).collect())
    }

    // The three accessors below are the crate's visibility contract: sibling modules read a
    // snapshot through them and never through its fields. Their callers arrive with those
    // modules, so a build without tests does not see a use yet.

    /// The published state the snapshot holds.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn read_state(&self) -> &ReadState {
        &self.state
    }

    /// The captured tags by stored name.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn captured_tags(&self) -> &BTreeMap<String, (Hash, Tag)> {
        &self.tags
    }

    /// The captured tag that names `name`'s version, as `tag:<version>` reads it: `1.2.0` finds
    /// `v1.2.0`. `E_NOT_FOUND` when the snapshot has none.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn captured_tag(&self, name: &str) -> Result<(Hash, Tag), RdyError> {
        let want = parse_version(name)?;
        for (stored, (hash, tag)) in &self.tags {
            if parse_version(stored)?.cmp_precedence(&want) == Ordering::Equal {
                return Ok((*hash, tag.clone()));
            }
        }
        Err(RdyError::new(
            E_NOT_FOUND,
            format!("no version {name}"),
            "list versions with rdy tags",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_engine::{Engine, Rdy, Session, SessionKind, WriteRequest};
    use rdy_model::Role;
    use serde_json::json;

    fn session(role: Role) -> Session {
        Session {
            actor: if role == Role::Approver {
                "human:founder".into()
            } else {
                "agent:a".into()
            },
            run: None,
            role,
            kind: SessionKind::Human,
        }
    }

    /// A project with one landing, closed again.
    fn landed() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        r.write(
            &session(Role::Writer),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "fields": {"title": "T", "doc_type": "page"}}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
        let p = r
            .propose(&session(Role::Writer), "work", "T", None)
            .unwrap();
        r.land(&session(Role::Approver), &p.id).unwrap();
        r.close().unwrap();
        dir
    }

    /// Adds the tag `name` through the handle's own `Local`.
    fn tag(handle: &StoreHandle, name: &'static str) {
        handle
            .local
            .write(Box::new(move |r| {
                r.tag(&session(Role::Approver), name, None, None)?;
                Ok(serde_json::Value::Null)
            }))
            .unwrap();
    }

    #[test]
    fn ha3_snapshot_refs_do_not_follow_later_writes() {
        let dir = landed();
        let handle = StoreHandle::open(dir.path()).unwrap();
        let before = handle.snapshot().unwrap();
        assert!(before.canon().is_some());
        assert!(before.tags().unwrap().is_empty());
        tag(&handle, "v0.1.0");
        // the older snapshot keeps its refs, though the reader's own refs are live
        assert!(before.tags().unwrap().is_empty());
        assert!(before.captured_tags().is_empty());
        assert_eq!(
            before.captured_tag("0.1.0").unwrap_err().code,
            "E_NOT_FOUND"
        );
        assert_eq!(before.read_state().reader().tags().unwrap().len(), 1);
        // a new snapshot sees it, and `0.1.0` finds `v0.1.0`
        let after = handle.snapshot().unwrap();
        let tags = after.tags().unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].1.name, "v0.1.0");
        assert_eq!(after.captured_tag("0.1.0").unwrap(), tags[0].clone());
        assert_eq!(after.identity(), before.identity());
        handle.close().unwrap();
    }

    #[test]
    fn ha3_tags_are_listed_by_version_precedence() {
        let dir = landed();
        let handle = StoreHandle::open(dir.path()).unwrap();
        // a tag must be above the newest, so they are made in order; name order differs
        for name in ["v0.2.0", "v0.10.0-rc.1", "v0.10.0"] {
            tag(&handle, name);
        }
        let names: Vec<String> = handle
            .snapshot()
            .unwrap()
            .tags()
            .unwrap()
            .into_iter()
            .map(|(_, t)| t.name)
            .collect();
        assert_eq!(names, ["v0.2.0", "v0.10.0-rc.1", "v0.10.0"]);
        handle.close().unwrap();
    }
}
