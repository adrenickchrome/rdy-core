//! The closure of a tag (D-Q0AVZ): every portable object a receiver needs to hold a tag's
//! history, in dependency order, as one sync batch carries it (`crate::sync_stream`).
//!
//! # What the closure holds
//!
//! The walk starts at the root TAG and follows each record's object links (`refs_of`; a
//! dropped object's link is its residue). Identity hashes (pins, card and report hashes,
//! residue identities, pair and content keys) are no links. A kind-None link resolves to the one
//! stored object of that hash; two are `ambiguous object kind`.
//!
//! Reverse attachments join it, because no forward link reaches them:
//!
//! - every ANSWER that targets a closure landing or tag;
//! - every SIGNATURE whose subject is a closure landing or the root tag (D-1Z951), read with
//!   `signatures_before`; its subject link then orders it after its subject;
//! - the ANSWER that the source ref `answer/tag:<root name>` names, when it has act Tag and item
//!   `tag:<root name>` (O11): a relayed tag's answer targets the tag (`put_tag_answered`), and a
//!   holder requires it with the tag;
//! - the REDACTION request whose carrier is a closure change;
//! - the active REDACTION_APPLY of every request that names a closure block version, change,
//!   blob manifest or chunk: a later removal proof, so a receiver can erase its own copies too.
//!
//! Each attachment's own links join the walk (an apply's request and its human landing, with
//! that landing's history), and attachments are taken again until no new key appears: a key is
//! taken once, so the walk ends. No other ref is followed.
//!
//! The answers have no index by target: one pass over the stored answers finds those that
//! target the closure's landings and tags not asked yet, after the other links and attachments
//! reach their fixed point; a pass runs again only when new landings or tags joined. A pass
//! keeps only the answers it attaches, so unrelated answers cost a read and nothing of the
//! closure's bounds. (An index of answers by target would make each pass a lookup, but it is a
//! new derived table of the index; the closure needs none for its bounds.)
//!
//! A key the receiver claims to hold (`known`) leaves the batch, but the walk goes on through
//! it: a held object does not mean its history or its later attachments are held (a quarantined
//! root's landing, a later removal proof of a held landing). A known key that a record of the
//! batch needs, or that is a removal proof a placeholder of the batch names, is a prerequisite;
//! one no record of the batch needs is not. The take verifies every prerequisite again; a claim
//! is no authorization. A known key the source does not hold is not walked.
//!
//! The upload's bounds are charged to the walk: at most [`MAX_RECORDS`] records and
//! [`MAX_EDGES`] distinct references of each record, summed, known records included.
//!
//! # Carried removals
//!
//! A carried record (kind REDACTION_CARRIED, D-0VG72) is never sent, and it is no removal proof.
//! A tag whose walk reaches an object that a carried span names, while no validated apply of
//! this store before the end covers that span (a held payload, or zeros only the carried record
//! explains), is refused with E_REDACTED (`StreamError::carried_content`), with the fix of the
//! store's mode. Other tags export as before; once an apply covers every such span, the tag
//! exports with that apply as its placeholders' proof.
//!
//! # The order
//!
//! Depth-first postorder over the batch's records, with each record's needed keys in `ObjectKey`
//! order: from the root first, then from each record not yet placed, in key order. Every record
//! follows what it needs of the batch; a removed object follows its residue; an apply may follow
//! what it removes (the later proof: a placeholder's apply is a reference, never an order). The
//! order depends on the set of records only, never on the order they were found or stored.
use crate::error::StoreError;
use crate::kinds;
use crate::object_refs::{ObjectKey, ObjectRef};
use crate::segment::{HEADER as FRAME_HEADER, Location};
use crate::store::StoreReader;
use crate::stream_error::{
    LIMIT_GRAPH_EDGES, LIMIT_HISTORY_DEPTH, LIMIT_OBJECT_RECORDS, LIMIT_PREREQUISITES,
    LIMIT_TOTAL_BYTES, Reason, StreamError,
};
use crate::sync_proof::ObjectSource;
use crate::sync_stream::{
    HEADER_FIXED, KEY_LEN, MAGIC, MAX_EDGES, MAX_HISTORY_DEPTH, MAX_PREREQUISITES, MAX_RECORDS,
    MAX_STREAM_BYTES, SYNC_KINDS, StreamHeader, StreamRecord, StreamTotals, StreamWriter,
    TRAILER_LEN, depth_of,
};
use rdy_model::{Answer, AnswerAct, Hash, Tag};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;

/// The end of every log: a closure read with it sees everything the reader's index holds.
pub const END_OF_LOG: Location = Location {
    segment: u32::MAX,
    offset: u64::MAX,
};

/// A tag's closure as one batch: its records in dependency order and its prerequisites, the
/// keys its records need that it does not hold (with the root TAG when it does not hold it), in
/// `ObjectKey` order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Closure {
    pub records: Vec<ObjectKey>,
    pub prerequisites: Vec<ObjectKey>,
}

/// The closure of the TAG `tag` that `reader` holds, less the keys in `known`.
pub fn closure(
    reader: &StoreReader,
    tag: Hash,
    known: &BTreeSet<ObjectKey>,
) -> Result<Closure, StreamError> {
    closure_before(reader, tag, known, END_OF_LOG)
}

/// [`closure`] as the log stood at `end` (a read state's captured end): an object, signature,
/// answer, request or apply whose frame lies at or after `end` is not held.
pub fn closure_before(
    reader: &StoreReader,
    tag: Hash,
    known: &BTreeSet<ObjectKey>,
    end: Location,
) -> Result<Closure, StreamError> {
    let root = ObjectKey {
        kind: kinds::TAG,
        hash: tag,
    };
    let mut w = Walk {
        source: StoreSource { reader, end },
        root,
        nodes: BTreeMap::new(),
        seen: HashSet::new(),
        queue: Vec::new(),
        edges: 0,
        asked: HashSet::new(),
        carried: uncovered_carried(reader, end)?,
    };
    w.reach(root)?;
    loop {
        // forward links and the indexed attachments, to their fixed point
        loop {
            while let Some(k) = w.queue.pop() {
                w.expand(k, known)?;
            }
            let fresh: Vec<ObjectKey> = w
                .nodes
                .iter()
                .filter(|(_, n)| !n.attached)
                .map(|(k, _)| *k)
                .collect();
            if fresh.is_empty() {
                break;
            }
            for k in fresh {
                for a in w.attachments(k)? {
                    w.reach(a)?;
                }
                w.nodes.get_mut(&k).expect("an expanded key").attached = true;
            }
        }
        // the answers of the landings and tags not asked yet: one pass over the answers
        let ask: HashSet<Hash> = w
            .nodes
            .keys()
            .filter(|k| matches!(k.kind, kinds::LANDING | kinds::TAG))
            .map(|k| k.hash)
            .filter(|h| !w.asked.contains(h))
            .collect();
        if ask.is_empty() {
            break;
        }
        w.attach_answers(&ask)?;
        w.asked.extend(ask);
    }
    // the batch: every record found that the receiver does not hold
    let children: BTreeMap<ObjectKey, Vec<ObjectKey>> = w
        .nodes
        .iter()
        .filter(|(k, _)| !known.contains(k))
        .map(|(k, n)| (*k, n.children.clone()))
        .collect();
    let records = dependency_order(root, &children)?;
    // the prerequisites: what the batch's records need or name as a proof, and do not hold
    let mut prerequisites: BTreeSet<ObjectKey> = BTreeSet::new();
    let mut body = 0u64;
    for k in children.keys() {
        let n = &w.nodes[k];
        body += FRAME_HEADER + n.len;
        for c in n.children.iter().chain(&n.proof) {
            if !children.contains_key(c) {
                prerequisites.insert(*c);
                if prerequisites.len() > MAX_PREREQUISITES {
                    return Err(StreamError::limit(None, 0, LIMIT_PREREQUISITES));
                }
            }
        }
    }
    if !children.contains_key(&root) {
        prerequisites.insert(root);
    }
    if prerequisites.len() > MAX_PREREQUISITES {
        return Err(StreamError::limit(None, 0, LIMIT_PREREQUISITES));
    }
    let header = (HEADER_FIXED + prerequisites.len() * KEY_LEN) as u64;
    let bytes =
        MAGIC.len() as u64 + 4 + FRAME_HEADER + header + body + FRAME_HEADER + TRAILER_LEN as u64;
    if bytes > MAX_STREAM_BYTES {
        return Err(StreamError::limit(None, 0, LIMIT_TOTAL_BYTES));
    }
    Ok(Closure {
        records,
        prerequisites: prerequisites.into_iter().collect(),
    })
}

/// The dependency order of the records `children` maps (each to the keys it needs, in
/// `ObjectKey` order): depth-first postorder from `root` when it is a record, then from each
/// record not yet placed, in key order. Keys outside `children` are prerequisites and are not
/// placed. Iterative; a key on the current path met again is `dependency cycle`, and a chain of
/// more than [`MAX_HISTORY_DEPTH`] records is E_LIMIT.
pub fn dependency_order(
    root: ObjectKey,
    children: &BTreeMap<ObjectKey, Vec<ObjectKey>>,
) -> Result<Vec<ObjectKey>, StreamError> {
    let keys: Vec<ObjectKey> = children.keys().copied().collect();
    let index = |k: &ObjectKey| keys.binary_search(k).ok();
    let kids: Vec<Vec<usize>> = children
        .values()
        .map(|c| c.iter().filter_map(index).collect())
        .collect();
    // 0: not met; 1: on the path; 2: placed
    let mut state = vec![0u8; keys.len()];
    let mut depth = vec![0u64; keys.len()];
    let mut out = Vec::new();
    out.try_reserve_exact(keys.len())
        .map_err(|_| StreamError::allocation(None, 0))?;
    let starts = index(&root).into_iter().chain(0..keys.len());
    for s in starts {
        if state[s] != 0 {
            continue;
        }
        let mut path: Vec<(usize, usize)> = vec![(s, 0)];
        state[s] = 1;
        while let Some((node, next)) = path.last_mut() {
            let node = *node;
            if let Some(&c) = kids[node].get(*next) {
                *next += 1;
                match state[c] {
                    0 => {
                        if path.len() as u64 >= MAX_HISTORY_DEPTH {
                            return Err(StreamError::limit(None, 0, LIMIT_HISTORY_DEPTH));
                        }
                        state[c] = 1;
                        path.push((c, 0));
                    }
                    1 => {
                        return Err(StreamError::bad_record(0, 0, Reason::DependencyCycle)
                            .with_object(Some(keys[c])));
                    }
                    _ => {}
                }
                continue;
            }
            path.pop();
            let n = out.len() as u64 + 1;
            depth[node] = depth_of(n, 0, kids[node].iter().map(|c| depth[*c]))
                .map_err(|e| e.with_object(Some(keys[node])))?;
            state[node] = 2;
            out.push(keys[node]);
        }
    }
    Ok(out)
}

/// Writes `records` (in their order, from `source`) as one batch with `header`, and returns
/// what its trailer states.
pub fn write_batch<W: Write>(
    source: &mut dyn ObjectSource,
    header: &StreamHeader,
    records: &[ObjectKey],
    out: W,
) -> Result<(W, StreamTotals), StreamError> {
    let mut w = StreamWriter::new(out, header)?;
    for k in records {
        let r = source
            .record(*k)?
            .ok_or_else(|| StreamError::unresolved(0, 0, *k))?;
        w.record(&r)?;
    }
    w.finish()
}

/// A store's records as a proof and batch source: each portable object whose first frame lies
/// before `end`, its original bytes, or its dropped identity when an applied removal holds that
/// frame (the original length, the apply and the residue).
pub struct StoreSource<'a> {
    pub reader: &'a StoreReader,
    pub end: Location,
}

impl StoreSource<'_> {
    /// Where the log holds the first frame of `kind` and `hash`, when it lies before `end`.
    fn at(&self, kind: u8, hash: &Hash) -> Result<Option<Location>, StreamError> {
        let key = ObjectKey { kind, hash: *hash };
        Ok(self
            .reader
            .index()
            .typed(kind, hash)
            .map_err(|e| store_error(e, Some(key)))?
            .filter(|l| *l < self.end))
    }

    /// Whether the log holds `kind` and `hash` before `end`.
    fn holds(&self, kind: u8, hash: &Hash) -> Result<bool, StreamError> {
        Ok(self.at(kind, hash)?.is_some())
    }
}

impl ObjectSource for StoreSource<'_> {
    fn record(&mut self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        let Some(loc) = self.at(key.kind, &key.hash)? else {
            return Ok(None);
        };
        let err = |e| store_error(e, Some(key));
        if let Some(s) = self.reader.index().redaction_span_at(&loc).map_err(err)? {
            return Ok(Some(StreamRecord::Dropped {
                key,
                original_len: s.span.len,
                apply: s.apply,
                residue: s.span.residue,
            }));
        }
        let payload = self.reader.get_kind(key.kind, &key.hash).map_err(err)?;
        Ok(Some(StreamRecord::Present { key, payload }))
    }
}

/// The objects that a carried span of a record before `end` names, while no validated apply of
/// the store before `end` covers that span, each with the first carried record (in log order)
/// that names it. A span covers when the active mask holds its frame with an apply as its proof:
/// a held span, or one only a carried record explains, does not.
fn uncovered_carried(
    reader: &StoreReader,
    end: Location,
) -> Result<BTreeMap<ObjectKey, Hash>, StreamError> {
    let index = reader.index();
    let mut out: BTreeMap<ObjectKey, Hash> = BTreeMap::new();
    let mut failed: Option<StreamError> = None;
    let mut visit = |c: crate::redaction_carried::CarriedAt| -> Result<(), StoreError> {
        for s in &c.record.spans {
            let loc = Location {
                segment: s.place.segment,
                offset: s.place.offset,
            };
            let covered = match index.redaction_span_at(&loc)? {
                Some(row) => index
                    .redaction_applied(&row.request)?
                    .is_some_and(|(apply, at)| apply == row.apply && at < end),
                None => false,
            };
            if !covered {
                let key = ObjectKey {
                    kind: s.kind,
                    hash: s.hash,
                };
                if !out.contains_key(&key) {
                    if out.len() >= MAX_SEEN {
                        failed = Some(StreamError::limit(None, 0, LIMIT_OBJECT_RECORDS));
                        return Err(StoreError::Refused("the carried pass stopped".into()));
                    }
                    out.insert(key, c.hash);
                }
            }
        }
        Ok(())
    };
    let walked = reader.visit_carried(end, &mut visit);
    if let Some(e) = failed {
        return Err(e);
    }
    walked.map_err(|e| store_error(e, None))?;
    Ok(out)
}

/// A store failure while a closure reads its source: an I/O failure is E_IO; a missing object
/// E_UNRESOLVED_REF; a stored record that does not decode E_BAD_RECORD `record does not decode`
/// (run rdy fsck at the source); a signing read over its bound E_LIMIT with that bound; any
/// other store refusal E_IO with the store's own text.
pub(crate) fn store_error(e: StoreError, object: Option<ObjectKey>) -> StreamError {
    match e {
        StoreError::Io(io) => StreamError::io(None, 0, &io).with_object(object),
        StoreError::NotFound(hash) => {
            let key = object.unwrap_or(ObjectKey { kind: 0, hash });
            StreamError::unresolved(0, 0, key)
        }
        StoreError::Decode(_) | StoreError::RedactionCorrupt { .. } => {
            StreamError::bad_record(0, 0, Reason::RecordDoesNotDecode).with_object(object)
        }
        StoreError::Refused(m) if m.starts_with("signing input exceeds ") => {
            StreamError::limit(None, 0, &m["signing input exceeds ".len()..]).with_object(object)
        }
        other => {
            StreamError::io(None, 0, &std::io::Error::other(other.to_string())).with_object(object)
        }
    }
}

/// The walk of one closure.
struct Walk<'a> {
    source: StoreSource<'a>,
    root: ObjectKey,
    /// The records found so far, each with the keys it needs; known records included.
    nodes: BTreeMap<ObjectKey, Node>,
    /// Every key met: a record found or queued, or a known key the source does not hold.
    seen: HashSet<ObjectKey>,
    queue: Vec<ObjectKey>,
    /// The distinct references of the records found, summed.
    edges: u64,
    /// The landings and tags whose answers a pass looked for.
    asked: HashSet<Hash>,
    /// The objects that a carried span names and no apply covers, each with the first carried
    /// record that names it.
    carried: BTreeMap<ObjectKey, Hash>,
}

/// One record of the closure.
struct Node {
    /// The keys it needs before it, in `ObjectKey` order, no repeats.
    children: Vec<ObjectKey>,
    /// The removal proof a placeholder names: its apply, a reference that keeps no order.
    proof: Option<ObjectKey>,
    /// Its frame's payload bytes in a batch.
    len: u64,
    /// Whether its attachments were taken.
    attached: bool,
}

/// The most keys a walk meets: its records and the known keys the source does not hold.
const MAX_SEEN: usize = MAX_RECORDS as usize + MAX_PREREQUISITES;

impl Walk<'_> {
    /// Notes `k`: a key met for the first time is queued. The bound is checked before the sets
    /// grow.
    fn reach(&mut self, k: ObjectKey) -> Result<(), StreamError> {
        if self.seen.contains(&k) {
            return Ok(());
        }
        if self.seen.len() >= MAX_SEEN {
            return Err(StreamError::limit(None, 0, LIMIT_OBJECT_RECORDS));
        }
        let grow = |_| StreamError::allocation(None, 0);
        self.seen.try_reserve(1).map_err(grow)?;
        self.queue.try_reserve(1).map_err(grow)?;
        self.seen.insert(k);
        self.queue.push(k);
        Ok(())
    }

    /// Reads record `k` and notes the keys it needs and the proof it names. A known key the
    /// source does not hold is left as it is.
    fn expand(&mut self, k: ObjectKey, known: &BTreeSet<ObjectKey>) -> Result<(), StreamError> {
        if self.nodes.len() as u64 >= MAX_RECORDS {
            return Err(StreamError::limit(None, 0, LIMIT_OBJECT_RECORDS));
        }
        if let Some(carried) = self.carried.get(&k) {
            return Err(self.carried_refusal(*carried).with_object(Some(k)));
        }
        let Some(record) = self.source.record(k)? else {
            if known.contains(&k) {
                return Ok(());
            }
            return Err(StreamError::unresolved(0, 0, k));
        };
        let refs = record
            .refs()
            .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(k)))?;
        let mut children = Vec::new();
        children
            .try_reserve_exact(refs.len())
            .map_err(|_| StreamError::allocation(None, 0))?;
        for r in refs {
            children.push(self.resolve(k, r)?);
        }
        children.sort_unstable();
        children.dedup();
        let proof = record.proof();
        self.edges += children.len() as u64 + u64::from(proof.is_some());
        if self.edges > MAX_EDGES {
            return Err(StreamError::limit(None, 0, LIMIT_GRAPH_EDGES).with_object(Some(k)));
        }
        for c in children.iter().chain(&proof) {
            self.reach(*c)?;
        }
        self.nodes.insert(
            k,
            Node {
                children,
                proof,
                len: record.frame_len(),
                attached: false,
            },
        );
        Ok(())
    }

    /// The refusal of this walk's tag for reaching content that carried record `carried` names.
    fn carried_refusal(&self, carried: Hash) -> StreamError {
        let reader = self.source.reader;
        let err = |e| store_error(e, Some(self.root));
        let name = match reader.get_kind(kinds::TAG, &self.root.hash) {
            Ok(payload) => match crate::bounded::decode::<Tag>(&payload) {
                Ok(t) => t.name,
                Err(e) => return StreamError::from_record(0, 0, &e).with_object(Some(self.root)),
            },
            Err(e) => return err(e),
        };
        match reader.mode() {
            Ok(mode) => StreamError::carried_content(&name, carried, mode),
            Err(e) => err(e),
        }
    }

    /// The key a link of record `from` names: its own kind, or for a kind-None link the one
    /// stored portable object of its hash.
    fn resolve(&self, from: ObjectKey, r: ObjectRef) -> Result<ObjectKey, StreamError> {
        if let Some(kind) = r.kind {
            return Ok(ObjectKey { kind, hash: r.hash });
        }
        let mut found = None;
        for &kind in &SYNC_KINDS {
            if self.source.holds(kind, &r.hash)? {
                if found.is_some() {
                    return Err(StreamError::bad_record(0, 0, Reason::AmbiguousObjectKind)
                        .with_object(Some(from)));
                }
                found = Some(ObjectKey { kind, hash: r.hash });
            }
        }
        // kind 0: no stored object of any kind has the hash
        Ok(found.unwrap_or(ObjectKey {
            kind: 0,
            hash: r.hash,
        }))
    }

    /// The indexed reverse attachments of record `k` (see the module docs); the answers that
    /// target it come from [`Walk::attach_answers`].
    fn attachments(&mut self, k: ObjectKey) -> Result<Vec<ObjectKey>, StreamError> {
        let mut out = Vec::new();
        let err = |e| store_error(e, Some(k));
        let reader = self.source.reader;
        let end = self.source.end;
        match k.kind {
            kinds::LANDING | kinds::TAG => {
                if k.kind == kinds::LANDING || k == self.root {
                    for g in reader.signatures_before(&k.hash, end).map_err(err)? {
                        out.push(ObjectKey {
                            kind: kinds::SIGNATURE,
                            hash: g,
                        });
                    }
                }
                if k == self.root {
                    out.extend(self.relay_answer()?);
                }
            }
            kinds::CHANGE => {
                if let Some(r) = reader.redaction_for_carrier(&k.hash).map_err(err)?
                    && self.source.holds(kinds::REDACTION, &r)?
                {
                    out.push(ObjectKey {
                        kind: kinds::REDACTION,
                        hash: r,
                    });
                }
            }
            _ => {}
        }
        if matches!(
            k.kind,
            kinds::BLOCK | kinds::CHANGE | kinds::BLOB_MANIFEST | kinds::BLOB_CHUNK
        ) {
            let index = reader.index();
            for request in index
                .redaction_requests_naming(k.kind, &k.hash)
                .map_err(err)?
            {
                if let Some((apply, at)) = index.redaction_applied(&request).map_err(err)?
                    && at < end
                {
                    out.push(ObjectKey {
                        kind: kinds::REDACTION_APPLY,
                        hash: apply,
                    });
                }
            }
        }
        Ok(out)
    }

    /// One pass over the answers before the end: each answer not met yet that targets a hash of
    /// `ask` joins the walk. Nothing else of an answer is kept, so an unrelated answer charges
    /// nothing to the closure's bounds.
    fn attach_answers(&mut self, ask: &HashSet<Hash>) -> Result<(), StreamError> {
        let reader = self.source.reader;
        let mut source = StoreSource {
            reader,
            end: self.source.end,
        };
        let mut failed: Option<StreamError> = None;
        let mut visit = |h: Hash, w: &mut Self| -> Result<(), StreamError> {
            let key = ObjectKey {
                kind: kinds::ANSWER,
                hash: h,
            };
            if w.seen.contains(&key) {
                return Ok(());
            }
            let Some(StreamRecord::Present { payload, .. }) = source.record(key)? else {
                return Ok(());
            };
            let a: Answer = crate::bounded::decode(&payload)
                .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(key)))?;
            if a.targets.iter().any(|t| ask.contains(t)) {
                w.reach(key)?;
            }
            Ok(())
        };
        let walked = reader.for_each_object_of(kinds::ANSWER, |h| {
            visit(h, self).map_err(|e| {
                failed = Some(e);
                StoreError::Refused("the closure's answer pass stopped".into())
            })
        });
        if let Some(e) = failed {
            return Err(e);
        }
        walked.map_err(|e| store_error(e, None))
    }

    /// The ANSWER that the ref `answer/tag:<root name>` names, when it has act Tag and that item
    /// and lies before the end (O11).
    fn relay_answer(&mut self) -> Result<Option<ObjectKey>, StreamError> {
        let root = self.root;
        let Some(StreamRecord::Present { payload, .. }) = self.source.record(root)? else {
            return Ok(None);
        };
        let tag: Tag = crate::bounded::decode(&payload)
            .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(root)))?;
        let item = format!("tag:{}", tag.name);
        let Some((h, a)) = self
            .source
            .reader
            .answer_of(&item)
            .map_err(|e| store_error(e, Some(root)))?
        else {
            return Ok(None);
        };
        if a.act != AnswerAct::Tag || a.item != item || !self.source.holds(kinds::ANSWER, &h)? {
            return Ok(None);
        }
        Ok(Some(ObjectKey {
            kind: kinds::ANSWER,
            hash: h,
        }))
    }
}
