//! The project's MCP bearer (D-6M0AF, D-QG54Z): what a local MCP client sends to call the site's
//! loopback endpoint (`Authorization: Bearer <token>`).
//!
//! - **What it is.** 32 bytes from the operating system's random source, written as 52
//!   characters of uppercase RFC 4648 base32 without padding and one LF: the 53 bytes of
//!   `.rdy/run/mcp-token`.
//! - **Who makes it.** A daemon's start, under the start lock, once the store is open and before
//!   its site binds ([`prepare`]). The write is the store's durable private write
//!   (`rdy_store::fsutil::write_private_durable`), so the file is whole and private before any
//!   HTTP call can be accepted. Later starts read it and keep it: a restart, a stop and a build
//!   replacement never rotate it. A projectless MCP process and an adapter holder never make one.
//! - **What is believed.** The run directory opened through `.rdy` without following a link at
//!   its name, private to this account; in it, the token's own entry opened without following a
//!   link or a reparse point, a regular file with one name, private to this account on the
//!   handle (`sys::token_file_is_private`), and at most 53 bytes read before any allocation past
//!   that. Anything else fails closed as [`TokenError::Invalid`]: the file is never replaced.
//! - **What never shows it.** [`McpToken`] holds the decoded bytes. Its `Debug` is redacted, and
//!   it compares a presented bearer in constant time ([`McpToken::matches`]). No error, log line
//!   or reply holds a token's bytes, nor the bytes of a file that failed the check.

use crate::layout::{RUN_DIR, RunDir};
use crate::sys::{self, records::read_capped};
use rdy_store::{fileid, fsutil, pio};
use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io;
use subtle::ConstantTimeEq;

/// The token's file in the run directory.
pub const TOKEN_FILE: &str = "mcp-token";
/// The bytes of a token: 32, from the operating system's random source.
pub const TOKEN_BYTES: usize = 32;
/// The characters of a token as written: 52 of uppercase RFC 4648 base32, no padding.
pub const TOKEN_CHARS: usize = 52;
/// The whole token file: its characters and one LF. A read never takes more than one byte past it.
pub const MAX_TOKEN_FILE: usize = TOKEN_CHARS + 1;

/// The project's bearer, decoded. Its `Debug` never shows it.
pub struct McpToken([u8; TOKEN_BYTES]);

impl McpToken {
    /// Whether `presented` (the text after `Bearer `) is this token: exactly 52 characters of
    /// uppercase RFC 4648 base32 that decode to its bytes. The decoded bytes are compared in
    /// constant time; a presented text of another form is refused before any comparison.
    pub fn matches(&self, presented: &[u8]) -> bool {
        match decode_token(presented) {
            Some(bytes) => self.0.ct_eq(&bytes).into(),
            None => false,
        }
    }
}

impl fmt::Debug for McpToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("McpToken(<redacted>)")
    }
}

/// Why a daemon has no MCP token: MCP over HTTP is then unavailable, and the read-only site and
/// the private ctl and stdio channel still serve. Neither form holds a token's bytes.
#[derive(Debug)]
pub enum TokenError {
    /// `.rdy/run/mcp-token` is not a valid private token file: a link or a reparse point, not a
    /// regular file, a file with a second name, of another account or with other access, or not
    /// 52 base32 characters and one LF. It stays as it is.
    Invalid,
    /// The file system's own error while the token was made or read.
    Io(io::Error),
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenError::Invalid => {
                f.write_str("the project's MCP token is not a valid private token file")
            }
            TokenError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TokenError {}

/// A daemon's token, or why it has none.
pub type Prepared = Result<McpToken, TokenError>;

/// The project's token: read from `.rdy/run/mcp-token`, or made there durably when no entry
/// stands at that name. The caller holds the project's start lock, so no other daemon makes it at
/// the same time. Any entry at the name that is not a valid private token file is
/// [`TokenError::Invalid`] and is left as it is.
pub fn prepare(run: &RunDir) -> Prepared {
    let dir = open_run_dir(run)?;
    if let Some(token) = read_in(&dir, run)? {
        return Ok(token);
    }
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| TokenError::Io(io::Error::other(e)))?;
    let mut text = encode(&bytes);
    text.push('\n');
    fsutil::write_private_durable(&run.path().join(TOKEN_FILE), text.as_bytes())
        .map_err(TokenError::Io)?;
    // what was written is believed only as a later start would believe it, through the directory
    // opened before the write
    let made = McpToken(bytes);
    match read_in(&dir, run)? {
        Some(read) if bool::from(read.0.ct_eq(&made.0)) => Ok(made),
        _ => Err(TokenError::Invalid),
    }
}

/// The project's token, read as [`prepare`] reads it. It never makes one: a missing file is the
/// `NotFound` error.
pub fn read(run: &RunDir) -> Prepared {
    let dir = open_run_dir(run)?;
    read_in(&dir, run)?.ok_or_else(|| {
        TokenError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not exist", run.path().join(TOKEN_FILE).display()),
        ))
    })
}

/// The run directory, opened through `.rdy` without following a link or a reparse point at its
/// own name, and private to this account on the handle.
fn open_run_dir(run: &RunDir) -> Result<File, TokenError> {
    let rdy = pio::open_dir(run.rdy_dir()).map_err(TokenError::Io)?;
    let dir = pio::open_dir_at(&rdy, OsStr::new(RUN_DIR)).map_err(TokenError::Io)?;
    let privacy = pio::privacy(&dir).map_err(TokenError::Io)?;
    if !(privacy.mine && privacy.private) {
        return Err(TokenError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not private to this account", run.path().display()),
        )));
    }
    Ok(dir)
}

/// The token in the open run directory `dir`; `None` when no entry stands at its name. Every
/// other entry that is not a valid private token file is [`TokenError::Invalid`].
fn read_in(dir: &File, run: &RunDir) -> Result<Option<McpToken>, TokenError> {
    let f = match pio::open_read_at(dir, OsStr::new(TOKEN_FILE)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        // a link, a reparse point, or a file this account may not open
        Err(e) if pio::is_link_refusal(&e) || e.kind() == io::ErrorKind::PermissionDenied => {
            return Err(TokenError::Invalid);
        }
        Err(e) => return Err(TokenError::Io(e)),
    };
    let path = run.path().join(TOKEN_FILE);
    let believed = f.metadata().map_err(TokenError::Io)?.is_file()
        && sys::token_file_is_private(&f).map_err(TokenError::Io)?
        && fileid::links_of(&f, &path).map_err(TokenError::Io)? == 1;
    if !believed {
        return Err(TokenError::Invalid);
    }
    let bytes = read_capped(f, MAX_TOKEN_FILE)
        .map_err(TokenError::Io)?
        .ok_or(TokenError::Invalid)?;
    match bytes.split_last() {
        Some((b'\n', text)) => decode_token(text)
            .map(|b| Some(McpToken(b)))
            .ok_or(TokenError::Invalid),
        _ => Err(TokenError::Invalid),
    }
}

/// RFC 4648's base32 alphabet.
const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// `bytes` in uppercase RFC 4648 base32, without padding.
fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((acc >> bits) & 31) as usize]));
        }
        acc &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((acc << (5 - bits)) & 31) as usize]));
    }
    out
}

/// The value of one base32 character, or `None` for any byte outside the uppercase alphabet.
fn value(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some(u32::from(c - b'A')),
        b'2'..=b'7' => Some(u32::from(c - b'2') + 26),
        _ => None,
    }
}

/// The bytes of `text`, uppercase RFC 4648 base32 without padding, in its one canonical form: a
/// length that no byte count gives, a character outside the alphabet, padding or a set bit after
/// the last whole byte is `None`.
fn decode(text: &[u8]) -> Option<Vec<u8>> {
    if matches!(text.len() % 8, 1 | 3 | 6) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in text {
        acc = (acc << 5) | value(c)?;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
        acc &= (1 << bits) - 1;
    }
    (acc == 0).then_some(out)
}

/// The token that the 52 characters `text` write, or `None` for any other text.
fn decode_token(text: &[u8]) -> Option<[u8; TOKEN_BYTES]> {
    if text.len() != TOKEN_CHARS {
        return None;
    }
    decode(text)?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648 section 10, without the padding this form never writes.
    const VECTORS: [(&str, &str); 7] = [
        ("", ""),
        ("f", "MY"),
        ("fo", "MZXQ"),
        ("foo", "MZXW6"),
        ("foob", "MZXW6YQ"),
        ("fooba", "MZXW6YTB"),
        ("foobar", "MZXW6YTBOI"),
    ];

    #[test]
    fn mht2_base32_matches_the_rfc_vectors() {
        for (plain, coded) in VECTORS {
            assert_eq!(encode(plain.as_bytes()), coded, "{plain:?}");
            assert_eq!(decode(coded.as_bytes()).as_deref(), Some(plain.as_bytes()));
        }
    }

    #[test]
    fn mht2_base32_decodes_only_the_canonical_form() {
        for bad in [
            "my",        // lowercase
            "MY======",  // padding
            "MZ",        // a set bit after the last byte (canonical: MY)
            "M",         // no byte count gives 1, 3 or 6 characters
            "MZX",       //
            "MZXW6Y",    //
            "MZXW6YT1",  // outside the alphabet
            "MZXW6YT8",  //
            "MZXW 6YT",  //
            "MZXW6YT\n", //
        ] {
            assert_eq!(decode(bad.as_bytes()), None, "{bad:?}");
        }
    }

    #[test]
    fn mht2_token_text_round_trips_and_debug_is_redacted() {
        let bytes: [u8; TOKEN_BYTES] = std::array::from_fn(|i| (i * 37 + 11) as u8);
        let text = encode(&bytes);
        assert_eq!(text.len(), TOKEN_CHARS);
        let token = McpToken(bytes);
        assert!(token.matches(text.as_bytes()));
        assert!(!token.matches(&text.as_bytes()[..51]));
        assert!(!token.matches(text.to_lowercase().as_bytes()));
        assert!(!token.matches(format!("{text}=").as_bytes()));
        let mut other = bytes;
        other[31] ^= 1;
        assert!(!token.matches(encode(&other).as_bytes()));
        let debug = format!("{token:?}");
        assert_eq!(debug, "McpToken(<redacted>)");
        assert!(!debug.contains(&text));
    }
}
