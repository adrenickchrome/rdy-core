//! The diagram cache (D-XM8MP, D-6H0ZX): each drawn diagram, kept in the index's derived
//! `diagrams` table by a key the renderer builds (the BLAKE3 hash of the diagram's normalised
//! source, the drawing crate's version, a 0x00 byte and the renderer's version).
//!
//! A row is a drawing's result: one tag byte (1 a standalone SVG file, 2 a `Failed` reason, 3 an
//! `Invalid` reason), the BLAKE3 hash of the payload, then the payload in UTF-8. A row is exactly
//! what a miss of its key computes, so no build's bytes depend on the cache. A damaged row reads
//! as no row, and the next put of its key replaces it.
//!
//! The cache is derived: no frame, ref or record holds a row, so no row reaches the log, a dump,
//! a sync batch or `IndexDump`. A reindex starts it empty, and so does an open after an unclean
//! shutdown. A put is one commit that is not durable; the store's next durable commit keeps it.
//! Only a reader that may serve a new response (`check_redaction_epoch`) and is no preview reads
//! or writes it. A redb error makes a get give none and a put do nothing: the cache never warns
//! and never fails a build.
use crate::store::StoreReader;
use rdy_model::Hash;

/// The most bytes the `diagrams` table counts: key, value and 64 per row (64 MiB).
pub const DIAGRAM_CACHE_BYTES: u64 = 67_108_864;
/// The longest key the cache reads or writes.
pub const MAX_DIAGRAM_KEY_BYTES: usize = 256;
/// The longest payload a row holds (`diagram_svg::MAX_BYTES`, 4 MiB).
pub const MAX_DIAGRAM_PAYLOAD_BYTES: usize = 4_194_304;
/// The shortest key: a 32-byte source hash and at least one more byte.
const MIN_DIAGRAM_KEY_BYTES: usize = 33;
/// The bytes before a row's payload: its tag and the payload's hash.
const HEAD: usize = 33;
const TAG_FILE: u8 = 1;
const TAG_FAILED: u8 = 2;
const TAG_INVALID: u8 = 3;

/// A drawing's result, as a cache row holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CachedDrawing {
    /// The drawing's standalone SVG file.
    File(String),
    /// Why the drawing cannot be used (`Diagram::Failed`).
    Failed(String),
    /// The pinned crate's parse error after admission (`Diagram::Invalid`).
    Invalid(String),
}

/// The store's diagram cache, through one reader: get and put of drawings by key, nothing else.
#[derive(Clone)]
pub struct DiagramCache {
    reader: StoreReader,
}

impl StoreReader {
    /// This reader's diagram cache (D-XM8MP). It reads and writes the index this reader holds.
    pub fn diagram_cache(&self) -> DiagramCache {
        DiagramCache {
            reader: self.clone(),
        }
    }
}

impl DiagramCache {
    /// Whether this reader may read or write the cache: it may serve a new response and it is no
    /// preview reader.
    fn open(&self) -> bool {
        self.reader.check_redaction_epoch().is_ok() && self.reader.preview().is_none()
    }

    /// The drawing under `key`, or none: a key out of bounds, a closed reader, a missing or
    /// damaged row, or a redb error.
    pub fn get(&self, key: &[u8]) -> Option<CachedDrawing> {
        if !key_fits(key) || !self.open() {
            return None;
        }
        decode(&self.reader.index().diagram(key).ok()??)
    }

    /// Puts `drawing` under `key`. A key out of bounds, a closed reader, a payload over the
    /// bound or a redb error stores nothing.
    pub fn put(&self, key: &[u8], drawing: &CachedDrawing) {
        if !key_fits(key) || !self.open() {
            return;
        }
        let Some(value) = encode(drawing) else {
            return;
        };
        let _ = self
            .reader
            .index()
            .put_diagram(key, &value, DIAGRAM_CACHE_BYTES);
    }

    /// The rows the table holds (0 on a redb error).
    #[doc(hidden)]
    pub fn rows_for_tests(&self) -> usize {
        self.reader.index().diagram_counts().map_or(0, |c| c.0)
    }

    /// The `meta` count of the table's bytes.
    #[doc(hidden)]
    pub fn bytes_for_tests(&self) -> Option<u64> {
        self.reader.index().diagram_counts().ok()?.1
    }

    /// Puts raw `value` under raw `key`, under the cache's bound, past every other check.
    #[doc(hidden)]
    pub fn put_raw_for_tests(&self, key: &[u8], value: &[u8]) {
        let _ = self
            .reader
            .index()
            .put_diagram(key, value, DIAGRAM_CACHE_BYTES);
    }
}

fn key_fits(key: &[u8]) -> bool {
    (MIN_DIAGRAM_KEY_BYTES..=MAX_DIAGRAM_KEY_BYTES).contains(&key.len())
}

/// A row's value, or none for a payload over the bound.
fn encode(drawing: &CachedDrawing) -> Option<Vec<u8>> {
    let (tag, payload) = match drawing {
        CachedDrawing::File(p) => (TAG_FILE, p),
        CachedDrawing::Failed(p) => (TAG_FAILED, p),
        CachedDrawing::Invalid(p) => (TAG_INVALID, p),
    };
    if payload.len() > MAX_DIAGRAM_PAYLOAD_BYTES {
        return None;
    }
    let mut v = Vec::with_capacity(HEAD + payload.len());
    v.push(tag);
    v.extend_from_slice(&Hash::of(payload.as_bytes()).0);
    v.extend_from_slice(payload.as_bytes());
    Some(v)
}

/// A row's drawing; none for a damaged row: too short, an unknown tag, a payload over the bound,
/// a hash that differs or a payload that is not UTF-8.
fn decode(value: &[u8]) -> Option<CachedDrawing> {
    if value.len() < HEAD {
        return None;
    }
    let payload = &value[HEAD..];
    if payload.len() > MAX_DIAGRAM_PAYLOAD_BYTES || Hash::of(payload).0[..] != value[1..HEAD] {
        return None;
    }
    let text = std::str::from_utf8(payload).ok()?.to_string();
    match value[0] {
        TAG_FILE => Some(CachedDrawing::File(text)),
        TAG_FAILED => Some(CachedDrawing::Failed(text)),
        TAG_INVALID => Some(CachedDrawing::Invalid(text)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use crate::index::DIAGRAM_COUNT_KEY;

    /// A key as the renderer builds one: a source hash, the crate, 0x00, the renderer.
    fn key(n: u8) -> Vec<u8> {
        let mut k = vec![n; 32];
        k.extend_from_slice(b"mmdflux 2.6.1\0");
        k.extend_from_slice(b"0.24.0");
        k
    }

    /// A row value: the tag, the payload's hash, the payload.
    fn value(tag: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![tag];
        v.extend_from_slice(&Hash::of(payload).0);
        v.extend_from_slice(payload);
        v
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::create(&dir.path().join("s")).unwrap();
        (dir, s)
    }

    /// rdb7 (D-XM8MP): each tag round-trips; a damaged row or a key out of bounds reads as
    /// none; a put over a damaged row replaces it.
    #[test]
    fn rdb7_rows_round_trip_and_damage_is_a_miss() {
        let (_d, s) = store();
        let c = s.reader().diagram_cache();
        let k = key(1);
        for d in [
            CachedDrawing::File("<svg>a</svg>".into()),
            CachedDrawing::Failed("the drawn diagram is over 4194304 bytes".into()),
            CachedDrawing::Invalid("Expected a node".into()),
        ] {
            c.put(&k, &d);
            assert_eq!(c.get(&k), Some(d));
        }
        assert_eq!(c.rows_for_tests(), 1);
        // damage: a short value, unknown tags, a changed payload byte, a payload not UTF-8
        let good = value(1, b"<svg>b</svg>");
        let mut changed = good.clone();
        *changed.last_mut().unwrap() ^= 1;
        let damaged = [
            good[..32].to_vec(),
            value(0, b"<svg>b</svg>"),
            value(4, b"<svg>b</svg>"),
            changed,
            value(1, &[0xff, 0xfe, b'x']),
        ];
        for v in &damaged {
            c.put_raw_for_tests(&k, v);
            assert_eq!(c.get(&k), None, "{v:?}");
        }
        // a put over a damaged row replaces it
        c.put(&k, &CachedDrawing::File("<svg>c</svg>".into()));
        assert_eq!(c.get(&k), Some(CachedDrawing::File("<svg>c</svg>".into())));
        assert_eq!(c.rows_for_tests(), 1);
        // keys of 32 and 257 bytes are never read or written
        for (len, rows) in [(32, 1), (257, 2)] {
            let k = vec![9u8; len];
            c.put(&k, &CachedDrawing::Failed("x".into()));
            assert_eq!(c.rows_for_tests(), rows, "{len}: no put");
            // a raw row under it is never read
            c.put_raw_for_tests(&k, &value(2, b"x"));
            assert_eq!(c.get(&k), None, "{len}");
        }
        // a payload over the bound is not stored
        let big = "x".repeat(MAX_DIAGRAM_PAYLOAD_BYTES + 1);
        c.put(&key(2), &CachedDrawing::File(big));
        assert_eq!(c.get(&key(2)), None);
    }

    /// rdb7 (D-XM8MP): the count is exact; a replaced key subtracts its old row; the put that
    /// would pass the cap leaves only its own row; a row over the cap stores nothing; a `meta`
    /// count that is not 8 bytes long empties the table first.
    #[test]
    fn rdb7_the_cache_empties_when_full() {
        let (_d, s) = store();
        let r = s.reader();
        let ix = r.index();
        let cost = |k: &[u8], v: &[u8]| (k.len() + v.len() + 64) as u64;
        let (a, b, c, d) = (key(1), key(2), key(3), key(4));
        let va = value(1, &[b'a'; 67]); // 100 bytes
        let vb = value(1, &[b'b'; 167]); // 200 bytes
        let va2 = value(1, &[b'a'; 117]); // 150 bytes
        let vc = value(1, &[b'c'; 367]); // 400 bytes
        ix.put_diagram(&a, &va, 1000).unwrap();
        assert_eq!(ix.diagram_counts().unwrap(), (1, Some(cost(&a, &va))));
        ix.put_diagram(&b, &vb, 1000).unwrap();
        let two = cost(&a, &va) + cost(&b, &vb);
        assert_eq!(ix.diagram_counts().unwrap(), (2, Some(two)));
        ix.put_diagram(&a, &va2, 1000).unwrap();
        let replaced = two - cost(&a, &va) + cost(&a, &va2);
        assert_eq!(ix.diagram_counts().unwrap(), (2, Some(replaced)));
        assert!(replaced + cost(&c, &vc) > 1000);
        ix.put_diagram(&c, &vc, 1000).unwrap();
        assert_eq!(ix.diagram_counts().unwrap(), (1, Some(cost(&c, &vc))));
        assert_eq!(ix.diagram(&c).unwrap(), Some(vc.clone()));
        assert_eq!(ix.diagram(&a).unwrap(), None);
        // a row over the cap alone stores nothing
        let huge = value(1, &[b'h'; 1000]);
        ix.put_diagram(&d, &huge, 1000).unwrap();
        assert_eq!(ix.diagram_counts().unwrap(), (1, Some(cost(&c, &vc))));
        // a 7-byte count reads as a full cache
        ix.set_meta_for_tests(DIAGRAM_COUNT_KEY, &[0u8; 7]).unwrap();
        assert_eq!(ix.diagram_counts().unwrap(), (1, None));
        ix.put_diagram(&a, &va, 1000).unwrap();
        assert_eq!(ix.diagram_counts().unwrap(), (1, Some(cost(&a, &va))));
        assert_eq!(ix.diagram(&c).unwrap(), None);
    }

    /// rdb7 (D-XM8MP, D-6H0ZX): a reindex starts the cache empty; a preview reader neither
    /// reads nor writes it.
    #[test]
    fn rdb7_reindex_and_previews_drop_or_skip_the_cache() {
        let (_d, mut s) = store();
        let f = CachedDrawing::File("<svg>a</svg>".into());
        s.reader().diagram_cache().put(&key(1), &f);
        assert_eq!(s.reader().diagram_cache().get(&key(1)), Some(f.clone()));
        s.reindex().unwrap();
        let c = s.reader().diagram_cache();
        assert_eq!(c.get(&key(1)), None);
        assert_eq!(c.rows_for_tests(), 0);
        c.put(&key(1), &f);
        assert_eq!(c.get(&key(1)), Some(f.clone()));
        // a preview reader: no read, no write
        let p = s.redaction_preview_reader(&[]).unwrap().diagram_cache();
        assert_eq!(p.get(&key(1)), None);
        p.put(&key(2), &f);
        assert_eq!(c.get(&key(2)), None);
        assert_eq!(c.rows_for_tests(), 1);
    }

    /// rdb7 (D-XM8MP): an index written before the table existed gains it empty, with no
    /// rebuild.
    #[test]
    fn rdb7_an_index_without_the_table_gains_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let s = Store::create(&path).unwrap();
        s.reader()
            .diagram_cache()
            .put(&key(1), &CachedDrawing::Invalid("x".into()));
        s.reader().index().drop_diagram_table_for_tests().unwrap();
        s.close().unwrap();
        let s = Store::open(&path).unwrap();
        assert!(!s.open_report().index_rebuilt);
        let c = s.reader().diagram_cache();
        assert_eq!(c.rows_for_tests(), 0);
        assert_eq!(c.get(&key(1)), None);
        c.put(&key(1), &CachedDrawing::Invalid("x".into()));
        assert_eq!(c.get(&key(1)), Some(CachedDrawing::Invalid("x".into())));
    }
}
