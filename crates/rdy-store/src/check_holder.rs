//! The structural check of a holder (D-BPYN8, D-DAGNB): a holder keeps uploaded objects and tags
//! with no canon, so its fsck starts from the tags. Each `tag/<name>` names a tag record, and its
//! landing starts a chain that the landings' `prev` links follow. The tags' divergent histories
//! are checked one chain at a time, never against canon or the global change -> landing index.
//!
//! The check reads every object a tag reaches through its object links (`object_refs::refs_of`),
//! once per `ObjectKey`: snapshot versions, change dependencies, proposal, approval and relay
//! links, blob manifests and chunks. Each object it reads is checked by its own rules too: every
//! landing it reaches, by any link, keeps the relay and evidence rules (the project's own rule);
//! every snapshot entry names a version of that entry's block (a removed version reads as its
//! stub); every blob keeps the rules a blob read keeps (canonical manifest, chunk count, chunk
//! lengths). Then it checks each chain: each change landed once in the chain, and each landed
//! change's dependencies landed at or before its landing in that chain. The dependency DAG has no
//! cycle, the answers keep the answer rules, the change plans keep their chain and seal rules
//! (`crate::plan`, with no `plan/` ref), the signature table holds exactly its records, and
//! (O11, ruling task-13) each `answer/tag-object:<hash>` names a stored TAG and the stored answer
//! that relays it: act `tag`, item `tag:<its name>`, targets exactly that TAG. Each such TAG is
//! read with the tags' objects too: a twin's tag kept beside the version that names the same
//! landing. There is no pack validation: a holder has no pack.
//!
//! Every walk is iterative and bounded: [`MAX_LANDING_DEPTH`] landings in one chain,
//! [`MAX_OBJECTS`] objects and [`MAX_EDGES`] object links. Past a bound the check refuses with
//! `holder check exceeds {limit}` ([`holder_check_limit`] reads the limit back).
use crate::StoreReader;
use crate::check::StoreCheck;
use crate::error::StoreError;
use crate::holder::{
    ANSWER_TAG_OBJECT_REF, StoreMode, TAG_REF, answer_tag_object, is_relay_answer_of, not_allowed,
};
use crate::index::hash_from;
use crate::kinds;
use crate::object_refs::{ObjectKey, PORTABLE_KINDS, refs_of};
use crate::store::decode;
use rdy_model::{Answer, Block, BlockId, Change, Hash, LandingRecord, StateSnapshot, Tag};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

/// The most landings one tag's chain may hold.
pub const MAX_LANDING_DEPTH: usize = 100_000;
/// The most distinct objects one holder check reads.
pub const MAX_OBJECTS: usize = 1_000_000;
/// The most object links one holder check follows.
pub const MAX_EDGES: usize = 1_000_000;

/// The start of the bound refusal's text.
const LIMIT_PREFIX: &str = "holder check exceeds ";

/// The bounds of one holder check.
#[derive(Clone, Copy, Debug)]
struct Limits {
    landing_depth: usize,
    objects: usize,
    edges: usize,
}

const LIMITS: Limits = Limits {
    landing_depth: MAX_LANDING_DEPTH,
    objects: MAX_OBJECTS,
    edges: MAX_EDGES,
};

/// The refusal of a check past one of its bounds: `holder check exceeds {limit}`.
fn exceeds(limit: String) -> StoreError {
    StoreError::Refused(format!("{LIMIT_PREFIX}{limit}"))
}

/// The bound named by a holder check's refusal (`100000 landing depth`, `1000000 objects` or
/// `1000000 edges`), or `None` for any other error.
pub fn holder_check_limit(e: &StoreError) -> Option<&str> {
    match e {
        StoreError::Refused(m) => m.strip_prefix(LIMIT_PREFIX),
        _ => None,
    }
}

fn bad(m: String) -> StoreError {
    StoreError::Decode(m)
}

/// What one tag ref names: its name (the part after `tag/`), the tag record's hash and the tag.
struct TagRef {
    refname: String,
    hash: Hash,
    tag: Tag,
}

/// The objects one check has read, with the bounds it keeps.
struct Closure<'a> {
    reader: &'a StoreReader,
    limits: Limits,
    seen: HashSet<ObjectKey>,
    stack: Vec<ObjectKey>,
    edges: usize,
    /// Every landing read, each checked by the landing rules once.
    landings: HashMap<Hash, Arc<LandingRecord>>,
    /// The block id of each block version read.
    ids: HashMap<Hash, BlockId>,
    /// The snapshot entries whose version was not read yet, each with the first snapshot that
    /// holds it: checked once the walk has read every version it reaches.
    pending: BTreeMap<(BlockId, Hash), Hash>,
    /// The length of each blob chunk read, so each is read once across every manifest.
    chunk_lens: HashMap<Hash, u64>,
}

impl<'a> Closure<'a> {
    fn new(reader: &'a StoreReader, limits: Limits) -> Closure<'a> {
        Closure {
            reader,
            limits,
            seen: HashSet::new(),
            stack: Vec::new(),
            edges: 0,
            landings: HashMap::new(),
            ids: HashMap::new(),
            pending: BTreeMap::new(),
            chunk_lens: HashMap::new(),
        }
    }

    /// Adds `key` to what the walk reads, once.
    fn add(&mut self, key: ObjectKey) -> Result<(), StoreError> {
        if self.mark(key)? {
            self.stack.push(key);
        }
        Ok(())
    }

    /// Counts `key` as read, once, without reading it (again): true the first time.
    fn mark(&mut self, key: ObjectKey) -> Result<bool, StoreError> {
        if self.seen.contains(&key) {
            return Ok(false);
        }
        if self.seen.len() >= self.limits.objects {
            return Err(exceeds(format!("{} objects", self.limits.objects)));
        }
        self.seen.insert(key);
        Ok(true)
    }

    /// Reads every object the added ones reach. A missing object is `Decode` naming the object
    /// whose link named it; a payload a removal erased has no links to follow (the removal's own
    /// checks cover it). Once an object's links are known to be stored, its own structure is
    /// checked: each landing by the landing rules, each snapshot entry's version as a version of
    /// that entry's block, and each blob by the rules a blob read keeps.
    fn run(&mut self) -> Result<(), StoreError> {
        while let Some(key) = self.stack.pop() {
            let payload = match self.reader.get_kind(key.kind, &key.hash) {
                Ok(p) => p,
                Err(StoreError::Redacted { .. }) => continue,
                Err(e) => return Err(e),
            };
            let refs =
                refs_of(key.kind, &payload).map_err(|e| bad(format!("object {key}: {e}")))?;
            let mut children = Vec::with_capacity(refs.len());
            for r in refs {
                self.edges += 1;
                if self.edges > self.limits.edges {
                    return Err(exceeds(format!("{} edges", self.limits.edges)));
                }
                let kind = match r.kind {
                    Some(k) => {
                        if !self.reader.has_kind(k, &r.hash)? {
                            let child = ObjectKey {
                                kind: k,
                                hash: r.hash,
                            };
                            return Err(bad(format!(
                                "object {key} names {child}, which is not stored"
                            )));
                        }
                        k
                    }
                    None => self.resolve(&key, &r.hash)?,
                };
                children.push(ObjectKey { kind, hash: r.hash });
            }
            // a blob's chunks are read and checked with it, each once across every manifest, and
            // not read again
            let mut chunks_read = false;
            match key.kind {
                kinds::LANDING => {
                    let rec: LandingRecord = decode(&payload)?;
                    self.reader.check_landing_fields(&key.hash, &rec)?;
                    self.landings.insert(key.hash, Arc::new(rec));
                }
                kinds::BLOCK => {
                    let b: Block = decode(&payload)?;
                    self.ids.insert(key.hash, b.id);
                }
                kinds::SNAPSHOT => {
                    let state: StateSnapshot = decode(&payload)?;
                    for (id, v) in state.blocks {
                        match self.ids.get(&v) {
                            Some(of) => check_entry(&key.hash, &id, &v, of)?,
                            None => {
                                self.pending.entry((id, v)).or_insert(key.hash);
                            }
                        }
                    }
                }
                kinds::BLOB_MANIFEST => {
                    chunks_read = self.reader.check_blob(&key.hash, &mut self.chunk_lens)?
                }
                _ => {}
            }
            for child in children {
                if chunks_read && child.kind == kinds::BLOB_CHUNK {
                    self.mark(child)?;
                } else {
                    self.add(child)?;
                }
            }
        }
        self.check_pending()
    }

    /// The snapshot entries whose version the walk read after their snapshot. A version whose
    /// payload a removal erased reads as its stub, by the removal's projection rules; one that
    /// no residue reads has nothing to check.
    fn check_pending(&mut self) -> Result<(), StoreError> {
        for ((id, v), snapshot) in std::mem::take(&mut self.pending) {
            if let Some(of) = self.ids.get(&v) {
                check_entry(&snapshot, &id, &v, of)?;
                continue;
            }
            match self.reader.get_block(&v) {
                Ok(b) => check_entry(&snapshot, &id, &v, &b.id)?,
                Err(StoreError::Redacted { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// The one portable kind a link of any kind resolves to: the hash must be stored as exactly
    /// one portable object.
    fn resolve(&self, from: &ObjectKey, h: &Hash) -> Result<u8, StoreError> {
        let mut found = None;
        for k in PORTABLE_KINDS {
            if self.reader.has_kind(k, h)? {
                if found.is_some() {
                    return Err(bad(format!(
                        "object {from} names {h}, which is stored as more than one kind"
                    )));
                }
                found = Some(k);
            }
        }
        found.ok_or_else(|| bad(format!("object {from} names {h}, which is not stored")))
    }
}

/// Snapshot `snapshot`'s entry for block `id` names version `v`, a version of block `of`: the
/// two must be the same block.
fn check_entry(snapshot: &Hash, id: &BlockId, v: &Hash, of: &BlockId) -> Result<(), StoreError> {
    if of != id {
        return Err(bad(format!(
            "snapshot {snapshot}: block {id} names version {v}, which is a version of {of}"
        )));
    }
    Ok(())
}

/// The landings of the chain that starts at landing `head`, head first, as `landing` reads
/// each: `head`, its `prev`, and so on to the first landing. More than `depth` landings is
/// `holder check exceeds {depth} landing depth`; a landing met twice is `Decode`.
fn walk_chain<E: From<StoreError>>(
    head: &Hash,
    depth: usize,
    landing: &mut dyn FnMut(&Hash) -> Result<Arc<LandingRecord>, E>,
) -> Result<Vec<(Hash, Arc<LandingRecord>)>, E> {
    let mut out: Vec<(Hash, Arc<LandingRecord>)> = Vec::new();
    let mut on: HashSet<Hash> = HashSet::new();
    let mut cur = Some(*head);
    while let Some(h) = cur {
        if out.len() >= depth {
            return Err(exceeds(format!("{depth} landing depth")).into());
        }
        if !on.insert(h) {
            return Err(bad(format!("landing {h} is on its own chain twice")).into());
        }
        let rec = landing(&h)?;
        cur = rec.prev;
        out.push((h, rec));
    }
    Ok(out)
}

/// [`walk_chain`] with the holder check's depth bound ([`MAX_LANDING_DEPTH`]): the chain a tag
/// at landing `head` names, read through `landing` (a holder's take reads it over the upload
/// and the holder).
pub fn tag_chain_with<E: From<StoreError>>(
    head: &Hash,
    landing: &mut dyn FnMut(&Hash) -> Result<Arc<LandingRecord>, E>,
) -> Result<Vec<(Hash, Arc<LandingRecord>)>, E> {
    walk_chain(head, MAX_LANDING_DEPTH, landing)
}

/// How [`chain_rules`] reads a landed change: from its landing and its hash.
pub type ChangeOf<'a, E> = dyn FnMut(&Hash, &Hash) -> Result<Option<Arc<Change>>, E> + 'a;

/// The rules of one tag's chain `chain` (head first), named `refname` in its refusals: each
/// change lands once in the chain, and each landed change's dependencies landed at or before
/// its landing in that chain. `change(landing, change)` reads a landed change; None skips its
/// dependencies (a change that is not stored, or one whose landing the caller already
/// checked). The one list of chain rules: the holder's fsck and a holder's take apply it.
pub fn chain_rules<E: From<StoreError>>(
    refname: &str,
    chain: &[(Hash, Arc<LandingRecord>)],
    change: &mut ChangeOf<'_, E>,
) -> Result<(), E> {
    // where each change of the chain landed: its position, newest first
    let mut at: HashMap<Hash, usize> = HashMap::new();
    for (i, (l, rec)) in chain.iter().enumerate() {
        for c in &rec.changes {
            if let Some(j) = at.insert(*c, i) {
                let first = chain[j].0;
                return Err(bad(format!(
                    "{refname}: change {c} lands in landing {first} and in landing {l}"
                ))
                .into());
            }
        }
    }
    for (i, (l, rec)) in chain.iter().enumerate() {
        for c in &rec.changes {
            let Some(ch) = change(l, c)? else {
                continue;
            };
            for dep in &ch.deps {
                if !at.get(dep).is_some_and(|j| *j >= i) {
                    return Err(bad(format!(
                        "{refname}: change {c} of landing {l} depends on {dep}, which is not landed at or before it"
                    ))
                    .into());
                }
            }
        }
    }
    Ok(())
}

impl StoreReader {
    /// Checks a holder (see the module docs): its meta refs and ref history first, then its
    /// `answer/tag-object:` refs, its tags and every object they reach, each tag's chain, the
    /// dependency DAG, the answers and the signature table. The first failure found is
    /// returned: `StoreError::Decode` for damage, `holder check exceeds {limit}` past a bound.
    /// The counts: `landed_changes` and `relayed` count distinct changes and landings across
    /// the tags' chains, `answers` the ANSWER records; the rest are zero (a holder has no views,
    /// coordination refs or transitions).
    pub fn check_holder(&self) -> Result<StoreCheck, StoreError> {
        self.check_holder_within(LIMITS)
    }

    /// Test hook: [`StoreReader::check_holder`] with lower bounds, so a test reaches each bound
    /// without building a history of its full size.
    #[doc(hidden)]
    pub fn check_holder_bounded_for_tests(
        &self,
        landing_depth: usize,
        objects: usize,
        edges: usize,
    ) -> Result<StoreCheck, StoreError> {
        self.check_holder_within(Limits {
            landing_depth,
            objects,
            edges,
        })
    }

    /// The landings of the chain that starts at landing `head`, head first: `head`, its `prev`,
    /// and so on to the first landing. More than [`MAX_LANDING_DEPTH`] landings is `holder
    /// check exceeds 100000 landing depth`; a landing met twice is `Decode`.
    pub fn tag_chain(&self, head: &Hash) -> Result<Vec<(Hash, LandingRecord)>, StoreError> {
        let mut cache = HashMap::new();
        Ok(self
            .chain_within(head, MAX_LANDING_DEPTH, &mut cache)?
            .into_iter()
            .map(|(h, rec)| (h, (*rec).clone()))
            .collect())
    }

    /// The distinct landings of the chains that start at `heads`, in hash order. The chains
    /// are walked together: a walk stops at a landing an earlier one read, so each landing is
    /// read once. Each chain keeps its own bound: more than [`MAX_LANDING_DEPTH`] landings in
    /// one is `holder check exceeds 100000 landing depth`; a landing met twice is `Decode`.
    pub fn tag_histories(&self, heads: &[Hash]) -> Result<Vec<Hash>, StoreError> {
        self.histories_within(heads, MAX_LANDING_DEPTH)
    }

    /// Test hook: [`StoreReader::tag_histories`] with a lower depth bound.
    #[doc(hidden)]
    pub fn tag_histories_bounded_for_tests(
        &self,
        heads: &[Hash],
        depth: usize,
    ) -> Result<Vec<Hash>, StoreError> {
        self.histories_within(heads, depth)
    }

    fn histories_within(&self, heads: &[Hash], depth: usize) -> Result<Vec<Hash>, StoreError> {
        // each walked landing's chain length: the landing and every landing before it
        let mut len: HashMap<Hash, usize> = HashMap::new();
        for head in heads {
            if len.contains_key(head) {
                continue;
            }
            // the landings this walk reads, newest first, down to one an earlier walk read
            let mut path: Vec<Hash> = vec![];
            let mut on: HashSet<Hash> = HashSet::new();
            let mut below = 0;
            let mut cur = Some(*head);
            while let Some(h) = cur {
                if let Some(n) = len.get(&h) {
                    below = *n;
                    break;
                }
                if path.len() >= depth {
                    return Err(exceeds(format!("{depth} landing depth")));
                }
                if !on.insert(h) {
                    return Err(bad(format!("landing {h} is on its own chain twice")));
                }
                cur = self.landing(&h)?.prev;
                path.push(h);
            }
            if path.len() + below > depth {
                return Err(exceeds(format!("{depth} landing depth")));
            }
            let n = path.len();
            for (i, h) in path.into_iter().enumerate() {
                len.insert(h, n - i + below);
            }
        }
        let mut out: Vec<Hash> = len.into_keys().collect();
        out.sort_unstable();
        Ok(out)
    }

    fn chain_within(
        &self,
        head: &Hash,
        depth: usize,
        cache: &mut HashMap<Hash, Arc<LandingRecord>>,
    ) -> Result<Vec<(Hash, Arc<LandingRecord>)>, StoreError> {
        walk_chain(head, depth, &mut |h| {
            Ok(match cache.get(h) {
                Some(r) => Arc::clone(r),
                None => {
                    let r = Arc::new(self.landing(h)?);
                    cache.insert(*h, Arc::clone(&r));
                    r
                }
            })
        })
    }

    fn check_holder_within(&self, limits: Limits) -> Result<StoreCheck, StoreError> {
        if self.mode()? != StoreMode::Holder {
            return Err(bad("the store is not a holder".to_string()));
        }
        // the holder rule first: the mode's history and the closed ref families
        self.check_meta()?;
        let relayed_tags = self.check_answer_tags()?;
        let tags = self.holder_tags()?;
        let mut closure = Closure::new(self, limits);
        for hash in tags.iter().map(|t| t.hash).chain(relayed_tags) {
            closure.add(ObjectKey {
                kind: kinds::TAG,
                hash,
            })?;
        }
        closure.run()?;
        // each chain on its own: never the global change -> landing index. The walk read and
        // checked every landing the chains hold.
        let mut landings = std::mem::take(&mut closure.landings);
        let mut changes: HashMap<Hash, Option<Arc<Change>>> = HashMap::new();
        let mut landed: BTreeSet<Hash> = BTreeSet::new();
        let mut relayed: BTreeSet<Hash> = BTreeSet::new();
        for t in &tags {
            let chain = self.chain_within(&t.tag.landing, limits.landing_depth, &mut landings)?;
            for (l, rec) in &chain {
                if rec.relayed_by.is_some() {
                    relayed.insert(*l);
                }
                landed.extend(rec.changes.iter().copied());
            }
            chain_rules(&t.refname, &chain, &mut |_, c| {
                self.change_for_dag(&mut changes, c)
            })?;
        }
        let roots: BTreeSet<Hash> = closure
            .seen
            .iter()
            .filter(|k| k.kind == kinds::CHANGE)
            .map(|k| k.hash)
            .collect();
        self.check_acyclic(&roots, &mut changes)?;
        let answers = self.check_answers()?;
        // the chains and seals a holder took, with no `plan/` ref (`check_meta` refuses one)
        self.check_plans()?;
        self.check_signatures()?;
        Ok(StoreCheck {
            views: 0,
            view_ops: 0,
            landed_changes: landed.len(),
            relayed: relayed.len(),
            conflicts_recorded: 0,
            intents: 0,
            alt_sets: 0,
            reviews: 0,
            tensions: 0,
            inboxes: 0,
            transitions: 0,
            answers,
        })
    }

    /// O11 (ruling task-13): each `answer/tag-object:<hash>` ref names, by 64 lowercase hex
    /// digits, a stored TAG, and its value names a stored ANSWER whose act is `tag`, whose item
    /// is `tag:<that tag's name>` and whose targets are exactly that TAG (`is_relay_answer_of`);
    /// otherwise `holder ref {name} is not allowed`. Returns the TAG hashes the refs name.
    fn check_answer_tags(&self) -> Result<Vec<Hash>, StoreError> {
        let mut out = vec![];
        for (name, value) in self.refs_with_prefix(ANSWER_TAG_OBJECT_REF)? {
            let Some(t) = answer_tag_object(&name) else {
                return Err(not_allowed(&name));
            };
            let tag = match self.read_tag(&t) {
                Ok(tag) => tag,
                Err(StoreError::NotFound(_) | StoreError::Decode(_)) => {
                    return Err(not_allowed(&name));
                }
                Err(e) => return Err(e),
            };
            let Ok(h) = hash_from(&value, &name) else {
                return Err(not_allowed(&name));
            };
            let a: Answer = match self.get_typed(kinds::ANSWER, &h) {
                Ok(a) => a,
                Err(StoreError::NotFound(_) | StoreError::Decode(_)) => {
                    return Err(not_allowed(&name));
                }
                Err(e) => return Err(e),
            };
            if !is_relay_answer_of(&a, &t, &tag.name) {
                return Err(not_allowed(&name));
            }
            out.push(t);
        }
        Ok(out)
    }

    /// Every `tag/` ref, in ref order, with the tag record it names: a ref that is not a 32-byte
    /// hash, names no stored tag, or names a tag of another name is `Decode`.
    fn holder_tags(&self) -> Result<Vec<TagRef>, StoreError> {
        let mut out = vec![];
        for (refname, value) in self.refs_with_prefix(TAG_REF)? {
            let Ok(bytes) = <[u8; 32]>::try_from(value.as_slice()) else {
                return Err(bad(format!("{refname}: the ref is not a 32-byte hash")));
            };
            let hash = Hash(bytes);
            let tag = match self.read_tag(&hash) {
                Ok(t) => t,
                Err(StoreError::NotFound(_)) => {
                    return Err(bad(format!(
                        "{refname} names {hash}, which is not in the store"
                    )));
                }
                Err(StoreError::Decode(e)) => {
                    return Err(bad(format!(
                        "{refname} names {hash}, which is not a tag: {e}"
                    )));
                }
                Err(e) => return Err(e),
            };
            if refname[TAG_REF.len()..] != tag.name {
                return Err(bad(format!("{refname} holds the tag {:?}", tag.name)));
            }
            out.push(TagRef { refname, hash, tag });
        }
        Ok(out)
    }
}
