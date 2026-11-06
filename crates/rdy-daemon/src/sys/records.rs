//! The exact Windows forms of a project's pipe name and of its run files, as pure functions, so
//! that every platform's tests pin them (D-FPKPS). `sys::windows` is their one product caller.
//! Nothing here touches the system.

use rdy_model::Hash;
use rdy_store::fileid::FileId;
use std::io::{self, Read};
use std::path::PathBuf;

/// What every project's pipe name starts with: the local pipe namespace, then rdy's prefix.
pub const PIPE_PREFIX: &str = r"\\.\pipe\rdy-";
/// The most of `daemon.sock` that is read on Windows.
pub const MAX_PIPE_RECORD: usize = 256;
/// The most of `daemon.pid` that is read on Windows.
pub const MAX_PID_RECORD: usize = 64;
/// The hex digits of a pipe name's hash: all of a BLAKE3 digest.
const HASH_DIGITS: usize = 64;

/// The BLAKE3 hash of `units` as UTF-16LE bytes. Each code unit counts as it is: no case folding,
/// and no conversion that could lose an unpaired surrogate.
pub fn hash_of_units(units: impl Iterator<Item = u16>) -> Hash {
    let bytes: Vec<u8> = units.flat_map(u16::to_le_bytes).collect();
    Hash::of(&bytes)
}

/// The pipe of a project whose canonical path has the hash `hash`.
pub fn pipe_name(hash: &Hash) -> PathBuf {
    PathBuf::from(format!("{PIPE_PREFIX}{hash}"))
}

/// The pipe that a `daemon.sock` line names: exactly the local namespace, rdy's prefix and 64
/// lowercase hex digits.
fn pipe_from_line(line: &[u8]) -> Option<PathBuf> {
    let digits = line.strip_prefix(PIPE_PREFIX.as_bytes())?;
    let well_formed = digits.len() == HASH_DIGITS
        && digits
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b));
    if !well_formed {
        return None;
    }
    std::str::from_utf8(line).ok().map(PathBuf::from)
}

/// `daemon.sock` on Windows: the pipe, a newline, the `.rdy` identity in its Windows form, a
/// newline. `None` for anything else, a record of more than [`MAX_PIPE_RECORD`] bytes included.
pub fn parse_pipe_record(bytes: &[u8]) -> Option<(PathBuf, FileId)> {
    if bytes.len() > MAX_PIPE_RECORD {
        return None;
    }
    let (pipe, rest) = bytes.split_at(bytes.iter().position(|&b| b == b'\n')?);
    let id = rest.strip_prefix(b"\n")?.strip_suffix(b"\n")?;
    let id = std::str::from_utf8(id).ok()?.parse::<FileId>().ok()?;
    if !matches!(id, FileId::Windows { .. }) {
        return None;
    }
    Some((pipe_from_line(pipe)?, id))
}

/// `daemon.pid` on Windows: the pid, a newline, the process's creation `FILETIME`, a newline.
pub fn pipe_pid_record(pid: u32, created: u64) -> Vec<u8> {
    format!("{pid}\n{created}\n").into_bytes()
}

/// One number of a pid record: decimal digits, with no sign and no leading zero, so zero is
/// refused too: no process has it, and each value has one spelling.
fn pid_number(bytes: &[u8]) -> Option<&str> {
    let digits = bytes.iter().all(u8::is_ascii_digit) && bytes.first().is_some_and(|&b| b != b'0');
    digits.then(|| std::str::from_utf8(bytes).ok()).flatten()
}

/// The pid and the creation time of a `daemon.pid` on Windows ([`pipe_pid_record`]). `None` for
/// anything else: a sign, an overflow, a missing or an extra line, or more than
/// [`MAX_PID_RECORD`] bytes.
pub fn parse_pipe_pid_record(bytes: &[u8]) -> Option<(u32, u64)> {
    if bytes.len() > MAX_PID_RECORD {
        return None;
    }
    let body = bytes.strip_suffix(b"\n")?;
    let at = body.iter().position(|&b| b == b'\n')?;
    let (pid, created) = (&body[..at], &body[at + 1..]);
    Some((
        pid_number(pid)?.parse().ok()?,
        pid_number(created)?.parse().ok()?,
    ))
}

/// All of `reader`, when that is at most `most` bytes; `None` when it is more. At most one byte
/// past the bound is read, and no more than that is ever held.
pub fn read_capped(reader: impl Read, most: usize) -> io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::with_capacity(most + 1);
    reader.take(most as u64 + 1).read_to_end(&mut buf)?;
    Ok((buf.len() <= most).then_some(buf))
}
