//! The candidate rule of the span map (D-1ER84): which exact byte runs of a sealed payload an
//! earlier sealed payload also holds. Only three pairs are mapped, each located by a bounded,
//! offset-reporting walk of the released encoding and verified byte for byte:
//!
//! 1. a CHANGE create or update's embedded block, against the earlier BLOCK frame of that same
//!    block version, when the standalone BLOCK's bytes are exactly the embedded encoding;
//! 2. a PLAN's explicit encoded answers against its immediate `prev` in the same stable chain:
//!    equal answers by item hash, joined only where they are adjacent in both payloads. A whole
//!    version's answers (encoding 0 or 1) and a delta's set answers are explicit; a delta's
//!    inherited answers are never hydrated for mapping;
//! 3. a SNAPSHOT's encoded map entries against the preceding SNAPSHOT in sealed log order:
//!    equal `(block id, version)` entries, joined only where they are adjacent in both.
//!
//! No arbitrary text is scanned, no run crosses a field boundary, and framing, length prefixes,
//! refs, markers and removal proofs are never mapped. A joined run shorter than
//! [`crate::dedup::JOIN_FLOOR`] is discarded; the pass then trims each run to its aligned
//! interior ([`align`]).
use crate::dedup::{FrameSpan, JOIN_FLOOR};
use crate::kinds;
use crate::plan_record::PlanRecord;
use crate::segment::{HEADER, Location};
use rdy_model::{Change, Hash, Op, StateSnapshot};

/// The most field ranges one payload offers.
pub const MAX_FIELD_RANGES: usize = 100_000;
/// The most bytes of one payload a pass reads into a buffer.
pub const MAX_PAYLOAD_BYTES: usize = 16_777_216;

/// The class of a candidate, for the byte table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Change,
    Plan,
    Snapshot,
}

impl Class {
    pub fn index(self) -> usize {
        match self {
            Class::Change => 0,
            Class::Plan => 1,
            Class::Snapshot => 2,
        }
    }
}

/// One frame as the candidate rule reads it: its place, header facts and logical payload.
#[derive(Clone, Copy)]
pub struct FrameRef<'a> {
    pub place: Location,
    pub raw_kind: u8,
    pub hash: Hash,
    pub payload: &'a [u8],
}

impl FrameRef<'_> {
    fn span(&self, start: u64, len: u64) -> FrameSpan {
        FrameSpan {
            place: self.place,
            raw_kind: self.raw_kind,
            payload_len: self.payload.len() as u32,
            hash: self.hash,
            start: start as u32,
            len: len as u32,
        }
    }

    fn end(&self) -> Location {
        Location {
            segment: self.place.segment,
            offset: self.place.offset + HEADER + self.payload.len() as u64,
        }
    }
}

/// One verified run of equal bytes: the target range and the source range that holds the same
/// bytes, in payload offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub class: Class,
    pub target: FrameSpan,
    pub source: FrameSpan,
}

/// The postcard varint of `n`.
fn varint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

/// Reads a postcard varint at `at` of `buf`: its value and its length. None for a varint that
/// runs past the buffer or over 10 bytes.
fn read_varint(buf: &[u8], at: usize) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..10 {
        let b = *buf.get(at + i)?;
        v |= u64::from(b & 0x7f).checked_shl(7 * i as u32)?;
        if b & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

/// A cursor over a payload that checks each expected encoding byte for byte as it moves.
struct Cursor<'a> {
    buf: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    /// Moves past `bytes` if the payload holds exactly them here; returns where they start.
    fn expect(&mut self, bytes: &[u8]) -> Option<usize> {
        let start = self.at;
        let end = start.checked_add(bytes.len())?;
        (self.buf.get(start..end)? == bytes).then(|| {
            self.at = end;
            start
        })
    }

    /// Moves past the postcard encoding of `v`, checked byte for byte.
    fn value<T: serde::Serialize>(&mut self, v: &T) -> Option<usize> {
        let bytes = postcard::to_allocvec(v).ok()?;
        self.expect(&bytes)
    }
}

/// The embedded blocks of a CHANGE frame: for each create or update in order whose block has
/// no foreign part, its block's version hash and its byte range in the payload. The walk stops
/// at the first op whose bytes it cannot place exactly (a foreign part, an encoding whose op
/// layout differs): nothing after it is offered.
pub fn change_blocks(raw_kind: u8, payload: &[u8]) -> Vec<(Hash, u64, u64)> {
    let mut out = vec![];
    let Ok(o) = crate::codec::split(raw_kind, payload) else {
        return out;
    };
    if o.kind != kinds::CHANGE {
        return out;
    }
    // a local, hash-checked payload within MAX_PAYLOAD_BYTES: the plain decode
    let Ok(c) = crate::codec::decode_split::<Change>(&o) else {
        return out;
    };
    let mut cur = Cursor {
        buf: payload,
        at: payload.len() - o.body.len(),
    };
    if cur.value(&c.view).is_none() || cur.expect(&varint(c.ops.len() as u64)).is_none() {
        return out;
    }
    for op in c.ops.iter().take(MAX_FIELD_RANGES) {
        let Ok(op_bytes) = postcard::to_allocvec(op) else {
            return out;
        };
        let Some(start) = cur.expect(&op_bytes) else {
            return out;
        };
        let (block, prefix) = match op {
            Op::Create { block } => (block, 1),
            Op::Update { block, .. } => (block, 1 + 32),
            Op::Delete { .. } => continue,
        };
        let Ok(bytes) = postcard::to_allocvec(block) else {
            continue;
        };
        // the op's bytes are its variant, its base (an update) and then the block's encoding
        if op_bytes.get(prefix..) != Some(&bytes[..]) {
            continue;
        }
        out.push((
            Hash::of(&bytes),
            (start + prefix) as u64,
            bytes.len() as u64,
        ));
    }
    out
}

/// The runs of a CHANGE `target` against the standalone BLOCK frames `find` gives for a block
/// version hash: each embedded local block whose earlier BLOCK frame holds exactly its bytes.
pub fn change_runs<E>(
    target: &FrameRef<'_>,
    mut find: impl FnMut(&Hash) -> Result<Option<(Location, u8, Hash, Vec<u8>)>, E>,
) -> Result<Vec<Run>, E> {
    let mut out = vec![];
    for (version, start, len) in change_blocks(target.raw_kind, target.payload) {
        if len < JOIN_FLOOR {
            continue;
        }
        let Some((place, raw, hash, payload)) = find(&version)? else {
            continue;
        };
        let source = FrameRef {
            place,
            raw_kind: raw,
            hash,
            payload: &payload,
        };
        // a source ends before the target frame starts, and holds exactly these bytes
        if raw != kinds::BLOCK || source.end() > target.place || hash != version {
            continue;
        }
        let range = start as usize..(start + len) as usize;
        if target.payload.get(range) != Some(&payload[..]) {
            continue;
        }
        out.push(Run {
            class: Class::Change,
            target: target.span(start, len),
            source: source.span(0, len),
        });
    }
    Ok(out)
}

/// One field of a payload: its key (the item hash, or the map entry's bytes) and its range.
struct Field<K> {
    key: K,
    start: usize,
    len: usize,
}

/// The open run of [`join_runs`]: target start, source start, length and the index of its last
/// source field.
type OpenRun = Option<(usize, usize, usize, usize)>;

/// Joins the fields of `t` that equal a field of `s` with the same key into runs, where equal
/// fields are adjacent in both payloads, and keeps the runs of at least [`JOIN_FLOOR`] bytes.
fn join_runs<K: Ord + Clone>(
    class: Class,
    target: &FrameRef<'_>,
    source: &FrameRef<'_>,
    t: &[Field<K>],
    s: &[Field<K>],
) -> Vec<Run> {
    let by_key: std::collections::BTreeMap<K, usize> = s
        .iter()
        .enumerate()
        .map(|(i, f)| (f.key.clone(), i))
        .collect();
    let mut out = vec![];
    let mut run: OpenRun = None;
    let mut close = |run: &mut OpenRun| {
        if let Some((ts, ss, len, _)) = run.take()
            && len as u64 >= JOIN_FLOOR
        {
            out.push(Run {
                class,
                target: target.span(ts as u64, len as u64),
                source: source.span(ss as u64, len as u64),
            });
        }
    };
    for tf in t {
        let matched = by_key.get(&tf.key).copied().filter(|&j| {
            let sf = &s[j];
            sf.len == tf.len
                && target.payload.get(tf.start..tf.start + tf.len)
                    == source.payload.get(sf.start..sf.start + sf.len)
        });
        match (matched, run) {
            (Some(j), Some((ts, ss, len, last)))
                if j == last + 1 && ts + len == tf.start && ss + len == s[j].start =>
            {
                run = Some((ts, ss, len + tf.len, j));
            }
            (Some(j), _) => {
                close(&mut run);
                run = Some((tf.start, s[j].start, tf.len, j));
            }
            (None, _) => close(&mut run),
        }
    }
    close(&mut run);
    out
}

/// The encoded map entries of a SNAPSHOT payload (encoding 0): each entry's bytes (its block id
/// and version) and range. None for a payload that is not exactly the canonical map or holds
/// more than [`MAX_FIELD_RANGES`] entries.
fn snapshot_fields(raw_kind: u8, payload: &[u8]) -> Option<Vec<Field<Vec<u8>>>> {
    if raw_kind != kinds::SNAPSHOT {
        return None;
    }
    let snap: StateSnapshot = crate::codec::decode_record(kinds::SNAPSHOT, payload).ok()?;
    let (n, mut at) = read_varint(payload, 0)?;
    if n as usize != snap.blocks.len() || snap.blocks.len() > MAX_FIELD_RANGES {
        return None;
    }
    let mut out = Vec::new();
    out.try_reserve_exact(snap.blocks.len()).ok()?;
    for (id, version) in &snap.blocks {
        let start = at;
        let mut cur = Cursor { buf: payload, at };
        cur.value(id)?;
        cur.value(version)?;
        at = cur.at;
        out.push(Field {
            key: payload[start..at].to_vec(),
            start,
            len: at - start,
        });
    }
    (at == payload.len()).then_some(out)
}

/// The runs of a SNAPSHOT `target` against the preceding SNAPSHOT `source`.
pub fn snapshot_runs(target: &FrameRef<'_>, source: &FrameRef<'_>) -> Vec<Run> {
    if source.end() > target.place {
        return vec![];
    }
    let (Some(t), Some(s)) = (
        snapshot_fields(target.raw_kind, target.payload),
        snapshot_fields(source.raw_kind, source.payload),
    ) else {
        return vec![];
    };
    join_runs(Class::Snapshot, target, source, &t, &s)
}

/// A PLAN frame as the candidate rule reads it: its `prev`, and its record (a whole version of
/// encoding 0 or 1, or a delta).
pub struct PlanFrame {
    pub prev: Option<Hash>,
    record: PlanRecord,
    /// The encoding of the frame.
    version: u16,
}

/// The decoded PLAN of a frame (encoding 0 or 1), or None.
pub fn plan_of(raw_kind: u8, payload: &[u8]) -> Option<PlanFrame> {
    if crate::codec::logical_kind(raw_kind) != kinds::PLAN {
        return None;
    }
    let (record, identity) = crate::plan_record::decode(raw_kind, payload).ok()?;
    Some(PlanFrame {
        prev: record.prev(),
        record,
        version: identity.version,
    })
}

/// The encoded explicit answers of a PLAN payload: each answer's item hash and range (a whole
/// version's answers, a delta's set answers). None for a payload whose fields before its
/// answers or whose answers it cannot place exactly.
fn plan_fields(plan: &PlanFrame, payload: &[u8]) -> Option<Vec<Field<Hash>>> {
    let answers = plan.record.explicit_answers();
    if answers.len() > MAX_FIELD_RANGES {
        return None;
    }
    let mut cur = Cursor {
        buf: payload,
        at: 0,
    };
    if plan.version != 0 {
        cur.expect(&plan.version.to_le_bytes())?;
    }
    match &plan.record {
        PlanRecord::Full(p) => {
            if plan.version != 0 {
                cur.expect(&[0])?;
            }
            cur.value(&p.stable)?;
            cur.value(&p.prev)?;
            cur.value(&p.view)?;
            cur.value(&p.proposal)?;
        }
        PlanRecord::Delta(d) => {
            cur.expect(&[1])?;
            cur.value(&d.stable)?;
            cur.value(&d.prev)?;
            cur.value(&d.view)?;
            cur.value(&d.proposal)?;
        }
    }
    cur.expect(&varint(answers.len() as u64))?;
    let mut out = Vec::new();
    out.try_reserve_exact(answers.len()).ok()?;
    for a in answers {
        let start = cur.value(a)?;
        out.push(Field {
            key: a.item,
            start,
            len: cur.at - start,
        });
    }
    Some(out)
}

/// The runs of a PLAN `target` against its immediate `prev` in the same stable chain, `source`.
pub fn plan_runs(target: &FrameRef<'_>, source: &FrameRef<'_>) -> Vec<Run> {
    if source.end() > target.place {
        return vec![];
    }
    let (Some(tp), Some(sp)) = (
        plan_of(target.raw_kind, target.payload),
        plan_of(source.raw_kind, source.payload),
    ) else {
        return vec![];
    };
    // the same chain: the source is the target's `prev`, and both name the same first version
    let first = |p: &PlanFrame, own: Hash| p.record.stable().unwrap_or(own);
    if tp.prev != Some(source.hash) || first(&tp, target.hash) != first(&sp, source.hash) {
        return vec![];
    }
    let (Some(t), Some(s)) = (
        plan_fields(&tp, target.payload),
        plan_fields(&sp, source.payload),
    ) else {
        return vec![];
    };
    join_runs(Class::Plan, target, source, &t, &s)
}

/// Trims `run` to its aligned interior on the target's physical side, with alignment `a`: the
/// target's physical start rounds up and its end rounds down to a multiple of `a`, and the
/// source start advances by the same amount. None when less than one whole block `a` remains.
pub fn align(run: &Run, a: u64) -> Option<Run> {
    let start = run.target.physical_start();
    let end = start + u64::from(run.target.len);
    let s = crate::dedup::round_up(start, a)?;
    let e = crate::dedup::round_down(end, a);
    if e < s || e - s < a {
        return None;
    }
    let cut = s - start;
    let len = (e - s) as u32;
    let mut out = *run;
    out.target.start += cut as u32;
    out.target.len = len;
    out.source.start += cut as u32;
    out.source.len = len;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(t_off: u64, start: u32, len: u32) -> Run {
        let f = |off: u64| FrameSpan {
            place: Location {
                segment: 0,
                offset: off,
            },
            raw_kind: kinds::CHANGE,
            payload_len: 1 << 20,
            hash: Hash([1; 32]),
            start,
            len,
        };
        Run {
            class: Class::Change,
            target: f(t_off),
            source: f(0),
        }
    }

    #[test]
    fn r5902_alignment_keeps_one_whole_block() {
        // a run whose physical start is aligned: 4,095 bytes map nothing, 4,096 map one block
        let at = 4096 - HEADER;
        assert_eq!(align(&run(at, 0, 4095), 4096), None);
        let a = align(&run(at, 0, 4096), 4096).unwrap();
        assert_eq!((a.target.start, a.target.len), (0, 4096));
        // a misaligned start rounds up, and the source advances by the same cut
        let a = align(&run(at, 10, 8192), 4096).unwrap();
        assert_eq!(a.target.physical_start() % 4096, 0);
        assert_eq!(
            (a.target.start, a.target.len, a.source.start),
            (4096, 4096, 4096)
        );
        // injected Windows clusters: 511/512 and 65,535/65,536
        let at = 512 - HEADER;
        assert_eq!(align(&run(at, 0, 511), 512), None);
        assert_eq!(align(&run(at, 0, 512), 512).unwrap().target.len, 512);
        let at = 65_536 - HEADER;
        assert_eq!(align(&run(at, 0, 65_535), 65_536), None);
        assert_eq!(
            align(&run(at, 0, 65_536), 65_536).unwrap().target.len,
            65_536
        );
    }

    #[test]
    fn r5902_varints_round_trip() {
        for n in [0u64, 1, 127, 128, 300, u64::from(u32::MAX), u64::MAX] {
            let b = varint(n);
            assert_eq!(read_varint(&b, 0), Some((n, b.len())));
            assert_eq!(postcard::to_allocvec(&(n as usize)).unwrap(), b);
        }
    }
}
