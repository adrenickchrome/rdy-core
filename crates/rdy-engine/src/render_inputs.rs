//! The rendering inputs a publication records (D-KDFZ3): everything besides the tagged source
//! that decides an artifact's bytes. Replaying equal inputs over the same source renders the same
//! bytes on every renderer path.
//!
//! `capture` takes the source's settings: this rdy's version, the `rdy-standard` theme, the
//! effective pack in full, and the project's title and description as the read state parsed
//! them. When the config sets no name, the title is the project directory's name (config.rs
//! `site_identity`), so two unnamed clones can capture different titles; a shared `.rdy.yaml`
//! name makes the title the same in every clone. No original YAML, config path, remote,
//! credential or signing setting enters the inputs.
//!
//! The disagreement list is the reader projection of D-QWSTV: one of six fixed sentences, the
//! blocks and the lead block, in captured order. `capture` leaves it empty; the renderer's
//! scope-aware capture fills it. No stored review prose or caller-authored sentence enters it.
//!
//! These are transient types: no store record holds them.
use crate::error::{E_BAD_INPUT, E_LIMIT, RdyError};
use crate::read_state::ReadState;
use rdy_model::BlockId;
use rdy_schema::Pack;
use serde::{Deserialize, Serialize, ser};
use std::collections::BTreeSet;
use std::fmt;
use std::io;

/// The version of this rdy: the compiled workspace version.
pub const RDY_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The one theme this renderer draws.
pub const THEME: &str = "rdy-standard";

/// Most bytes of the inputs' compact JSON.
pub const MAX_INPUT_BYTES: usize = 8_388_608;
/// Deepest nesting of objects and lists.
pub const MAX_INPUT_DEPTH: usize = 32;
/// Most map members and list elements in total.
pub const MAX_INPUT_VALUES: usize = 200_000;
/// Most bytes in one string (a key or a value).
pub const MAX_INPUT_SCALAR_BYTES: usize = 1_048_576;
/// Most bytes in the title.
pub const MAX_TITLE_BYTES: usize = 4096;
/// Most bytes in the description.
pub const MAX_DESCRIPTION_BYTES: usize = 65_536;
/// Most disagreement rows.
pub const MAX_DISAGREEMENT_ROWS: usize = 100_000;
/// Most block references across all disagreement rows.
pub const MAX_DISAGREEMENT_BLOCKS: usize = 100_000;

/// The reader sentence of each tension kind that persists in canon (D-QWSTV, D-84QSN), by the
/// kind's wire name. The table is closed.
pub const DISAGREEMENT_SENTENCES: [(&str, &str); 6] = [
    ("fork", "These pages give two versions of one point."),
    (
        "duplicate_definition",
        "These pages define the same thing twice.",
    ),
    (
        "deprecated_reference",
        "This page still points to something that was retired.",
    ),
    (
        "value_disagreement",
        "These pages state different values for the same thing.",
    ),
    (
        "restatement_drift",
        "This page restates a point that has since changed.",
    ),
    (
        "semantic",
        "A reviewer found that these statements contradict each other.",
    ),
];

/// The reader sentence of tension kind `kind` (its wire name); `None` for a kind that has none.
pub fn disagreement_sentence(kind: &str) -> Option<&'static str> {
    DISAGREEMENT_SENTENCES
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, s)| *s)
}

/// One disagreement as a reader sees it: the sentence, the blocks it is about in captured order,
/// and the block that "this page" names, when the sentence names one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisagreementInput {
    pub sentence: String,
    pub blocks: Vec<BlockId>,
    pub lead: Option<BlockId>,
}

/// The rendering inputs of one publication.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderInputs {
    /// The rdy version that renders: this one.
    pub rdy: String,
    /// The effective pack, in full.
    pub pack: Pack,
    /// The theme: `rdy-standard`.
    pub theme: String,
    pub title: String,
    pub description: Option<String>,
    pub disagreements: Vec<DisagreementInput>,
}

/// The inputs of the source `read`: its settings, with no disagreement.
pub fn capture(read: &ReadState) -> Result<RenderInputs, RdyError> {
    let inputs = RenderInputs {
        rdy: RDY_VERSION.to_string(),
        pack: read.pack().clone(),
        theme: THEME.to_string(),
        title: read.project_name(),
        description: read.project_description(),
        disagreements: vec![],
    };
    validate_inputs(&inputs)?;
    Ok(inputs)
}

/// `E_BAD_INPUT`: inputs for another rdy version or theme, or with no title.
fn mismatch() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "render inputs do not match this renderer",
        "use this rdy version and the rdy-standard theme",
    )
}

/// `E_LIMIT`: inputs over `limit` (`8388608 bytes`, `32 depth`, ...).
fn over(limit: &str) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("render inputs exceed {limit}"),
        "reduce the render inputs to the stated limit",
    )
}

/// `E_BAD_INPUT`: a disagreement list that is no projection rdy captures.
fn bad_disagreements() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "render disagreement inputs are invalid",
        "capture the render inputs from the source again",
    )
}

/// Validates `inputs`: this renderer's version and theme, a title of 1 to 4096 bytes, a
/// description of at most 65,536 bytes, well-formed disagreement rows within their bounds, and
/// the bounds of the compact JSON (bytes, depth, values, scalar bytes).
pub fn validate_inputs(inputs: &RenderInputs) -> Result<(), RdyError> {
    if inputs.rdy != RDY_VERSION || inputs.theme != THEME || inputs.title.is_empty() {
        return Err(mismatch());
    }
    if inputs.title.len() > MAX_TITLE_BYTES {
        return Err(over("4096 title bytes"));
    }
    if inputs
        .description
        .as_ref()
        .is_some_and(|d| d.len() > MAX_DESCRIPTION_BYTES)
    {
        return Err(over("65536 description bytes"));
    }
    check_disagreements(&inputs.disagreements)?;
    let mut meter = Meter::default();
    inputs.serialize(&mut meter).map_err(MeterError::into_rdy)?;
    let mut sink = Bounded {
        out: None,
        len: 0,
        over: false,
    };
    write_json(inputs, &mut sink)
}

/// The compact JSON of `inputs` after `validate_inputs`. The writer stops at the byte limit, so
/// no more than 8,388,608 bytes are ever held.
pub fn to_bounded_json(inputs: &RenderInputs) -> Result<Vec<u8>, RdyError> {
    validate_inputs(inputs)?;
    let mut sink = Bounded {
        out: Some(Vec::new()),
        len: 0,
        over: false,
    };
    write_json(inputs, &mut sink)?;
    Ok(sink.out.unwrap_or_default())
}

fn write_json(inputs: &RenderInputs, sink: &mut Bounded) -> Result<(), RdyError> {
    match serde_json::to_writer(&mut *sink, inputs) {
        Ok(()) => Ok(()),
        Err(_) if sink.over => Err(over("8388608 bytes")),
        Err(e) if e.io_error_kind() == Some(io::ErrorKind::OutOfMemory) => {
            Err(crate::publication_manifest::allocation_failed())
        }
        // the meter has walked the same value, so nothing else fails here
        Err(_) => Err(mismatch()),
    }
}

/// The row and reference bounds, then each row's form: a sentence of the table, one or more
/// distinct valid blocks, a lead among them or none, and no row twice.
fn check_disagreements(rows: &[DisagreementInput]) -> Result<(), RdyError> {
    if rows.len() > MAX_DISAGREEMENT_ROWS {
        return Err(over("100000 disagreement rows"));
    }
    let mut refs: usize = 0;
    for r in rows {
        refs = refs
            .checked_add(r.blocks.len())
            .filter(|n| *n <= MAX_DISAGREEMENT_BLOCKS)
            .ok_or_else(|| over("100000 disagreement block references"))?;
    }
    let mut seen: BTreeSet<&DisagreementInput> = BTreeSet::new();
    for r in rows {
        let known = DISAGREEMENT_SENTENCES.iter().any(|(_, s)| *s == r.sentence);
        let mut blocks: BTreeSet<&BlockId> = BTreeSet::new();
        let distinct = r.blocks.iter().all(|b| blocks.insert(b));
        // a typed id may still be the empty `BlockId::MIN` sentinel, which no reader parses;
        // the lead is one of the blocks, so checking the blocks covers it too
        let valid = r.blocks.iter().all(|b| BlockId::parse(b.as_str()).is_ok());
        let lead_ok = r.lead.as_ref().is_none_or(|l| blocks.contains(l));
        if !known || r.blocks.is_empty() || !distinct || !valid || !lead_ok || !seen.insert(r) {
            return Err(bad_disagreements());
        }
    }
    Ok(())
}

/// A writer that counts bytes, keeps them when `out` is set, and fails past the byte limit.
struct Bounded {
    out: Option<Vec<u8>>,
    len: usize,
    over: bool,
}

impl io::Write for Bounded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = self
            .len
            .checked_add(buf.len())
            .filter(|n| *n <= MAX_INPUT_BYTES);
        let Some(len) = len else {
            self.over = true;
            return Err(io::Error::other("render inputs over the byte limit"));
        };
        if let Some(out) = self.out.as_mut() {
            out.try_reserve(buf.len())
                .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
            out.extend_from_slice(buf);
        }
        self.len = len;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// The structure bounds, on the typed value
// ---------------------------------------------------------------------------------------------

/// Walks a value as serde serializes it to JSON, checking its depth, its map members and list
/// elements in total, and each string's bytes. It writes nothing.
#[derive(Default)]
struct Meter {
    depth: usize,
    values: usize,
}

#[derive(Debug)]
enum MeterError {
    Over(&'static str),
    Other,
}

impl MeterError {
    fn into_rdy(self) -> RdyError {
        match self {
            MeterError::Over(limit) => over(limit),
            MeterError::Other => mismatch(),
        }
    }
}

impl fmt::Display for MeterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MeterError::Over(limit) => write!(f, "over {limit}"),
            MeterError::Other => f.write_str("not serializable"),
        }
    }
}

impl std::error::Error for MeterError {}

impl ser::Error for MeterError {
    fn custom<T: fmt::Display>(_: T) -> Self {
        MeterError::Other
    }
}

impl Meter {
    /// Opens `levels` nested containers.
    fn open(&mut self, levels: usize) -> Result<Nested<'_>, MeterError> {
        let depth = self.depth + levels;
        if depth > MAX_INPUT_DEPTH {
            return Err(MeterError::Over("32 depth"));
        }
        self.depth = depth;
        Ok(Nested {
            meter: self,
            levels,
        })
    }

    /// Counts one map member or list element.
    fn count(&mut self) -> Result<(), MeterError> {
        let n = self.values + 1;
        if n > MAX_INPUT_VALUES {
            return Err(MeterError::Over("200000 values"));
        }
        self.values = n;
        Ok(())
    }

    fn scalar(&self, len: usize) -> Result<(), MeterError> {
        if len > MAX_INPUT_SCALAR_BYTES {
            return Err(MeterError::Over("1048576 scalar bytes"));
        }
        Ok(())
    }

    /// The `{"variant": …}` wrapper of a non-unit enum variant: one member named `variant`.
    fn variant(&mut self, variant: &str, inner: usize) -> Result<Nested<'_>, MeterError> {
        self.scalar(variant.len())?;
        let nested = self.open(1 + inner)?;
        nested.meter.count()?;
        Ok(nested)
    }
}

/// The containers one compound value opened; `end` closes them.
struct Nested<'a> {
    meter: &'a mut Meter,
    levels: usize,
}

impl Nested<'_> {
    fn element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), MeterError> {
        self.meter.count()?;
        value.serialize(&mut *self.meter)
    }

    fn member<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) -> Result<(), MeterError> {
        self.meter.count()?;
        self.meter.scalar(key.len())?;
        value.serialize(&mut *self.meter)
    }

    fn close(self) -> Result<(), MeterError> {
        self.meter.depth -= self.levels;
        Ok(())
    }
}

impl<'a> ser::Serializer for &'a mut Meter {
    type Ok = ();
    type Error = MeterError;
    type SerializeSeq = Nested<'a>;
    type SerializeTuple = Nested<'a>;
    type SerializeTupleStruct = Nested<'a>;
    type SerializeTupleVariant = Nested<'a>;
    type SerializeMap = Nested<'a>;
    type SerializeStruct = Nested<'a>;
    type SerializeStructVariant = Nested<'a>;

    fn serialize_bool(self, _: bool) -> Result<(), MeterError> {
        Ok(())
    }
    fn serialize_i8(self, _: i8) -> Result<(), MeterError> {
        Ok(())
    }
    fn serialize_i16(self, _: i16) -> Result<(), MeterError> {
        Ok(())
    }
    fn serialize_i32(self, _: i32) -> Result<(), MeterError> {
        Ok(())
    }
    fn serialize_i64(self, _: i64) -> Result<(), MeterError> {
        Ok(())
    }
    fn serialize_u8(self, _: u8) -> Result<(), MeterError> {
        Ok(())
    }
    fn serialize_u16(self, _: u16) -> Result<(), MeterError> {
        Ok(())
    }
    fn serialize_u32(self, _: u32) -> Result<(), MeterError> {
        Ok(())
    }
    fn serialize_u64(self, _: u64) -> Result<(), MeterError> {
        Ok(())
    }
    fn serialize_f32(self, _: f32) -> Result<(), MeterError> {
        Ok(())
    }
    fn serialize_f64(self, _: f64) -> Result<(), MeterError> {
        Ok(())
    }
    fn serialize_char(self, c: char) -> Result<(), MeterError> {
        self.scalar(c.len_utf8())
    }
    fn serialize_str(self, v: &str) -> Result<(), MeterError> {
        self.scalar(v.len())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<(), MeterError> {
        // JSON writes bytes as a list of numbers
        let list = self.open(1)?;
        for _ in v {
            list.meter.count()?;
        }
        list.close()
    }
    fn serialize_none(self) -> Result<(), MeterError> {
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), MeterError> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), MeterError> {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), MeterError> {
        Ok(())
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<(), MeterError> {
        self.scalar(variant.len())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<(), MeterError> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), MeterError> {
        let nested = self.variant(variant, 0)?;
        value.serialize(&mut *nested.meter)?;
        nested.close()
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Nested<'a>, MeterError> {
        self.open(1)
    }
    fn serialize_tuple(self, _: usize) -> Result<Nested<'a>, MeterError> {
        self.open(1)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Nested<'a>, MeterError> {
        self.open(1)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Nested<'a>, MeterError> {
        self.variant(variant, 1)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Nested<'a>, MeterError> {
        self.open(1)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Nested<'a>, MeterError> {
        self.open(1)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Nested<'a>, MeterError> {
        self.variant(variant, 1)
    }
}

impl ser::SerializeSeq for Nested<'_> {
    type Ok = ();
    type Error = MeterError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), MeterError> {
        self.element(value)
    }
    fn end(self) -> Result<(), MeterError> {
        self.close()
    }
}

impl ser::SerializeTuple for Nested<'_> {
    type Ok = ();
    type Error = MeterError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), MeterError> {
        self.element(value)
    }
    fn end(self) -> Result<(), MeterError> {
        self.close()
    }
}

impl ser::SerializeTupleStruct for Nested<'_> {
    type Ok = ();
    type Error = MeterError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), MeterError> {
        self.element(value)
    }
    fn end(self) -> Result<(), MeterError> {
        self.close()
    }
}

impl ser::SerializeTupleVariant for Nested<'_> {
    type Ok = ();
    type Error = MeterError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), MeterError> {
        self.element(value)
    }
    fn end(self) -> Result<(), MeterError> {
        self.close()
    }
}

impl ser::SerializeMap for Nested<'_> {
    type Ok = ();
    type Error = MeterError;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), MeterError> {
        // a key is a string (or a unit variant's name) in JSON: it opens nothing
        self.meter.count()?;
        key.serialize(&mut *self.meter)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), MeterError> {
        value.serialize(&mut *self.meter)
    }
    fn end(self) -> Result<(), MeterError> {
        self.close()
    }
}

impl ser::SerializeStruct for Nested<'_> {
    type Ok = ();
    type Error = MeterError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), MeterError> {
        self.member(key, value)
    }
    fn end(self) -> Result<(), MeterError> {
        self.close()
    }
}

impl ser::SerializeStructVariant for Nested<'_> {
    type Ok = ();
    type Error = MeterError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), MeterError> {
        self.member(key, value)
    }
    fn end(self) -> Result<(), MeterError> {
        self.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn meter(v: &Value) -> Result<(), MeterError> {
        v.serialize(&mut Meter::default())
    }

    #[test]
    fn hpb1_the_meter_bounds_depth_values_and_scalars() {
        let nest = |n: usize| (0..n).fold(json!(0), |v, _| json!([v]));
        meter(&nest(32)).unwrap();
        assert!(matches!(
            meter(&nest(33)),
            Err(MeterError::Over("32 depth"))
        ));
        let obj = |n: usize| (0..n).fold(json!(0), |v, _| json!({ "k": v }));
        meter(&obj(32)).unwrap();
        assert!(matches!(meter(&obj(33)), Err(MeterError::Over("32 depth"))));
        // depth is the deepest path, not the sum of siblings
        meter(&json!([nest(31), nest(31), obj(31)])).unwrap();

        let list = |n: usize| Value::Array(vec![Value::Null; n]);
        meter(&list(200_000)).unwrap();
        assert!(matches!(
            meter(&list(200_001)),
            Err(MeterError::Over("200000 values"))
        ));
        // members and elements count together
        meter(&json!({"a": list(199_998), "b": 1})).unwrap();
        assert!(meter(&json!({"a": list(199_999), "b": 1})).is_err());

        meter(&json!("x".repeat(1_048_576))).unwrap();
        assert!(matches!(
            meter(&json!("x".repeat(1_048_577))),
            Err(MeterError::Over("1048576 scalar bytes"))
        ));
        let mut long_key = serde_json::Map::new();
        long_key.insert("k".repeat(1_048_577), Value::Null);
        assert!(matches!(
            meter(&Value::Object(long_key)),
            Err(MeterError::Over("1048576 scalar bytes"))
        ));
    }

    #[test]
    fn hpb1_the_meter_counts_like_json() {
        // enum variants open the wrapper object JSON writes
        #[derive(Serialize)]
        enum E {
            Unit,
            New(u8),
            Tuple(u8, u8),
            Struct { a: u8 },
        }
        for (v, values, depth_ok) in [
            (E::Unit, 0, 32),
            (E::New(1), 1, 31),
            (E::Tuple(1, 2), 3, 30),
            (E::Struct { a: 1 }, 2, 30),
        ] {
            let mut m = Meter::default();
            v.serialize(&mut m).unwrap();
            assert_eq!(m.values, values);
            assert_eq!(m.depth, 0, "every container closed");
            let as_json = serde_json::to_value(&v).unwrap();
            let mut n = Meter::default();
            as_json.serialize(&mut n).unwrap();
            assert_eq!(n.values, values, "the same count as the JSON value");
            let mut deep = Meter {
                depth: depth_ok,
                values: 0,
            };
            v.serialize(&mut deep).unwrap();
            let mut deeper = Meter {
                depth: depth_ok + 1,
                values: 0,
            };
            if depth_ok < 32 {
                assert!(v.serialize(&mut deeper).is_err());
            }
        }
    }
}
