//! The artifact file sender and the final publish request (D-KDFZ3, D-50JXR; hub i-4TMNM,
//! i-PXH0J, i-F19YZ): send a rendered artifact's content-addressed files, then the publication
//! record that names them.
//!
//! - **The source.** An [`ArtifactSource`] is the renderer's immutable result: its manifest and
//!   a reader per manifest path. It is never an output directory walked after rendering.
//! - **Checked before any request.** [`validate_artifact`] validates the manifest, holds it to
//!   the negotiated limits and core's caps, requires the source's paths to be exactly the
//!   manifest's, and reads every file once through a 65,536-byte buffer: its exact size and hash,
//!   and the exact empty notice slot at each HTML notice offset. A reader is never collected
//!   into a file-sized buffer.
//! - **By content.** [`upload_files`] names each distinct hash once in one files/known request,
//!   in raw hash order, and sends only the files the hub lacks, in that order, each with the
//!   media type of its lexicographically first path. The manifest keeps every path's own media
//!   type. A sent file is hashed again while it streams; bytes that changed since the check are
//!   withheld before the request completes.
//! - **One final call.** [`publish_record`] checks the record against its manifest and sends the
//!   one publish request; `keep_latest` reaches the wire unchanged.
//! - **No retry.** Each request is sent once. A failed or lost one ends the upload with the
//!   wire's error, so a lost write stays `E_UPSTREAM_UNCERTAIN`.
use crate::error::{E_BAD_INPUT, E_LIMIT, RdyError, upstream_protocol, upstream_uncertain};
use crate::hub_publish_wire::{
    CORE_ARTIFACT_BYTES, CORE_ARTIFACT_FILES, Candidate, FileIdentity, MAX_KNOWN_FILES,
    MAX_SELECTION_ROWS, PublicationInputs, PublishWire, UploadContext,
};
use crate::hub_types::{LimitUnit, check_count};
use crate::publication_manifest::{
    ArtifactHasher, MANIFEST_RECIPE, MAX_FILES, Manifest, ManifestEntry, NOTICE_SLOT,
    READ_BUFFER_BYTES, allocation_failed, artifact_mismatch, artifact_unreadable, manifest_hash,
};
use rdy_model::Hash;
use std::collections::BTreeMap;
use std::io::{self, ErrorKind, Read};

/// A rendered artifact: its manifest and its files' bytes, which never change.
pub trait ArtifactSource {
    /// The artifact's manifest.
    fn manifest(&self) -> &Manifest;
    /// Every file path the artifact holds, in UTF-8 byte order. An implementation bounds its
    /// path count at 32,000 before it allocates this list.
    fn paths(&self) -> Vec<&str>;
    /// A reader over the bytes of the file at `path`.
    fn open(&self, path: &str) -> Result<Box<dyn Read + '_>, RdyError>;
}

/// The artifact limits the hub's state reply negotiated (hub i-DETA1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArtifactLimits {
    pub bytes: u64,
    pub files: u32,
}

/// What an upload sent: the file requests and their bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilesSent {
    pub files: u32,
    pub bytes: u64,
}

/// `E_LIMIT`: an artifact over the hub limit `limit` (`201326592 bytes`, `32000 files`).
fn over_hub(limit: String) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("publication artifact exceeds the hub limit of {limit}"),
        "publish a smaller site",
    )
}

/// One 65,536-byte read buffer, reserved without aborting.
fn buffer() -> Result<Vec<u8>, RdyError> {
    let mut buf: Vec<u8> = Vec::new();
    buf.try_reserve_exact(READ_BUFFER_BYTES)
        .map_err(|_| allocation_failed())?;
    buf.resize(READ_BUFFER_BYTES, 0);
    Ok(buf)
}

/// Holds `manifest` to the negotiated `limits`, each first held to core's caps.
fn check_limits(manifest: &Manifest, limits: ArtifactLimits) -> Result<(), RdyError> {
    let files = u64::from(limits.files).min(CORE_ARTIFACT_FILES);
    let bytes = limits.bytes.min(CORE_ARTIFACT_BYTES);
    let count = u64::try_from(manifest.files.len()).unwrap_or(u64::MAX);
    if count > files {
        return Err(over_hub(format!("{files} files")));
    }
    let mut total: u64 = 0;
    for e in &manifest.files {
        total = total
            .checked_add(e.size)
            .filter(|t| *t <= bytes)
            .ok_or_else(|| over_hub(format!("{bytes} bytes")))?;
    }
    Ok(())
}

/// The manifest's entries in UTF-8 byte order of their paths.
fn by_path(manifest: &Manifest) -> Vec<&ManifestEntry> {
    let mut order: Vec<&ManifestEntry> = manifest.files.iter().collect();
    order.sort_unstable_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    order
}

/// Requires the source's paths, in their given order, to be exactly `order`'s.
fn check_paths(source: &dyn ArtifactSource, order: &[&ManifestEntry]) -> Result<(), RdyError> {
    let paths = source.paths();
    if paths.len() > MAX_FILES
        || paths.len() != order.len()
        || paths.iter().zip(order).any(|(p, e)| *p != e.path)
    {
        return Err(artifact_mismatch());
    }
    Ok(())
}

/// The distinct files of `order` by raw hash bytes, each with the entry of its first path in
/// `order`. A hash that claims two sizes is refused.
fn distinct<'a>(
    order: &[&'a ManifestEntry],
) -> Result<BTreeMap<[u8; 32], &'a ManifestEntry>, RdyError> {
    let mut files: BTreeMap<[u8; 32], &ManifestEntry> = BTreeMap::new();
    for &e in order {
        match files.get(&e.hash.0) {
            Some(first) if first.size != e.size => return Err(artifact_mismatch()),
            Some(_) => {}
            None => {
                files.insert(e.hash.0, e);
            }
        }
    }
    Ok(files)
}

/// Compares the part of `chunk`, read at file position `pos`, that overlaps the notice slot at
/// `offset` with the slot's exact bytes.
fn check_slot(offset: u64, pos: u64, chunk: &[u8]) -> Result<(), RdyError> {
    let slot = NOTICE_SLOT.len() as u64;
    let end = u64::try_from(chunk.len())
        .ok()
        .and_then(|n| pos.checked_add(n));
    let (Some(end), Some(slot_end)) = (end, offset.checked_add(slot)) else {
        return Err(artifact_mismatch());
    };
    let lo = offset.max(pos);
    let hi = slot_end.min(end);
    if lo >= hi {
        return Ok(());
    }
    // Both ranges lie inside the chunk and inside the 18-byte slot.
    let at = |n: u64| usize::try_from(n).map_err(|_| artifact_mismatch());
    let seen = chunk.get(at(lo - pos)?..at(hi - pos)?);
    let want = NOTICE_SLOT.get(at(lo - offset)?..at(hi - offset)?);
    if seen.is_none() || seen != want {
        return Err(artifact_mismatch());
    }
    Ok(())
}

/// Reads the file of `e` once: its exact size and hash, and the empty slot at its offset.
fn check_file(
    source: &dyn ArtifactSource,
    e: &ManifestEntry,
    buf: &mut [u8],
) -> Result<(), RdyError> {
    let mut reader = source.open(&e.path)?;
    let mut hasher = ArtifactHasher::new(e.size)?;
    let mut pos: u64 = 0;
    loop {
        let n = match reader.read(buf) {
            Ok(0) => break,
            Ok(n) if n <= buf.len() => n,
            Ok(_) => return Err(artifact_unreadable()),
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return Err(artifact_unreadable()),
        };
        let chunk = &buf[..n];
        hasher.update(chunk)?;
        if let Some(offset) = e.notice_offset {
            check_slot(offset, pos, chunk)?;
        }
        // The hasher has bounded the total at the entry's size.
        pos = pos.saturating_add(n as u64);
    }
    // The exact size covers the whole slot: the manifest placed it inside the file.
    if hasher.finish()? != e.hash {
        return Err(artifact_mismatch());
    }
    Ok(())
}

/// Checks `source` before any artifact request: its manifest, the negotiated `limits` and
/// core's caps, its paths against the manifest's, and every file's exact size, hash and notice
/// slot. Returns the manifest hash under recipe 1.
pub fn validate_artifact(
    source: &dyn ArtifactSource,
    limits: ArtifactLimits,
) -> Result<Hash, RdyError> {
    let manifest = source.manifest();
    let hash = manifest_hash(manifest, MANIFEST_RECIPE)?;
    check_limits(manifest, limits)?;
    let order = by_path(manifest);
    check_paths(source, &order)?;
    distinct(&order)?;
    let mut buf = buffer()?;
    for e in &order {
        check_file(source, e, &mut buf)?;
    }
    Ok(hash)
}

/// How far a streamed file's check has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Check {
    Pending,
    Verified,
    Changed,
    Unreadable,
}

/// A file's bytes as they stream to the hub, hashed again on the way. The read that would
/// complete the file first confirms that the source ends there and that the hash is the claimed
/// one; otherwise it fails, so the request never receives its last bytes.
struct Streamed<'a> {
    inner: Box<dyn Read + 'a>,
    hasher: Option<ArtifactHasher>,
    expected: Hash,
    remaining: u64,
    check: Check,
}

impl<'a> Streamed<'a> {
    fn new(inner: Box<dyn Read + 'a>, file: &FileIdentity) -> Result<Streamed<'a>, RdyError> {
        Ok(Streamed {
            inner,
            hasher: Some(ArtifactHasher::new(file.size)?),
            expected: file.hash,
            remaining: file.size,
            check: Check::Pending,
        })
    }

    fn fail(&mut self, check: Check) -> io::Result<usize> {
        self.check = check;
        self.hasher = None;
        Err(io::Error::other(
            "publication artifact changed while it was sent",
        ))
    }

    /// At the file's end: the source has no further byte and the hash is the claimed one.
    fn settle(&mut self) -> Check {
        let mut probe = [0u8; 1];
        loop {
            match self.inner.read(&mut probe) {
                Ok(0) => break,
                Ok(_) => return Check::Changed,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Check::Unreadable,
            }
        }
        match self.hasher.take().map(ArtifactHasher::finish) {
            Some(Ok(h)) if h == self.expected => Check::Verified,
            _ => Check::Changed,
        }
    }

    /// Completes the check of a file the wire did not read to its end.
    fn finish(&mut self) -> Result<(), RdyError> {
        if self.check == Check::Pending {
            let mut buf = buffer()?;
            loop {
                match self.read(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
        self.outcome()
    }

    /// The check so far: a changed file is the artifact mismatch, an unreadable one `E_IO`.
    fn outcome(&self) -> Result<(), RdyError> {
        match self.check {
            Check::Pending | Check::Verified => Ok(()),
            Check::Changed => Err(artifact_mismatch()),
            Check::Unreadable => Err(artifact_unreadable()),
        }
    }
}

impl Read for Streamed<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match self.check {
            Check::Verified => return Ok(0),
            Check::Changed | Check::Unreadable => return self.fail(self.check),
            Check::Pending => {}
        }
        if self.remaining == 0 {
            return match self.settle() {
                Check::Verified => {
                    self.check = Check::Verified;
                    Ok(0)
                }
                failed => self.fail(failed),
            };
        }
        if out.is_empty() {
            return Ok(0);
        }
        let want = usize::try_from(self.remaining).map_or(out.len(), |r| r.min(out.len()));
        let n = match self.inner.read(&mut out[..want]) {
            Ok(0) => return self.fail(Check::Changed),
            Ok(n) if n <= want => n,
            Ok(_) => return self.fail(Check::Unreadable),
            Err(e) if e.kind() == ErrorKind::Interrupted => return Err(e),
            Err(_) => return self.fail(Check::Unreadable),
        };
        let fed = self
            .hasher
            .as_mut()
            .is_some_and(|h| h.update(&out[..n]).is_ok());
        if !fed {
            return self.fail(Check::Changed);
        }
        // `n` is at most `remaining`.
        self.remaining -= n as u64;
        if self.remaining == 0 {
            match self.settle() {
                Check::Verified => self.check = Check::Verified,
                failed => return self.fail(failed),
            }
        }
        Ok(n)
    }
}

/// Sends the files of `source` that the hub lacks, for push `push`. The artifact is checked
/// first ([`validate_artifact`]). Then one files/known request names each distinct hash once,
/// and each missing file is sent once, in hash order. `context` gives each request its upload
/// context. Any failed request ends the upload with its error; nothing is retried.
pub fn upload_files(
    source: &dyn ArtifactSource,
    push: &str,
    limits: ArtifactLimits,
    wire: &dyn PublishWire,
    context: &mut dyn FnMut() -> Result<UploadContext, RdyError>,
) -> Result<FilesSent, RdyError> {
    validate_artifact(source, limits)?;
    let order = by_path(source.manifest());
    let files = distinct(&order)?;
    check_count(files.len(), MAX_KNOWN_FILES, LimitUnit::Entries)?;
    let identities: Vec<FileIdentity> = files
        .values()
        .map(|e| FileIdentity {
            hash: e.hash,
            size: e.size,
        })
        .collect();
    let cx = context()?;
    let missing = wire.known_files(&cx, push, &identities)?;
    let subset = missing.windows(2).all(|w| w[0].0 < w[1].0)
        && missing.iter().all(|h| files.contains_key(&h.0));
    if !subset {
        return Err(upstream_protocol());
    }
    let mut sent = FilesSent { files: 0, bytes: 0 };
    for hash in &missing {
        let e = files.get(&hash.0).ok_or_else(upstream_protocol)?;
        let file = FileIdentity {
            hash: e.hash,
            size: e.size,
        };
        let cx = context()?;
        let mut body = Streamed::new(source.open(&e.path)?, &file)?;
        let reply = wire.put_file(&cx, push, &file, &e.media_type, &mut body);
        // Changed bytes were withheld, so the hub cannot hold them.
        body.outcome()?;
        if reply? != file {
            return Err(upstream_uncertain());
        }
        body.finish()?;
        // At most 32,000 files of at most 2 GiB in all.
        sent = FilesSent {
            files: sent.files.checked_add(1).ok_or_else(artifact_mismatch)?,
            bytes: sent
                .bytes
                .checked_add(file.size)
                .ok_or_else(artifact_mismatch)?,
        };
    }
    Ok(sent)
}

/// Sends the publication record `record` with its `manifest` for push `push` and site `site`:
/// the one final request, after every file. Before it, recipe 1 is recomputed and must equal
/// the record's manifest hash, and the selection must be sorted by id with each id once.
/// `keep_latest` reaches the wire unchanged.
pub fn publish_record(
    record: &PublicationInputs,
    manifest: &Manifest,
    push: &str,
    site: &str,
    keep_latest: Option<bool>,
    wire: &dyn PublishWire,
    context: UploadContext,
) -> Result<Candidate, RdyError> {
    if manifest_hash(manifest, record.manifest_recipe)? != record.manifest_hash {
        return Err(artifact_mismatch());
    }
    check_count(
        record.selection.len(),
        MAX_SELECTION_ROWS,
        LimitUnit::Entries,
    )?;
    if record.selection.windows(2).any(|w| w[0].id >= w[1].id) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "the selection rows are not sorted and distinct",
            "sort the selection rows and name each once",
        ));
    }
    wire.publish(&context, push, site, record, manifest, keep_latest)
}
