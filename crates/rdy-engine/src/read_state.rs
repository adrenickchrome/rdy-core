//! Published read states (snap1): what a reader holds is an immutable `ReadState`, built lazily
//! and shared. The writer publishes a new one after every write, before the write returns, and
//! seeds the cells it can fill in O(ops) (the written view after a commit, canon after a landing).
use crate::error::*;
use crate::paths::TreeNode;
use crate::version::VposCache;
use crate::{AS_OF_CACHE_ENTRIES, Materialized, Scope};
use rdy_graph::Graph;
use rdy_model::{Block, BlockId, Change, Hash, Op, Tag};
use rdy_schema::Pack;
use rdy_store::segment::Location;
use rdy_store::{Conflict, Merged, StoreReader, ViewHead};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

/// What a write changed, so that `Rdy::publish` re-reads only that and carries the rest over.
#[derive(Clone, Debug)]
pub enum Delta {
    /// Re-read everything; every cell starts empty.
    Full,
    /// `ops` were committed to `view`.
    Commit { view: String, ops: Vec<Op> },
    /// `view`'s proposal landed as `landing`, applying `ops` to canon.
    Landed {
        landing: Hash,
        view: String,
        ops: Vec<Op>,
    },
    /// A tag was written.
    Tags,
    /// Only the log moved (a proposal): no view content changed.
    Seq,
}

/// One scope's built state: its materialization, the merge's conflicts, and the derived
/// structures filled by whoever needs them first.
pub(crate) struct Built {
    pub(crate) m: Arc<Materialized>,
    pub(crate) conflicts: Arc<Vec<Conflict>>,
    pub(crate) graph: OnceLock<Arc<Graph>>,
    /// The full tree, memoised (filled by the tree reads).
    pub(crate) tree: OnceLock<Arc<Vec<TreeNode>>>,
}

impl Built {
    pub(crate) fn new(m: Arc<Materialized>, conflicts: Arc<Vec<Conflict>>) -> Built {
        Built {
            m,
            conflicts,
            graph: OnceLock::new(),
            tree: OnceLock::new(),
        }
    }
}

/// What one scope reads, captured when the state was published, and its build cell.
pub(crate) struct Entry {
    /// The view's name; None for canon and as-of entries.
    pub(crate) name: Option<String>,
    /// The landing the entry's changes apply to (None: the empty state).
    pub(crate) base: Option<Hash>,
    pub(crate) head: Option<ViewHead>,
    pub(crate) included: Arc<Vec<Hash>>,
    pub(crate) cell: OnceLock<Result<Arc<Built>, RdyError>>,
}

impl Entry {
    /// An unbuilt entry.
    pub(crate) fn new(
        name: Option<String>,
        base: Option<Hash>,
        head: Option<ViewHead>,
        included: Arc<Vec<Hash>>,
    ) -> Entry {
        Entry {
            name,
            base,
            head,
            included,
            cell: OnceLock::new(),
        }
    }

    /// The build, if one finished without an error.
    pub(crate) fn built_ok(&self) -> Option<&Arc<Built>> {
        self.cell.get().and_then(|r| r.as_ref().ok())
    }

    /// Pre-sets the build (a seed the writer computed in O(ops)).
    pub(crate) fn seed(&mut self, built: Built) {
        self.cell = OnceLock::from(Ok(Arc::new(built)));
    }
}

/// The as-of entries, shared by `Rdy` and every state it publishes: at most
/// `AS_OF_CACHE_ENTRIES`, least recently used at the front.
#[derive(Default)]
pub(crate) struct AsOfCache {
    pub(crate) lru: Mutex<VecDeque<(Hash, Arc<Entry>)>>,
}

impl AsOfCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<(Hash, Arc<Entry>)>> {
        self.lru.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The entry for landing `h`, marked most recently used; a new one evicts the least recently
    /// used beyond the cap. `h` must be a landing. The index read that checks it runs outside the
    /// lock, so readers of other landings never wait on it.
    fn touch(&self, reader: &StoreReader, h: &Hash) -> Result<Arc<Entry>, RdyError> {
        if let Some(entry) = Self::hit(&mut self.lock(), h) {
            return Ok(entry);
        }
        if !reader.has_kind(rdy_store::kinds::LANDING, h)? {
            return Err(RdyError::new(
                E_NOT_FOUND,
                format!("{} is not a landing", h.short()),
                "read as of a version (tag:<version>) or a landing in rdy log (landing:<id>)",
            ));
        }
        let mut lru = self.lock();
        // another reader may have added it while the lock was free
        if let Some(entry) = Self::hit(&mut lru, h) {
            return Ok(entry);
        }
        let entry = Arc::new(Entry::new(None, Some(*h), None, Arc::new(vec![])));
        lru.push_back((*h, Arc::clone(&entry)));
        while lru.len() > AS_OF_CACHE_ENTRIES {
            lru.pop_front();
        }
        Ok(entry)
    }

    /// The cached entry for `h`, moved to the most recently used end.
    fn hit(lru: &mut VecDeque<(Hash, Arc<Entry>)>, h: &Hash) -> Option<Arc<Entry>> {
        let at = lru.iter().position(|(x, _)| x == h)?;
        let hit = lru.remove(at).expect("found above");
        let entry = Arc::clone(&hit.1);
        lru.push_back(hit);
        Some(entry)
    }

    /// The entry for `h` if cached, without marking it used.
    fn peek(&self, h: &Hash) -> Option<Arc<Entry>> {
        self.lock()
            .iter()
            .find(|(x, _)| x == h)
            .map(|(_, e)| Arc::clone(e))
    }

    /// Drops `entry` (a failed build) so that a later read retries it: an as-of entry outlives
    /// every publish, so its cell must not keep an error.
    fn forget(&self, entry: &Arc<Entry>) {
        self.lock().retain(|(_, e)| !Arc::ptr_eq(e, entry));
    }
}

/// An immutable published state of the project: the heads, view sets and tags it was published
/// with, and lazily built materializations. A held state never follows a later write.
pub struct ReadState {
    pub(crate) seq: Location,
    pub(crate) canon_head: Option<Hash>,
    pub(crate) canon: Arc<Entry>,
    /// Exactly the views with a `view/` ref.
    pub(crate) views: imbl::HashMap<String, Arc<Entry>>,
    pub(crate) tags: Arc<Vec<(semver::Version, Tag)>>,
    pub(crate) pack: Arc<Pack>,
    pub(crate) reader: StoreReader,
    pub(crate) as_of: Arc<AsOfCache>,
    /// The version-position cache, shared like `as_of` (read by the state's version reads).
    pub(crate) vpos: Arc<Mutex<VposCache>>,
    /// A `Full` publish that failed to read the heads: every read of this state returns it.
    pub(crate) error: Option<RdyError>,
}

impl ReadState {
    /// The store's log end when this state was published.
    pub fn seq(&self) -> Location {
        self.seq
    }

    pub fn canon_head(&self) -> Option<Hash> {
        self.canon_head
    }

    pub fn pack(&self) -> &Pack {
        &self.pack
    }

    pub fn reader(&self) -> &StoreReader {
        &self.reader
    }

    /// The views with a `view/` ref, sorted.
    ///
    /// On a state that holds an error (every content read returns it) this answers from the
    /// state before the failed publish, as `has_view`, `view_head` and `included` do: a caller
    /// that reads only these four sees the earlier views and no error.
    pub fn view_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.views.keys().cloned().collect();
        names.sort();
        names
    }

    /// Whether `name` has a `view/` ref; on a state that holds an error, as of the state before
    /// the failed publish (see `view_names`).
    pub fn has_view(&self, name: &str) -> bool {
        self.views.contains_key(name)
    }

    /// View `name`'s head; on a state that holds an error, as of the state before the failed
    /// publish (see `view_names`).
    pub fn view_head(&self, name: &str) -> Option<&ViewHead> {
        self.views.get(name).and_then(|e| e.head.as_ref())
    }

    /// The changes view `name` includes (empty for a view with no ref); on a state that holds an
    /// error, as of the state before the failed publish (see `view_names`).
    pub fn included(&self, name: &str) -> Arc<Vec<Hash>> {
        self.views
            .get(name)
            .map(|e| Arc::clone(&e.included))
            .unwrap_or_default()
    }

    pub fn materialized(&self, scope: &Scope) -> Result<Arc<Materialized>, RdyError> {
        Ok(Arc::clone(&self.built(scope)?.m))
    }

    pub fn graph(&self, scope: &Scope) -> Result<Arc<Graph>, RdyError> {
        let built = self.built(scope)?;
        Ok(Arc::clone(built.graph.get_or_init(|| {
            Arc::new(Graph::build_from(built.m.iter()))
        })))
    }

    /// The conflicts the scope's merge recorded (always empty for canon and as-of scopes).
    pub fn conflicts(&self, scope: &Scope) -> Result<Arc<Vec<Conflict>>, RdyError> {
        Ok(Arc::clone(&self.built(scope)?.conflicts))
    }

    pub fn block(&self, scope: &Scope, id: &BlockId) -> Result<Block, RdyError> {
        self.materialized(scope)?.get(id).cloned().ok_or_else(|| {
            RdyError::new(
                E_UNKNOWN_BLOCK,
                format!("no block {id}"),
                "check the id with rdy_read",
            )
        })
    }

    /// Test hook: whether `scope`'s materialization and graph are built in this state. Never
    /// builds, and never marks an as-of scope used.
    #[doc(hidden)]
    pub fn is_built(&self, scope: &Scope) -> (bool, bool) {
        let entry = match scope {
            Scope::Canon => Some(Arc::clone(&self.canon)),
            Scope::View(v) => self.views.get(v).cloned(),
            Scope::At(h) => self.as_of.peek(h),
        };
        match entry.as_deref().and_then(Entry::built_ok) {
            Some(b) => (true, b.graph.get().is_some()),
            None => (false, false),
        }
    }

    /// The one lookup behind every read of a scope: its entry's build, built on first use. A view
    /// with no ref reads canon's entry; an as-of read marks its entry used in the shared cache.
    pub(crate) fn built(&self, scope: &Scope) -> Result<Arc<Built>, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        match scope {
            Scope::Canon => self.build(&self.canon),
            Scope::View(v) => match self.views.get(v) {
                Some(entry) => self.build(entry),
                None => self.build(&self.canon),
            },
            Scope::At(h) => {
                let entry = self.as_of.touch(&self.reader, h)?;
                let built = self.build(&entry);
                if built.is_err() {
                    self.as_of.forget(&entry);
                }
                built
            }
        }
    }

    fn build(&self, entry: &Entry) -> Result<Arc<Built>, RdyError> {
        entry
            .cell
            .get_or_init(|| {
                build_entry(
                    &self.reader,
                    &self.pack,
                    self.canon_for(entry).map(|b| &**b),
                    entry,
                    None,
                )
                .map(Arc::new)
            })
            .clone()
    }

    /// Canon's build when `entry` is a view on canon's head and canon is already built: what
    /// `build_entry` may seed the view from.
    pub(crate) fn canon_for(&self, entry: &Entry) -> Option<&Arc<Built>> {
        if entry.name.is_some() && entry.base == self.canon_head {
            self.canon.built_ok()
        } else {
            None
        }
    }
}

/// Builds `entry` from what it captured (never from the store's current heads). `canon` is
/// canon's build, given only when `entry` is a view whose base is canon's head and canon is
/// built (`ReadState::canon_for`); `merged` replaces the merge of the entry's changes.
pub(crate) fn build_entry(
    reader: &StoreReader,
    _pack: &Pack,
    canon: Option<&Built>,
    entry: &Entry,
    merged: Option<Merged>,
) -> Result<Built, RdyError> {
    let canon = canon.filter(|_| entry.name.is_some());
    if entry.included.is_empty() {
        if let Some(c) = canon {
            // a view with nothing included floats on canon: share its build
            return Ok(Built::new(Arc::clone(&c.m), Arc::new(vec![])));
        }
        let snap = reader.state_of(entry.base)?;
        let blocks = reader.blocks(&snap)?;
        return Ok(Built::new(
            Arc::new(Materialized::build(snap, blocks)),
            Arc::new(vec![]),
        ));
    }
    // the changes the entry captured, read by hash (content-addressed, so safe from any state)
    let (merged, changes) = match merged {
        Some(m) => (m, HashMap::new()),
        None => {
            let list = entry
                .included
                .iter()
                .map(|c| reader.change(c))
                .collect::<Result<Vec<Change>, _>>()?;
            let merged = reader.merge_onto(&reader.state_of(entry.base)?, &list)?;
            let by_id: HashMap<Hash, Change> = entry.included.iter().copied().zip(list).collect();
            (merged, by_id)
        }
    };
    let m = match canon {
        Some(c) if merged.conflicts.is_empty() => {
            let mut m: Option<Materialized> = None;
            for id in &merged.order {
                let read;
                let change = match changes.get(id) {
                    Some(c) => c,
                    None => {
                        read = reader.change(id)?;
                        &read
                    }
                };
                m = Some(m.as_ref().unwrap_or(&c.m).with(&change.ops));
            }
            m.map(Arc::new).unwrap_or_else(|| Arc::clone(&c.m))
        }
        _ => {
            let blocks = reader.blocks(&merged.state)?;
            Arc::new(Materialized::build(merged.state, blocks))
        }
    };
    Ok(Built::new(m, Arc::new(merged.conflicts)))
}
