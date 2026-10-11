//! Published read states (snap1): what a reader holds is an immutable `ReadState`, built lazily
//! and shared. The writer publishes a new one after every write, before the write returns, and
//! seeds the cells it can fill in O(ops) (the written view after a commit, canon after a landing).
use crate::coverage::{CoverageMemo, GraphCoverage, Structural};
use crate::error::*;
use crate::lexicon::Lexicon;
use crate::paths::TreeNode;
use crate::suggest::MentionScan;
use crate::tensions::Detector;
use crate::tensions::index::CanonIndex;
use crate::tensions::set::TensionCell;
use crate::version::VposCache;
use crate::{AS_OF_CACHE_ENTRIES, Materialized, Scope};
use rdy_graph::Graph;
use rdy_model::{AltSet, AltStatus, Block, BlockId, Change, Hash, Intent, IntentStatus, Op, Tag};
use rdy_schema::{AreaMatcher, Pack};
use rdy_store::segment::Location;
use rdy_store::{Conflict, Merged, StoreReader, ViewHead};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

/// What a write changed, so that `Rdy::publish` re-reads only that and carries the rest over.
#[derive(Clone, Debug)]
pub enum Delta {
    /// Re-read everything; every cell starts empty.
    Full,
    /// `ops` were committed to `view` as the change `change`.
    Commit {
        view: String,
        change: Hash,
        ops: Vec<Op>,
    },
    /// `view`'s proposal landed as `landing`, applying `ops` to canon; the same batch rebased
    /// the views in `rebased` (the proposing view included) and closed the views in `closed`
    /// (the other members of the alternative set the landing chose, altg1).
    Landed {
        landing: Hash,
        view: String,
        ops: Vec<Op>,
        rebased: Vec<String>,
        closed: Vec<String>,
    },
    /// View `view` was created, forked, or had view ops appended (its head and set re-read).
    View { view: String },
    /// Coordination objects (intents, alternative sets) were written, and the same batch closed
    /// the views in `closed` (each one's head and set re-read, as `View` does).
    Coord { closed: Vec<String> },
    /// A tag was written.
    Tags,
    /// Review `review` dismissed the tensions of vkey `vkey`: every cell is carried, and the
    /// tension set too (a dismissal is state on a still-computed tension, FW-CH7).
    Dismissed { vkey: Hash, review: Hash },
    /// Review `review` set the mention of `entity` in `block` to dismissed (`dismissed`) or back
    /// to open: every cell is carried, as `Dismissed` carries them (no view content changed).
    MentionReviewed {
        block: BlockId,
        entity: BlockId,
        review: Hash,
        dismissed: bool,
    },
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
    /// The entity lexicon (FW-EN1), memoised; carried across a write like the graph.
    pub(crate) lexicon: OnceLock<Arc<Lexicon>>,
    /// The whole scope's graph coverage (FW-EN4), memoised; never carried.
    pub(crate) coverage: OnceLock<Arc<GraphCoverage>>,
    /// The structural layer of coverage (FW-CH1), memoised; carried across a write with the
    /// graph and the lexicon (`coverage::carry`, cc11).
    pub(crate) structural: OnceLock<Arc<Structural>>,
    /// The mention scans briefings made (FW-EN3), most recent first; carried across a write.
    pub(crate) mentions: Mutex<Vec<MentionScan>>,
    /// Canon's tension index (names, texts, supersedes), memoised; never carried.
    pub(crate) tension_index: OnceLock<Arc<CanonIndex>>,
    /// The canon part of the tension set for the state's detectors (FW-CH6), memoised; never
    /// carried.
    pub(crate) tension_canon: OnceLock<Result<Arc<crate::tensions::set::CanonPart>, RdyError>>,
    /// Canon's store state (each block's version at canon's head), memoised for the gate's
    /// merges onto canon (a proposal and its landing read it on one canon); never carried.
    pub(crate) snapshot: OnceLock<Arc<rdy_model::StateSnapshot>>,
}

impl Built {
    pub(crate) fn new(m: Arc<Materialized>, conflicts: Arc<Vec<Conflict>>) -> Built {
        Built {
            m,
            conflicts,
            graph: OnceLock::new(),
            tree: OnceLock::new(),
            lexicon: OnceLock::new(),
            coverage: OnceLock::new(),
            structural: OnceLock::new(),
            mentions: Mutex::new(vec![]),
            tension_index: OnceLock::new(),
            tension_canon: OnceLock::new(),
            snapshot: OnceLock::new(),
        }
    }

    /// gcar1: this state with `ops` applied: the same conflicts and, only if this state's graph is
    /// built, that graph carried across the ops (their blocks' old edges out, their new ones in),
    /// which equals a rebuild. The lexicon is carried the same way when it is built (lex1), and
    /// so are the mention scans (`MentionScan::carried`). cc11: when the graph, the lexicon and
    /// the structural layer of coverage are all built, that layer is carried too
    /// (`coverage::carry`, under `pack`), unless the ops change entries its dirty set does not
    /// reach; then the next read rebuilds it. The tree, the graph coverage and the tension index
    /// and canon part are not carried.
    pub(crate) fn after(&self, ops: &[Op], pack: &Pack) -> Built {
        let m = Arc::new(self.m.with(ops));
        let next = Built::new(Arc::clone(&m), Arc::clone(&self.conflicts));
        {
            let scans = self.mentions.lock().unwrap_or_else(|e| e.into_inner());
            if !scans.is_empty() {
                *next.mentions.lock().unwrap_or_else(|e| e.into_inner()) =
                    MentionScan::carried(&scans, &self.m, ops);
            }
        }
        let (graph, lexicon) = (self.graph.get(), self.lexicon.get());
        if graph.is_none() && lexicon.is_none() {
            return next;
        }
        let ids: BTreeSet<&BlockId> = ops.iter().map(Op::block_id).collect();
        let old = || ids.iter().filter_map(|id| self.m.get(id));
        let new = || ids.iter().filter_map(|id| m.get(id));
        if let Some(g) = graph {
            let _ = next.graph.set(Arc::new(g.with_changes(old(), new())));
        }
        if let Some(l) = lexicon {
            let _ = next.lexicon.set(Arc::new(l.with_changes(old(), new())));
        }
        if let (Some(prev), Some(g), Some(lex)) =
            (self.structural.get(), next.graph.get(), next.lexicon.get())
            && let Some(s) = crate::coverage::carry(prev, &self.m, &m, g, lex, pack, ops)
        {
            let _ = next.structural.set(Arc::new(s));
        }
        next
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
    /// How many blocks the entry's merge records a conflict on, when counted without a build
    /// (`ReadState::view_list`); an entry carried over by a publish keeps it.
    pub(crate) conflict_count: OnceLock<usize>,
    /// The block ids the included changes write, memoised (`ReadState::view_touched`).
    pub(crate) touched: OnceLock<Arc<BTreeSet<BlockId>>>,
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
            conflict_count: OnceLock::new(),
            touched: OnceLock::new(),
        }
    }

    /// The build, if one finished without an error.
    pub(crate) fn built_ok(&self) -> Option<&Arc<Built>> {
        self.cell.get().and_then(|r| r.as_ref().ok())
    }

    /// Pre-sets the build (a seed the writer computed in O(ops)).
    pub(crate) fn seed(&mut self, built: Built) {
        self.seed_shared(Arc::new(built));
    }

    /// Pre-sets the build to one another entry holds (same base, same set).
    pub(crate) fn seed_shared(&mut self, built: Arc<Built>) {
        self.cell = OnceLock::from(Ok(built));
    }

    /// Whether this view entry floats on canon: a view (with a head) that is unpinned and
    /// includes nothing reads as canon, whatever its stored base.
    pub(crate) fn floats(&self) -> bool {
        self.head.as_ref().is_some_and(|h| !h.pinned) && self.included.is_empty()
    }
}

/// insc1: the changes the in-flight state merges onto canon: the distinct changes of every open
/// view that includes any (from a pinned view, those not yet landed), sorted by hash, and for
/// each the open views whose set holds it, by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InflightSet {
    pub changes: Vec<Hash>,
    pub views_of: BTreeMap<Hash, Vec<String>>,
}

/// An intent whose latest version is open (withdrawn ones are left out; an expired one stays,
/// since expiry is read-time): that version, decoded, and when the intent was first posted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenIntent {
    pub version: Hash,
    pub intent: Intent,
    /// The first version's `at_ms`.
    pub posted_at_ms: i64,
}

/// The coordination refs as a state was published: each intent's and each alternative set's
/// stable id mapped to its latest version, and indexes of the open intents and of the open and
/// the settled sets, so that a write reads its view's set and the intents on what it wrote, a
/// briefing the intents on what it covers, and the gate a member view's settled set, without
/// reading every intent or set ever made.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoordHeads {
    pub intents: BTreeMap<Hash, Hash>,
    pub alt_sets: BTreeMap<Hash, Hash>,
    /// Each intent whose latest version is open, by stable id. Readers filter expiry by `now`.
    pub open_intents: BTreeMap<Hash, OpenIntent>,
    /// Each open alternative set: stable id to its latest version and that version.
    pub open_alts: BTreeMap<Hash, (Hash, AltSet)>,
    /// Each member view of an open set, mapped to that set's stable id.
    pub alt_members: BTreeMap<String, Hash>,
    /// Each chosen or abandoned set: stable id to its latest version and that version.
    pub settled_alts: BTreeMap<Hash, (Hash, AltSet)>,
    /// Each member view of a settled set's latest version that the set did not choose (every
    /// member of an abandoned set, every other member of a chosen one), mapped to those sets'
    /// stable ids. A version of such a view that chose nothing can no longer land (altg1).
    pub lost_members: BTreeMap<String, Vec<Hash>>,
    /// Each open or chosen set's bindings, by stable id (msw20). An abandoned set has no entry.
    pub alt_binds: BTreeMap<Hash, Arc<AltBinds>>,
}

/// The bindings of one alternative set (msw20, FW-AL1): what each change is bound to, fixed when
/// it entered the set, and for a chosen set, the choice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AltBinds {
    /// Each bound change, mapped to the member view it is bound to.
    pub bound: BTreeMap<Hash, String>,
    /// The choice, when the set is chosen.
    pub chosen: Option<AltChosenBy>,
}

/// The member a chosen set chose, the landing that chose it, and the decisions that landing's
/// changes created (sorted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AltChosenBy {
    pub member: String,
    pub landing: Hash,
    pub decisions: Vec<BlockId>,
}

impl CoordHeads {
    /// The heads `intents` and `alt_sets` as the store holds them now, with the open-intent and
    /// the open-set indexes. An intent or a set whose latest version equals its version in `prev`
    /// keeps `prev`'s entry (a version is a content hash, so its status and fields are the same);
    /// only a new or moved one is read. A publish therefore reads the intents and sets it
    /// changed, not every one ever made.
    pub(crate) fn read(
        reader: &StoreReader,
        prev: &CoordHeads,
        intents: BTreeMap<Hash, Hash>,
        alt_sets: BTreeMap<Hash, Hash>,
    ) -> Result<CoordHeads, RdyError> {
        let mut open_intents = BTreeMap::new();
        for (stable, version) in &intents {
            if prev.intents.get(stable) == Some(version) {
                if let Some(entry) = prev.open_intents.get(stable) {
                    open_intents.insert(*stable, entry.clone());
                }
                continue;
            }
            let intent = reader.intent(version)?;
            if intent.status != IntentStatus::Open {
                continue;
            }
            // the first post's time: the version itself, the moved entry's, or the first version
            let posted_at_ms = if stable == version {
                intent.at_ms
            } else if let Some(entry) = prev.open_intents.get(stable) {
                entry.posted_at_ms
            } else {
                reader.intent(stable)?.at_ms
            };
            open_intents.insert(
                *stable,
                OpenIntent {
                    version: *version,
                    intent,
                    posted_at_ms,
                },
            );
        }
        let mut open_alts = BTreeMap::new();
        let mut settled_alts = BTreeMap::new();
        for (stable, version) in &alt_sets {
            if prev.alt_sets.get(stable) == Some(version) {
                if let Some(entry) = prev.open_alts.get(stable) {
                    open_alts.insert(*stable, entry.clone());
                } else if let Some(entry) = prev.settled_alts.get(stable) {
                    settled_alts.insert(*stable, entry.clone());
                }
                continue;
            }
            let set = reader.alt_set(version)?;
            if set.status == AltStatus::Open {
                open_alts.insert(*stable, (*version, set));
            } else {
                settled_alts.insert(*stable, (*version, set));
            }
        }
        let mut alt_members = BTreeMap::new();
        for (stable, (_, set)) in &open_alts {
            for m in &set.members {
                // a view is in at most one open set; were it in two, the first by id wins
                alt_members.entry(m.view.clone()).or_insert(*stable);
            }
        }
        // msw20: each open and each chosen set's bindings; history does not change, so a set
        // whose latest version is unchanged keeps `prev`'s entry
        let mut alt_binds = BTreeMap::new();
        for (stable, (version, set)) in open_alts.iter().chain(&settled_alts) {
            if prev.alt_sets.get(stable) == Some(version)
                && let Some(entry) = prev.alt_binds.get(stable)
            {
                alt_binds.insert(*stable, Arc::clone(entry));
                continue;
            }
            if let Some(b) = crate::alt::alt_binds(reader, version, set)? {
                alt_binds.insert(*stable, Arc::new(b));
            }
        }
        let mut lost_members: BTreeMap<String, Vec<Hash>> = BTreeMap::new();
        for (stable, (_, set)) in &settled_alts {
            let chosen = match &set.status {
                AltStatus::Chosen { member, .. } => Some(member.as_str()),
                _ => None,
            };
            for m in set
                .members
                .iter()
                .filter(|m| Some(m.view.as_str()) != chosen)
            {
                lost_members
                    .entry(m.view.clone())
                    .or_default()
                    .push(*stable);
            }
        }
        Ok(CoordHeads {
            intents,
            alt_sets,
            open_intents,
            open_alts,
            alt_members,
            settled_alts,
            lost_members,
            alt_binds,
        })
    }
}

/// For each block an in-flight change writes, the last such change in the merge's order.
pub(crate) type LastWriters = imbl::HashMap<BlockId, Hash>;

/// The in-flight scope's cell: its set, then the entry built from it (named `inflight`, on
/// canon's head), both computed on first read unless a publish carried them over. `last` is
/// filled with the build (or carried with it): a commit carries the build only when its change
/// merges after the last writer of every block it writes (see `Rdy::inflight_after_commit`).
#[derive(Default)]
pub(crate) struct InflightCell {
    pub(crate) set: OnceLock<Result<Arc<InflightSet>, RdyError>>,
    pub(crate) entry: OnceLock<Result<Arc<Entry>, RdyError>>,
    pub(crate) last: OnceLock<Arc<LastWriters>>,
}

impl InflightCell {
    /// A cell whose set is known and whose entry is still to build.
    pub(crate) fn with_set(set: Arc<InflightSet>) -> InflightCell {
        InflightCell {
            set: OnceLock::from(Ok(set)),
            entry: OnceLock::new(),
            last: OnceLock::new(),
        }
    }

    /// The set, if it was computed without an error.
    pub(crate) fn set_ok(&self) -> Option<&Arc<InflightSet>> {
        self.set.get().and_then(|r| r.as_ref().ok())
    }

    /// The entry's build, if one finished without an error.
    pub(crate) fn built_ok(&self) -> Option<&Arc<Built>> {
        self.entry
            .get()
            .and_then(|r| r.as_ref().ok())
            .and_then(|e| e.built_ok())
    }
}

/// The in-flight entry for `set` on canon's head `canon_head`, unbuilt.
pub(crate) fn inflight_entry(canon_head: Option<Hash>, set: &InflightSet) -> Entry {
    Entry::new(
        Some(crate::session::INFLIGHT_SCOPE.to_string()),
        canon_head,
        None,
        Arc::new(set.changes.clone()),
    )
}

/// The in-flight set of `views` (insc1): every open view with a non-empty set contributes its
/// changes, a pinned view only those no landing carried yet.
pub(crate) fn inflight_set_of(
    reader: &StoreReader,
    views: &imbl::HashMap<String, Arc<Entry>>,
) -> Result<InflightSet, RdyError> {
    let mut views_of: BTreeMap<Hash, Vec<String>> = BTreeMap::new();
    for (name, entry) in views {
        let Some(head) = entry.head.as_ref() else {
            continue;
        };
        if head.closed || entry.included.is_empty() {
            continue;
        }
        for c in entry.included.iter() {
            if head.pinned && reader.landing_of(c)?.is_some() {
                continue;
            }
            views_of.entry(*c).or_default().push(name.clone());
        }
    }
    for names in views_of.values_mut() {
        names.sort();
        names.dedup();
    }
    Ok(InflightSet {
        changes: views_of.keys().copied().collect(),
        views_of,
    })
}

/// Whether `ops`, in order, apply cleanly to `m`: each create names a block absent there, and
/// each update or delete names as its base the block's version there (after the earlier ops).
pub(crate) fn applies_cleanly(m: &Materialized, ops: &[Op]) -> bool {
    let mut now: HashMap<&BlockId, Option<Hash>> = HashMap::new();
    for op in ops {
        let id = op.block_id();
        let current = match now.get(id) {
            Some(v) => *v,
            None => m.version_of(id),
        };
        if current != op.base() {
            return false;
        }
        let next = match op {
            Op::Create { block } | Op::Update { block, .. } => Some(block.version_hash()),
            Op::Delete { .. } => None,
        };
        now.insert(id, next);
    }
    true
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
    /// The pack's areas, compiled once (arn1).
    pub(crate) areas: Arc<AreaMatcher>,
    /// The project directory.
    pub(crate) root: Arc<std::path::Path>,
    /// The site's name and description, parsed from `.rdy.yaml` by a full publish.
    pub(crate) name: Arc<str>,
    pub(crate) description: Option<Arc<str>>,
    /// `reviews.require_up_to_date` in `.rdy.yaml`, read by a full publish (FW-HB1).
    pub(crate) require_up_to_date: bool,
    /// `views.idle_days` in `.rdy.yaml` (default 14), read by a full publish.
    pub(crate) idle_days: u32,
    /// `decisions.home` in `.rdy.yaml`, read by a full publish (FW-PR2).
    pub(crate) decisions_home: Option<Arc<str>>,
    pub(crate) reader: StoreReader,
    pub(crate) as_of: Arc<AsOfCache>,
    /// The version-position cache, shared like `as_of` (read by the state's version reads).
    pub(crate) vpos: Arc<Mutex<VposCache>>,
    /// The blocks each open proposal version writes, shared like `vpos` (read by `inflight`).
    pub(crate) proposal_touched: crate::inflight::ProposalTouched,
    /// insc1: the in-flight scope's set and build.
    pub(crate) inflight_cell: Arc<InflightCell>,
    /// The coordination refs as this state was published.
    pub(crate) coord_heads: Arc<CoordHeads>,
    /// cgr1: the coordination graph, built on first read; never carried to the next state.
    pub(crate) coord: OnceLock<Result<Arc<crate::coord_graph::CoordIndex>, RdyError>>,
    /// The event-derived tension set, built on first read; a publish that changes nothing it
    /// reads keeps it.
    pub(crate) tensions: Arc<TensionCell>,
    /// Each dismissed tension's vkey, mapped to the review that dismissed it.
    pub(crate) dismissals: Arc<imbl::OrdMap<Hash, Hash>>,
    /// Each (block, entity) pair whose latest mention review dismisses it, mapped to that review
    /// (FW-CV4).
    pub(crate) mention_dismissals: Arc<imbl::OrdMap<(BlockId, BlockId), Hash>>,
    /// The coverage issues of the scopes read in this state (FW-CH1), memoised; never carried.
    pub(crate) coverage_memo: CoverageMemo,
    /// The tension detectors: the registry, unless a test set others.
    pub(crate) detectors: &'static [&'static dyn Detector],
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

    pub fn root(&self) -> &std::path::Path {
        &self.root
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

    /// Whether the project requires a proposal's base to be canon's head when it lands
    /// (`reviews.require_up_to_date`), as the config read when this state was published.
    pub fn require_up_to_date(&self) -> bool {
        self.require_up_to_date
    }

    /// Days without a write after which an open view is idle (`views.idle_days`), as the config
    /// read when this state was published.
    pub fn idle_days(&self) -> u32 {
        self.idle_days
    }

    /// The doc a decision given no parent is recorded in (`decisions.home`: a path, key or id),
    /// as the config read when this state was published.
    pub fn decisions_home(&self) -> Option<&str> {
        self.decisions_home.as_deref()
    }

    /// The landing view `name` reads on: canon's head for a view with no ref or one that floats
    /// (unpinned, nothing included), else the view's stored base.
    pub fn effective_base(&self, name: &str) -> Option<Hash> {
        match self.views.get(name) {
            Some(e) if !e.floats() => e.head.as_ref().and_then(|h| h.base),
            _ => self.canon_head,
        }
    }

    /// The conflict the scope's merge recorded on block `id`, if any.
    pub fn conflict_on(&self, scope: &Scope, id: &BlockId) -> Result<Option<Conflict>, RdyError> {
        Ok(self
            .conflicts(scope)?
            .iter()
            .find(|c| c.block == *id)
            .cloned())
    }

    pub fn materialized(&self, scope: &Scope) -> Result<Arc<Materialized>, RdyError> {
        Ok(Arc::clone(&self.built(scope)?.m))
    }

    pub fn graph(&self, scope: &Scope) -> Result<Arc<Graph>, RdyError> {
        let built = self.built(scope)?;
        Ok(Arc::clone(built.graph.get_or_init(|| {
            Arc::new(Graph::build_with(
                built.m.iter(),
                Arc::clone(self.reader.tracking()),
            ))
        })))
    }

    /// The scope's entity lexicon (FW-EN1), built with the scope and filled on first use.
    pub fn lexicon(&self, scope: &Scope) -> Result<Arc<Lexicon>, RdyError> {
        let built = self.built(scope)?;
        Ok(Arc::clone(built.lexicon.get_or_init(|| {
            Arc::new(Lexicon::build(&self.pack, built.m.iter()))
        })))
    }

    /// Whether `scope`'s lexicon is built in this state. Never builds, and never marks an as-of
    /// scope used.
    pub fn lexicon_built(&self, scope: &Scope) -> bool {
        self.peek_entry(scope)
            .as_deref()
            .and_then(Entry::built_ok)
            .is_some_and(|b| b.lexicon.get().is_some())
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

    /// insc1: the changes the in-flight scope merges onto canon, and the open views holding each.
    pub fn inflight_set(&self) -> Result<Arc<InflightSet>, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        self.inflight_cell
            .set
            .get_or_init(|| inflight_set_of(&self.reader, &self.views).map(Arc::new))
            .clone()
    }

    /// The coordination refs (intents and alternative sets, stable id to latest version) as
    /// this state was published.
    pub fn coord_heads(&self) -> &CoordHeads {
        &self.coord_heads
    }

    /// The in-flight entry, made from the set on first use.
    fn inflight_entry(&self) -> Result<Arc<Entry>, RdyError> {
        self.inflight_cell
            .entry
            .get_or_init(|| {
                let set = self.inflight_set()?;
                Ok(Arc::new(inflight_entry(self.canon_head, &set)))
            })
            .clone()
    }

    /// Test hook: whether `scope`'s materialization and graph are built in this state. Never
    /// builds, and never marks an as-of scope used.
    #[doc(hidden)]
    pub fn is_built(&self, scope: &Scope) -> (bool, bool) {
        match self.peek_entry(scope).as_deref().and_then(Entry::built_ok) {
            Some(b) => (true, b.graph.get().is_some()),
            None => (false, false),
        }
    }

    /// The entry `scope` reads in this state, if there is one. Never builds, and never marks an
    /// as-of scope used. A floating view reads canon's entry, as `built` does; the in-flight
    /// scope reads its cell's entry when one was made without an error.
    fn peek_entry(&self, scope: &Scope) -> Option<Arc<Entry>> {
        match scope {
            Scope::Canon => Some(Arc::clone(&self.canon)),
            Scope::View(v) => match self.views.get(v) {
                Some(e) if e.floats() => Some(Arc::clone(&self.canon)),
                e => e.cloned(),
            },
            Scope::At(h) => self.as_of.peek(h),
            Scope::Inflight => self
                .inflight_cell
                .entry
                .get()
                .and_then(|r| r.as_ref().ok())
                .cloned(),
        }
    }

    /// The one lookup behind every read of a scope: its entry's build, built on first use. A view
    /// with no ref, or one that floats (unpinned, nothing included), reads canon's entry; an
    /// as-of read marks its entry used in the shared cache.
    /// The store's state of canon's head, read once per canon build: what the gate merges a
    /// version's changes onto.
    pub(crate) fn canon_snapshot(&self) -> Result<Arc<rdy_model::StateSnapshot>, RdyError> {
        let built = self.built(&Scope::Canon)?;
        if let Some(s) = built.snapshot.get() {
            return Ok(Arc::clone(s));
        }
        let s = self.reader.state_of(self.canon_head)?;
        Ok(Arc::clone(built.snapshot.get_or_init(|| Arc::new(s))))
    }

    pub(crate) fn built(&self, scope: &Scope) -> Result<Arc<Built>, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        match scope {
            Scope::Canon => self.build(&self.canon),
            Scope::View(v) => match self.views.get(v) {
                Some(entry) if !entry.floats() => self.build(entry),
                _ => self.build(&self.canon),
            },
            Scope::At(h) => {
                let entry = self.as_of.touch(&self.reader, h)?;
                let built = self.build(&entry);
                if built.is_err() {
                    self.as_of.forget(&entry);
                }
                built
            }
            Scope::Inflight => {
                let entry = self.inflight_entry()?;
                entry
                    .cell
                    .get_or_init(|| self.build_inflight(&entry).map(Arc::new))
                    .clone()
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

    /// The in-flight entry's build (`build_entry`'s), recording the last writer of each block
    /// in the merge's order in the cell.
    fn build_inflight(&self, entry: &Entry) -> Result<Built, RdyError> {
        let canon = self.canon_for(entry).map(|b| &**b);
        if entry.included.is_empty() {
            let _ = self.inflight_cell.last.set(Arc::default());
            return build_entry(&self.reader, &self.pack, canon, entry, None);
        }
        let list = entry
            .included
            .iter()
            .map(|c| self.reader.change(c))
            .collect::<Result<Vec<Change>, _>>()?;
        let merged =
            self.reader
                .merge_onto(entry.base, &self.reader.state_of(entry.base)?, &list)?;
        let by_id: HashMap<Hash, Change> = entry.included.iter().copied().zip(list).collect();
        let mut last = LastWriters::new();
        for id in &merged.order {
            if let Some(c) = by_id.get(id) {
                for op in &c.ops {
                    last.insert(op.block_id().clone(), *id);
                }
            }
        }
        let built = build_merged(&self.reader, canon, entry, merged, &by_id)?;
        let _ = self.inflight_cell.last.set(Arc::new(last));
        Ok(built)
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
            let merged = reader.merge_onto(entry.base, &reader.state_of(entry.base)?, &list)?;
            let by_id: HashMap<Hash, Change> = entry.included.iter().copied().zip(list).collect();
            (merged, by_id)
        }
    };
    build_merged(reader, canon, entry, merged, &changes)
}

/// `build_entry` once the merge is known: seeded from canon's build when there is one and the
/// merge is clean, else built from the merged state. `changes` holds the changes already read
/// (any other is read by hash).
fn build_merged(
    reader: &StoreReader,
    canon: Option<&Built>,
    entry: &Entry,
    merged: Merged,
    changes: &HashMap<Hash, Change>,
) -> Result<Built, RdyError> {
    let canon = canon.filter(|_| entry.name.is_some());
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

impl ReadState {
    /// The name of the project as a site shows it, as parsed when this state was published.
    pub fn project_name(&self) -> String {
        self.name.to_string()
    }

    /// The config's `description`, when it had a non-empty one when this state was published.
    pub fn project_description(&self) -> Option<String> {
        self.description.as_deref().map(str::to_string)
    }
}
