//! The foreign log (repo links, D-FK58A): the entries of kind FOREIGN, the BLOCK versions they
//! hold and the ref of each source.
//!
//! - A Delivery or a Revoked entry is one sealed batch of its own: its new held versions, the
//!   entry and its source's ref. The first one turns `foreign_copy` on in that batch
//!   (`crate::format`). No landing, change, proposal, view op, signature or TAG joins it, and
//!   canon never moves. Through [`Store::append_foreign`] the batch stays open for its writer,
//!   which seals it with the moves it makes (a TRANSITION as its last frame).
//! - The ref `foreign/link/<id>` (a Link source) or `foreign/adr/<name>` (an ADR source) names
//!   the newest entry of its source. Each entry names the one before it (`prev`), so a source is
//!   a chain, and a reindex rebuilds the refs from the log as it rebuilds every ref.
//! - A source's selection is the fold of its chain's deltas. The current view of a repo is, per
//!   block, the newest version in log order across that repo's live sources. A revoked source
//!   keeps its last view, takes no more entries and leaves the repo's current view.
//! - Every delivered version stays held until an authorized erasure: a removed or replaced
//!   version leaves the selection and stays readable through a held pin.
//!
//! A Removal entry is never taken through this writer: `Store::take_foreign_removal`
//! (`crate::foreign_removal`) seals one with its proof.
use crate::error::StoreError;
use crate::holder::StoreMode;
use crate::index::hash_from;
use crate::segment::Location;
use crate::{Store, StoreReader, codec, kinds};
use rdy_model::foreign::{
    ForeignEntry, ForeignEvent, ForeignId, ForeignPublication, ForeignSet, ForeignSource,
    ForeignVersion, delivery_selection,
};
use rdy_model::{Block, BlockId, Hash, RepoId};
use std::collections::{BTreeMap, BTreeSet};

/// The prefix of every foreign source ref; only [`Store::put_foreign`] writes one.
pub const FOREIGN_REF_PREFIX: &str = "foreign/";
/// The ref of a Link source: `foreign/link/<repo_link block id>`.
pub const LINK_REF_PREFIX: &str = "foreign/link/";
/// The ref of an ADR source: `foreign/adr/<source name>`.
pub const ADR_REF_PREFIX: &str = "foreign/adr/";

/// The most entries one source's chain may hold, and the most traversed by one read.
pub const MAX_CHAIN: usize = 100_000;
/// The most members of one selection, or of one delta list, or held versions in one entry.
pub const MAX_LIST: usize = 100_000;
/// The `(raw kind, payload)` items of one batch.
type Items = Vec<(u8, Vec<u8>)>;

/// The longest source key (a link's block id or an ADR source's name), in bytes.
const MAX_SOURCE_KEY: usize = 256;

/// The ref of `source`'s chain.
pub fn source_ref(source: &ForeignSource) -> String {
    match source {
        ForeignSource::Link { link, .. } => format!("{LINK_REF_PREFIX}{link}"),
        ForeignSource::Adr { source, .. } => format!("{ADR_REF_PREFIX}{source}"),
    }
}

/// A delivered version as one source holds it: the version, the entry that delivered it and
/// that entry's place in the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldVersion {
    pub version: ForeignVersion,
    pub entry: Hash,
    pub at: Location,
}

/// One source's view: its newest entry, whether a revocation ended it, its last delivery's
/// publication, set and selection digest, and its members by block id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceView {
    pub repo: RepoId,
    pub head: Hash,
    pub revoked: bool,
    pub publication: ForeignPublication,
    pub set: Option<ForeignSet>,
    pub selection: Hash,
    pub members: BTreeMap<BlockId, HeldVersion>,
}

fn refused(msg: String) -> StoreError {
    StoreError::Refused(msg)
}

fn damaged(msg: String) -> StoreError {
    StoreError::Decode(msg)
}

impl Store {
    /// Writes one Delivery or Revoked entry as its own sealed batch, with the BLOCK versions in
    /// `blocks` that the store does not hold yet and its source's ref, and returns the entry's
    /// hash. Everything is checked first, and a refusal writes nothing:
    /// - the store is a project, and the entry's repo is not the store's own;
    /// - a Delivery's `prev` is its source's head (none for a first delivery), its delta fits
    ///   the source's selection (an added id is new, a changed or removed id is a member, no id
    ///   twice), and its `selection` is the digest of the selection the delta leaves;
    /// - each added or changed version is a BLOCK in `blocks` or in the store, with the version's
    ///   id, and each block in `blocks` is one of those versions;
    /// - a Revoked entry names its source's head, and a revoked source takes no entry at all.
    ///
    /// A Removal entry is refused: this writer does not take one.
    pub fn put_foreign(
        &mut self,
        entry: &ForeignEntry,
        blocks: &[Block],
    ) -> Result<Hash, StoreError> {
        self.ensure_writable()?;
        let (id, items) = self.prepare_foreign(entry, blocks)?;
        self.write_sealed(items)?;
        Ok(id)
    }

    /// [`Store::put_foreign`] for a writer that seals the batch itself: the same checks, then
    /// the same items start a held batch of their own, which the caller seals (the engine's
    /// delivery take, with the moves the batch makes). With held batches off the items are a
    /// sealed batch at once, as `put_foreign` writes them. A held batch that is open already
    /// refuses it, so nothing else joins the entry's batch before it, and a refusal writes
    /// nothing.
    pub fn append_foreign(
        &mut self,
        entry: &ForeignEntry,
        blocks: &[Block],
    ) -> Result<Hash, StoreError> {
        self.ensure_writable()?;
        if self.open_batch().is_some() {
            return Err(refused(
                "a held batch is open: seal it before a foreign entry, which starts its own batch"
                    .into(),
            ));
        }
        let (id, items) = self.prepare_foreign(entry, blocks)?;
        self.write_guarded(items)?;
        Ok(id)
    }

    /// The checks of [`Store::put_foreign`], and its batch: the new held versions, the entry and
    /// the source's ref. Returns the entry's hash and the items.
    fn prepare_foreign(
        &self,
        entry: &ForeignEntry,
        blocks: &[Block],
    ) -> Result<(Hash, Items), StoreError> {
        if self.mode()? != StoreMode::Project {
            return Err(refused("a holder store holds no foreign entry".into()));
        }
        if self.repo_id()? == Some(entry.repo) {
            return Err(refused(format!(
                "foreign repo {} is this store's own repo",
                entry.repo
            )));
        }
        check_source_key(&entry.source)?;
        let name = source_ref(&entry.source);
        let held = self.source_view(&name)?;
        if let Some(view) = &held {
            if view.revoked {
                return Err(refused(format!(
                    "source {name} is revoked: it takes no more entries"
                )));
            }
            if view.repo != entry.repo {
                return Err(refused(format!(
                    "source {name} delivers repo {}, not {}",
                    view.repo, entry.repo
                )));
            }
        }
        let head = held.as_ref().map(|v| v.head);
        let mut items = vec![];
        match &entry.event {
            ForeignEvent::Delivery {
                prev,
                added,
                changed,
                removed,
                shells,
                selection,
                ..
            } => {
                if *prev != head {
                    return Err(refused(format!(
                        "the delivery's prev {} is not the head {} of source {name}",
                        show(prev),
                        show(&head)
                    )));
                }
                if added.len() + changed.len() > MAX_LIST
                    || removed.len() > MAX_LIST
                    || blocks.len() > MAX_LIST
                {
                    return Err(refused(format!(
                        "a delivery holds at most {MAX_LIST} versions, removals and blocks"
                    )));
                }
                let mut sel: BTreeMap<BlockId, Hash> = held
                    .map(|v| {
                        v.members
                            .into_iter()
                            .map(|(k, h)| (k, h.version.version))
                            .collect()
                    })
                    .unwrap_or_default();
                fold_delta(&mut sel, added, changed, removed).map_err(refused)?;
                if sel.len() > MAX_LIST {
                    return Err(refused(format!(
                        "a selection holds at most {MAX_LIST} blocks"
                    )));
                }
                let rebuilt = delivery_selection(sel.iter(), shells).map_err(|why| {
                    refused(format!("the selection does not fit its digest: {why}"))
                })?;
                if rebuilt != *selection {
                    return Err(refused(format!(
                        "the delivery's selection {selection} is not the digest {rebuilt} its delta rebuilds"
                    )));
                }
                items.extend(self.held_items(added.iter().chain(changed), blocks)?);
            }
            ForeignEvent::Revoked { prev } => {
                if head != Some(*prev) {
                    return Err(refused(format!(
                        "the revocation's prev {prev} is not the head {} of source {name}",
                        show(&head)
                    )));
                }
                if !blocks.is_empty() {
                    return Err(refused("a revocation holds no version".into()));
                }
            }
            ForeignEvent::Removal { .. } => {
                return Err(refused(
                    "a foreign removal is not taken through this writer".into(),
                ));
            }
        }
        let item = codec::item_new(kinds::FOREIGN, entry);
        // the entry keeps the bounds every reader of a typed record keeps (`crate::bounded`), so
        // no acknowledged entry is one a bounded reader refuses
        if let Err(e) = codec::decode_bounded::<ForeignEntry>(item.0, &item.1) {
            return Err(refused(format!(
                "the foreign entry exceeds a record bound: {e}"
            )));
        }
        let id = Hash::of(&item.1);
        items.push(item);
        items.extend(self.ref_items_pub(vec![(name, id.0.to_vec())]));
        Ok((id, items))
    }

    /// The BLOCK items of `versions` that the store does not hold yet, from `blocks`. Each
    /// version is a block of `blocks` or of the store, with the version's id; each block of
    /// `blocks` is one of `versions`.
    fn held_items<'a>(
        &self,
        versions: impl Iterator<Item = &'a ForeignVersion>,
        blocks: &[Block],
    ) -> Result<Items, StoreError> {
        let mut given: BTreeMap<Hash, (&Block, (u8, Vec<u8>))> = BTreeMap::new();
        for b in blocks {
            let item = codec::item_new(kinds::BLOCK, b);
            given.insert(Hash::of(&item.1), (b, item));
        }
        let mut named = BTreeSet::new();
        let mut out = vec![];
        for v in versions {
            named.insert(v.version);
            let id = match given.get(&v.version) {
                Some((b, _)) => b.id.clone(),
                None if self.has_kind(kinds::BLOCK, &v.version)? => self.get_block(&v.version)?.id,
                None => {
                    return Err(refused(format!(
                        "delivered version {} of {} has no BLOCK",
                        v.version, v.id
                    )));
                }
            };
            if id != v.id {
                return Err(refused(format!(
                    "delivered version {} is a block of {id}, not of {}",
                    v.version, v.id
                )));
            }
        }
        for (h, (_, item)) in given {
            if !named.contains(&h) {
                return Err(refused(format!(
                    "block version {h} is not a version of the delivery"
                )));
            }
            if !self.has_kind(kinds::BLOCK, &h)? {
                out.push(item);
            }
        }
        Ok(out)
    }
}

/// Checks that `source` has a source key: its link's block id or its ADR source's name, 1 to 256
/// bytes with no `/`. A refusal is `StoreError::Refused`.
pub fn check_source_key(source: &ForeignSource) -> Result<(), StoreError> {
    let key = match source {
        ForeignSource::Link { link, .. } => link,
        ForeignSource::Adr { source, .. } => source,
    };
    if key.is_empty() || key.len() > MAX_SOURCE_KEY || key.contains('/') {
        return Err(refused(format!(
            "foreign source {key:?} is not a source key: 1 to {MAX_SOURCE_KEY} bytes, no `/`"
        )));
    }
    Ok(())
}

/// Applies a delivery's delta to `sel`: an added id is new, a changed or removed id is a member,
/// and no id is listed twice.
pub(crate) fn fold_delta(
    sel: &mut BTreeMap<BlockId, Hash>,
    added: &[ForeignVersion],
    changed: &[ForeignVersion],
    removed: &[BlockId],
) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    let mut once = |id: &BlockId| {
        if seen.insert(id.clone()) {
            Ok(())
        } else {
            Err(format!("the delivery lists {id} twice"))
        }
    };
    for v in added {
        once(&v.id)?;
        if sel.insert(v.id.clone(), v.version).is_some() {
            return Err(format!("added {} is already in the selection", v.id));
        }
    }
    for v in changed {
        once(&v.id)?;
        if sel.insert(v.id.clone(), v.version).is_none() {
            return Err(format!("changed {} is not in the selection", v.id));
        }
    }
    for id in removed {
        once(id)?;
        if sel.remove(id).is_none() {
            return Err(format!("removed {id} is not in the selection"));
        }
    }
    Ok(())
}

/// `a + b`, or the refusal of a count past `u64::MAX`.
fn add_bytes(a: u64, b: u64) -> Result<u64, StoreError> {
    a.checked_add(b)
        .ok_or_else(|| refused(format!("the foreign byte count exceeds {} bytes", u64::MAX)))
}

fn show(h: &Option<Hash>) -> String {
    h.map_or_else(|| "none".to_string(), |h| h.to_string())
}

impl StoreReader {
    /// The foreign entry with hash `h`.
    pub fn foreign_entry(&self, h: &Hash) -> Result<ForeignEntry, StoreError> {
        self.get_typed(kinds::FOREIGN, h)
    }

    /// Each source ref and the newest entry it names, in name order.
    pub fn foreign_sources(&self) -> Result<Vec<(String, Hash)>, StoreError> {
        self.refs_with_prefix(FOREIGN_REF_PREFIX)?
            .into_iter()
            .map(|(name, value)| {
                let h = hash_from(&value, "foreign source ref")?;
                Ok((name, h))
            })
            .collect()
    }

    /// The view of the source whose ref is `name`: the fold of its chain, from its first entry
    /// to the newest. `None` when the source has no entry. A chain that breaks the log's rules
    /// (an entry of another source or repo, a revocation before the newest entry, a removal, a
    /// chain past [`MAX_CHAIN`]) is `StoreError::Decode`.
    pub fn source_view(&self, name: &str) -> Result<Option<SourceView>, StoreError> {
        let Some(value) = self.get_ref(name)? else {
            return Ok(None);
        };
        let head = hash_from(&value, "foreign source ref")?;
        // newest first: the hashes alone, so the walk holds one entry at a time
        let mut chain = vec![];
        let mut next = Some(head);
        while let Some(h) = next {
            if chain.len() == MAX_CHAIN {
                return Err(damaged(format!(
                    "source {name} has more than {MAX_CHAIN} entries"
                )));
            }
            let e = self.foreign_entry(&h)?;
            if source_ref(&e.source) != name {
                return Err(damaged(format!(
                    "foreign entry {h} is not an entry of source {name}"
                )));
            }
            next = match e.event {
                ForeignEvent::Delivery { prev, .. } => prev,
                ForeignEvent::Revoked { prev } if h == head => Some(prev),
                ForeignEvent::Revoked { .. } => {
                    return Err(damaged(format!(
                        "source {name} has entries after its revocation {h}"
                    )));
                }
                ForeignEvent::Removal { .. } => {
                    return Err(damaged(format!(
                        "foreign removal {h} is in the chain of source {name}"
                    )));
                }
            };
            chain.push(h);
        }
        let mut view: Option<SourceView> = None;
        for h in chain.into_iter().rev() {
            let e = self.foreign_entry(&h)?;
            let at = self
                .index()
                .typed(kinds::FOREIGN, &h)?
                .ok_or_else(|| damaged(format!("foreign entry {h} has no place in the log")))?;
            // a source never changes repo
            if let Some(v) = &view
                && v.repo != e.repo
            {
                return Err(damaged(format!(
                    "foreign entry {h} of source {name} names repo {}, not {}",
                    e.repo, v.repo
                )));
            }
            match e.event {
                ForeignEvent::Delivery {
                    publication,
                    set,
                    added,
                    changed,
                    removed,
                    selection,
                    ..
                } => {
                    let v = view.get_or_insert_with(|| SourceView {
                        repo: e.repo,
                        head: h,
                        revoked: false,
                        publication: publication.clone(),
                        set: None,
                        selection,
                        members: BTreeMap::new(),
                    });
                    for fv in added.into_iter().chain(changed) {
                        v.members.insert(
                            fv.id.clone(),
                            HeldVersion {
                                version: fv,
                                entry: h,
                                at,
                            },
                        );
                    }
                    for id in removed {
                        v.members.remove(&id);
                    }
                    if v.members.len() > MAX_LIST {
                        return Err(damaged(format!(
                            "source {name} holds more than {MAX_LIST} blocks"
                        )));
                    }
                    v.head = h;
                    v.publication = publication;
                    v.set = set;
                    v.selection = selection;
                }
                ForeignEvent::Revoked { .. } => {
                    let v = view.as_mut().ok_or_else(|| {
                        damaged(format!(
                            "source {name} is revoked before its first delivery"
                        ))
                    })?;
                    v.head = h;
                    v.revoked = true;
                }
                ForeignEvent::Removal { .. } => unreachable!("the walk refused it"),
            }
        }
        Ok(view)
    }

    /// The current delivered view of `repo`: per block, the newest version in log order that a
    /// live (not revoked) source of `repo` holds. A block that no live source holds is not in it.
    pub fn repo_view(&self, repo: &RepoId) -> Result<BTreeMap<BlockId, HeldVersion>, StoreError> {
        let mut out: BTreeMap<BlockId, HeldVersion> = BTreeMap::new();
        for (name, _) in self.foreign_sources()? {
            let Some(view) = self.source_view(&name)? else {
                continue;
            };
            if view.revoked || view.repo != *repo {
                continue;
            }
            for (id, held) in view.members {
                match out.get(&id) {
                    Some(have) if have.at >= held.at => {}
                    _ => {
                        out.insert(id, held);
                    }
                }
            }
        }
        Ok(out)
    }

    /// The bytes the foreign log holds of `repo`: each FOREIGN entry of `repo` and each BLOCK
    /// version that one of its deliveries names, each counted once at its stored frame (the
    /// 37-byte frame header and the payload). Every source counts, live or revoked, and so does
    /// every version a later delivery replaced or removed: each stays held. A version that local
    /// content shares counts too, so a guard over these bytes never undercounts. Another repo's
    /// entries and versions are its own. The set of counted versions grows fallibly: a refused
    /// growth is `StoreError::Refused`.
    pub fn foreign_bytes(&self, repo: &RepoId) -> Result<u64, StoreError> {
        self.count_foreign_bytes(repo, None)
    }

    /// [`StoreReader::foreign_bytes`] as it stood at the log end `end`: only the FOREIGN entries
    /// stored before `end` count, with the versions their deliveries name (a read state's cut,
    /// whose watermark is the log end it was published at).
    pub fn foreign_bytes_before(&self, repo: &RepoId, end: Location) -> Result<u64, StoreError> {
        self.count_foreign_bytes(repo, Some(end))
    }

    fn count_foreign_bytes(&self, repo: &RepoId, end: Option<Location>) -> Result<u64, StoreError> {
        let mut total = 0u64;
        let mut counted: std::collections::HashSet<Hash> = std::collections::HashSet::new();
        self.for_each_object_of(kinds::FOREIGN, |h| {
            if let Some(end) = end
                && !self.has_kind_before(kinds::FOREIGN, &h, end)?
            {
                return Ok(());
            }
            let e = self.foreign_entry(&h)?;
            if e.repo != *repo {
                return Ok(());
            }
            total = add_bytes(total, self.frame_bytes(kinds::FOREIGN, &h)?)?;
            if let ForeignEvent::Delivery { added, changed, .. } = &e.event {
                for v in added.iter().chain(changed) {
                    counted.try_reserve(1).map_err(|_| {
                        refused("the foreign byte count could not grow its version set".into())
                    })?;
                    if counted.insert(v.version) {
                        total = add_bytes(total, self.frame_bytes(kinds::BLOCK, &v.version)?)?;
                    }
                }
            }
            Ok(())
        })?;
        Ok(total)
    }

    /// The bytes of the stored frame of the object `h` of `kind`: its header and its payload.
    fn frame_bytes(&self, kind: u8, h: &Hash) -> Result<u64, StoreError> {
        let at = self
            .index()
            .typed(kind, h)?
            .ok_or_else(|| damaged(format!("foreign object {h} has no place in the log")))?;
        let (len, _, _) = self
            .segments()
            .header(at)?
            .ok_or_else(|| damaged(format!("foreign object {h} has no frame in the log")))?;
        Ok(crate::segment::HEADER + u64::from(len))
    }

    /// The block of a held pin: version `version` of `target`, when an entry of `target`'s repo
    /// delivered it, whatever source and whether that source is live, revoked or no longer
    /// selects it. `None` when no entry delivered it.
    pub fn held_version(
        &self,
        target: &ForeignId,
        version: &Hash,
    ) -> Result<Option<Block>, StoreError> {
        if !self.has_kind(kinds::BLOCK, version)? {
            return Ok(None);
        }
        let mut found = false;
        self.for_each_object_of(kinds::FOREIGN, |h| {
            if found {
                return Ok(());
            }
            let e = self.foreign_entry(&h)?;
            if e.repo != target.repo {
                return Ok(());
            }
            if let ForeignEvent::Delivery { added, changed, .. } = &e.event {
                found = added
                    .iter()
                    .chain(changed)
                    .any(|v| v.id == target.id && v.version == *version);
            }
            Ok(())
        })?;
        if !found {
            return Ok(None);
        }
        let block = self.get_block(version)?;
        Ok((block.id == target.id).then_some(block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_model::foreign::selection_identity;

    #[test]
    fn source_refs_by_variant() {
        let link = ForeignSource::Link {
            link: "rl-00001".into(),
            site: "s".into(),
            at_ms: 1,
        };
        let adr = ForeignSource::Adr {
            source: "adrs".into(),
            revision: "r".into(),
        };
        assert_eq!(source_ref(&link), "foreign/link/rl-00001");
        assert_eq!(source_ref(&adr), "foreign/adr/adrs");
    }

    /// A first, empty delivery from link `rl-00001` whose source names `site`.
    fn empty_delivery(site: String) -> ForeignEntry {
        ForeignEntry {
            repo: RepoId([7; 16]),
            source: ForeignSource::Link {
                link: "rl-00001".into(),
                site,
                at_ms: 1,
            },
            event: ForeignEvent::Delivery {
                prev: None,
                publication: ForeignPublication {
                    tag: "v1".into(),
                    profile: "pp-00001".into(),
                    profile_version: None,
                },
                set: None,
                added: vec![],
                changed: vec![],
                removed: vec![],
                shells: vec![],
                selection: selection_identity(std::iter::empty::<(&BlockId, &Hash)>()).unwrap(),
            },
        }
    }

    /// An entry a bounded reader would refuse (a text over 1,048,576 bytes) is refused before
    /// anything is written, so no acknowledged entry breaks its source's chain.
    #[test]
    fn rpl10_an_entry_past_a_record_bound_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Store::create(&d.path().join("s")).unwrap();
        let end = s.log_end();
        let big = empty_delivery("s".repeat(crate::bounded::MAX_TEXT_BYTES + 1));
        let e = s.put_foreign(&big, &[]).unwrap_err();
        assert!(
            matches!(&e, StoreError::Refused(m) if m.starts_with("the foreign entry exceeds a record bound")),
            "{e:?}"
        );
        assert_eq!(s.log_end(), end);
        // at the bound it is taken
        let at = empty_delivery("s".repeat(crate::bounded::MAX_TEXT_BYTES));
        s.put_foreign(&at, &[]).unwrap();
    }

    /// `append_foreign` starts a held batch that its writer seals; an open held batch refuses
    /// it, and nothing is appended.
    #[test]
    fn rpl10_append_foreign_leaves_its_batch_for_the_writer() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Store::create(&d.path().join("s")).unwrap();
        s.set_held(true).unwrap();
        s.write(vec![(kinds::INTENT, vec![1])]).unwrap();
        let end = s.log_end();
        let e = s
            .append_foreign(&empty_delivery("s".into()), &[])
            .unwrap_err();
        assert!(matches!(e, StoreError::Refused(_)), "{e:?}");
        assert_eq!(s.log_end(), end);
        s.seal(vec![]).unwrap();
        let h = s.append_foreign(&empty_delivery("s".into()), &[]).unwrap();
        let open: Vec<u8> = s.open_batch().unwrap().iter().map(|(k, _)| *k).collect();
        assert!(open.contains(&kinds::FOREIGN), "{open:?}");
        s.seal(vec![]).unwrap();
        assert!(s.open_batch().is_none());
        assert_eq!(
            s.reader()
                .source_view("foreign/link/rl-00001")
                .unwrap()
                .unwrap()
                .head,
            h
        );
    }
}
