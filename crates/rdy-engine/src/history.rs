//! A block's history: the versions it has had, each with the change that wrote it, who wrote it,
//! why, and the landing that carried it into canon (hist1).
use crate::error::*;
use crate::{Materialized, ReadState, Scope};
use rdy_model::{
    ActorRun, BlockId, Change, EditorialClass, Hash, Justification, LandingRecord, Op, Provenance,
    Role,
};
use rdy_store::StoreReader;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// The most versions one history call returns.
pub const HISTORY_MAX: usize = 1000;

thread_local! {
    static CANON_STEP_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test hook: this thread's count of canon version steps read from the store (restatement
/// drift's canon form).
#[doc(hidden)]
pub fn canon_step_reads() -> u64 {
    CANON_STEP_READS.with(std::cell::Cell::get)
}

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
    Implements(Vec<String>),
    Editorial { class: String, reason: String },
}

impl JustificationInfo {
    /// `implements <id>, <id>` or `editorial <class>`.
    pub fn label(&self) -> String {
        match self {
            JustificationInfo::Implements(ids) => format!("implements {}", ids.join(", ")),
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
    /// The agent that relayed the approval, as `actor` or `actor/run` (the way `rdy log` names
    /// it); absent for an approval the person gave themselves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relayed_by: Option<String>,
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
    /// The importer, when `rdy import` wrote the change (FW-F7b).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_by: Option<ActorRun>,
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

pub(crate) fn role_str(r: Role) -> &'static str {
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
        Justification::Implements(ids) => {
            JustificationInfo::Implements(ids.iter().map(ToString::to_string).collect())
        }
        Justification::Editorial { class, reason } => JustificationInfo::Editorial {
            class: class_str(*class).into(),
            reason: reason.clone(),
        },
    }
}

/// One version of a canon block (restatement drift, FW-CH2): the version, the landing
/// (`at_ms`, landing id) that carried it (`None` when unknown), and the version before it
/// (`None` after a create).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionStep {
    pub version: Hash,
    pub landed_at: Option<(i64, String)>,
    pub base: Option<Hash>,
}

/// What one call has already decoded: changes by hash (and each one's ops by block), landing
/// refs by change and by landing hash, the view's included set, and the landing the scope reads
/// as of. Never kept across calls, so a landing shows on the first read after it.
struct Memo<'a> {
    reader: &'a StoreReader,
    included: Option<HashSet<Hash>>,
    head: Option<Hash>,
    changes: HashMap<Hash, Arc<Change>>,
    /// For each change read by [`Memo::step`], its op positions by block.
    ops: HashMap<Hash, HashMap<BlockId, Vec<usize>>>,
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
    fn new(state: &'a ReadState, scope: &Scope) -> Result<Self, RdyError> {
        let (included, head) = match scope {
            Scope::View(name) => (
                Some(state.included(name).iter().copied().collect()),
                state.effective_base(name),
            ),
            Scope::Canon => (None, state.canon_head()),
            Scope::At(h) => (None, Some(*h)),
            // the in-flight state is canon with every open view's changes
            Scope::Inflight => (
                Some(state.inflight_set()?.changes.iter().copied().collect()),
                state.canon_head(),
            ),
        };
        Ok(Memo {
            reader: state.reader(),
            included,
            head,
            changes: HashMap::new(),
            ops: HashMap::new(),
            landed: HashMap::new(),
            landings: HashMap::new(),
        })
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
            relayed_by: rec.relayed_by.as_ref().map(|r| match &r.run {
                Some(run) if run != crate::HUMAN_RUN => format!("{}/{run}", r.actor),
                _ => r.actor.clone(),
            }),
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
            imported_by: change.links.imported_by.clone(),
            message: change.message.clone(),
            justification: change.justification.as_ref().map(justification),
            landing,
        };
        Ok((entry, prev))
    }

    /// The (`at_ms`, landing id) of the landing that carried change `c`, when it landed.
    fn landed_at(&mut self, c: &Hash) -> Result<Option<(i64, String)>, RdyError> {
        Ok(self.landing(c)?.map(|l| (l.at_ms, l.id)))
    }

    /// The step for `version` of `id` and the change that wrote it there, never one in `used`
    /// (see [`Memo::writer_of`]); `None` when every writer is in `used`. The change's ops are
    /// indexed by block once, so a change that writes many blocks is scanned once.
    fn step(
        &mut self,
        version: &Hash,
        id: &BlockId,
        used: &HashSet<Hash>,
    ) -> Result<Option<(Hash, VersionStep)>, RdyError> {
        let Some(c) = self.writer_of(version, id, used)? else {
            return Ok(None);
        };
        let change = self.change(&c)?;
        let by_block = self.ops.entry(c).or_insert_with(|| {
            let mut m: HashMap<BlockId, Vec<usize>> = HashMap::new();
            for (i, op) in change.ops.iter().enumerate() {
                m.entry(op.block_id().clone()).or_default().push(i);
            }
            m
        });
        let base = by_block
            .get(id)
            .into_iter()
            .flatten()
            .rev()
            .find_map(|&i| match &change.ops[i] {
                Op::Create { block } if block.version_hash() == *version => Some(None),
                Op::Update { base, block } if block.version_hash() == *version => Some(Some(*base)),
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
        let landed_at = self.landed_at(&c)?;
        Ok(Some((
            c,
            VersionStep {
                version: *version,
                landed_at,
                base,
            },
        )))
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
        if *scope == Scope::Inflight {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "inflight is an analysis scope: blame reads canon, a view or a version",
                "read the history in canon, a view (view:<name>) or a version (tag:<version>)",
            ));
        }
        let version = version_in(&*self.materialized(scope)?, id)?;
        Memo::new(self, scope)?.walk(version, id, limit)
    }

    /// For each distinct id, the entry `history(scope, id, 1)` gives, sharing one memo.
    pub fn last_changes(
        &self,
        scope: &Scope,
        ids: &[BlockId],
    ) -> Result<BTreeMap<BlockId, HistoryEntry>, RdyError> {
        let m = self.materialized(scope)?;
        let mut memo = Memo::new(self, scope)?;
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

    /// Canon's history, read one version at a time with one memo (restatement drift, FW-CH2),
    /// through the version steps kept across canon builds ([`StepCache`]).
    pub(crate) fn canon_steps(&self) -> Result<CanonSteps<'_>, RdyError> {
        let head = self.canon_head();
        let kept = self.canon_step_cache.take_for(self.reader(), head);
        Ok(CanonSteps {
            memo: Memo::new(self, &Scope::Canon)?,
            m: self.materialized(&Scope::Canon)?,
            kept,
            cache: Arc::clone(&self.canon_step_cache),
            head,
        })
    }
}

/// The most version steps a [`StepCache`] keeps; past it the cache starts again empty.
const STEP_CACHE_MAX: usize = 1 << 18;

/// The most landings a [`StepCache`] walks back to catch up with a new canon head; past it the
/// cache starts again empty.
const STEP_CACHE_CATCH_UP: usize = 64;

/// One block version's step and the change that wrote it, shared without a copy.
pub(crate) type Step = Arc<(Hash, VersionStep)>;

/// The kept steps, by version (a version hash covers the block's id, so it names one block).
type Steps = HashMap<Hash, Step, std::hash::BuildHasherDefault<HashBits>>;

/// A hasher for keys that are already uniform hashes ([`Hash`]): their first eight bytes.
#[derive(Default)]
struct HashBits(u64);

impl std::hash::Hasher for HashBits {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // a `Hash` writes its 32 bytes once (after its length, which `write_usize` drops)
        for (i, b) in bytes.iter().take(8).enumerate() {
            self.0 ^= u64::from(*b) << (8 * i);
        }
    }

    fn write_usize(&mut self, _: usize) {}
}

/// Canon's version steps, kept across canon builds (restatement drift's canon form, final
/// review I2). A version's step (the change that wrote it, the landing that carried it, its
/// base) is the change chosen among the version's producers by [`Memo::writer_of`]: of those
/// landed at or before canon's head, the one landed last. History is append-only, so a later
/// landing changes the step only of a version one of its changes writes again (a revert, or a
/// change landed once more). The cache holds the steps for one canon head; a newer head drops
/// the steps of the versions the landings since then write, and any other head starts it again
/// empty. Only a step whose change has landed is kept. Shared by every state one handle
/// publishes.
#[derive(Debug, Default)]
pub(crate) struct StepCache(std::sync::Mutex<StepsAt>);

#[derive(Debug, Default)]
struct StepsAt {
    /// The canon head the steps hold for (`None`: none yet).
    head: Option<Option<Hash>>,
    steps: Steps,
}

impl StepCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, StepsAt> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Drops every kept step (a reindex).
    pub(crate) fn clear(&self) {
        *self.lock() = StepsAt::default();
    }

    /// The steps kept for canon head `head`, taken out (a concurrent build reads without them):
    /// caught up to `head` first when it is newer than the cache's head, else started again.
    fn take_for(&self, reader: &StoreReader, head: Option<Hash>) -> Steps {
        let mut at = self.lock();
        if at.head != Some(head) {
            let caught_up = match at.head {
                Some(from) => catch_up(reader, &mut at.steps, from, head),
                None => false,
            };
            if !caught_up {
                at.steps.clear();
            }
            at.head = Some(head);
        }
        std::mem::take(&mut at.steps)
    }

    /// Puts back `steps` read for canon head `head`, unless the cache moved to another head.
    fn put_back(&self, head: Option<Hash>, steps: Steps) {
        let mut at = self.lock();
        if at.head != Some(head) {
            return;
        }
        if at.steps.len() + steps.len() > STEP_CACHE_MAX {
            at.steps.clear();
            if steps.len() > STEP_CACHE_MAX {
                return;
            }
        }
        at.steps.extend(steps);
    }
}

/// Drops from `steps` every version the landings after `from` up to `to` write, walking back
/// from `to`; `false` when `from` is not reached within [`STEP_CACHE_CATCH_UP`] landings (or a
/// landing cannot be read), and the caller starts again.
fn catch_up(reader: &StoreReader, steps: &mut Steps, from: Option<Hash>, to: Option<Hash>) -> bool {
    let mut cur = to;
    let mut written: Vec<Hash> = vec![];
    for _ in 0..=STEP_CACHE_CATCH_UP {
        if cur == from {
            for k in &written {
                steps.remove(k);
            }
            return true;
        }
        let Some(h) = cur else { return false };
        let Ok(rec) = reader.landing(&h) else {
            return false;
        };
        for c in &rec.changes {
            let Ok(change) = reader.change(c) else {
                return false;
            };
            for op in &change.ops {
                if let Op::Create { block } | Op::Update { block, .. } = op {
                    written.push(block.version_hash());
                }
            }
        }
        cur = rec.prev;
    }
    false
}

/// Canon's history, read one version at a time (restatement drift, FW-CH2). One memo is shared
/// across calls, so a change that writes many blocks is decoded once; the steps kept across
/// canon builds are read first, and the steps read here go back to the cache when it is dropped.
pub(crate) struct CanonSteps<'a> {
    memo: Memo<'a>,
    m: Arc<Materialized>,
    kept: Steps,
    cache: Arc<StepCache>,
    head: Option<Hash>,
}

impl Drop for CanonSteps<'_> {
    fn drop(&mut self) {
        self.cache
            .put_back(self.head, std::mem::take(&mut self.kept));
    }
}

impl CanonSteps<'_> {
    /// The step for `id`'s canon version and the change that wrote it.
    pub(crate) fn current(&mut self, id: &BlockId) -> Result<Step, RdyError> {
        let version = version_in(&self.m, id)?;
        self.step(&version, id, &HashSet::new())?
            .ok_or_else(|| no_writer(&version, id))
    }

    /// The step for `version` of `id` and the change that wrote it, never one in `used`; `None`
    /// when every writer is in `used`. The step with no change left out is kept (when its change
    /// has landed); when its change is in `used` (a block written back to an earlier text), the
    /// next writer is read from the store.
    pub(crate) fn step(
        &mut self,
        version: &Hash,
        id: &BlockId,
        used: &HashSet<Hash>,
    ) -> Result<Option<Step>, RdyError> {
        let best = match self.kept.get(version) {
            Some(s) => Arc::clone(s),
            None => {
                CANON_STEP_READS.with(|c| c.set(c.get() + 1));
                let Some(s) = self.memo.step(version, id, &HashSet::new())? else {
                    return Ok(None);
                };
                let s = Arc::new(s);
                if s.1.landed_at.is_some() {
                    self.kept.insert(*version, Arc::clone(&s));
                }
                s
            }
        };
        if !used.contains(&best.0) {
            return Ok(Some(best));
        }
        CANON_STEP_READS.with(|c| c.set(c.get() + 1));
        Ok(self.memo.step(version, id, used)?.map(Arc::new))
    }

    /// The body of version `v`, read from the store.
    pub(crate) fn body(&self, v: &Hash) -> Result<String, RdyError> {
        Ok(self.memo.reader.get_block(v)?.body)
    }
}
