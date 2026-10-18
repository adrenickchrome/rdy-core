//! The local trust file `signing.allowed_signers` (D-1Z951, O6): which key may sign for which
//! principal, in OpenSSH's allowed-signers format.
//!
//! Each line is blank, a `#` comment, or `principals [options] key [comment]`, with spaces or
//! tabs between the fields. `principals` is a comma-separated pattern list, optionally in double
//! quotes. The options are a comma-separated list of `namespaces="..."`, `valid-after="..."` and
//! `valid-before="..."` (names in any case, as OpenSSH reads them). The time values are read as
//! strings and ignored. `cert-authority`, a repeated option and any other option refuse the
//! file. The key is an `ssh-ed25519` or `sk-ssh-ed25519@openssh.com` public key; anything after
//! it is a comment.
//!
//! Principal and namespace lists hold patterns with `*` and `?`, and a leading `!` negates one.
//! A list matches when no negated pattern matches and at least one other pattern does. Matching
//! is case-sensitive, byte by byte, and never recursive.
//!
//! The file is read whole and parsed strictly: no key becomes trusted through a partial parse.
//! Every bound is checked before anything is kept.

use crate::did_you_mean::nearest;
use crate::signing_crypto::{KEY_TYPES, key_fingerprint};
use crate::{E_BAD_INPUT, E_IO, RdyError, signing_limit};
use std::io::Read;
use std::path::Path;

/// The largest allowed-signers file, in bytes.
pub const MAX_FILE: usize = 1_048_576;
/// The longest line, in bytes, its LF not counted.
pub const MAX_LINE: usize = 16_384;
/// The most entries (lines that are not blank or comments).
pub const MAX_ENTRIES: usize = 4096;
/// The longest pattern, in bytes, a leading `!` included.
pub const MAX_PATTERN: usize = 1024;
/// The most principal and namespace patterns of one entry, together.
pub const MAX_PATTERNS: usize = 128;

/// The options an entry may carry, in their canonical spelling.
const OPTIONS: [&str; 3] = ["namespaces", "valid-after", "valid-before"];
/// The setting the file comes from, as errors name it.
const SETTING: &str = "signing.allowed_signers";

/// A parsed allowed-signers file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AllowedSigners {
    entries: Vec<Entry>,
}

/// One line of the file that names a key.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    principals: Vec<String>,
    /// `None` when the line has no `namespaces` option: every namespace.
    namespaces: Option<Vec<String>>,
    /// The key's SHA256 fingerprint.
    key: String,
}

/// Why a line is refused: the cause in its error text, or a bound it breaks.
enum LineError {
    Cause(String),
    Limit(RdyError),
}

impl From<&str> for LineError {
    fn from(cause: &str) -> Self {
        LineError::Cause(cause.to_string())
    }
}

const INVALID_PRINCIPALS: &str = "invalid principal list";
const INVALID_KEY: &str = "invalid key";
const INVALID_OPTION: &str = "invalid option";
const DUPLICATE_OPTION: &str = "duplicate option";
const CERT_AUTHORITY: &str = "cert-authority is not accepted";

impl AllowedSigners {
    /// Parses a whole allowed-signers file. A bound it breaks is the shared signing `E_LIMIT`; a
    /// line it cannot read is `E_BAD_INPUT` naming the line (from 1) and its cause.
    pub fn parse(bytes: &[u8]) -> Result<AllowedSigners, RdyError> {
        if bytes.len() > MAX_FILE {
            return Err(signing_limit(&format!("{MAX_FILE} bytes")));
        }
        let mut entries = Vec::new();
        let body = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        for (i, line) in body.split(|b| *b == b'\n').enumerate() {
            if line.len() > MAX_LINE {
                return Err(signing_limit(&format!("{MAX_LINE} bytes")));
            }
            let rest = skip_space(line);
            if rest.is_empty() || rest[0] == b'#' {
                continue;
            }
            if entries.len() == MAX_ENTRIES {
                return Err(signing_limit(&format!("{MAX_ENTRIES} entries")));
            }
            match parse_entry(rest) {
                Ok(entry) => entries.push(entry),
                Err(LineError::Limit(e)) => return Err(e),
                Err(LineError::Cause(cause)) => return Err(invalid_line(i + 1, &cause)),
            }
        }
        Ok(AllowedSigners { entries })
    }

    /// The number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether an entry trusts the key with fingerprint `key` for `principal` in `namespace`:
    /// its key is that key, its principals match `principal`, and its namespaces (when it lists
    /// any) match `namespace`.
    pub fn trusts(&self, principal: &str, namespace: &str, key: &str) -> bool {
        self.entries.iter().any(|e| {
            e.key == key
                && list_matches(principal, &e.principals)
                && e.namespaces
                    .as_ref()
                    .is_none_or(|ns| list_matches(namespace, ns))
        })
    }
}

/// Reads the configured trust file: `None` when no file is configured. A file that cannot be
/// read is `E_IO`; one over [`MAX_FILE`] is the shared signing `E_LIMIT`, found without reading
/// past the bound.
pub fn read_allowed_signers(path: Option<&Path>) -> Result<Option<AllowedSigners>, RdyError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let unreadable = |_| {
        RdyError::new(
            E_IO,
            format!("cannot read {SETTING}"),
            "check the configured path and its permissions",
        )
    };
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    AllowedSigners::parse(&bytes).map(Some)
}

/// The `E_BAD_INPUT` of line `line` with `cause`.
fn invalid_line(line: usize, cause: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("{SETTING} line {line} is invalid: {cause}"),
        format!("fix line {line} in {SETTING}"),
    )
}

/// One entry, from its first non-space byte.
fn parse_entry(line: &[u8]) -> Result<Entry, LineError> {
    let (field, rest) = principals_field(line)?;
    let text = std::str::from_utf8(field).map_err(|_| INVALID_PRINCIPALS)?;
    let mut patterns = 0usize;
    let principals = pattern_list(text, &mut patterns, INVALID_PRINCIPALS)?;
    let rest = skip_space(rest);
    // OpenSSH reads a key here first and options only when that fails; a field that names a
    // key type is the key, anything else the options
    let (field, after) = option_field(rest)?;
    let (namespaces, rest) = if looks_like_key_type(field) {
        (None, rest)
    } else {
        let namespaces = options(field, &mut patterns)?;
        (namespaces, skip_space(after))
    };
    let key = key_field(rest)?;
    Ok(Entry {
        principals,
        namespaces,
        key,
    })
}

/// The principals field and what follows it: a double-quoted field, or one up to the next
/// space or tab. Nothing after it is a line with no key.
fn principals_field(line: &[u8]) -> Result<(&[u8], &[u8]), LineError> {
    let (field, rest) = if let Some(quoted) = line.strip_prefix(b"\"") {
        let end = quoted
            .iter()
            .position(|b| *b == b'"')
            .ok_or(INVALID_PRINCIPALS)?;
        let rest = &quoted[end + 1..];
        if !rest.is_empty() && !is_space(rest[0]) {
            return Err(INVALID_PRINCIPALS.into());
        }
        (&quoted[..end], rest)
    } else {
        let end = line.iter().position(|b| is_space(*b)).unwrap_or(line.len());
        line.split_at(end)
    };
    if skip_space(rest).is_empty() {
        return Err(INVALID_KEY.into());
    }
    Ok((field, rest))
}

/// The field up to the next space or tab outside double quotes (where `\` escapes the next
/// byte), and what follows it. An unclosed quote is an invalid option.
fn option_field(s: &[u8]) -> Result<(&[u8], &[u8]), LineError> {
    let mut quoted = false;
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'\\' if quoted => i += 1,
            b'"' => quoted = !quoted,
            b if !quoted && is_space(b) => break,
            _ => {}
        }
        i += 1;
    }
    if quoted || i > s.len() {
        return Err(INVALID_OPTION.into());
    }
    Ok(s.split_at(i))
}

/// Whether `field` names an SSH key type, so is the key rather than options.
fn looks_like_key_type(field: &[u8]) -> bool {
    !field.contains(&b'=')
        && !field.contains(&b',')
        && ["ssh-", "sk-", "ecdsa-", "rsa-"]
            .iter()
            .any(|p| field.starts_with(p.as_bytes()))
}

/// The options field: the `namespaces` patterns when present. Every pattern counts toward
/// `patterns`.
fn options(field: &[u8], patterns: &mut usize) -> Result<Option<Vec<String>>, LineError> {
    let text = std::str::from_utf8(field).map_err(|_| INVALID_OPTION)?;
    let mut seen = [false; OPTIONS.len()];
    let mut namespaces = None;
    let mut rest = text;
    loop {
        let end = rest.find(['=', ',']).unwrap_or(rest.len());
        let name = &rest[..end];
        let lower = name.to_ascii_lowercase();
        if lower == "cert-authority" {
            return Err(CERT_AUTHORITY.into());
        }
        let Some(which) = OPTIONS.iter().position(|o| *o == lower) else {
            if name.is_empty() {
                return Err(INVALID_OPTION.into());
            }
            let hint = nearest(&lower, &OPTIONS)
                .map(|n| format!("; did you mean {n}?"))
                .unwrap_or_default();
            return Err(LineError::Cause(format!("unknown option {name}{hint}")));
        };
        if seen[which] {
            return Err(DUPLICATE_OPTION.into());
        }
        seen[which] = true;
        let (value, after) = quoted_value(&rest[end..])?;
        if which == 0 {
            namespaces = Some(pattern_list(&value, patterns, INVALID_OPTION)?);
        }
        match after.strip_prefix(',') {
            None if after.is_empty() => return Ok(namespaces),
            Some(next) if !next.is_empty() => rest = next,
            _ => return Err(INVALID_OPTION.into()),
        }
    }
}

/// `="value"` at the start of `s`: the value with each `\"` read as `"`, and what follows the
/// closing quote.
fn quoted_value(s: &str) -> Result<(String, &str), LineError> {
    let body = s.strip_prefix("=\"").ok_or(INVALID_OPTION)?;
    let mut value = String::new();
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Ok((value, &body[i + 1..])),
            '\\' if body[i + 1..].starts_with('"') => {
                chars.next();
                value.push('"');
            }
            c => value.push(c),
        }
    }
    Err(INVALID_OPTION.into())
}

/// The comma-separated patterns of `text`: none empty (a bare `!` included), each at most
/// [`MAX_PATTERN`] bytes, and with `patterns` at most [`MAX_PATTERNS`] for the entry.
fn pattern_list(
    text: &str,
    patterns: &mut usize,
    invalid: &'static str,
) -> Result<Vec<String>, LineError> {
    let mut out = Vec::new();
    for p in text.split(',') {
        if p.strip_prefix('!').unwrap_or(p).is_empty() {
            return Err(invalid.into());
        }
        if p.len() > MAX_PATTERN {
            return Err(LineError::Limit(signing_limit(&format!(
                "{MAX_PATTERN} bytes"
            ))));
        }
        *patterns += 1;
        if *patterns > MAX_PATTERNS {
            return Err(LineError::Limit(signing_limit(&format!(
                "{MAX_PATTERNS} patterns"
            ))));
        }
        out.push(p.to_string());
    }
    Ok(out)
}

/// The key field: an accepted key type and its base64 blob, read with the shared key parser;
/// anything after a space or tab is a comment.
fn key_field(s: &[u8]) -> Result<String, LineError> {
    let (ty, rest) = split_field(s);
    let (b64, _comment) = split_field(skip_space(rest));
    let (Ok(ty), Ok(b64)) = (std::str::from_utf8(ty), std::str::from_utf8(b64)) else {
        return Err(INVALID_KEY.into());
    };
    if !KEY_TYPES.contains(&ty) || b64.is_empty() {
        return Err(INVALID_KEY.into());
    }
    key_fingerprint(&format!("{ty} {b64}")).map_err(|()| INVALID_KEY.into())
}

/// The bytes up to the next space or tab, and the rest.
fn split_field(s: &[u8]) -> (&[u8], &[u8]) {
    s.split_at(s.iter().position(|b| is_space(*b)).unwrap_or(s.len()))
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r')
}

fn skip_space(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|b| !is_space(*b)).unwrap_or(s.len());
    &s[start..]
}

/// Whether `s` matches the pattern list: no negated pattern matches, and another one does.
fn list_matches(s: &str, patterns: &[String]) -> bool {
    let mut positive = false;
    for p in patterns {
        match p.strip_prefix('!') {
            Some(negated) if wildcard(s.as_bytes(), negated.as_bytes()) => return false,
            Some(_) => {}
            None => positive |= wildcard(s.as_bytes(), p.as_bytes()),
        }
    }
    positive
}

/// OpenSSH's `*` and `?` match of the whole of `s` against `pattern`, byte by byte. Iterative,
/// with one backtrack point: time at most `s.len() * pattern.len()` steps, no recursion.
fn wildcard(s: &[u8], pattern: &[u8]) -> bool {
    let (mut si, mut pi) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        match pattern.get(pi) {
            Some(b'*') => {
                star = Some((pi, si));
                pi += 1;
            }
            Some(&b) if b == b'?' || b == s[si] => {
                si += 1;
                pi += 1;
            }
            _ => match star {
                Some((sp, ss)) => {
                    pi = sp + 1;
                    si = ss + 1;
                    star = Some((sp, ss + 1));
                }
                None => return false,
            },
        }
    }
    pattern[pi..].iter().all(|b| *b == b'*')
}

#[cfg(test)]
mod tests {
    use super::wildcard;

    #[test]
    fn sig6_wildcard_follows_openssh() {
        for (s, p, want) in [
            ("abc", "abc", true),
            ("abc", "a*", true),
            ("abc", "*c", true),
            ("abc", "a?c", true),
            ("abc", "a?", false),
            ("", "*", true),
            ("", "", true),
            ("a", "", false),
            ("aXbYc", "a*b*c", true),
            ("aXbYd", "a*b*c", false),
            ("Abc", "abc", false),
            ("a*c", "a\\*c", false),
        ] {
            assert_eq!(wildcard(s.as_bytes(), p.as_bytes()), want, "{s:?} {p:?}");
        }
    }
}
