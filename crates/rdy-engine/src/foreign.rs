//! Foreign state beside canon (repo links, D-FK58A, D-8VJAZ). A read state holds, next to canon
//! and the views, what the foreign log delivered: each source's current selection, the current
//! view of each cited repo, and a lookup of every delivered version a pin may hold.
//!
//! - Three things stay apart: canon (the project's own blocks), the foreign current selection
//!   (what live sources deliver now) and the held pin versions (every version a delivery ever
//!   carried). A delivered block enters none of canon's tree, key index, graph, lexicon,
//!   coverage or mention scans, because it is never a `Materialized` block.
//! - A delivery moves no canon head. A state read holds one foreign cut: the sources as they
//!   stood at its watermark, the log end the state was published at. A later delivery shows in
//!   the next state only, and the held lookup of this state ignores entries past its watermark.
//! - A publish re-reads the sources whose newest entry moved and carries the rest. A project
//!   without the `foreign_copy` feature has no foreign state and pays nothing.
use crate::{Rdy, RdyError, ReadState};
use rdy_model::foreign::{ForeignEvent, ForeignId, ForeignPin};
use rdy_model::{Block, BlockId, Hash, RepoId};
use rdy_store::foreign::{HeldVersion, SourceView};
use rdy_store::format::FEATURE_FOREIGN_COPY;
use rdy_store::segment::Location;
use rdy_store::{StoreReader, kinds};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

/// What entries before a watermark delivered: the (repo, block, version) triples, and per
/// (repo, block, content, content encoding) the version that carried it.
#[derive(Debug, Default)]
struct HeldIndex {
    versions: BTreeSet<(RepoId, BlockId, Hash)>,
    contents: BTreeMap<(RepoId, BlockId, Hash, u16), Hash>,
}

/// The foreign side of one read state.
#[derive(Debug)]
pub struct ForeignState {
    /// The log end of the state this cut was read for.
    watermark: Location,
    /// Each source's folded view, by its ref name (`foreign/link/<id>`, `foreign/adr/<name>`).
    sources: BTreeMap<String, Arc<SourceView>>,
    /// Per cited repo and block, the newest version in log order across the repo's live sources.
    current: BTreeMap<RepoId, BTreeMap<BlockId, HeldVersion>>,
    /// Every delivered version, built on first use of a held lookup.
    held: OnceLock<Result<Arc<HeldIndex>, RdyError>>,
}

impl Default for ForeignState {
    fn default() -> ForeignState {
        ForeignState::empty(Location {
            segment: 0,
            offset: 0,
        })
    }
}

impl ForeignState {
    /// No source, at `watermark`.
    pub(crate) fn empty(watermark: Location) -> ForeignState {
        ForeignState {
            watermark,
            sources: BTreeMap::new(),
            current: BTreeMap::new(),
            held: OnceLock::new(),
        }
    }

    /// The state of every source `reader` holds, at `watermark`. A source whose newest entry is
    /// the one `prev` folded keeps `prev`'s view.
    pub(crate) fn read(
        reader: &StoreReader,
        watermark: Location,
        prev: Option<&ForeignState>,
    ) -> Result<ForeignState, RdyError> {
        Self::from_heads(reader, reader.foreign_sources()?, watermark, prev)
    }

    fn from_heads(
        reader: &StoreReader,
        heads: Vec<(String, Hash)>,
        watermark: Location,
        prev: Option<&ForeignState>,
    ) -> Result<ForeignState, RdyError> {
        let mut sources = BTreeMap::new();
        for (name, head) in heads {
            let kept = prev
                .and_then(|p| p.sources.get(&name))
                .filter(|v| v.head == head);
            let view = match kept {
                Some(v) => Arc::clone(v),
                None => match reader.source_view(&name)? {
                    Some(v) => Arc::new(v),
                    None => continue,
                },
            };
            sources.insert(name, view);
        }
        let mut current: BTreeMap<RepoId, BTreeMap<BlockId, HeldVersion>> = BTreeMap::new();
        for view in sources.values().filter(|v| !v.revoked) {
            let repo = current.entry(view.repo).or_default();
            for (id, held) in &view.members {
                match repo.get(id) {
                    Some(have) if have.at >= held.at => {}
                    _ => {
                        repo.insert(id.clone(), held.clone());
                    }
                }
            }
        }
        Ok(ForeignState {
            watermark,
            sources,
            current,
            held: OnceLock::new(),
        })
    }

    /// The state after a publish at `watermark`: `prev` itself when no source's newest entry
    /// moved, else a new cut that re-reads only the sources that did.
    pub(crate) fn after(
        prev: &Arc<ForeignState>,
        reader: &StoreReader,
        watermark: Location,
    ) -> Result<Arc<ForeignState>, RdyError> {
        let heads = reader.foreign_sources()?;
        let same = heads.len() == prev.sources.len()
            && heads
                .iter()
                .all(|(n, h)| prev.sources.get(n).is_some_and(|v| v.head == *h));
        if same {
            return Ok(Arc::clone(prev));
        }
        Self::from_heads(reader, heads, watermark, Some(prev)).map(Arc::new)
    }

    /// The log end of the state this cut belongs to.
    pub fn watermark(&self) -> Location {
        self.watermark
    }

    /// Whether no source delivered anything.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Every source's ref name and view, in name order.
    pub fn sources(&self) -> impl Iterator<Item = (&str, &SourceView)> + '_ {
        self.sources.iter().map(|(n, v)| (n.as_str(), &**v))
    }

    /// The view of the source whose ref is `name`.
    pub fn source(&self, name: &str) -> Option<&SourceView> {
        self.sources.get(name).map(|v| &**v)
    }

    /// The view of the Link source of `repo_link` block `link`.
    pub fn link_source(&self, link: &BlockId) -> Option<&SourceView> {
        self.source(&format!("{}{link}", rdy_store::foreign::LINK_REF_PREFIX))
    }

    /// The repos with a live source, in id order.
    pub fn repos(&self) -> impl Iterator<Item = &RepoId> + '_ {
        self.current.keys()
    }

    /// The current delivered view of `repo`: per block, the newest version in log order across
    /// its live sources. A block that no live source holds is absent.
    pub fn current(&self, repo: &RepoId) -> Option<&BTreeMap<BlockId, HeldVersion>> {
        self.current.get(repo)
    }

    /// The version of `target` the current view of its repo holds.
    pub fn current_version(&self, target: &ForeignId) -> Option<&HeldVersion> {
        self.current.get(&target.repo)?.get(&target.id)
    }

    /// The current view of `repo` under `profile` alone: per block, the newest version in log
    /// order across the live sources of `repo` whose last delivery came from that profile.
    /// Another profile's delivered blocks never widen it.
    pub fn current_for_profile(
        &self,
        repo: &RepoId,
        profile: &str,
    ) -> BTreeMap<&BlockId, &HeldVersion> {
        let mut out: BTreeMap<&BlockId, &HeldVersion> = BTreeMap::new();
        let live = self
            .sources
            .values()
            .filter(|v| !v.revoked && v.repo == *repo && v.publication.profile == profile);
        for view in live {
            for (id, held) in &view.members {
                match out.get(id) {
                    Some(have) if have.at >= held.at => {}
                    _ => {
                        out.insert(id, held);
                    }
                }
            }
        }
        out
    }

    /// The index of every version an entry before the watermark delivered, built on first use.
    fn held_index(&self, reader: &StoreReader) -> Result<Arc<HeldIndex>, RdyError> {
        self.held
            .get_or_init(|| {
                let mut index = HeldIndex::default();
                reader.for_each_object_of(kinds::FOREIGN, |h| {
                    if !reader.has_kind_before(kinds::FOREIGN, &h, self.watermark)? {
                        return Ok(());
                    }
                    let e = reader.foreign_entry(&h)?;
                    if let ForeignEvent::Delivery { added, changed, .. } = e.event {
                        for v in added.into_iter().chain(changed) {
                            index
                                .contents
                                .insert((e.repo, v.id.clone(), v.content, v.encoding), v.version);
                            index.versions.insert((e.repo, v.id, v.version));
                        }
                    }
                    Ok(())
                })?;
                Ok(Arc::new(index))
            })
            .clone()
    }
}

impl ReadState {
    /// The foreign state of this read state: what the foreign log delivered by its watermark.
    pub fn foreign(&self) -> Result<&Arc<ForeignState>, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        Ok(&self.foreign)
    }

    /// The block a held pin names: version `version` of `target`, when an entry before this
    /// state's watermark delivered it, whatever source delivered it and whether that source is
    /// live, revoked or no longer selects it. `None` when no such entry exists.
    pub fn foreign_held(
        &self,
        target: &ForeignId,
        version: &Hash,
    ) -> Result<Option<Block>, RdyError> {
        let foreign = self.foreign()?;
        let index = foreign.held_index(&self.reader)?;
        if !index
            .versions
            .contains(&(target.repo, target.id.clone(), *version))
        {
            return Ok(None);
        }
        let block = self.reader.get_block(version)?;
        Ok((block.id == target.id).then_some(block))
    }

    /// The version that carried the content a foreign pin records for `target`, when an entry
    /// before this state's watermark delivered it, whatever its source's state now. `None` when
    /// no such entry exists: the pin's version is not held here (repo links, D-FK58A).
    pub fn foreign_held_pin(
        &self,
        target: &ForeignId,
        pin: &ForeignPin,
    ) -> Result<Option<Hash>, RdyError> {
        let foreign = self.foreign()?;
        let index = foreign.held_index(&self.reader)?;
        Ok(index
            .contents
            .get(&(target.repo, target.id.clone(), pin.content, pin.encoding))
            .copied())
    }
}

impl Rdy {
    /// Whether this store may hold foreign entries now.
    pub(crate) fn has_foreign_feature(&self) -> bool {
        self.store.has_feature_now(FEATURE_FOREIGN_COPY)
    }

    /// Test hook: publishes the state after foreign entries were written straight to the store
    /// (`store_mut().put_foreign`), as a write that moves nothing else does.
    #[doc(hidden)]
    pub fn publish_after_foreign_for_tests(&mut self) -> Result<(), RdyError> {
        self.publish(crate::Delta::Seq)
    }
}
