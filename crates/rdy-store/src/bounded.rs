//! Bounded decoding of one typed record from outside bytes (D-D2NPN verify-on-receive,
//! quality-gate tb-A4DAV). The bytes are the record's postcard encoding, unchanged: this module
//! wraps postcard's own deserializer with serde's seed and access wrappers and adds no encoding.
//!
//! Each limit is checked before the value it bounds is built:
//!
//! - a record holds at most [`MAX_RECORD_BYTES`] encoded bytes, checked first;
//! - a text or byte string holds at most [`MAX_TEXT_BYTES`] bytes, checked when its length is
//!   read and before any visitor owns it;
//! - a sequence or map holds at most [`MAX_ELEMENTS`] elements: its length hint is checked
//!   before serde's collection visitor sees it, and the hint the visitor sees is never more than
//!   the bytes left, so a hostile hint cannot make a visitor preallocate;
//! - a record decodes at most [`MAX_VALUES`] values (see the counting rule below);
//! - containers (options, newtypes, sequences, tuples, maps, structs and enums) nest at most
//!   [`MAX_DEPTH`] deep: opening the next one is refused before the decoder recurses into it.
//!
//! The counting rule (O3): containers and leaves count, scalars do not. Each struct, newtype,
//! tuple, sequence, map, enum and option is one value, and so is each string, byte string and
//! unit. Fixed-width scalars (booleans, integers, floats and chars) are not values. A
//! [`Hash`](rdy_model::Hash) is 2 values and 2 nesting levels: a newtype around a 32-byte tuple.
//! So a snapshot entry is 3 values (its block id string and its hash). Why: the record bytes and
//! the element bound already limit scalars, and no stored type has a scalar sequence other than
//! bytes, so scalars cannot amplify memory. Counting them would refuse a 50,000-block snapshot.
//!
//! A raw BLOB_CHUNK has no typed decode; [`MAX_CHUNK_BYTES`] bounds it.
use serde::de::{
    self, DeserializeOwned, DeserializeSeed, Deserializer, EnumAccess, MapAccess, SeqAccess,
    VariantAccess, Visitor,
};
use std::cell::Cell;
use std::fmt;

/// The most encoded bytes of one typed record (the model's record bound).
pub const MAX_RECORD_BYTES: usize = rdy_model::redaction::MAX_RECORD_BYTES;
/// The most bytes of one text or byte string.
pub const MAX_TEXT_BYTES: usize = 1_048_576;
/// The most elements of one sequence or map.
pub const MAX_ELEMENTS: usize = 100_000;
/// The most decoded values of one record.
pub const MAX_VALUES: u64 = 1_000_000;
/// The deepest container nesting of one record.
pub const MAX_DEPTH: u32 = 64;
/// The most bytes of one raw BLOB_CHUNK.
pub const MAX_CHUNK_BYTES: usize = 1_048_576;

/// The limit name of [`MAX_RECORD_BYTES`].
pub const RECORD_BYTES: &str = "record bytes";
/// The limit name of [`MAX_TEXT_BYTES`].
pub const TEXT_BYTES: &str = "text bytes";
/// The limit name of [`MAX_ELEMENTS`].
pub const COLLECTION_ELEMENTS: &str = "collection elements";
/// The limit name of [`MAX_VALUES`].
pub const DECODED_VALUES: &str = "decoded values";
/// The limit name of [`MAX_DEPTH`].
pub const NESTING: &str = "nesting";
/// The limit name of [`MAX_CHUNK_BYTES`].
pub const CHUNK_BYTES: &str = "chunk bytes";

/// Why a record was refused. A closed list; the record layer returns no text of its own beyond
/// these short displays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    /// The kind is not an assigned portable object kind.
    #[error("unknown kind")]
    Kind,
    /// The bytes are not exactly one record of the kind: truncated input, trailing bytes, an
    /// invalid enum discriminant or option tag, a malformed varint, invalid UTF-8 or an invalid
    /// value (such as a malformed hash text where a hash is required).
    #[error("record does not decode")]
    Decode,
    /// A bound was exceeded. Displays as the bound followed by its name, such as
    /// `16777216 record bytes`.
    #[error("{maximum} {name}")]
    Limit { name: &'static str, maximum: u64 },
}

impl RecordError {
    /// The limit error of bound `maximum` named `name`.
    pub(crate) fn limit(name: &'static str, maximum: usize) -> RecordError {
        RecordError::Limit {
            name,
            maximum: maximum as u64,
        }
    }
}

/// Decodes `bytes` as exactly one `T`, within the limits above. Any input left after the value
/// is `Decode`.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, RecordError> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(RecordError::limit(RECORD_BYTES, MAX_RECORD_BYTES));
    }
    let gate = Gate::default();
    run(&gate, bytes)
}

/// A record's in-place shape: a type with the record's encoding whose decode borrows its texts
/// from the input and keeps no collection, so decoding it allocates nothing.
pub trait InPlace {
    /// Decodes one value of the shape from `d` and drops it.
    fn check<'de, D: Deserializer<'de>>(d: D) -> Result<(), D::Error>;
}

/// Checks that `bytes` decode as exactly one value of the shape `T`, within the limits above,
/// and returns the refusal [`decode`] gives the record `T` is the shape of. The check
/// allocates nothing, so a hostile record cannot make it abort on a failed allocation.
pub fn check<T: InPlace>(bytes: &[u8]) -> Result<(), RecordError> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(RecordError::limit(RECORD_BYTES, MAX_RECORD_BYTES));
    }
    let gate = Gate::default();
    let mut postcard = postcard::Deserializer::from_flavor(Source {
        rest: bytes,
        gate: &gate,
    });
    let value = T::check(Bounded {
        inner: &mut postcard,
        gate: &gate,
    });
    let left = postcard.finalize().map(|rest| rest.len());
    match (value, left) {
        (Ok(_), Ok(0)) => Ok(()),
        _ => Err(gate.limit.get().unwrap_or(RecordError::Decode)),
    }
}

/// A sequence decoded in place: each element decodes and is dropped, so the shape of a
/// `Vec<T>` keeps nothing. It decodes exactly as the `Vec<T>` does.
pub struct Each<T>(std::marker::PhantomData<T>);

impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Each<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);
        impl<'de, T: serde::Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Each<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a sequence")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Each<T>, A::Error> {
                while seq.next_element::<T>()?.is_some() {}
                Ok(Each(std::marker::PhantomData))
            }
        }
        d.deserialize_seq(V(std::marker::PhantomData))
    }
}

/// Decodes with `gate` counting. Separate from [`decode`] so the deserializer's lifetime is the
/// borrow of `gate` (`T` is owned, so any lifetime serves).
fn run<'g, T: DeserializeOwned>(gate: &'g Gate, bytes: &'g [u8]) -> Result<T, RecordError> {
    let mut postcard = postcard::Deserializer::from_flavor(Source { rest: bytes, gate });
    let value = T::deserialize(Bounded {
        inner: &mut postcard,
        gate,
    });
    let left = postcard.finalize().map(|rest| rest.len());
    match (value, left) {
        (Ok(v), Ok(0)) => Ok(v),
        _ => Err(gate.limit.get().unwrap_or(RecordError::Decode)),
    }
}

/// The decode's counters, and the first limit it hit.
#[derive(Default)]
struct Gate {
    values: Cell<u64>,
    depth: Cell<u32>,
    left: Cell<usize>,
    limit: Cell<Option<RecordError>>,
}

impl Gate {
    /// Records `name`'s limit (the first one only) and returns an error that stops the decode.
    fn refuse<E: de::Error>(&self, name: &'static str, maximum: usize) -> E {
        if self.limit.get().is_none() {
            self.limit.set(Some(RecordError::limit(name, maximum)));
        }
        E::custom("record limit exceeded")
    }

    /// Counts one decoded value.
    fn value<E: de::Error>(&self) -> Result<(), E> {
        let n = self.values.get().saturating_add(1);
        if n > MAX_VALUES {
            return Err(self.refuse(DECODED_VALUES, MAX_VALUES as usize));
        }
        self.values.set(n);
        Ok(())
    }

    /// Counts one decoded container and opens it.
    fn open<E: de::Error>(&self) -> Result<(), E> {
        self.value()?;
        let d = self.depth.get().saturating_add(1);
        if d > MAX_DEPTH {
            return Err(self.refuse(NESTING, MAX_DEPTH as usize));
        }
        self.depth.set(d);
        Ok(())
    }

    fn close(&self) {
        self.depth.set(self.depth.get().saturating_sub(1));
    }

    /// Refuses a text or byte string longer than [`MAX_TEXT_BYTES`].
    fn text<E: de::Error>(&self, len: usize) -> Result<(), E> {
        if len > MAX_TEXT_BYTES {
            return Err(self.refuse(TEXT_BYTES, MAX_TEXT_BYTES));
        }
        Ok(())
    }

    /// Refuses a collection length hint over [`MAX_ELEMENTS`].
    fn elements<E: de::Error>(&self, hint: Option<usize>) -> Result<(), E> {
        if hint.is_some_and(|n| n > MAX_ELEMENTS) {
            return Err(self.refuse(COLLECTION_ELEMENTS, MAX_ELEMENTS));
        }
        Ok(())
    }

    /// `hint`, never more than the bytes left: every element takes at least one byte.
    fn hint(&self, hint: Option<usize>) -> Option<usize> {
        hint.map(|n| n.min(self.left.get()))
    }
}

/// postcard's input: the bytes left. It reports no size hint, so postcard hands each collection
/// visitor the raw encoded length, which [`Gate::elements`] checks. Every take of a length (the
/// bytes of a text or byte string) is checked against [`MAX_TEXT_BYTES`] before it is made.
struct Source<'g> {
    rest: &'g [u8],
    gate: &'g Gate,
}

impl<'g> Source<'g> {
    fn moved(&mut self, rest: &'g [u8]) {
        self.rest = rest;
        self.gate.left.set(rest.len());
    }
}

impl<'g> postcard::de_flavors::Flavor<'g> for Source<'g> {
    type Remainder = &'g [u8];
    type Source = &'g [u8];

    fn pop(&mut self) -> postcard::Result<u8> {
        let (&b, rest) = self
            .rest
            .split_first()
            .ok_or(postcard::Error::DeserializeUnexpectedEnd)?;
        self.moved(rest);
        Ok(b)
    }

    fn try_take_n(&mut self, ct: usize) -> postcard::Result<&'g [u8]> {
        if ct > MAX_TEXT_BYTES {
            return Err(self.gate.refuse(TEXT_BYTES, MAX_TEXT_BYTES));
        }
        if self.rest.len() < ct {
            return Err(postcard::Error::DeserializeUnexpectedEnd);
        }
        let (taken, rest) = self.rest.split_at(ct);
        self.moved(rest);
        Ok(taken)
    }

    fn finalize(self) -> postcard::Result<&'g [u8]> {
        Ok(self.rest)
    }
}

/// A deserializer that checks each value against the gate before `inner` decodes it.
struct Bounded<'g, D> {
    inner: D,
    gate: &'g Gate,
}

/// A visitor that checks lengths and wraps nested access before `inner` sees them.
struct Guard<'g, V> {
    inner: V,
    gate: &'g Gate,
}

/// A seed that decodes through [`Bounded`].
struct Seed<'g, S> {
    inner: S,
    gate: &'g Gate,
}

impl<'g, 'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for Seed<'g, S> {
    type Value = S::Value;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<S::Value, D::Error> {
        self.inner.deserialize(Bounded {
            inner: d,
            gate: self.gate,
        })
    }
}

/// A scalar: no count, no nesting.
macro_rules! scalar {
    ($($method:ident),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
            self.inner.$method(Guard { inner: visitor, gate: self.gate })
        }
    )*};
}

/// A leaf value: counted, no nesting.
macro_rules! leaf {
    ($($method:ident),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
            self.gate.value()?;
            self.inner.$method(Guard { inner: visitor, gate: self.gate })
        }
    )*};
}

impl<'g, 'de, D: Deserializer<'de>> Bounded<'g, D> {
    /// Opens a container, runs `f` inside it and closes it.
    fn nested<T>(self, f: impl FnOnce(D, &'g Gate) -> Result<T, D::Error>) -> Result<T, D::Error> {
        self.gate.open()?;
        let out = f(self.inner, self.gate);
        self.gate.close();
        out
    }
}

impl<'g, 'de, D: Deserializer<'de>> Deserializer<'de> for Bounded<'g, D> {
    type Error = D::Error;

    scalar!(
        deserialize_bool,
        deserialize_i8,
        deserialize_i16,
        deserialize_i32,
        deserialize_i64,
        deserialize_i128,
        deserialize_u8,
        deserialize_u16,
        deserialize_u32,
        deserialize_u64,
        deserialize_u128,
        deserialize_f32,
        deserialize_f64,
        deserialize_char,
    );

    leaf!(
        deserialize_str,
        deserialize_string,
        deserialize_bytes,
        deserialize_byte_buf,
        deserialize_unit,
        deserialize_identifier,
        deserialize_any,
        deserialize_ignored_any,
    );

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        self.gate.value()?;
        let gate = self.gate;
        self.inner.deserialize_unit_struct(
            name,
            Guard {
                inner: visitor,
                gate,
            },
        )
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
        self.nested(|d, gate| {
            d.deserialize_option(Guard {
                inner: visitor,
                gate,
            })
        })
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        self.nested(|d, gate| {
            d.deserialize_newtype_struct(
                name,
                Guard {
                    inner: visitor,
                    gate,
                },
            )
        })
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
        self.nested(|d, gate| {
            d.deserialize_seq(Guard {
                inner: visitor,
                gate,
            })
        })
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        self.nested(|d, gate| {
            d.deserialize_tuple(
                len,
                Guard {
                    inner: visitor,
                    gate,
                },
            )
        })
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        self.nested(|d, gate| {
            d.deserialize_tuple_struct(
                name,
                len,
                Guard {
                    inner: visitor,
                    gate,
                },
            )
        })
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
        self.nested(|d, gate| {
            d.deserialize_map(Guard {
                inner: visitor,
                gate,
            })
        })
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        self.nested(|d, gate| {
            d.deserialize_struct(
                name,
                fields,
                Guard {
                    inner: visitor,
                    gate,
                },
            )
        })
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        self.nested(|d, gate| {
            d.deserialize_enum(
                name,
                variants,
                Guard {
                    inner: visitor,
                    gate,
                },
            )
        })
    }

    fn is_human_readable(&self) -> bool {
        self.inner.is_human_readable()
    }
}

/// Forwards a visit unchanged.
macro_rules! forward {
    ($($method:ident: $ty:ty),* $(,)?) => {$(
        fn $method<E: de::Error>(self, v: $ty) -> Result<V::Value, E> {
            self.inner.$method(v)
        }
    )*};
}

/// Forwards a text or byte visit after its length check.
macro_rules! text {
    ($($method:ident: $ty:ty),* $(,)?) => {$(
        fn $method<E: de::Error>(self, v: $ty) -> Result<V::Value, E> {
            self.gate.text(v.len())?;
            self.inner.$method(v)
        }
    )*};
}

impl<'g, 'de, V: Visitor<'de>> Visitor<'de> for Guard<'g, V> {
    type Value = V::Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.expecting(f)
    }

    forward!(
        visit_bool: bool,
        visit_i8: i8,
        visit_i16: i16,
        visit_i32: i32,
        visit_i64: i64,
        visit_i128: i128,
        visit_u8: u8,
        visit_u16: u16,
        visit_u32: u32,
        visit_u64: u64,
        visit_u128: u128,
        visit_f32: f32,
        visit_f64: f64,
        visit_char: char,
    );

    text!(
        visit_str: &str,
        visit_borrowed_str: &'de str,
        visit_string: String,
        visit_bytes: &[u8],
        visit_borrowed_bytes: &'de [u8],
        visit_byte_buf: Vec<u8>,
    );

    fn visit_none<E: de::Error>(self) -> Result<V::Value, E> {
        self.inner.visit_none()
    }

    fn visit_unit<E: de::Error>(self) -> Result<V::Value, E> {
        self.inner.visit_unit()
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<V::Value, D::Error> {
        self.inner.visit_some(Bounded {
            inner: d,
            gate: self.gate,
        })
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<V::Value, D::Error> {
        self.inner.visit_newtype_struct(Bounded {
            inner: d,
            gate: self.gate,
        })
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<V::Value, A::Error> {
        self.gate.elements(seq.size_hint())?;
        self.inner.visit_seq(Seq {
            inner: seq,
            gate: self.gate,
            seen: 0,
        })
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<V::Value, A::Error> {
        self.gate.elements(map.size_hint())?;
        self.inner.visit_map(Map {
            inner: map,
            gate: self.gate,
            seen: 0,
        })
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<V::Value, A::Error> {
        self.inner.visit_enum(Enum {
            inner: data,
            gate: self.gate,
        })
    }
}

/// A sequence whose elements decode through [`Bounded`].
struct Seq<'g, A> {
    inner: A,
    gate: &'g Gate,
    seen: usize,
}

impl<'g, 'de, A: SeqAccess<'de>> SeqAccess<'de> for Seq<'g, A> {
    type Error = A::Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, A::Error> {
        let out = self.inner.next_element_seed(Seed {
            inner: seed,
            gate: self.gate,
        })?;
        if out.is_some() {
            self.seen += 1;
            self.gate.elements::<A::Error>(Some(self.seen))?;
        }
        Ok(out)
    }

    fn size_hint(&self) -> Option<usize> {
        self.gate.hint(self.inner.size_hint())
    }
}

/// A map whose keys and values decode through [`Bounded`].
struct Map<'g, A> {
    inner: A,
    gate: &'g Gate,
    seen: usize,
}

impl<'g, 'de, A: MapAccess<'de>> MapAccess<'de> for Map<'g, A> {
    type Error = A::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, A::Error> {
        let out = self.inner.next_key_seed(Seed {
            inner: seed,
            gate: self.gate,
        })?;
        if out.is_some() {
            self.seen += 1;
            self.gate.elements::<A::Error>(Some(self.seen))?;
        }
        Ok(out)
    }

    fn next_value_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<T::Value, A::Error> {
        self.inner.next_value_seed(Seed {
            inner: seed,
            gate: self.gate,
        })
    }

    fn size_hint(&self) -> Option<usize> {
        self.gate.hint(self.inner.size_hint())
    }
}

/// An enum whose variant contents decode through [`Bounded`]. The variant index is a scalar.
struct Enum<'g, A> {
    inner: A,
    gate: &'g Gate,
}

impl<'g, 'de, A: EnumAccess<'de>> EnumAccess<'de> for Enum<'g, A> {
    type Error = A::Error;
    type Variant = Variant<'g, A::Variant>;

    fn variant_seed<T: DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<(T::Value, Self::Variant), A::Error> {
        let (value, inner) = self.inner.variant_seed(seed)?;
        Ok((
            value,
            Variant {
                inner,
                gate: self.gate,
            },
        ))
    }
}

struct Variant<'g, A> {
    inner: A,
    gate: &'g Gate,
}

impl<'g, 'de, A: VariantAccess<'de>> VariantAccess<'de> for Variant<'g, A> {
    type Error = A::Error;

    fn unit_variant(self) -> Result<(), A::Error> {
        self.inner.unit_variant()
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, A::Error> {
        self.inner.newtype_variant_seed(Seed {
            inner: seed,
            gate: self.gate,
        })
    }

    fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, A::Error> {
        self.inner.tuple_variant(
            len,
            Guard {
                inner: visitor,
                gate: self.gate,
            },
        )
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, A::Error> {
        self.inner.struct_variant(
            fields,
            Guard {
                inner: visitor,
                gate: self.gate,
            },
        )
    }
}
