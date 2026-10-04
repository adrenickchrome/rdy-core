use crate::fsutil::sync_dir;
use rdy_model::Hash;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

const HEADER: u64 = 4 + 1 + 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Location {
    pub segment: u32,
    pub offset: u64,
}

pub struct SegmentLog {
    dir: PathBuf,
    readers: Vec<File>,
    writer: File,
    len: u64,
    max_len: u64,
    /// Test fault injection (see `fail_next_roll` / `fail_next_append_after`).
    fail_roll: bool,
    fail_partial: Option<usize>,
}

/// The path of segment `id` in `dir`.
pub fn seg_path(dir: &Path, id: u32) -> PathBuf {
    dir.join(format!("seg-{id:06}.log"))
}

/// If the segment files in `dir` skip a number: the first missing number and the next number
/// present after it. Only `seg-<digits>.log` names count; quarantined `.torn-` copies don't.
pub fn find_gap(dir: &Path) -> io::Result<Option<(u32, u32)>> {
    if !dir.exists() {
        return Ok(None);
    }
    let mut ids: Vec<u32> = vec![];
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name();
        let Some(n) = name
            .to_str()
            .and_then(|n| n.strip_prefix("seg-"))
            .and_then(|n| n.strip_suffix(".log"))
        else {
            continue;
        };
        if n.len() >= 6
            && n.bytes().all(|b| b.is_ascii_digit())
            && let Ok(id) = n.parse()
        {
            ids.push(id);
        }
    }
    ids.sort_unstable();
    for (want, have) in (0u32..).zip(&ids) {
        if *have != want {
            return Ok(Some((want, *have)));
        }
    }
    Ok(None)
}

/// Reads the frame at `offset`, or returns None if it is torn or corrupt.
fn read_frame(f: &File, offset: u64, file_len: u64) -> io::Result<Option<(u8, Hash, Vec<u8>)>> {
    let Some(payload_start) = offset.checked_add(HEADER).filter(|end| *end <= file_len) else {
        return Ok(None);
    };
    let mut header = [0u8; HEADER as usize];
    f.read_exact_at(&mut header, offset)?;
    let len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as u64;
    if payload_start
        .checked_add(len)
        .is_none_or(|end| end > file_len)
    {
        return Ok(None);
    }
    let kind = header[4];
    let hash = Hash(header[5..37].try_into().unwrap());
    let mut payload = vec![0u8; len as usize];
    f.read_exact_at(&mut payload, payload_start)?;
    if Hash::of(&payload) != hash {
        return Ok(None);
    }
    Ok(Some((kind, hash, payload)))
}

/// True if any valid frame (header parses, fits within EOF, payload hash verifies)
/// starts after `offset`. A genuine torn tail never has one.
fn valid_frame_after(f: &File, offset: u64, file_len: u64) -> io::Result<bool> {
    let mut buf = vec![0u8; (file_len - offset) as usize];
    f.read_exact_at(&mut buf, offset)?;
    let h = HEADER as usize;
    for pos in 1..buf.len() {
        if pos + h > buf.len() {
            break;
        }
        let len = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        let Some(end) = (pos + h).checked_add(len) else {
            continue;
        };
        if end > buf.len() {
            continue;
        }
        if Hash::of(&buf[pos + h..end]).0 == buf[pos + 5..pos + 37] {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Copies `[offset, file_len)` to `<seg>.torn-<offset>` (or `.torn-<offset>.<n>` if that name is
/// taken; an existing quarantine file is never overwritten) and fsyncs it and the directory.
fn quarantine(f: &File, dir: &Path, seg: &Path, offset: u64, file_len: u64) -> io::Result<()> {
    let mut buf = vec![0u8; (file_len - offset) as usize];
    f.read_exact_at(&mut buf, offset)?;
    let base = format!(
        "{}.torn-{offset}",
        seg.file_name().unwrap().to_string_lossy()
    );
    let mut n = 0u32;
    let q = loop {
        let name = if n == 0 {
            base.clone()
        } else {
            format!("{base}.{n}")
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(name))
        {
            Ok(q) => break q,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => return Err(e),
        }
    };
    q.write_all_at(&buf, 0)?;
    q.sync_all()?;
    sync_dir(dir)
}

impl SegmentLog {
    pub fn open(dir: &Path, max_len: u64) -> io::Result<SegmentLog> {
        SegmentLog::open_acked(dir, max_len, None)
    }

    /// Opens the log, repairing a torn tail. `acked` is the end of the acknowledged log (the
    /// index's `indexed_upto`), or None when unknown. In the last segment, corruption at or
    /// beyond `acked` belongs to an unacknowledged batch: it is quarantined and truncated even if
    /// valid frames follow (one batch is fsynced once, so its frames can tear independently).
    /// Corruption below `acked` damages acknowledged data and is refused. With `acked` unknown,
    /// the tail is truncated only if no valid frame follows the corruption.
    pub fn open_acked(dir: &Path, max_len: u64, acked: Option<Location>) -> io::Result<SegmentLog> {
        crate::fsutil::create_dir_durable(dir)?;
        // seg1: a segment past a gap holds acknowledged frames the log would otherwise skip
        if let Some((missing, later)) = find_gap(dir)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is missing but {} exists: acknowledged frames are missing; restore the missing segment from backup, then reopen",
                    seg_path(dir, missing).display(),
                    seg_path(dir, later).display()
                ),
            ));
        }
        let mut readers = vec![];
        while seg_path(dir, readers.len() as u32).exists() {
            readers.push(File::open(seg_path(dir, readers.len() as u32))?);
        }
        if readers.is_empty() {
            File::create(seg_path(dir, 0))?.sync_all()?;
            sync_dir(dir)?;
            readers.push(File::open(seg_path(dir, 0))?);
        }
        let last = (readers.len() - 1) as u32;
        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(seg_path(dir, last))?;
        let file_len = writer.metadata()?.len();
        let mut offset = 0;
        while let Some((_, _, payload)) = read_frame(&writer, offset, file_len)? {
            offset += HEADER + payload.len() as u64;
        }
        if offset < file_len {
            let unacknowledged = match acked {
                Some(a) => a.segment < last || (a.segment == last && offset >= a.offset),
                None => !valid_frame_after(&writer, offset, file_len)?,
            };
            if !unacknowledged {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "corruption at offset {offset} in {} damages acknowledged data; refusing to truncate. Frames before offset {offset} are intact: move the damaged segment aside or restore it from backup, then reopen",
                        seg_path(dir, last).display()
                    ),
                ));
            }
            quarantine(&writer, dir, &seg_path(dir, last), offset, file_len)?;
            writer.set_len(offset)?;
            writer.sync_all()?;
        }
        let log = SegmentLog {
            dir: dir.to_path_buf(),
            readers,
            writer,
            len: offset,
            max_len,
            fail_roll: false,
            fail_partial: None,
        };
        Ok(log)
    }

    /// Discards everything appended at or after `to`, which must not be before the start of the
    /// segment that was current when the failed batch began. Segments created after `to` are
    /// removed. Only for rolling back frames that were never acknowledged.
    pub fn truncate_to(&mut self, to: Location) -> io::Result<()> {
        if to.segment > self.current() || (to.segment == self.current() && to.offset > self.len) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot truncate to {to:?}: past the end {:?}", self.end()),
            ));
        }
        if to.segment < self.current() {
            self.writer = OpenOptions::new()
                .read(true)
                .write(true)
                .open(seg_path(&self.dir, to.segment))?;
            while self.current() > to.segment {
                let id = self.current();
                self.readers.pop();
                std::fs::remove_file(seg_path(&self.dir, id))?;
            }
            sync_dir(&self.dir)?;
        }
        self.writer.set_len(to.offset)?;
        self.writer.sync_all()?;
        self.len = to.offset;
        // a roll that failed after creating its file may have left the next segment behind
        let stray = seg_path(&self.dir, self.current() + 1);
        if stray.exists() {
            std::fs::remove_file(&stray)?;
            sync_dir(&self.dir)?;
        }
        Ok(())
    }

    fn current(&self) -> u32 {
        (self.readers.len() - 1) as u32
    }

    pub fn end(&self) -> Location {
        Location {
            segment: self.current(),
            offset: self.len,
        }
    }

    /// Test hook: the next roll to a new segment fails after creating the new file.
    #[doc(hidden)]
    pub fn fail_next_roll(&mut self) {
        self.fail_roll = true;
    }

    /// Test hook: the next append writes only the first `bytes` bytes of its frame, then fails.
    #[doc(hidden)]
    pub fn fail_next_append_after(&mut self, bytes: usize) {
        self.fail_partial = Some(bytes);
    }

    /// Starts segment `current + 1`. The new writer, reader and length are committed together,
    /// only once the file and the directory are durable; on any failure the log is exactly as
    /// before and the new file is removed best-effort (`truncate_to` sweeps a leftover next segment).
    fn roll(&mut self) -> io::Result<()> {
        self.writer.sync_all()?;
        let path = seg_path(&self.dir, self.readers.len() as u32);
        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        match self.finish_roll(&writer, &path) {
            Ok(reader) => {
                self.writer = writer;
                self.readers.push(reader);
                self.len = 0;
                Ok(())
            }
            Err(e) => {
                drop(writer);
                let _ = std::fs::remove_file(&path);
                let _ = sync_dir(&self.dir);
                Err(e)
            }
        }
    }

    fn finish_roll(&mut self, writer: &File, path: &Path) -> io::Result<File> {
        writer.sync_all()?;
        if std::mem::take(&mut self.fail_roll) {
            return Err(io::Error::other("injected roll failure"));
        }
        sync_dir(&self.dir)?;
        File::open(path)
    }

    pub fn append(&mut self, kind: u8, payload: &[u8]) -> io::Result<(Location, Hash)> {
        if payload.len() > u32::MAX as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload too large",
            ));
        }
        let frame_len = HEADER + payload.len() as u64;
        if self.len > 0 && self.len + frame_len > self.max_len {
            self.roll()?;
        }
        let hash = Hash::of(payload);
        let mut buf = Vec::with_capacity(frame_len as usize);
        buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        buf.push(kind);
        buf.extend_from_slice(&hash.0);
        buf.extend_from_slice(payload);
        if let Some(n) = self.fail_partial.take() {
            self.writer
                .write_all_at(&buf[..n.min(buf.len())], self.len)?;
            return Err(io::Error::other("injected partial append"));
        }
        self.writer.write_all_at(&buf, self.len)?;
        let loc = Location {
            segment: self.current(),
            offset: self.len,
        };
        self.len += frame_len;
        Ok((loc, hash))
    }

    pub fn sync(&mut self) -> io::Result<()> {
        self.writer.sync_data()
    }

    /// Reads the frame at `loc` with the payload hash its header carries (already verified).
    pub fn read_hashed(&self, loc: Location) -> io::Result<(u8, Hash, Vec<u8>)> {
        let f = self
            .readers
            .get(loc.segment as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such segment"))?;
        let file_len = f.metadata()?.len();
        read_frame(f, loc.offset, file_len)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("corrupt or missing frame at {loc:?}"),
            )
        })
    }

    pub fn read(&self, loc: Location) -> io::Result<(u8, Vec<u8>)> {
        let (kind, _, payload) = self.read_hashed(loc)?;
        Ok((kind, payload))
    }

    pub fn scan_from(&self, from: Location) -> io::Result<Vec<(Location, u8, Hash)>> {
        let mut out = vec![];
        for seg in from.segment..=self.current() {
            let f = &self.readers[seg as usize];
            let file_len = f.metadata()?.len();
            let mut offset = if seg == from.segment { from.offset } else { 0 };
            while let Some((kind, hash, payload)) = read_frame(f, offset, file_len)? {
                out.push((
                    Location {
                        segment: seg,
                        offset,
                    },
                    kind,
                    hash,
                ));
                offset += HEADER + payload.len() as u64;
            }
            if offset < file_len {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("corrupt frame in segment {seg} at offset {offset}"),
                ));
            }
        }
        Ok(out)
    }
}
