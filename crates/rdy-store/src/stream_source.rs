//! A checked record source without a store (D-D2NPN: "verify-on-receive"; D-48NYM; hub
//! r-7GS51: a stream render "with no store open"). [`CheckedStream`] checks one sync batch as a
//! take checks an upload, and then answers reads from it: a render reads the tagged state of a
//! stream that no holder or project ever stored.
//!
//! The completeness and removal checks here are the take's own (`crate::take_batch` calls the
//! same functions over the holder and the incoming files): every prerequisite is held, the root
//! TAG decodes, every incoming record is in the root's closure (forward links and the closure's
//! attachments), every incoming PLAN and ANSWER keeps its typed context as fsck checks a
//! holder's ([`check_typed_context`]), every new apply passes its portable proof (`sync_proof::check`), every dropped
//! record names a validated apply that approves its exact tuple, and each approved or dropped
//! key has one tuple. [`check_complete`] runs them over any [`ObjectSource`] with nothing held.
//! The take's step 7 (a present change that embeds an approved block version and that no
//! approval covers) reads only incoming records too: [`CheckedStream`] finds it with the same
//! rule as the take ([`CheckedStream::uncovered_change`]), and its caller refuses it with the
//! take's own error.
//! The receiver's own checks stay in the take: the repo and mode, the upload target, SIG's
//! subject rule over the holder, the scan of the holder's local copies, the tag collision policy
//! and the append.
//!
//! A [`CheckedStream`] holds no payload: it indexes each record's frame offset (or a dropped
//! record's descriptor), at most [`MAX_RECORDS`] descriptors, and reads a record again, with its
//! frame and hash checked, when a read needs it. A record that an approved removal covers reads
//! as its dropped identity, never as its bytes.
use crate::bounded;
use crate::error::StoreError;
use crate::kinds;
use crate::object_refs::{ObjectKey, ObjectRef};
use crate::plan::Place as PlanPlace;
use crate::segment::HEADER;
use crate::stream_error::{LIMIT_GRAPH_EDGES, LIMIT_OBJECT_RECORDS, Reason, StreamError};
use crate::sync_proof::{self, ObjectSource, PortableRemoval, RemovalObject};
use crate::sync_stream::{
    MAX_EDGES, MAX_RECORDS, SYNC_KINDS, StreamHeader, StreamRecord, check_batch_with,
};
use crate::take_batch::UploadFacts;
use rdy_model::redaction::{Redaction, RedactionApply, RedactionResidue};
use rdy_model::signature::Signature;
use rdy_model::{Answer, Block, BlockId, Hash, LandingRecord, StateSnapshot, Tag};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};

/// The refusal of a prerequisite that neither the incoming records nor the receiver holds.
pub(crate) fn missing(key: ObjectKey) -> StreamError {
    StreamError {
        code: "E_UNRESOLVED_REF",
        message: format!("stream prerequisite {key} is missing"),
        fix: "send the missing object with its dependencies".into(),
        record: Some(0),
        offset: 0,
        object: Some(key),
    }
}

/// E_BAD_RECORD `reason`, naming `object`.
pub(crate) fn bad(reason: Reason, object: ObjectKey) -> StreamError {
    StreamError::bad_record(0, 0, reason).with_object(Some(object))
}

/// A dropped record's descriptor: its original length, the apply that removed it and its
/// residue (none for a blob chunk).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Drop {
    pub(crate) original_len: u32,
    pub(crate) apply: Hash,
    pub(crate) residue: Option<Hash>,
}

/// How the shared checks look records up: through a record source, with cheaper answers where
/// the source keeps an index (a take's union, a checked stream). Each answer is what
/// [`ObjectSource::record`] would give.
pub(crate) trait Records {
    /// The source the proofs and typed reads go through.
    fn object_source(&mut self) -> &mut dyn ObjectSource;
    /// Whether the source holds a record of `key`, present or dropped.
    fn has(&mut self, key: &ObjectKey) -> Result<bool, StreamError>;
    /// The forward links of `key`'s records: a present record's references (a kind-None link as
    /// it is written), then each dropped representation's residue and apply. None when the
    /// source does not hold `key`.
    fn links(&mut self, key: ObjectKey) -> Result<Option<Vec<ObjectRef>>, StreamError>;
    /// The record number and frame offset of `key`'s present record in the incoming files, when
    /// the source indexes them; a refusal names them.
    fn position(&self, _key: &ObjectKey) -> Option<(u64, u64)> {
        None
    }
}

/// The forward links of one record: its references, then its removal proof.
fn record_links(key: ObjectKey, record: &StreamRecord) -> Result<Vec<ObjectRef>, StreamError> {
    let mut links = record
        .refs()
        .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(key)))?;
    links.extend(record.proof().map(|k| ObjectRef {
        kind: Some(k.kind),
        hash: k.hash,
    }));
    Ok(links)
}

/// Any record source, every answer read through [`ObjectSource::record`].
struct Plain<'a>(&'a mut dyn ObjectSource);

impl Records for Plain<'_> {
    fn object_source(&mut self) -> &mut dyn ObjectSource {
        &mut *self.0
    }

    fn has(&mut self, key: &ObjectKey) -> Result<bool, StreamError> {
        Ok(self.0.record(*key)?.is_some())
    }

    fn links(&mut self, key: ObjectKey) -> Result<Option<Vec<ObjectRef>>, StreamError> {
        match self.0.record(key)? {
            Some(r) => record_links(key, &r).map(Some),
            None => Ok(None),
        }
    }
}

/// The record `key`, decoded within its bounds; None when the source does not hold it present.
pub(crate) fn typed<T: serde::de::DeserializeOwned>(
    source: &mut dyn ObjectSource,
    key: ObjectKey,
) -> Result<Option<T>, StreamError> {
    match source.record(key)? {
        Some(StreamRecord::Present { payload, .. }) => bounded::decode(&payload)
            .map(Some)
            .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(key))),
        _ => Ok(None),
    }
}

/// Checks that every prerequisite is a record of the source: the first missing one, in key
/// order, is E_UNRESOLVED_REF.
pub(crate) fn check_prerequisites(
    rec: &mut dyn Records,
    prerequisites: &BTreeSet<ObjectKey>,
) -> Result<(), StreamError> {
    for p in prerequisites {
        if !rec.has(p)? {
            return Err(missing(*p));
        }
    }
    Ok(())
}

/// The root TAG `root_key`, decoded; a source without it is E_UNRESOLVED_REF.
pub(crate) fn root_tag(rec: &mut dyn Records, root_key: ObjectKey) -> Result<Tag, StreamError> {
    typed(rec.object_source(), root_key)?.ok_or_else(|| missing(root_key))
}

/// The key a link names: its own kind, or for a kind-None link the one record of the source of
/// its hash (none: kind 0; two or more: `ambiguous object kind`).
pub(crate) fn resolve(
    rec: &mut dyn Records,
    from: ObjectKey,
    r: ObjectRef,
) -> Result<ObjectKey, StreamError> {
    if let Some(kind) = r.kind {
        return Ok(ObjectKey { kind, hash: r.hash });
    }
    let mut found = None;
    for &kind in &SYNC_KINDS {
        let k = ObjectKey { kind, hash: r.hash };
        if rec.has(&k)? {
            if found.is_some() {
                return Err(bad(Reason::AmbiguousObjectKind, from));
            }
            found = Some(k);
        }
    }
    Ok(found.unwrap_or(ObjectKey {
        kind: 0,
        hash: r.hash,
    }))
}

/// The reverse attachments of the incoming records, by what triggers them.
#[derive(Default)]
struct Attachments {
    /// Answers by each hash they target.
    answers: HashMap<Hash, Vec<ObjectKey>>,
    /// Signatures by subject.
    signatures: HashMap<Hash, Vec<ObjectKey>>,
    /// Requests by carrier.
    requests: HashMap<Hash, Vec<ObjectKey>>,
    /// Applies by each object their request names.
    applies: HashMap<ObjectKey, Vec<ObjectKey>>,
    /// Sealed plans by the landing their seal names.
    seals: HashMap<Hash, Vec<ObjectKey>>,
    /// The relay answers of the root (O11).
    relay: Vec<ObjectKey>,
}

/// Collects the incoming records' attachments: every answer, signature, request and apply they
/// hold. The root's relay answer is one that targets exactly the root TAG
/// (`is_relay_answer_of`): an answer to a tag of the same name that targets another tag object
/// is not attached by name.
fn attachments(
    source: &mut dyn ObjectSource,
    root_key: ObjectKey,
    root: &Tag,
    input_keys: &[ObjectKey],
) -> Result<Attachments, StreamError> {
    let mut a = Attachments::default();
    let keys = input_keys.iter().filter(|k| {
        matches!(
            k.kind,
            kinds::ANSWER
                | kinds::SIGNATURE
                | kinds::REDACTION
                | kinds::REDACTION_APPLY
                | kinds::PLAN
        )
    });
    for k in keys.copied() {
        match k.kind {
            kinds::ANSWER => {
                let Some(answer) = typed::<Answer>(source, k)? else {
                    continue;
                };
                for t in &answer.targets {
                    a.answers.entry(*t).or_default().push(k);
                }
                if crate::holder::is_relay_answer_of(&answer, &root_key.hash, &root.name) {
                    a.relay.push(k);
                }
            }
            kinds::SIGNATURE => {
                if let Some(g) = typed::<Signature>(source, k)? {
                    a.signatures.entry(g.subject).or_default().push(k);
                }
            }
            kinds::REDACTION => {
                if let Some(r) = typed::<Redaction>(source, k)? {
                    a.requests.entry(r.carrier).or_default().push(k);
                }
            }
            kinds::PLAN => {
                if let Some(seal) = typed::<rdy_model::plan::Plan>(source, k)?.and_then(|p| p.seal)
                {
                    a.seals.entry(seal.landing).or_default().push(k);
                }
            }
            _ => {
                let Some(apply) = typed::<RedactionApply>(source, k)? else {
                    continue;
                };
                let request = ObjectKey {
                    kind: kinds::REDACTION,
                    hash: apply.request,
                };
                if let Some(r) = typed::<Redaction>(source, request)? {
                    for named in named_objects(&r) {
                        a.applies.entry(named).or_default().push(k);
                    }
                }
            }
        }
    }
    Ok(a)
}

/// The objects a request names, as the index's request entry names them: its spans' objects,
/// its versions and its blobs.
fn named_objects(r: &Redaction) -> BTreeSet<ObjectKey> {
    let m = &r.manifest;
    m.spans
        .iter()
        .map(|s| ObjectKey {
            kind: s.kind,
            hash: s.hash,
        })
        .chain(m.versions.iter().map(|(_, v)| ObjectKey {
            kind: kinds::BLOCK,
            hash: *v,
        }))
        .chain(m.blobs.iter().map(|b| ObjectKey {
            kind: kinds::BLOB_MANIFEST,
            hash: *b,
        }))
        .collect()
}

/// Checks that every incoming key is in the root's closure: reached from the root by forward
/// links (through every record of the source), or attached as the closure attaches it
/// (`crate::closure`): answers of reached landings and tags, signatures of reached landings and
/// of the root, the root's relay answer (O11), the sealed plans of reached landings, requests of
/// reached carriers, and applies whose request names a reached object. The first incoming key outside it, in key order, is
/// `object is outside the tag closure`. A record of the source that is not incoming (a holder's)
/// may be reached, and is never required to be.
pub(crate) fn check_closure(
    rec: &mut dyn Records,
    root_key: ObjectKey,
    root: &Tag,
    input_keys: &[ObjectKey],
) -> Result<(), StreamError> {
    let att = attachments(rec.object_source(), root_key, root, input_keys)?;
    let mut reached: HashSet<ObjectKey> = HashSet::new();
    let mut queue: Vec<ObjectKey> = Vec::new();
    let mut edges = 0u64;
    fn reach(
        k: ObjectKey,
        reached: &mut HashSet<ObjectKey>,
        queue: &mut Vec<ObjectKey>,
    ) -> Result<(), StreamError> {
        if reached.contains(&k) {
            return Ok(());
        }
        if reached.len() as u64 >= MAX_RECORDS {
            return Err(StreamError::limit(None, 0, LIMIT_OBJECT_RECORDS));
        }
        reached
            .try_reserve(1)
            .map_err(|_| StreamError::allocation(None, 0))?;
        queue
            .try_reserve(1)
            .map_err(|_| StreamError::allocation(None, 0))?;
        reached.insert(k);
        queue.push(k);
        Ok(())
    }
    reach(root_key, &mut reached, &mut queue)?;
    for r in &att.relay {
        reach(*r, &mut reached, &mut queue)?;
    }
    while let Some(k) = queue.pop() {
        // forward links
        let mut links: Vec<ObjectKey> = Vec::new();
        if let Some(refs) = rec.links(k)? {
            for r in refs {
                links.push(resolve(rec, k, r)?);
            }
        }
        // attachments
        if matches!(k.kind, kinds::LANDING | kinds::TAG) {
            links.extend(att.answers.get(&k.hash).into_iter().flatten());
        }
        if k.kind == kinds::LANDING || k == root_key {
            links.extend(att.signatures.get(&k.hash).into_iter().flatten());
        }
        if k.kind == kinds::LANDING {
            links.extend(att.seals.get(&k.hash).into_iter().flatten());
        }
        if k.kind == kinds::CHANGE {
            links.extend(att.requests.get(&k.hash).into_iter().flatten());
        }
        links.extend(att.applies.get(&k).into_iter().flatten());
        edges += links.len() as u64;
        if edges > MAX_EDGES {
            return Err(StreamError::limit(None, 0, LIMIT_GRAPH_EDGES));
        }
        for l in links {
            if l.kind != 0 {
                reach(l, &mut reached, &mut queue)?;
            }
        }
    }
    match input_keys.iter().find(|k| !reached.contains(k)) {
        Some(k) => Err(bad(Reason::OutsideTagClosure, *k)),
        None => Ok(()),
    }
}

/// The writers' and fsck's view of a plan, as the receive checks see it: an incoming record is
/// in the plan's own batch (a take writes one batch), a record the source holds otherwise is
/// the holder's, and the seals and bound proposals are the incoming plans checked so far and
/// the holder's tables. A stream error of a lookup is kept in `failed`: the store error the
/// plan check returns then only stops it.
struct ContextLookup<'a> {
    rec: RefCell<&'a mut dyn Records>,
    holder: Option<&'a crate::StoreReader>,
    incoming: &'a [ObjectKey],
    bound: HashMap<Hash, Hash>,
    sealed: HashMap<Hash, Hash>,
    failed: RefCell<Option<StreamError>>,
}

impl ContextLookup<'_> {
    /// `r`, with a stream error kept in `failed`.
    fn keep<T>(&self, r: Result<T, StreamError>) -> Result<T, StoreError> {
        r.map_err(|e| {
            *self.failed.borrow_mut() = Some(e);
            StoreError::Refused(String::new())
        })
    }

    /// Whether the record of `key` is incoming or the source holds it.
    fn holds(&self, key: &ObjectKey) -> Result<bool, StreamError> {
        Ok(self.incoming.binary_search(key).is_ok() || self.rec.borrow_mut().has(key)?)
    }

    /// The refusal of the record `key` that breaks `rule` (fsck's own text).
    fn broken(&self, key: ObjectKey, rule: &str) -> StreamError {
        let (n, offset) = self.rec.borrow().position(&key).unwrap_or((0, 0));
        StreamError::broken_rule(n, offset, rule).with_object(Some(key))
    }

    /// The error a failed plan check ends with: the kept stream error, else `e`'s.
    fn failure(&self, e: StoreError, key: ObjectKey) -> StreamError {
        self.failed
            .borrow_mut()
            .take()
            .unwrap_or_else(|| crate::closure::store_error(e, Some(key)))
    }
}

impl crate::plan::PlanLookup for ContextLookup<'_> {
    fn place(&self, kind: u8, h: &Hash) -> Result<PlanPlace, StoreError> {
        let key = ObjectKey { kind, hash: *h };
        if self.incoming.binary_search(&key).is_ok() {
            return Ok(PlanPlace::Batch);
        }
        Ok(if self.keep(self.rec.borrow_mut().has(&key))? {
            PlanPlace::Log
        } else {
            PlanPlace::Missing
        })
    }

    fn read(&self, kind: u8, h: &Hash) -> Result<Vec<u8>, StoreError> {
        let key = ObjectKey { kind, hash: *h };
        let record = self.keep(self.rec.borrow_mut().object_source().record(key))?;
        match record {
            Some(StreamRecord::Present { payload, .. }) => Ok(payload),
            // a removed record reads as removed, as fsck reads it: by the apply that removed it
            Some(StreamRecord::Dropped { apply, .. }) => {
                Err(StoreError::Redacted { request: apply })
            }
            None => Err(StoreError::NotFound(*h)),
        }
    }

    fn latest(&self, _: &str) -> Result<Option<Hash>, StoreError> {
        // a receiver has no view: its chains are checked as a holder's
        Ok(None)
    }

    fn bound(&self, p: &Hash) -> Result<Option<Hash>, StoreError> {
        if let Some(h) = self.bound.get(p) {
            return Ok(Some(*h));
        }
        match self.holder {
            Some(r) => Ok(r.index().plan_proposal(p)?.map(|(h, _)| h)),
            None => Ok(None),
        }
    }

    fn sealed(&self, l: &Hash) -> Result<Option<Hash>, StoreError> {
        if let Some(h) = self.sealed.get(l) {
            return Ok(Some(*h));
        }
        match self.holder {
            Some(r) => Ok(r.index().plan_seal(l)?.map(|(h, _)| h)),
            None => Ok(None),
        }
    }

    fn holder(&self) -> bool {
        true
    }
}

/// Checks the typed context of the incoming plans and plan answers before anything is written,
/// as fsck checks a holder's: each new PLAN keeps its chain, bound proposal and seal
/// (`crate::plan::invalid_context`) against the incoming records, the records `rec` holds and
/// the `holder`'s seal and proposal tables, and each new ANSWER keeps its typed target rule
/// (`crate::answer::invalid_targets`: a KeepSuspect names BLOCK versions only). Two different
/// seals of one landing, in the upload or against the holder, are `duplicate landing seal`; a
/// record the holder holds already is not incoming, so a repeated hash is accepted. `incoming`
/// is sorted. A break is E_BAD_RECORD with fsck's own text, `plan <h>: <reason>` or `answer <h>:
/// <reason>`. A take runs it over the upload and the holder; a checked stream with nothing held.
pub(crate) fn check_typed_context(
    rec: &mut dyn Records,
    holder: Option<&crate::StoreReader>,
    incoming: &[ObjectKey],
) -> Result<(), StreamError> {
    let mut look = ContextLookup {
        rec: RefCell::new(rec),
        holder,
        incoming,
        bound: HashMap::new(),
        sealed: HashMap::new(),
        failed: RefCell::new(None),
    };
    let grow = || StreamError::allocation(None, 0);
    for k in incoming.iter().copied() {
        match k.kind {
            kinds::PLAN => {
                let record = look.rec.get_mut().object_source().record(k)?;
                let Some(StreamRecord::Present { payload, .. }) = record else {
                    continue;
                };
                let p = crate::plan::decode_plan(&payload)
                    .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(k)))?;
                let rule = match crate::plan::invalid_record(&p) {
                    Some(rule) => Some(rule),
                    None => crate::plan::invalid_context(&k.hash, &p, &look)
                        .map_err(|e| look.failure(e, k))?,
                };
                if let Some(rule) = rule {
                    return Err(look.broken(k, &crate::plan::damage(&k.hash, rule)));
                }
                match &p.seal {
                    Some(seal) => {
                        look.sealed.try_reserve(1).map_err(|_| grow())?;
                        look.sealed.entry(seal.landing).or_insert(k.hash);
                    }
                    None => {
                        if let Some(prop) = p.proposal {
                            look.bound.try_reserve(1).map_err(|_| grow())?;
                            look.bound.entry(prop).or_insert(k.hash);
                        }
                    }
                }
            }
            kinds::ANSWER => {
                let Some(a) = typed::<Answer>(look.rec.get_mut().object_source(), k)? else {
                    continue;
                };
                let is_block = |t: &Hash| {
                    look.holds(&ObjectKey {
                        kind: kinds::BLOCK,
                        hash: *t,
                    })
                };
                if let Some(rule) = crate::answer::invalid_targets(&a, is_block)? {
                    return Err(look.broken(k, &crate::answer::damage(&k.hash, rule)));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Checks each apply of `applies` by its portable proof over the source, and returns what each
/// one authorizes, by apply hash.
pub(crate) fn check_applies(
    rec: &mut dyn Records,
    applies: impl IntoIterator<Item = Hash>,
) -> Result<BTreeMap<Hash, PortableRemoval>, StreamError> {
    let mut out = BTreeMap::new();
    for a in applies {
        out.insert(a, sync_proof::check(rec.object_source(), a)?);
    }
    Ok(out)
}

/// The approved tuples of each key: those of the `held` approvals in their order, then those of
/// the `incoming` ones in apply order.
pub(crate) fn approved_tuples<'a>(
    held: impl IntoIterator<Item = (Hash, &'a PortableRemoval)>,
    incoming: &BTreeMap<Hash, PortableRemoval>,
) -> BTreeMap<ObjectKey, Vec<(Hash, RemovalObject)>> {
    let mut approved: BTreeMap<ObjectKey, Vec<(Hash, RemovalObject)>> = BTreeMap::new();
    for (h, r) in held {
        for (k, t) in &r.objects {
            approved.entry(*k).or_default().push((h, *t));
        }
    }
    for (h, r) in incoming {
        for (k, t) in &r.objects {
            approved.entry(*k).or_default().push((*h, *t));
        }
    }
    approved
}

/// Whether dropped record `d` of `key` fits the removal its apply validated (`removal`): that
/// removal approves exactly the record's length and residue.
pub(crate) fn drop_fits(removal: Option<&PortableRemoval>, key: &ObjectKey, d: &Drop) -> bool {
    removal
        .and_then(|r| r.objects.get(key))
        .is_some_and(|t| t.len == d.original_len && t.residue == d.residue)
}

/// The one tuple of `key` over every approval and every dropped representation `drops`: two or
/// more (or none) is `invalid removal proof`.
pub(crate) fn one_tuple<'a>(
    approved: &BTreeMap<ObjectKey, Vec<(Hash, RemovalObject)>>,
    key: &ObjectKey,
    drops: impl IntoIterator<Item = &'a Drop>,
) -> Result<RemovalObject, StreamError> {
    let mut tuples: BTreeSet<(u32, Option<Hash>)> = BTreeSet::new();
    for (_, t) in approved.get(key).into_iter().flatten() {
        tuples.insert((t.len, t.residue));
    }
    for d in drops {
        tuples.insert((d.original_len, d.residue));
    }
    match tuples.into_iter().collect::<Vec<_>>()[..] {
        [(len, residue)] => Ok(RemovalObject { len, residue }),
        _ => Err(bad(Reason::InvalidRemovalProof, *key)),
    }
}

/// The relay answer of each TAG object of the source that has one among the incoming records
/// (tag hash to answer hash): an incoming answer of act `tag`, item `tag:<name>` and targets
/// exactly that TAG object, whose name is `<name>` (`crate::holder::is_relay_answer_of`). An
/// answer to a tag of the same name that targets another tag object is not that tag's. Two
/// relay answers of one tag object refuse at the second.
pub(crate) fn relay_answers(
    rec: &mut dyn Records,
    input_keys: &[ObjectKey],
) -> Result<BTreeMap<Hash, Hash>, StreamError> {
    let source = rec.object_source();
    let mut out: BTreeMap<Hash, Hash> = BTreeMap::new();
    for k in input_keys.iter().filter(|k| k.kind == kinds::ANSWER) {
        let Some(a) = typed::<Answer>(source, *k)? else {
            continue;
        };
        let [tag] = a.targets[..] else {
            continue;
        };
        if a.act != rdy_model::AnswerAct::Tag {
            continue;
        }
        // the tag object of the incoming records, or one the receiver holds already
        let tag_key = ObjectKey {
            kind: kinds::TAG,
            hash: tag,
        };
        let Some(t) = typed::<Tag>(source, tag_key)? else {
            continue;
        };
        if !crate::holder::is_relay_answer_of(&a, &tag, &t.name) {
            continue;
        }
        if out.contains_key(&tag) {
            return Err(bad(Reason::OutsideTagClosure, *k));
        }
        out.insert(tag, k.hash);
    }
    Ok(out)
}

/// The block versions the approvals `approved` cover.
pub(crate) fn approved_block_versions(
    approved: &BTreeMap<ObjectKey, Vec<(Hash, RemovalObject)>>,
) -> HashSet<Hash> {
    approved
        .keys()
        .filter(|k| k.kind == kinds::BLOCK)
        .map(|k| k.hash)
        .collect()
}

/// The take's step 7 for one present record that no approval covers: whether `key` is a CHANGE
/// whose payload embeds one of the approved block versions `versions` (a Create or Update that
/// writes it). Such a change needs its own approval (`TakeError::UncoveredChange`). A payload
/// that does not decode as a change embeds nothing.
pub(crate) fn embeds_approved(key: &ObjectKey, payload: &[u8], versions: &HashSet<Hash>) -> bool {
    key.kind == kinds::CHANGE
        && !versions.is_empty()
        && crate::store::decode::<rdy_model::Change>(payload)
            .is_ok_and(|c| crate::redaction_import::embeds(&c, versions))
}

/// The take's step 7 over the incoming records with nothing held: the first key of `input_keys`
/// (in key order) that is a present CHANGE no approval of `approved` covers and that embeds an
/// approved block version ([`embeds_approved`]). None when there is none.
pub(crate) fn uncovered_change(
    rec: &mut dyn Records,
    approved: &BTreeMap<ObjectKey, Vec<(Hash, RemovalObject)>>,
    input_keys: &[ObjectKey],
) -> Result<Option<Hash>, StreamError> {
    let versions = approved_block_versions(approved);
    if versions.is_empty() {
        return Ok(None);
    }
    let mut keys: Vec<&ObjectKey> = input_keys
        .iter()
        .filter(|k| k.kind == kinds::CHANGE && !approved.contains_key(k))
        .collect();
    keys.sort_unstable();
    for key in keys {
        if let Some(StreamRecord::Present { payload, .. }) = rec.object_source().record(*key)?
            && embeds_approved(key, &payload, &versions)
        {
            return Ok(Some(key.hash));
        }
    }
    Ok(None)
}

/// What the checks of a complete upload with nothing held found: its facts, every apply's
/// validated removal and the approved tuples of each key.
pub(crate) struct Complete {
    pub(crate) facts: UploadFacts,
    pub(crate) incoming: BTreeMap<Hash, PortableRemoval>,
    pub(crate) approved: BTreeMap<ObjectKey, Vec<(Hash, RemovalObject)>>,
}

/// How [`complete`] reads the dropped representations of one incoming key.
type DropsOf<'a> = dyn FnMut(&mut dyn Records, &ObjectKey) -> Result<Vec<Drop>, StreamError> + 'a;

/// The checks of [`check_complete`], over `rec`, with the dropped representations each incoming
/// key has (`drops`).
fn complete(
    rec: &mut dyn Records,
    headers: &[StreamHeader],
    input_keys: &[ObjectKey],
    drops: &mut DropsOf<'_>,
) -> Result<Complete, StreamError> {
    let Some(header) = headers.first() else {
        return Err(StreamError::bad_record(0, 0, Reason::NoBatchFiles));
    };
    for h in &headers[1..] {
        if (h.repo_id, h.purpose, h.tag, h.store_format)
            != (
                header.repo_id,
                header.purpose,
                header.tag,
                header.store_format,
            )
        {
            return Err(StreamError::bad_record(
                0,
                crate::sync_stream::MAGIC.len() as u64 + 4,
                Reason::BatchHeaderDiffers,
            ));
        }
    }
    let mut keys: Vec<ObjectKey> = input_keys.to_vec();
    keys.sort_unstable();
    keys.dedup();
    // every prerequisite of every header is a record of the source
    let prerequisites: BTreeSet<ObjectKey> = headers
        .iter()
        .flat_map(|h| h.prerequisites.iter().copied())
        .collect();
    check_prerequisites(rec, &prerequisites)?;
    // the root, and the closure
    let root_key = header.root();
    let root = root_tag(rec, root_key)?;
    check_closure(rec, root_key, &root, &keys)?;
    // the typed context of the plans and plan answers
    check_typed_context(rec, None, &keys)?;
    // the approvals: every apply's proof
    let incoming = check_applies(
        rec,
        keys.iter()
            .filter(|k| k.kind == kinds::REDACTION_APPLY)
            .map(|k| k.hash),
    )?;
    let approved = approved_tuples([], &incoming);
    // every dropped record names a validated apply that approves its exact tuple
    let mut dropped: Vec<(ObjectKey, Vec<Drop>)> = Vec::new();
    for key in keys
        .iter()
        .filter(|k| crate::redaction::is_removable(k.kind))
    {
        let ds = drops(rec, key)?;
        for d in &ds {
            if !drop_fits(incoming.get(&d.apply), key, d) {
                return Err(bad(Reason::InvalidRemovalProof, *key));
            }
        }
        if !ds.is_empty() {
            dropped.push((*key, ds));
        }
    }
    // each key the stream approves or drops has one tuple
    let incoming_keys: BTreeSet<ObjectKey> = incoming
        .values()
        .flat_map(|r| r.objects.keys().copied())
        .collect();
    let no_drops: Vec<Drop> = Vec::new();
    for key in &incoming_keys {
        let ds = dropped
            .binary_search_by(|(k, _)| k.cmp(key))
            .map_or(&no_drops, |i| &dropped[i].1);
        one_tuple(&approved, key, ds)?;
    }
    for (key, ds) in &dropped {
        one_tuple(&approved, key, ds)?;
    }
    let relay = relay_answers(rec, &keys)?;
    Ok(Complete {
        facts: UploadFacts {
            repo_id: header.repo_id,
            root_hash: root_key.hash,
            root,
            objects: keys,
            relay_answers: relay,
        },
        incoming,
        approved,
    })
}

/// Checks that the records of `source` make one complete upload with nothing held, as a take
/// checks the union of an upload's batch files: every prerequisite of every header is a record
/// of `source` (else E_UNRESOLVED_REF `stream prerequisite <key> is missing`, for the first
/// missing key in key order), the headers agree, the root TAG decodes, every key of
/// `input_keys` (the incoming objects) is in the root's closure, every PLAN and ANSWER among
/// them keeps its typed context ([`check_typed_context`]), every apply among them passes
/// its portable proof, every dropped record names a validated apply that approves its exact
/// tuple, and each approved or dropped key has one tuple. A record of `source` that is not in
/// `input_keys` is not incoming, and is never required to be reached. Returns the facts a take
/// gives its caller: the repo, the root, every incoming key (sorted) and the relay answers.
///
/// The batch rules of each file (`sync_stream::check_batch`) are the caller's to run first.
pub fn check_complete(
    source: &mut dyn ObjectSource,
    headers: &[StreamHeader],
    input_keys: &[ObjectKey],
) -> Result<UploadFacts, StreamError> {
    let mut rec = Plain(source);
    let mut drops =
        |rec: &mut dyn Records, key: &ObjectKey| match rec.object_source().record(*key)? {
            Some(StreamRecord::Dropped {
                original_len,
                apply,
                residue,
                ..
            }) => Ok(vec![Drop {
                original_len,
                apply,
                residue,
            }]),
            _ => Ok(vec![]),
        };
    Ok(complete(&mut rec, headers, input_keys, &mut drops)?.facts)
}

/// Where a stream holds one record: a present record's number, frame offset and payload length,
/// or a dropped record's descriptor.
#[derive(Clone, Copy, Debug)]
enum Place {
    Present { record: u64, offset: u64, len: u32 },
    Dropped(Drop),
}

/// One checked batch's records by key, read again from the input when a read needs them.
struct Indexed<R> {
    input: RefCell<R>,
    index: BTreeMap<ObjectKey, Place>,
    /// The approved removal that covers each key it covers: the first apply and its tuple.
    masked: BTreeMap<ObjectKey, (Hash, RemovalObject)>,
    /// The request that removed each object an approved request removes or names.
    removed: BTreeMap<ObjectKey, Hash>,
}

impl<R: Read + Seek> Indexed<R> {
    /// The record of `key`: a covered key as its dropped identity, a present record read again
    /// from its frame (its header must still be the frame indexed, and its payload its hash), a
    /// dropped one as its descriptor.
    fn read(&self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        if let Some((apply, t)) = self.masked.get(&key) {
            return Ok(Some(StreamRecord::Dropped {
                key,
                original_len: t.len,
                apply: *apply,
                residue: t.residue,
            }));
        }
        match self.index.get(&key) {
            None => Ok(None),
            Some(Place::Dropped(d)) => Ok(Some(StreamRecord::Dropped {
                key,
                original_len: d.original_len,
                apply: d.apply,
                residue: d.residue,
            })),
            Some(Place::Present {
                record,
                offset,
                len,
            }) => {
                let payload = self.frame(key, *record, *offset, *len)?;
                Ok(Some(StreamRecord::Present { key, payload }))
            }
        }
    }

    /// The payload of the present frame of `key` at `offset`, checked against what was indexed.
    fn frame(
        &self,
        key: ObjectKey,
        record: u64,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, StreamError> {
        let changed = || {
            StreamError::bad_record(record, offset, Reason::InputChangedDuringTake)
                .with_object(Some(key))
        };
        let io =
            |e: std::io::Error| StreamError::io(Some(record), offset, &e).with_object(Some(key));
        let mut input = self.input.borrow_mut();
        input.seek(SeekFrom::Start(offset)).map_err(io)?;
        // a short read is input that changed; any other read failure stays an I/O failure
        let read = |e: std::io::Error| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                changed()
            } else {
                io(e)
            }
        };
        let mut head = [0u8; HEADER as usize];
        input.read_exact(&mut head).map_err(read)?;
        let got = u32::from_le_bytes(head[0..4].try_into().expect("4 bytes"));
        if got != len || head[4] != key.kind || head[5..37] != key.hash.0 {
            return Err(changed());
        }
        let mut payload = Vec::new();
        payload
            .try_reserve_exact(len as usize)
            .map_err(|_| StreamError::allocation(Some(record), offset).with_object(Some(key)))?;
        payload.resize(len as usize, 0);
        input.read_exact(&mut payload).map_err(read)?;
        if Hash::of(&payload) != key.hash {
            return Err(changed());
        }
        Ok(payload)
    }
}

impl<R: Read + Seek> ObjectSource for Indexed<R> {
    fn record(&mut self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        self.read(key)
    }
}

impl<R: Read + Seek> Records for Indexed<R> {
    fn object_source(&mut self) -> &mut dyn ObjectSource {
        self
    }

    fn has(&mut self, key: &ObjectKey) -> Result<bool, StreamError> {
        Ok(self.index.contains_key(key))
    }

    fn position(&self, key: &ObjectKey) -> Option<(u64, u64)> {
        match self.index.get(key) {
            Some(Place::Present { record, offset, .. }) => Some((*record, *offset)),
            _ => None,
        }
    }

    fn links(&mut self, key: ObjectKey) -> Result<Option<Vec<ObjectRef>>, StreamError> {
        match self.index.get(&key) {
            None => Ok(None),
            // a chunk names nothing; its bytes are not read for that
            Some(Place::Present { .. }) if key.kind == kinds::BLOB_CHUNK => Ok(Some(vec![])),
            Some(_) => match self.read(key)? {
                Some(r) => record_links(key, &r).map(Some),
                None => Ok(None),
            },
        }
    }
}

/// One sync batch, checked whole before anything reads it, as a take checks an upload with
/// nothing held (see the module docs), and then a source of its records. The input is read
/// again, a record at a time, whenever a read needs a payload.
pub struct CheckedStream<R: Read + Seek> {
    records: Indexed<R>,
    facts: UploadFacts,
    /// The first present change that embeds an approved block version and that no approval
    /// covers (the take's step 7).
    uncovered: Option<Hash>,
}

impl<R: Read + Seek> CheckedStream<R> {
    /// Checks the batch `input` holds, from its first byte: every rule of
    /// `sync_stream::check_batch` (framing, hashes, bounded typed records, record rules, trailer,
    /// end of file, order and exact prerequisites), then [`check_complete`] with nothing held,
    /// its records as the incoming ones. A batch with prerequisites is E_UNRESOLVED_REF for its
    /// first one. Each record approved removals cover then reads as its dropped identity.
    ///
    /// The take's step 7 is not a stream error: `open` records it, and a caller that reads the
    /// stream must refuse it as a take does ([`CheckedStream::uncovered_change`]).
    pub fn open(mut input: R) -> Result<CheckedStream<R>, StreamError> {
        input
            .seek(SeekFrom::Start(0))
            .map_err(|e| StreamError::io(Some(0), 0, &e))?;
        let mut index: BTreeMap<ObjectKey, Place> = BTreeMap::new();
        let report = check_batch_with(
            &mut input,
            |_| Ok::<(), StreamError>(()),
            |n, offset, r| {
                let place = match r {
                    StreamRecord::Present { payload, .. } => Place::Present {
                        record: n,
                        offset,
                        len: payload.len() as u32,
                    },
                    StreamRecord::Dropped {
                        original_len,
                        apply,
                        residue,
                        ..
                    } => Place::Dropped(Drop {
                        original_len: *original_len,
                        apply: *apply,
                        residue: *residue,
                    }),
                };
                index.insert(r.key(), place);
                Ok(())
            },
        )?;
        let keys: Vec<ObjectKey> = index.keys().copied().collect();
        let mut records = Indexed {
            input: RefCell::new(input),
            index,
            masked: BTreeMap::new(),
            removed: BTreeMap::new(),
        };
        // the index answers the dropped representations, with no read
        let dropped: BTreeMap<ObjectKey, Drop> = records
            .index
            .iter()
            .filter_map(|(k, p)| match p {
                Place::Dropped(d) => Some((*k, *d)),
                Place::Present { .. } => None,
            })
            .collect();
        let mut from_index = |_: &mut dyn Records, key: &ObjectKey| {
            Ok(dropped.get(key).copied().into_iter().collect())
        };
        let done = complete(
            &mut records,
            std::slice::from_ref(&report.header),
            &keys,
            &mut from_index,
        )?;
        // the take's step 7 over the incoming records; it reads no key an approval covers
        let uncovered = uncovered_change(&mut records, &done.approved, &keys)?;
        drop(keys);
        records.mask(&done)?;
        Ok(CheckedStream {
            records,
            facts: done.facts,
            uncovered,
        })
    }

    /// What the checks found: the repo, the root TAG and its hash, every key (sorted) and the
    /// relay answers.
    pub fn facts(&self) -> &UploadFacts {
        &self.facts
    }

    /// The take's step 7 with nothing held: the first CHANGE of the stream, in key order, that
    /// is present, that no approval covers, and that embeds a block version an approval of the
    /// stream removes. A take refuses such a stream (`TakeError::UncoveredChange` with this
    /// hash), so a reader must too: the change would keep the removed content readable. None
    /// when there is none.
    pub fn uncovered_change(&self) -> Option<Hash> {
        self.uncovered
    }

    /// Whether the stream holds a record of `key`, present or dropped.
    pub fn holds(&self, key: &ObjectKey) -> bool {
        self.records.index.contains_key(key)
    }

    /// The record of `key`, as [`ObjectSource::record`] gives it.
    pub fn read(&self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        self.records.read(key)
    }

    /// The approved request that removed the object `key`: one that removes its record, or one
    /// whose manifest names it (a block version or a blob manifest, which a removal marks and
    /// never drops). None when no approved removal of the stream does. The first such apply in
    /// apply order answers, as a take masks it.
    pub fn redaction_of(&self, key: &ObjectKey) -> Option<Hash> {
        self.records.removed.get(key).copied()
    }

    /// The state the root TAG names: its landing's snapshot and each of its blocks by id. A
    /// removed block version reads as the stub its approved residue gives, with its original
    /// version hash; one no residue describes reads as nothing.
    pub fn materialize_tag(
        &self,
    ) -> Result<(StateSnapshot, BTreeMap<BlockId, Block>), StreamError> {
        let landing_key = ObjectKey {
            kind: kinds::LANDING,
            hash: self.facts.root.landing,
        };
        let landing: LandingRecord = self
            .typed(landing_key)?
            .ok_or_else(|| StreamError::unresolved(0, 0, landing_key))?;
        let snapshot_key = ObjectKey {
            kind: kinds::SNAPSHOT,
            hash: landing.state,
        };
        let snapshot: StateSnapshot = self
            .typed(snapshot_key)?
            .ok_or_else(|| StreamError::unresolved(0, 0, snapshot_key))?;
        let mut blocks: BTreeMap<BlockId, Block> = BTreeMap::new();
        for (id, version) in &snapshot.blocks {
            let Some(block) = self.block(*version)? else {
                continue;
            };
            if block.id != *id {
                return Err(StreamError {
                    code: "E_BAD_RECORD",
                    message: format!(
                        "snapshot {}: block {id} names version {version}, which is a version of {}",
                        landing.state, block.id
                    ),
                    fix: "upload the history as its source project exported it".into(),
                    record: None,
                    offset: 0,
                    object: Some(snapshot_key),
                });
            }
            blocks.insert(id.clone(), block);
        }
        Ok((snapshot, blocks))
    }

    /// Block version `version` as a read sees it: its record, or for a removed version the stub
    /// its residue gives (None when no residue describes it).
    fn block(&self, version: Hash) -> Result<Option<Block>, StreamError> {
        let key = ObjectKey {
            kind: kinds::BLOCK,
            hash: version,
        };
        match self.read(key)? {
            Some(StreamRecord::Present { payload, .. }) => bounded::decode(&payload)
                .map(Some)
                .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(key))),
            Some(StreamRecord::Dropped {
                apply,
                residue: Some(residue),
                ..
            }) => {
                let apply_key = ObjectKey {
                    kind: kinds::REDACTION_APPLY,
                    hash: apply,
                };
                let residue_key = ObjectKey {
                    kind: kinds::REDACTION_RESIDUE,
                    hash: residue,
                };
                let apply: RedactionApply = self
                    .typed(apply_key)?
                    .ok_or_else(|| StreamError::unresolved(0, 0, apply_key))?;
                match self.typed::<RedactionResidue>(residue_key)? {
                    Some(RedactionResidue::Block(b)) if b.version == version => {
                        Ok(Some(b.stub(apply.request)))
                    }
                    Some(_) => Err(bad(Reason::InvalidRemovalProof, key)),
                    None => Err(StreamError::unresolved(0, 0, residue_key)),
                }
            }
            Some(StreamRecord::Dropped { residue: None, .. }) => Ok(None),
            None => Err(StreamError::unresolved(0, 0, key)),
        }
    }

    /// The present record `key`, decoded within its bounds; None when the stream does not hold
    /// it present.
    pub fn typed<T: serde::de::DeserializeOwned>(
        &self,
        key: ObjectKey,
    ) -> Result<Option<T>, StreamError> {
        match self.read(key)? {
            Some(StreamRecord::Present { payload, .. }) => bounded::decode(&payload)
                .map(Some)
                .map_err(|e| StreamError::from_record(0, 0, &e).with_object(Some(key))),
            _ => Ok(None),
        }
    }
}

impl<R: Read + Seek> Indexed<R> {
    /// Masks every key the approvals cover with its first apply's tuple, and records the request
    /// that removed each object the approvals remove or name, the first apply in apply order
    /// first.
    fn mask(&mut self, done: &Complete) -> Result<(), StreamError> {
        for (k, by) in &done.approved {
            if let Some((apply, t)) = by.first() {
                self.masked.insert(*k, (*apply, *t));
            }
        }
        for r in done.incoming.values() {
            for k in r.objects.keys() {
                self.removed.entry(*k).or_insert(r.request);
            }
            let request_key = ObjectKey {
                kind: kinds::REDACTION,
                hash: r.request,
            };
            let request: Redaction =
                typed(self, request_key)?.ok_or_else(|| missing(request_key))?;
            for (_, v) in &request.manifest.versions {
                self.removed
                    .entry(ObjectKey {
                        kind: kinds::BLOCK,
                        hash: *v,
                    })
                    .or_insert(r.request);
            }
            for b in &request.manifest.blobs {
                self.removed
                    .entry(ObjectKey {
                        kind: kinds::BLOB_MANIFEST,
                        hash: *b,
                    })
                    .or_insert(r.request);
            }
        }
        Ok(())
    }
}

impl<R: Read + Seek> ObjectSource for CheckedStream<R> {
    fn record(&mut self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        self.records.read(key)
    }
}

/// A shared checked stream reads as a record source too: every read is through `&self`.
impl<R: Read + Seek> ObjectSource for &CheckedStream<R> {
    fn record(&mut self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        self.records.read(key)
    }
}
