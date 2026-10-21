//! The bounded log walk and the typed string visitor of a redaction report (D-3M81W, D-6B1W1,
//! D-33JR0).
//!
//! The walk lists every frame of the log through the store's record walker, dropped frames
//! included, under the frame and byte bounds. The visitor reads a decoded record through its own
//! `Serialize` impl and hands every `String` value to a callback: struct fields, enum payloads,
//! options, sequences, maps and map keys, at any length. Block ids are identifiers, not quote
//! strings: the visitor knows them by their type (the `BlockId` newtype struct), never by the
//! shape of a value. Hashes and raw byte vectors are never read as text. No `serde_json` copy of
//! a record is made. The visitor stops descending below a field nested deeper than the level
//! bound and says so; it never refuses a record for its depth.
//!
//! The matcher finds every complete source string in a scanned string in one pass over its
//! bytes (an Aho-Corasick automaton built once per report), so the cost of a scan grows with the
//! bytes scanned, not with the number of strings times the number of sources.
use super::Limits;
use crate::error::{E_BAD_INPUT, E_INTERNAL, RdyError, redaction_limit};
use rdy_store::StoreReader;
use rdy_store::kinds;
use rdy_store::redaction::{StoredFrame, limit};
use rdy_store::segment::Location;
use serde::ser::{self, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasherDefault, Hasher};

/// The bytes of a frame header (length, kind and hash).
const HEADER: u64 = 4 + 1 + 32;

/// The deepest nesting of containers the visitor follows. A field value counts its own levels
/// against `Limits::levels`; this guard bounds every other container. Below it the visitor
/// stops descending, as below a field that is too deep.
const MAX_VISIT_DEPTH: usize = 256;

/// The serde name of a block id's newtype struct (`rdy_model::BlockId`): an identifier, never a
/// quote string.
const BLOCK_ID: &str = "BlockId";

/// The serde name of the field value enum (`rdy_model::FieldValue`); its `List` and `Map`
/// variants are the nested field containers `Limits::levels` counts.
const FIELD_VALUE: &str = "FieldValue";

/// One frame of the log: where it is, its kind, object hash and payload length, and whether the
/// store still serves its payload.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame {
    pub(crate) loc: Location,
    pub(crate) kind: u8,
    pub(crate) hash: rdy_model::Hash,
    pub(crate) len: u32,
    pub(crate) present: bool,
}

/// Every frame in `[origin, upto)`, batch markers included. More than `limits.frames` frames, or
/// more than `limits.bytes` header and payload bytes, is `E_REDACTION_LIMIT` before anything
/// else is read.
pub(crate) fn walk(
    reader: &StoreReader,
    upto: Location,
    limits: &Limits,
) -> Result<Vec<Frame>, RdyError> {
    let origin = Location {
        segment: 0,
        offset: 0,
    };
    let mut out: Vec<Frame> = Vec::new();
    let mut bytes: u64 = 0;
    let mut over: Option<&'static str> = None;
    let walked = reader.for_each_record_from(origin, upto, |loc, frame, _| {
        let (kind, hash, len, present) = match frame {
            StoredFrame::Present {
                kind,
                hash,
                payload,
            } => (
                kind,
                hash,
                u32::try_from(payload.len()).unwrap_or(u32::MAX),
                true,
            ),
            StoredFrame::Dropped {
                kind, hash, len, ..
            } => (kind, hash, len, false),
        };
        let stop = |l: &'static str, over: &mut Option<&'static str>| {
            *over = Some(l);
            rdy_store::StoreError::Db("the redaction scan stopped at a bound".into())
        };
        if u64::try_from(out.len()).map_or(true, |n| n >= limits.frames) {
            return Err(stop(limit::FRAMES, &mut over));
        }
        bytes = match bytes
            .checked_add(HEADER)
            .and_then(|b| b.checked_add(u64::from(len)))
            .filter(|b| *b <= limits.bytes)
        {
            Some(b) => b,
            None => return Err(stop(limit::BYTES, &mut over)),
        };
        if out.try_reserve(1).is_err() {
            return Err(stop(limit::FRAMES, &mut over));
        }
        out.push(Frame {
            loc,
            kind,
            hash,
            len,
            present,
        });
        Ok(())
    });
    if let Some(l) = over {
        return Err(redaction_limit(l));
    }
    walked?;
    Ok(out)
}

/// How the report reads the text of one kind of record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextPolicy {
    /// Every `String` of the decoded record is a quote candidate.
    Strings,
    /// Only the record's name is text (`REF_UPDATE`); its value stays opaque.
    NameOnly,
    /// The record is reported under the identity of the object it replaces (residues).
    OriginalIdentity,
    /// Identifiers and numbers only, or opaque bytes: nothing to scan.
    NoText,
}

/// The assigned kinds of the store's closed registry, in code order. Code 12 is unassigned.
pub const ASSIGNED_KINDS: [u8; 23] = [
    kinds::BLOCK,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::BLOB_CHUNK,
    kinds::BLOB_MANIFEST,
    kinds::REF_UPDATE,
    kinds::TAG,
    kinds::ANSWER,
    kinds::SIGNATURE,
    kinds::VIEW_OP,
    kinds::BATCH_COMMIT,
    kinds::INTENT,
    kinds::ALT_SET,
    kinds::REVIEW,
    kinds::TENSION,
    kinds::TRANSITION,
    kinds::REDACTION,
    kinds::REDACTION_RESIDUE,
    kinds::REDACTION_APPLY,
    kinds::REDACTION_CARRIED,
    kinds::PLAN,
];

/// The text policy of `kind`; `None` for a kind the report has no visitor for. A new assigned
/// kind gets an arm here, or every report over a store holding it refuses.
pub fn text_policy(kind: u8) -> Option<TextPolicy> {
    match kind {
        kinds::BLOCK
        | kinds::CHANGE
        | kinds::PROPOSAL
        | kinds::LANDING
        | kinds::TAG
        | kinds::ANSWER
        | kinds::SIGNATURE
        | kinds::VIEW_OP
        | kinds::INTENT
        | kinds::ALT_SET
        | kinds::REVIEW
        | kinds::TENSION
        | kinds::TRANSITION
        | kinds::REDACTION
        | kinds::REDACTION_CARRIED
        | kinds::PLAN => Some(TextPolicy::Strings),
        kinds::REF_UPDATE => Some(TextPolicy::NameOnly),
        kinds::REDACTION_RESIDUE => Some(TextPolicy::OriginalIdentity),
        kinds::SNAPSHOT
        | kinds::BLOB_CHUNK
        | kinds::BLOB_MANIFEST
        | kinds::BATCH_COMMIT
        | kinds::REDACTION_APPLY => Some(TextPolicy::NoText),
        _ => None,
    }
}

/// Refuses the first of `kinds` with no text policy: the report never counts an unknown kind as
/// absent content (assessment support is a separate, explicit fact).
pub fn require_text_policies(kinds: &[u8]) -> Result<(), RdyError> {
    match kinds.iter().find(|k| text_policy(**k).is_none()) {
        Some(k) => Err(RdyError::new(
            E_INTERNAL,
            format!("the redaction report has no text policy for object kind {k}"),
            "report this as a bug in rdy",
        )),
        None => Ok(()),
    }
}

/// The error of a visit: the callback's own error, or a bound.
#[derive(Debug)]
pub(crate) struct VisitError(pub(crate) RdyError);

impl fmt::Display for VisitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for VisitError {}

impl ser::Error for VisitError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        VisitError(RdyError::new(
            E_BAD_INPUT,
            format!("a record could not be read for the redaction report: {msg}"),
            "Keep the store unchanged and inspect it with rdy fsck.",
        ))
    }
}

/// Calls `f` with every `String` value of `value` (map keys too), at any length, skipping block
/// ids by their type. The visit does not descend into a field container nested deeper than
/// `levels`, nor below the container guard; it returns true when it skipped anything that way.
pub(crate) fn visit_strings<T: Serialize + ?Sized>(
    value: &T,
    levels: usize,
    f: &mut dyn FnMut(&str),
) -> Result<bool, RdyError> {
    let mut v = Visitor {
        f,
        depth: 0,
        field_levels: 0,
        max_levels: levels,
        skipped: false,
    };
    value.serialize(&mut v).map_err(|e| e.0)?;
    Ok(v.skipped)
}

struct Visitor<'a> {
    f: &'a mut dyn FnMut(&str),
    /// Open containers of any kind.
    depth: usize,
    /// Open field containers (`FieldValue::List` and `FieldValue::Map`).
    field_levels: usize,
    max_levels: usize,
    /// Whether the visit skipped something below a bound.
    skipped: bool,
}

impl Visitor<'_> {
    fn enter(&mut self) {
        self.depth += 1;
        if self.depth > MAX_VISIT_DEPTH {
            self.skipped = true;
        }
    }

    /// Whether the open container is below the container guard: its elements are skipped.
    fn muted(&self) -> bool {
        self.depth > MAX_VISIT_DEPTH
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }
}

type R = Result<(), VisitError>;

impl<'a, 'b> ser::Serializer for &'a mut Visitor<'b> {
    type Ok = ();
    type Error = VisitError;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_bool(self, _: bool) -> R {
        Ok(())
    }
    fn serialize_i8(self, _: i8) -> R {
        Ok(())
    }
    fn serialize_i16(self, _: i16) -> R {
        Ok(())
    }
    fn serialize_i32(self, _: i32) -> R {
        Ok(())
    }
    fn serialize_i64(self, _: i64) -> R {
        Ok(())
    }
    fn serialize_u8(self, _: u8) -> R {
        Ok(())
    }
    fn serialize_u16(self, _: u16) -> R {
        Ok(())
    }
    fn serialize_u32(self, _: u32) -> R {
        Ok(())
    }
    fn serialize_u64(self, _: u64) -> R {
        Ok(())
    }
    fn serialize_f32(self, _: f32) -> R {
        Ok(())
    }
    fn serialize_f64(self, _: f64) -> R {
        Ok(())
    }
    fn serialize_char(self, _: char) -> R {
        Ok(())
    }
    fn serialize_str(self, v: &str) -> R {
        (self.f)(v);
        Ok(())
    }
    fn serialize_bytes(self, _: &[u8]) -> R {
        Ok(())
    }
    fn serialize_none(self) -> R {
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> R {
        value.serialize(self)
    }
    fn serialize_unit(self) -> R {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> R {
        Ok(())
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> R {
        Ok(())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, name: &'static str, value: &T) -> R {
        if name == BLOCK_ID {
            // an identifier, whatever its text
            return Ok(());
        }
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> R {
        if name == FIELD_VALUE && (variant == "List" || variant == "Map") {
            if self.field_levels >= self.max_levels {
                self.skipped = true;
                return Ok(());
            }
            self.field_levels += 1;
            let r = value.serialize(&mut *self);
            self.field_levels -= 1;
            return r;
        }
        value.serialize(self)
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self, VisitError> {
        self.enter();
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self, VisitError> {
        self.enter();
        Ok(self)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, VisitError> {
        self.enter();
        Ok(self)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, VisitError> {
        self.enter();
        Ok(self)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self, VisitError> {
        self.enter();
        Ok(self)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, VisitError> {
        self.enter();
        Ok(self)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, VisitError> {
        self.enter();
        Ok(self)
    }
    fn is_human_readable(&self) -> bool {
        // hashes take their byte form: never a string to scan
        false
    }
}

impl ser::SerializeSeq for &mut Visitor<'_> {
    type Ok = ();
    type Error = VisitError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> R {
        if self.muted() {
            return Ok(());
        }
        value.serialize(&mut **self)
    }
    fn end(self) -> R {
        self.leave();
        Ok(())
    }
}

impl ser::SerializeTuple for &mut Visitor<'_> {
    type Ok = ();
    type Error = VisitError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> R {
        if self.muted() {
            return Ok(());
        }
        value.serialize(&mut **self)
    }
    fn end(self) -> R {
        self.leave();
        Ok(())
    }
}

impl ser::SerializeTupleStruct for &mut Visitor<'_> {
    type Ok = ();
    type Error = VisitError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> R {
        if self.muted() {
            return Ok(());
        }
        value.serialize(&mut **self)
    }
    fn end(self) -> R {
        self.leave();
        Ok(())
    }
}

impl ser::SerializeTupleVariant for &mut Visitor<'_> {
    type Ok = ();
    type Error = VisitError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> R {
        if self.muted() {
            return Ok(());
        }
        value.serialize(&mut **self)
    }
    fn end(self) -> R {
        self.leave();
        Ok(())
    }
}

impl ser::SerializeMap for &mut Visitor<'_> {
    type Ok = ();
    type Error = VisitError;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> R {
        if self.muted() {
            return Ok(());
        }
        key.serialize(&mut **self)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> R {
        if self.muted() {
            return Ok(());
        }
        value.serialize(&mut **self)
    }
    fn end(self) -> R {
        self.leave();
        Ok(())
    }
}

impl ser::SerializeStruct for &mut Visitor<'_> {
    type Ok = ();
    type Error = VisitError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, _: &'static str, value: &T) -> R {
        if self.muted() {
            return Ok(());
        }
        value.serialize(&mut **self)
    }
    fn end(self) -> R {
        self.leave();
        Ok(())
    }
}

impl ser::SerializeStructVariant for &mut Visitor<'_> {
    type Ok = ();
    type Error = VisitError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, _: &'static str, value: &T) -> R {
        if self.muted() {
            return Ok(());
        }
        value.serialize(&mut **self)
    }
    fn end(self) -> R {
        self.leave();
        Ok(())
    }
}

/// No node, or no source.
const NONE: u32 = u32::MAX;

/// The hasher of the matcher's edge map: keys are (node, byte) pairs the matcher made itself, so a
/// fast multiplicative mix is enough.
#[derive(Clone, Copy, Debug, Default)]
struct EdgeHasher(u64);

impl EdgeHasher {
    fn mix(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl Hasher for EdgeHasher {
    fn finish(&self) -> u64 {
        self.0 ^ (self.0 >> 32)
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.mix(u64::from(*b));
        }
    }
    fn write_u8(&mut self, n: u8) {
        self.mix(u64::from(n));
    }
    fn write_u32(&mut self, n: u32) {
        self.mix(u64::from(n));
    }
}

type Edges = HashMap<(u32, u8), u32, BuildHasherDefault<EdgeHasher>>;

/// One step of the automaton from `s` on byte `b`, without failure links.
fn step(root: &[u32; 256], edges: &Edges, s: u32, b: u8) -> Option<u32> {
    if s == 0 {
        let n = root[usize::from(b)];
        (n != 0).then_some(n)
    } else {
        edges.get(&(s, b)).copied()
    }
}

/// An Aho-Corasick automaton over the bytes of the report's source strings, built once per
/// report. Node 0 is the root; every other node is a prefix of a source.
pub(crate) struct Matcher {
    /// The root's transitions (0: none).
    root: Box<[u32; 256]>,
    /// Every other node's transitions.
    edges: Edges,
    /// The longest proper suffix of each node that is also a node.
    fail: Vec<u32>,
    /// The source each node completes, or `NONE`.
    ends: Vec<u32>,
    /// The nearest node on each node's failure chain (itself excluded) that completes a source,
    /// or `NONE`.
    dict: Vec<u32>,
    /// How many sources it was built from.
    sources: usize,
}

/// A vector of `n` slots allocated fallibly, or the sources bound.
fn slots<T: Clone>(n: usize, fill: T) -> Result<Vec<T>, RdyError> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| redaction_limit(limit::SOURCES))?;
    v.resize(n, fill);
    Ok(v)
}

impl Matcher {
    /// The matcher of `sources`: distinct, non-empty strings. Its size is bounded by their total
    /// bytes, allocated fallibly; a set the automaton cannot index is `E_REDACTION_LIMIT`
    /// (`4096 source strings`).
    pub(crate) fn new(sources: &[String]) -> Result<Matcher, RdyError> {
        let too_many = || redaction_limit(limit::SOURCES);
        let most = sources
            .iter()
            .try_fold(1usize, |n, s| n.checked_add(s.len()))
            .ok_or_else(too_many)?;
        // node ids are u32, and NONE is reserved
        if u32::try_from(most).map_or(true, |n| n == NONE) || u32::try_from(sources.len()).is_err()
        {
            return Err(too_many());
        }
        let mut root = Box::new([0u32; 256]);
        let mut edges = Edges::default();
        edges.try_reserve(most - 1).map_err(|_| too_many())?;
        let mut ends: Vec<u32> = slots(0, NONE)?;
        let mut parent: Vec<u32> = slots(0, 0)?;
        let mut byte: Vec<u8> = slots(0, 0)?;
        let mut depth: Vec<u32> = slots(0, 0)?;
        for v in [&mut ends, &mut parent, &mut depth] {
            v.try_reserve_exact(most).map_err(|_| too_many())?;
        }
        byte.try_reserve_exact(most).map_err(|_| too_many())?;
        ends.push(NONE);
        parent.push(0);
        byte.push(0);
        depth.push(0);
        for (i, src) in sources.iter().enumerate() {
            let mut node = 0u32;
            for &b in src.as_bytes() {
                node = match step(&root, &edges, node, b) {
                    Some(n) => n,
                    None => {
                        // fits: there are at most `most` nodes, and `most` fits a u32
                        let n = ends.len() as u32;
                        let d = depth[node as usize] + 1;
                        ends.push(NONE);
                        parent.push(node);
                        byte.push(b);
                        depth.push(d);
                        if node == 0 {
                            root[usize::from(b)] = n;
                        } else {
                            edges.insert((node, b), n);
                        }
                        n
                    }
                };
            }
            ends[node as usize] = i as u32;
        }
        let count = ends.len();
        let mut fail: Vec<u32> = slots(count, 0)?;
        let mut dict: Vec<u32> = slots(count, NONE)?;
        // breadth first: a node's failure link is shallower than the node
        let mut order: Vec<u32> = slots(0, 0)?;
        order
            .try_reserve_exact(count.saturating_sub(1))
            .map_err(|_| too_many())?;
        order.extend(1..count as u32);
        order.sort_unstable_by_key(|v| depth[*v as usize]);
        for v in order {
            let (u, c) = (parent[v as usize], byte[v as usize]);
            let f = if u == 0 {
                0
            } else {
                let mut f = fail[u as usize];
                loop {
                    if let Some(n) = step(&root, &edges, f, c) {
                        break n;
                    }
                    if f == 0 {
                        break 0;
                    }
                    f = fail[f as usize];
                }
            };
            fail[v as usize] = f;
            dict[v as usize] = if ends[f as usize] != NONE {
                f
            } else {
                dict[f as usize]
            };
        }
        Ok(Matcher {
            root,
            edges,
            fail,
            ends,
            dict,
            sources: sources.len(),
        })
    }

    /// Marks in `hits` every source that occurs in `hay` as a byte substring.
    pub(crate) fn search(&self, hay: &str, hits: &mut Hits) {
        let mut s = 0u32;
        for &b in hay.as_bytes() {
            s = loop {
                if let Some(n) = step(&self.root, &self.edges, s, b) {
                    break n;
                }
                if s == 0 {
                    break 0;
                }
                s = self.fail[s as usize];
            };
            let mut o = if self.ends[s as usize] != NONE {
                s
            } else {
                self.dict[s as usize]
            };
            // a source already marked in this record had its whole chain marked with it
            while o != NONE && hits.mark(self.ends[o as usize]) {
                o = self.dict[o as usize];
            }
        }
    }
}

/// The sources one record holds: marked per record, without clearing per-source state.
pub(crate) struct Hits {
    stamp: Vec<u32>,
    round: u32,
    found: Vec<u32>,
}

impl Hits {
    /// Room for the sources of `m`, allocated fallibly.
    pub(crate) fn new(m: &Matcher) -> Result<Hits, RdyError> {
        let n = m.sources;
        let mut found = Vec::new();
        found
            .try_reserve_exact(n)
            .map_err(|_| redaction_limit(limit::SOURCES))?;
        Ok(Hits {
            stamp: slots(n, 0)?,
            round: 0,
            found,
        })
    }

    /// Starts a new record.
    pub(crate) fn begin(&mut self) {
        self.found.clear();
        self.round = self.round.wrapping_add(1);
        if self.round == 0 {
            self.stamp.fill(0);
            self.round = 1;
        }
    }

    /// Marks source `src`; false when this record had it already.
    fn mark(&mut self, src: u32) -> bool {
        let st = &mut self.stamp[src as usize];
        if *st == self.round {
            return false;
        }
        *st = self.round;
        self.found.push(src);
        true
    }

    /// The sources this record holds, by index, in the order found.
    pub(crate) fn found(&self) -> &[u32] {
        &self.found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn found(sources: &[String], hays: &[&str]) -> Vec<usize> {
        let m = Matcher::new(sources).unwrap();
        let mut hits = Hits::new(&m).unwrap();
        hits.begin();
        for h in hays {
            m.search(h, &mut hits);
        }
        let mut out: Vec<usize> = hits.found().iter().map(|i| *i as usize).collect();
        out.sort();
        out
    }

    fn naive(sources: &[String], hays: &[&str]) -> Vec<usize> {
        (0..sources.len())
            .filter(|i| hays.iter().any(|h| h.contains(sources[*i].as_str())))
            .collect()
    }

    #[test]
    fn rdx5_the_matcher_finds_overlapping_and_nested_sources() {
        let sources: Vec<String> = ["he", "she", "his", "hers", "a", "aa", "aaa", "é"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(found(&sources, &["ushers"]), vec![0, 1, 3]);
        assert_eq!(found(&sources, &["aa"]), vec![4, 5]);
        assert_eq!(
            found(&sources, &["h", "e"]),
            Vec::<usize>::new(),
            "no match spans strings"
        );
        assert_eq!(found(&sources, &["café"]), vec![4, 7]);
        // a new record starts empty; a repeated match is reported once
        let m = Matcher::new(&sources).unwrap();
        let mut hits = Hits::new(&m).unwrap();
        hits.begin();
        m.search("aaaa aaa", &mut hits);
        let mut f = hits.found().to_vec();
        f.sort();
        assert_eq!(f, vec![4, 5, 6]);
        hits.begin();
        assert!(hits.found().is_empty());
        m.search("his", &mut hits);
        assert_eq!(hits.found(), &[2]);
    }

    proptest! {
        #[test]
        fn rdx5_the_matcher_agrees_with_a_substring_search(
            raw in proptest::collection::vec("[abc]{1,5}", 1..12),
            hays in proptest::collection::vec("[abcd]{0,30}", 0..4),
        ) {
            let mut sources = raw;
            sources.sort();
            sources.dedup();
            let hays: Vec<&str> = hays.iter().map(String::as_str).collect();
            prop_assert_eq!(found(&sources, &hays), naive(&sources, &hays));
        }
    }
}
