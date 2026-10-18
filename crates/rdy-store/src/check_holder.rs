//! The structural check of a holder (D-BPYN8, D-DAGNB): a holder keeps uploaded objects and tags
//! with no canon, so its fsck starts from the tags. Each `tag/<name>` names a tag record, and its
//! landing starts a chain that the landings' `prev` links follow. The tags' divergent histories
//! are checked one chain at a time, never against canon or the global change -> landing index.
//!
//! The check reads every object a tag reaches through its object links (`object_refs::refs_of`),
//! once per `ObjectKey`: snapshot versions, change dependencies, proposal, approval and relay
//! links, blob manifests and chunks. Then it checks each chain: the relay and evidence rules of
//! each landing (the project's own rule), each change landed once in the chain, and each landed
//! change's dependencies landed at or before its landing in that chain. The dependency DAG has no
//! cycle, the answers keep the answer rules, the signature table holds exactly its records, and
//! (O11) each `answer/tag:<name>` names the stored answer that relays its tag. There is no pack
//! validation: a holder has no pack.
//!
//! Every walk is iterative and bounded: [`MAX_LANDING_DEPTH`] landings in one chain,
//! [`MAX_OBJECTS`] objects and [`MAX_EDGES`] object links. Past a bound the check refuses with
//! `holder check exceeds {limit}` ([`holder_check_limit`] reads the limit back).
use crate::StoreReader;
use crate::check::StoreCheck;
use crate::error::StoreError;
use crate::holder::{ANSWER_TAG_REF, StoreMode, TAG_REF, not_allowed};
use crate::index::hash_from;
use crate::kinds;
use crate::object_refs::{ObjectKey, PORTABLE_KINDS, refs_of};
use rdy_model::{Answer, AnswerAct, Change, Hash, LandingRecord, Tag};
use std::collections::{BTreeSet, HashMap, HashSet};
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
}

impl<'a> Closure<'a> {
    fn new(reader: &'a StoreReader, limits: Limits) -> Closure<'a> {
        Closure {
            reader,
            limits,
            seen: HashSet::new(),
            stack: Vec::new(),
            edges: 0,
        }
    }

    /// Adds `key` to what the walk reads, once.
    fn add(&mut self, key: ObjectKey) -> Result<(), StoreError> {
        if self.seen.contains(&key) {
            return Ok(());
        }
        if self.seen.len() >= self.limits.objects {
            return Err(exceeds(format!("{} objects", self.limits.objects)));
        }
        self.seen.insert(key);
        self.stack.push(key);
        Ok(())
    }

    /// Reads every object the added ones reach. A missing object is `Decode` naming the object
    /// whose link named it; a payload a removal erased has no links to follow (the removal's own
    /// checks cover it).
    fn run(&mut self) -> Result<(), StoreError> {
        while let Some(key) = self.stack.pop() {
            let payload = match self.reader.get_kind(key.kind, &key.hash) {
                Ok(p) => p,
                Err(StoreError::Redacted { .. }) => continue,
                Err(e) => return Err(e),
            };
            let refs =
                refs_of(key.kind, &payload).map_err(|e| bad(format!("object {key}: {e}")))?;
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
                self.add(ObjectKey { kind, hash: r.hash })?;
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

impl StoreReader {
    /// Checks a holder (see the module docs): its meta refs and ref history first, then its
    /// `answer/tag:` refs, its tags and every object they reach, each tag's chain, the
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

    fn chain_within(
        &self,
        head: &Hash,
        depth: usize,
        cache: &mut HashMap<Hash, Arc<LandingRecord>>,
    ) -> Result<Vec<(Hash, Arc<LandingRecord>)>, StoreError> {
        let mut out: Vec<(Hash, Arc<LandingRecord>)> = Vec::new();
        let mut on: HashSet<Hash> = HashSet::new();
        let mut cur = Some(*head);
        while let Some(h) = cur {
            if out.len() >= depth {
                return Err(exceeds(format!("{depth} landing depth")));
            }
            if !on.insert(h) {
                return Err(bad(format!("landing {h} is on its own chain twice")));
            }
            let rec = match cache.get(&h) {
                Some(r) => Arc::clone(r),
                None => {
                    let r = Arc::new(self.landing(&h)?);
                    cache.insert(h, Arc::clone(&r));
                    r
                }
            };
            cur = rec.prev;
            out.push((h, rec));
        }
        Ok(out)
    }

    fn check_holder_within(&self, limits: Limits) -> Result<StoreCheck, StoreError> {
        if self.mode()? != StoreMode::Holder {
            return Err(bad("the store is not a holder".to_string()));
        }
        // the holder rule first: the mode's history and the closed ref families
        self.check_meta()?;
        self.check_answer_tags()?;
        let tags = self.holder_tags()?;
        let mut closure = Closure::new(self, limits);
        for t in &tags {
            closure.add(ObjectKey {
                kind: kinds::TAG,
                hash: t.hash,
            })?;
        }
        closure.run()?;
        // each chain on its own: never the global change -> landing index
        let mut landings: HashMap<Hash, Arc<LandingRecord>> = HashMap::new();
        let mut fields_checked: HashSet<Hash> = HashSet::new();
        let mut changes: HashMap<Hash, Option<Arc<Change>>> = HashMap::new();
        let mut landed: BTreeSet<Hash> = BTreeSet::new();
        let mut relayed: BTreeSet<Hash> = BTreeSet::new();
        for t in &tags {
            let chain = self.chain_within(&t.tag.landing, limits.landing_depth, &mut landings)?;
            // where each change of the chain landed: its position, newest first
            let mut at: HashMap<Hash, usize> = HashMap::new();
            for (i, (l, rec)) in chain.iter().enumerate() {
                if fields_checked.insert(*l) {
                    self.check_landing_fields(l, rec)?;
                }
                if rec.relayed_by.is_some() {
                    relayed.insert(*l);
                }
                for c in &rec.changes {
                    if let Some(j) = at.insert(*c, i) {
                        let first = chain[j].0;
                        return Err(bad(format!(
                            "{}: change {c} lands in landing {first} and in landing {l}",
                            t.refname
                        )));
                    }
                    landed.insert(*c);
                }
            }
            for (i, (l, rec)) in chain.iter().enumerate() {
                for c in &rec.changes {
                    let Some(ch) = self.change_for_dag(&mut changes, c)? else {
                        continue;
                    };
                    for dep in &ch.deps {
                        if !at.get(dep).is_some_and(|j| *j >= i) {
                            return Err(bad(format!(
                                "{}: change {c} of landing {l} depends on {dep}, which is not landed at or before it",
                                t.refname
                            )));
                        }
                    }
                }
            }
        }
        let roots: BTreeSet<Hash> = closure
            .seen
            .iter()
            .filter(|k| k.kind == kinds::CHANGE)
            .map(|k| k.hash)
            .collect();
        self.check_acyclic(&roots, &mut changes)?;
        let answers = self.check_answers()?;
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

    /// O11: each `answer/tag:<name>` ref stands beside `tag/<name>` and names a stored ANSWER
    /// whose act is `tag` and whose item is `tag:<name>`; otherwise `holder ref {name} is not
    /// allowed`.
    fn check_answer_tags(&self) -> Result<(), StoreError> {
        for (name, value) in self.refs_with_prefix(ANSWER_TAG_REF)? {
            let tag = &name[ANSWER_TAG_REF.len()..];
            if self.get_ref(&format!("{TAG_REF}{tag}"))?.is_none() {
                return Err(not_allowed(&name));
            }
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
            if a.act != AnswerAct::Tag || a.item != format!("tag:{tag}") {
                return Err(not_allowed(&name));
            }
        }
        Ok(())
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
