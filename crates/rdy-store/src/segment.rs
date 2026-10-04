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
}

fn seg_path(dir: &Path, id: u32) -> PathBuf {
    dir.join(format!("seg-{id:06}.log"))
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    let d = File::open(dir)?;
    d.sync_all()
}

/// Reads the frame at `offset`, or returns None if it is torn or corrupt.
fn read_frame(f: &File, offset: u64, file_len: u64) -> io::Result<Option<(u8, Hash, Vec<u8>)>> {
    if offset + HEADER > file_len {
        return Ok(None);
    }
    let mut header = [0u8; HEADER as usize];
    f.read_exact_at(&mut header, offset)?;
    let len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as u64;
    if offset + HEADER + len > file_len {
        return Ok(None);
    }
    let kind = header[4];
    let hash = Hash(header[5..37].try_into().unwrap());
    let mut payload = vec![0u8; len as usize];
    f.read_exact_at(&mut payload, offset + HEADER)?;
    if Hash::of(&payload) != hash {
        return Ok(None);
    }
    Ok(Some((kind, hash, payload)))
}

impl SegmentLog {
    pub fn open(dir: &Path, max_len: u64) -> io::Result<SegmentLog> {
        std::fs::create_dir_all(dir)?;
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
        let writer = OpenOptions::new().read(true).write(true).open(seg_path(dir, last))?;
        let file_len = writer.metadata()?.len();
        let mut offset = 0;
        while let Some((_, _, payload)) = read_frame(&writer, offset, file_len)? {
            offset += HEADER + payload.len() as u64;
        }
        if offset < file_len {
            writer.set_len(offset)?;
            writer.sync_all()?;
        }
        let log = SegmentLog { dir: dir.to_path_buf(), readers, writer, len: offset, max_len };
        Ok(log)
    }

    fn current(&self) -> u32 {
        (self.readers.len() - 1) as u32
    }

    pub fn end(&self) -> Location {
        Location { segment: self.current(), offset: self.len }
    }

    pub fn append(&mut self, kind: u8, payload: &[u8]) -> io::Result<(Location, Hash)> {
        if payload.len() > u32::MAX as usize {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "payload too large"));
        }
        let frame_len = HEADER + payload.len() as u64;
        if self.len > 0 && self.len + frame_len > self.max_len {
            self.writer.sync_all()?;
            let id = self.readers.len() as u32;
            self.writer = OpenOptions::new().read(true).write(true).create_new(true).open(seg_path(&self.dir, id))?;
            self.writer.sync_all()?;
            sync_dir(&self.dir)?;
            self.readers.push(File::open(seg_path(&self.dir, id))?);
            self.len = 0;
        }
        let hash = Hash::of(payload);
        let mut buf = Vec::with_capacity(frame_len as usize);
        buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        buf.push(kind);
        buf.extend_from_slice(&hash.0);
        buf.extend_from_slice(payload);
        self.writer.write_all_at(&buf, self.len)?;
        let loc = Location { segment: self.current(), offset: self.len };
        self.len += frame_len;
        Ok((loc, hash))
    }

    pub fn sync(&mut self) -> io::Result<()> {
        self.writer.sync_data()
    }

    pub fn read(&self, loc: Location) -> io::Result<(u8, Vec<u8>)> {
        let f = self.readers.get(loc.segment as usize).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such segment"))?;
        let file_len = f.metadata()?.len();
        match read_frame(f, loc.offset, file_len)? {
            Some((kind, _, payload)) => Ok((kind, payload)),
            None => Err(io::Error::new(io::ErrorKind::InvalidData, format!("corrupt or missing frame at {loc:?}"))),
        }
    }

    pub fn scan_from(&self, from: Location) -> io::Result<Vec<(Location, u8, Hash)>> {
        let mut out = vec![];
        for seg in from.segment..=self.current() {
            let f = &self.readers[seg as usize];
            let file_len = f.metadata()?.len();
            let mut offset = if seg == from.segment { from.offset } else { 0 };
            while let Some((kind, hash, payload)) = read_frame(f, offset, file_len)? {
                out.push((Location { segment: seg, offset }, kind, hash));
                offset += HEADER + payload.len() as u64;
            }
            if offset < file_len {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("corrupt frame in segment {seg} at offset {offset}")));
            }
        }
        Ok(out)
    }
}
