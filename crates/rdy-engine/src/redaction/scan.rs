//! The bounded log walk and the typed string visitor of a redaction report (D-3M81W, D-6B1W1,
//! D-33JR0).
//!
//! The walk lists every frame of the log through the store's record walker, dropped frames
//! included, under the frame and byte bounds. The visitor reads a decoded record through its own
//! `Serialize` impl and hands every `String` value to a callback: struct fields, enum payloads,
//! options, sequences, maps and map keys. Block ids are identifiers, not quote strings; hashes
//! and raw byte vectors are never read as text. No `serde_json` copy of a record is made.
use super::Limits;
use crate::error::{E_BAD_INPUT, E_INTERNAL, RdyError, redaction_limit};
use rdy_model::BlockId;
use rdy_store::StoreReader;
use rdy_store::kinds;
use rdy_store::redaction::{StoredFrame, limit};
use rdy_store::segment::Location;
use serde::ser::{self, Serialize};
use std::fmt;

/// The bytes of a frame header (length, kind and hash).
const HEADER: u64 = 4 + 1 + 32;

/// The deepest nesting of containers the visitor follows. A field value counts its own levels
/// against `Limits::levels`; this guard bounds every other record.
const MAX_VISIT_DEPTH: usize = 256;

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

/// The assigned kinds of the store's closed registry, in code order. Codes 11 and 12 are
/// unassigned.
pub const ASSIGNED_KINDS: [u8; 20] = [
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
        | kinds::VIEW_OP
        | kinds::INTENT
        | kinds::ALT_SET
        | kinds::REVIEW
        | kinds::TENSION
        | kinds::TRANSITION
        | kinds::REDACTION => Some(TextPolicy::Strings),
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

/// Calls `f` with every `String` value of `value` (map keys too), skipping block ids. A string
/// longer than `limits.string` bytes is `E_REDACTION_LIMIT`.
pub(crate) fn visit_strings<T: Serialize + ?Sized>(
    value: &T,
    limits: &Limits,
    f: &mut dyn FnMut(&str),
) -> Result<(), RdyError> {
    let mut v = Visitor {
        f,
        depth: 0,
        max_string: limits.string,
    };
    value.serialize(&mut v).map_err(|e| e.0)
}

struct Visitor<'a> {
    f: &'a mut dyn FnMut(&str),
    depth: usize,
    max_string: usize,
}

impl Visitor<'_> {
    fn enter(&mut self) -> Result<(), VisitError> {
        self.depth += 1;
        if self.depth > MAX_VISIT_DEPTH {
            return Err(VisitError(redaction_limit(limit::LEVELS)));
        }
        Ok(())
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
        if v.len() > self.max_string {
            return Err(VisitError(redaction_limit(limit::STRING)));
        }
        if BlockId::parse(v).is_err() {
            (self.f)(v);
        }
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
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, value: &T) -> R {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        value: &T,
    ) -> R {
        value.serialize(self)
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self, VisitError> {
        self.enter()?;
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self, VisitError> {
        self.enter()?;
        Ok(self)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, VisitError> {
        self.enter()?;
        Ok(self)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, VisitError> {
        self.enter()?;
        Ok(self)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self, VisitError> {
        self.enter()?;
        Ok(self)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, VisitError> {
        self.enter()?;
        Ok(self)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, VisitError> {
        self.enter()?;
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
        key.serialize(&mut **self)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> R {
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
        value.serialize(&mut **self)
    }
    fn end(self) -> R {
        self.leave();
        Ok(())
    }
}
