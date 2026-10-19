//! The publication manifest (D-KDFZ3, hub n-CT2GB): the list of an artifact's files and the one
//! recipe that hashes it. A publication records the manifest hash; core and the hub compute it
//! with this code only, so there is no second implementation to drift.
//!
//! Recipe 1 is a fixed byte sequence: ASCII `rdy-publication-manifest`, one NUL, then u32 1 and
//! u32 file count, then each file in the UTF-8 byte order of its path: u32 path length, path
//! bytes, 32 raw hash bytes, u32 media-type length, media-type bytes, u64 size, u8 offset
//! present and, when present, u64 notice offset. Every number is unsigned little-endian. The
//! manifest hash is BLAKE3 over that sequence (`Hash::of` of it). No tag, site, actor, time or
//! render input enters it.
//!
//! These are transient types: no store record holds them.
use crate::error::{E_BAD_INPUT, E_IO, E_LIMIT, RdyError};
use rdy_model::Hash;
use serde::{Deserialize, Serialize};
use std::io::{ErrorKind, Read};

/// The one manifest recipe this rdy computes.
pub const MANIFEST_RECIPE: u32 = 1;
/// The ASCII domain prefix of recipe 1, NUL included.
const PREFIX: &[u8] = b"rdy-publication-manifest\0";

/// Most files in one artifact.
pub const MAX_FILES: usize = 32_000;
/// Most bytes in a file path.
pub const MAX_PATH_BYTES: usize = 4096;
/// Most bytes in one path segment.
pub const MAX_SEGMENT_BYTES: usize = 255;
/// Most bytes in a media type.
pub const MAX_MEDIA_TYPE_BYTES: usize = 128;
/// Most bytes in one file (2 GiB).
pub const MAX_FILE_BYTES: u64 = 2_147_483_648;
/// Most bytes in all files of one artifact together (2 GiB).
pub const MAX_ARTIFACT_BYTES: u64 = 2_147_483_648;
/// The exact bytes of an empty notice slot, which a notice offset points at.
pub const NOTICE_SLOT: &[u8] = b"<!--rdy-notices-->";
/// The read buffer of a streamed artifact file.
pub const READ_BUFFER_BYTES: usize = 65_536;

/// The only media type that may carry a notice offset.
const HTML: &str = "text/html";

/// One file of an artifact. `notice_offset` is the byte offset of the empty notice slot in an
/// HTML file, when it has one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    pub path: String,
    #[serde(with = "hex_hash")]
    pub hash: Hash,
    pub media_type: String,
    pub size: u64,
    pub notice_offset: Option<u64>,
}

/// An artifact's files, in any order: the recipe sorts them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub files: Vec<ManifestEntry>,
}

/// A hash in strict JSON input: 64 lowercase hex digits, not the default array of 32 numbers.
pub mod hex_hash {
    use rdy_model::Hash;
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub fn serialize<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(h)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Hash, D::Error> {
        let text = String::deserialize(d)?;
        text.parse()
            .map_err(|_| de::Error::custom("expected 64 lowercase hex digits"))
    }
}

/// `E_BAD_INPUT`: a recipe other than 1.
fn unsupported(recipe: u32) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("manifest recipe {recipe} is not supported"),
        "use manifest recipe 1",
    )
}

/// `E_BAD_INPUT`: a manifest whose paths or file metadata are not valid.
fn invalid() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "publication manifest is invalid",
        "use unique relative paths and valid file metadata",
    )
}

/// `E_LIMIT`: a manifest over `limit` (`32000 files`, `4096 path bytes`, ...).
fn over(limit: &str) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("publication manifest exceeds {limit}"),
        "reduce the artifact to the stated limit",
    )
}

/// `E_BAD_INPUT`: artifact bytes whose length (or hash) differs from what the manifest states.
pub fn artifact_mismatch() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "publication artifact does not match its manifest",
        "render the publication again before uploading",
    )
}

/// `E_LIMIT`: a bounded buffer for artifact bytes could not be reserved.
pub fn allocation_failed() -> RdyError {
    RdyError::new(
        E_LIMIT,
        "publication allocation failed",
        "retry with more available memory",
    )
}

/// `E_IO`: artifact bytes could not be read while they were copied or hashed.
pub fn artifact_unreadable() -> RdyError {
    RdyError::new(
        E_IO,
        "publication artifact could not be read",
        "render the publication again before uploading",
    )
}

/// Checks `path`: 1 to 4096 bytes of slash-separated segments, each 1 to 255 ASCII letters,
/// digits, dots, underscores or hyphens, and neither `.` nor `..`.
fn check_path(path: &str) -> Result<(), RdyError> {
    if path.is_empty() {
        return Err(invalid());
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(over("4096 path bytes"));
    }
    for segment in path.split('/') {
        if segment.is_empty() {
            return Err(invalid());
        }
        if segment.len() > MAX_SEGMENT_BYTES {
            return Err(over("255 segment bytes"));
        }
        if segment == "." || segment == ".." {
            return Err(invalid());
        }
        if !segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Checks `media_type`: 1 to 128 lowercase ASCII bytes, `type/subtype`, each side of letters,
/// digits, dots, pluses or hyphens. No parameters.
fn check_media_type(media_type: &str) -> Result<(), RdyError> {
    if media_type.is_empty() {
        return Err(invalid());
    }
    if media_type.len() > MAX_MEDIA_TYPE_BYTES {
        return Err(over("128 media-type bytes"));
    }
    let side = |s: &str| {
        !s.is_empty()
            && s.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'+' | b'-')
            })
    };
    match media_type.split_once('/') {
        Some((kind, sub)) if side(kind) && side(sub) => Ok(()),
        _ => Err(invalid()),
    }
}

/// Checks one entry's own fields; the artifact total is the caller's.
fn check_entry(e: &ManifestEntry) -> Result<(), RdyError> {
    check_path(&e.path)?;
    check_media_type(&e.media_type)?;
    if e.size > MAX_FILE_BYTES {
        return Err(over("2147483648 file bytes"));
    }
    if let Some(offset) = e.notice_offset {
        let slot = NOTICE_SLOT.len() as u64;
        let fits = offset.checked_add(slot).is_some_and(|end| end <= e.size);
        if e.media_type != HTML || !fits {
            return Err(invalid());
        }
    }
    Ok(())
}

/// The ASCII case fold of `path`, byte by byte.
fn folded(path: &str) -> impl Iterator<Item = u8> + '_ {
    path.bytes().map(|b| b.to_ascii_lowercase())
}

/// Validates `manifest`: 1 to 32,000 files with unique paths (compared ignoring ASCII case),
/// valid paths and media types, each file and all of them together at most 2 GiB, and a notice
/// offset only on an HTML file with room for the 18-byte slot. It checks lengths, not file bytes.
pub fn validate_manifest(manifest: &Manifest) -> Result<(), RdyError> {
    let files = &manifest.files;
    if files.is_empty() {
        return Err(invalid());
    }
    if files.len() > MAX_FILES {
        return Err(over("32000 files"));
    }
    let mut total: u64 = 0;
    for e in files {
        check_entry(e)?;
        total = total
            .checked_add(e.size)
            .filter(|t| *t <= MAX_ARTIFACT_BYTES)
            .ok_or_else(|| over("2147483648 artifact bytes"))?;
    }
    let mut by_fold: Vec<&str> = files.iter().map(|e| e.path.as_str()).collect();
    by_fold.sort_unstable_by(|a, b| folded(a).cmp(folded(b)));
    if by_fold.windows(2).any(|w| w[0].eq_ignore_ascii_case(w[1])) {
        return Err(invalid());
    }
    Ok(())
}

/// The manifest hash of `manifest` under `recipe`, after validating it. Only recipe 1 exists.
pub fn manifest_hash(manifest: &Manifest, recipe: u32) -> Result<Hash, RdyError> {
    if recipe != MANIFEST_RECIPE {
        return Err(unsupported(recipe));
    }
    validate_manifest(manifest)?;
    let mut order: Vec<&ManifestEntry> = manifest.files.iter().collect();
    order.sort_unstable_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    // BLAKE3 fed in pieces equals `Hash::of` over the whole sequence; the sequence is never held
    // whole (32,000 files of 4 KiB paths would be about 137 MB)
    let mut h = blake3::Hasher::new();
    let u32_of = |n: usize| u32::try_from(n).map_err(|_| invalid());
    h.update(PREFIX);
    h.update(&MANIFEST_RECIPE.to_le_bytes());
    h.update(&u32_of(order.len())?.to_le_bytes());
    for e in order {
        h.update(&u32_of(e.path.len())?.to_le_bytes());
        h.update(e.path.as_bytes());
        h.update(&e.hash.0);
        h.update(&u32_of(e.media_type.len())?.to_le_bytes());
        h.update(e.media_type.as_bytes());
        h.update(&e.size.to_le_bytes());
        match e.notice_offset {
            None => {
                h.update(&[0]);
            }
            Some(offset) => {
                h.update(&[1]);
                h.update(&offset.to_le_bytes());
            }
        }
    }
    Ok(Hash(*h.finalize().as_bytes()))
}

/// BLAKE3 over one artifact file streamed in pieces, bound to the length its manifest entry
/// states. Its digest equals `Hash::of` over the same bytes, without holding the file whole: a
/// process output check and an upload read files through 65,536-byte buffers.
pub struct ArtifactHasher {
    inner: blake3::Hasher,
    expected: u64,
    seen: u64,
    /// An update was refused: the bytes no longer match, whatever follows.
    refused: bool,
}

impl ArtifactHasher {
    /// A hasher for a file of exactly `expected` bytes, at most 2 GiB.
    pub fn new(expected: u64) -> Result<Self, RdyError> {
        if expected > MAX_FILE_BYTES {
            return Err(over("2147483648 file bytes"));
        }
        Ok(ArtifactHasher {
            inner: blake3::Hasher::new(),
            expected,
            seen: 0,
            refused: false,
        })
    }

    /// Adds `bytes`; refused when the file would pass its expected length.
    pub fn update(&mut self, bytes: &[u8]) -> Result<(), RdyError> {
        let total = u64::try_from(bytes.len())
            .ok()
            .and_then(|n| self.seen.checked_add(n))
            .filter(|t| !self.refused && *t <= self.expected);
        let Some(total) = total else {
            self.refused = true;
            return Err(artifact_mismatch());
        };
        self.inner.update(bytes);
        self.seen = total;
        Ok(())
    }

    /// The file's hash; refused unless exactly the expected bytes were added.
    pub fn finish(self) -> Result<Hash, RdyError> {
        if self.refused || self.seen != self.expected {
            return Err(artifact_mismatch());
        }
        Ok(Hash(*self.inner.finalize().as_bytes()))
    }
}

/// The hash of everything `reader` yields, which must be exactly `expected` bytes (at most
/// 2 GiB), read through one 65,536-byte buffer. A read error is `E_IO`; an interrupted read is
/// retried.
pub fn hash_reader<R: Read + ?Sized>(reader: &mut R, expected: u64) -> Result<Hash, RdyError> {
    let mut hasher = ArtifactHasher::new(expected)?;
    let mut buf: Vec<u8> = Vec::new();
    buf.try_reserve_exact(READ_BUFFER_BYTES)
        .map_err(|_| allocation_failed())?;
    buf.resize(READ_BUFFER_BYTES, 0);
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return Err(artifact_unreadable()),
        };
        hasher.update(&buf[..n])?;
    }
    hasher.finish()
}
