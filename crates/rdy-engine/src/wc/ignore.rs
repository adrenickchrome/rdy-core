//! The .rdyignore matcher (FW-X5): a subset of gitignore. Patterns are matched by walking a table
//! of reachable states over a path's segments and each segment's characters, never by recursive
//! backtracking, so a hostile pattern stays linear in the length of the path.

use crate::error::*;
use std::io::Read;
use std::path::Path;

/// The ignore file, in the root of the working tree (or of an import directory).
pub const IGNORE_FILE: &str = ".rdyignore";
/// The largest `.rdyignore` rdy reads.
pub const MAX_IGNORE_BYTES: usize = 65_536;
/// The most patterns a `.rdyignore` may hold.
pub const MAX_IGNORE_PATTERNS: usize = 1024;

const LIMIT_FIX: &str = "shorten .rdyignore; one pattern can cover many files (**/*.draft.yaml)";

/// The parsed patterns of one `.rdyignore`, in file order (the last match wins).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ignore {
    patterns: Vec<Pattern>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pattern {
    /// `!pattern`: a match re-includes the path.
    negate: bool,
    /// `pattern/`: matches directories only.
    dir_only: bool,
    /// A pattern with a `/` before its end matches the whole path from the root; one without
    /// matches the last segment at any depth (then `segs` holds one glob).
    anchored: bool,
    segs: Vec<Seg>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Seg {
    /// `**` as a whole segment: any number of segments.
    Any,
    Glob(Vec<Tok>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Lit(char),
    /// `?`
    One,
    /// `*`
    Star,
    /// `[…]`: ranges, negated by a leading `!` or `^`.
    Class(bool, Vec<(char, char)>),
}

impl Ignore {
    /// No patterns: nothing is ignored.
    pub fn empty() -> Ignore {
        Ignore::default()
    }

    /// Reads `<root>/.rdyignore`. A missing file is no patterns; a file that is not a regular
    /// file, is over `MAX_IGNORE_BYTES` or is not UTF-8 is `E_CONFIG`. A link is never followed.
    pub fn load(root: &Path) -> Result<Ignore, RdyError> {
        let path = root.join(IGNORE_FILE);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ignore::empty()),
            Err(e) => return Err(read_error(&e)),
        };
        if !meta.is_file() {
            return Err(RdyError::new(
                E_CONFIG,
                ".rdyignore is not a regular file",
                "replace .rdyignore with a plain text file (rdy does not follow links)",
            ));
        }
        if meta.len() > MAX_IGNORE_BYTES as u64 {
            return Err(over_bytes());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .and_then(|f| f.take(MAX_IGNORE_BYTES as u64 + 1).read_to_end(&mut bytes))
            .map_err(|e| read_error(&e))?;
        let text = String::from_utf8(bytes).map_err(|_| {
            RdyError::new(
                E_CONFIG,
                ".rdyignore is not UTF-8",
                "save .rdyignore as UTF-8 text",
            )
        })?;
        Ignore::parse(&text)
    }

    /// Parses the text of a `.rdyignore`. Over `MAX_IGNORE_BYTES` bytes or `MAX_IGNORE_PATTERNS`
    /// patterns is `E_CONFIG`.
    pub fn parse(text: &str) -> Result<Ignore, RdyError> {
        if text.len() > MAX_IGNORE_BYTES {
            return Err(over_bytes());
        }
        let mut patterns = Vec::new();
        for raw in text.split('\n') {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line = trim_trailing_spaces(line);
            let Some(p) = Pattern::parse(line) else {
                continue;
            };
            if patterns.len() == MAX_IGNORE_PATTERNS {
                return Err(RdyError::new(
                    E_CONFIG,
                    format!(".rdyignore is over {MAX_IGNORE_PATTERNS} patterns"),
                    LIMIT_FIX,
                ));
            }
            patterns.push(p);
        }
        Ok(Ignore { patterns })
    }

    /// Whether `rel` (relative to the root, `/`-separated) is ignored. Each parent directory is
    /// checked first: a path under an ignored directory is ignored, and no pattern re-includes it.
    pub fn is_ignored(&self, rel: &str, is_dir: bool) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let segs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
        if segs.is_empty() {
            return false;
        }
        (1..segs.len()).any(|k| self.decide(&segs[..k], true)) || self.decide(&segs, is_dir)
    }

    /// The last pattern that matches decides.
    fn decide(&self, segs: &[&str], is_dir: bool) -> bool {
        self.patterns
            .iter()
            .rev()
            .find(|p| p.matches(segs, is_dir))
            .is_some_and(|p| !p.negate)
    }
}

fn over_bytes() -> RdyError {
    RdyError::new(
        E_CONFIG,
        format!(".rdyignore is over {MAX_IGNORE_BYTES} bytes"),
        LIMIT_FIX,
    )
}

fn read_error(e: &std::io::Error) -> RdyError {
    RdyError::new(
        E_CONFIG,
        format!("cannot read .rdyignore: {e}"),
        "check the file's permissions",
    )
}

/// Drops trailing spaces unless escaped with a backslash.
fn trim_trailing_spaces(mut line: &str) -> &str {
    while let Some(rest) = line.strip_suffix(' ') {
        let slashes = rest.chars().rev().take_while(|c| *c == '\\').count();
        if slashes % 2 == 1 {
            break;
        }
        line = rest;
    }
    line
}

impl Pattern {
    /// One non-comment line; None when it holds no pattern (e.g. `/` or `!`).
    fn parse(line: &str) -> Option<Pattern> {
        let (negate, body) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        let trimmed = body.trim_end_matches('/');
        let dir_only = trimmed.len() != body.len();
        let anchored = trimmed.contains('/');
        let parts: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return None;
        }
        let segs = parts
            .iter()
            .map(|s| {
                if anchored && *s == "**" {
                    Seg::Any
                } else {
                    Seg::Glob(glob(s))
                }
            })
            .collect();
        Some(Pattern {
            negate,
            dir_only,
            anchored,
            segs,
        })
    }

    fn matches(&self, path: &[&str], is_dir: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        if !self.anchored {
            let (Some(Seg::Glob(toks)), Some(name)) = (self.segs.first(), path.last()) else {
                return false;
            };
            return glob_match(toks, name);
        }
        segs_match(&self.segs, path)
    }
}

/// Tokens of one segment. `\x` is a literal `x`; a `[` with no closing `]` is a literal.
fn glob(s: &str) -> Vec<Tok> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' if i + 1 < chars.len() => {
                out.push(Tok::Lit(chars[i + 1]));
                i += 2;
            }
            '*' => {
                if out.last() != Some(&Tok::Star) {
                    out.push(Tok::Star);
                }
                i += 1;
            }
            '?' => {
                out.push(Tok::One);
                i += 1;
            }
            '[' => match class(&chars, i + 1) {
                Some((tok, next)) => {
                    out.push(tok);
                    i = next;
                }
                None => {
                    out.push(Tok::Lit('['));
                    i += 1;
                }
            },
            c => {
                out.push(Tok::Lit(c));
                i += 1;
            }
        }
    }
    out
}

/// A class starting after its `[` at `start`: the token and the index after its `]`.
fn class(chars: &[char], start: usize) -> Option<(Tok, usize)> {
    let mut i = start;
    let negated = matches!(chars.get(i), Some('!' | '^'));
    if negated {
        i += 1;
    }
    let mut ranges = Vec::new();
    let first = i;
    while i < chars.len() {
        let c = match chars[i] {
            ']' if i > first => return Some((Tok::Class(negated, ranges), i + 1)),
            '\\' if i + 1 < chars.len() => {
                i += 1;
                chars[i]
            }
            c => c,
        };
        i += 1;
        if chars.get(i) == Some(&'-') && chars.get(i + 1).is_some_and(|n| *n != ']') {
            let hi = if chars[i + 1] == '\\' && i + 2 < chars.len() {
                i += 1;
                chars[i + 1]
            } else {
                chars[i + 1]
            };
            ranges.push((c, hi));
            i += 2;
        } else {
            ranges.push((c, c));
        }
    }
    None
}

fn tok_takes(t: &Tok, c: char) -> bool {
    match t {
        Tok::Lit(x) => *x == c,
        Tok::One => c != '/',
        Tok::Star => false,
        Tok::Class(neg, ranges) => {
            c != '/' && (ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&c)) != *neg)
        }
    }
}

/// `name` against one segment's tokens: the set of reachable token positions, advanced per
/// character. A `*` stays in place on any character and may be skipped.
fn glob_match(toks: &[Tok], name: &str) -> bool {
    let n = toks.len();
    let close = |set: &mut [bool]| {
        for i in 0..n {
            if set[i] && toks[i] == Tok::Star {
                set[i + 1] = true;
            }
        }
    };
    let mut cur = vec![false; n + 1];
    let mut next = vec![false; n + 1];
    cur[0] = true;
    close(&mut cur);
    for c in name.chars() {
        next.fill(false);
        for i in 0..n {
            if !cur[i] {
                continue;
            }
            if toks[i] == Tok::Star {
                if c != '/' {
                    next[i] = true;
                }
            } else if tok_takes(&toks[i], c) {
                next[i + 1] = true;
            }
        }
        close(&mut next);
        std::mem::swap(&mut cur, &mut next);
        if !cur.iter().any(|s| *s) {
            return false;
        }
    }
    cur[n]
}

/// A path's segments against an anchored pattern's segments, the same way. A `**` matches zero
/// or more segments, except as the last segment (`x/**`), where it matches one or more.
fn segs_match(segs: &[Seg], path: &[&str]) -> bool {
    let n = segs.len();
    let close = |set: &mut [bool]| {
        for i in 0..n.saturating_sub(1) {
            if set[i] && segs[i] == Seg::Any {
                set[i + 1] = true;
            }
        }
    };
    let mut cur = vec![false; n + 1];
    let mut next = vec![false; n + 1];
    cur[0] = true;
    close(&mut cur);
    for name in path {
        next.fill(false);
        for i in 0..n {
            if !cur[i] {
                continue;
            }
            match &segs[i] {
                Seg::Any => {
                    next[i] = true;
                    if i + 1 == n {
                        next[n] = true;
                    }
                }
                Seg::Glob(toks) => {
                    if glob_match(toks, name) {
                        next[i + 1] = true;
                    }
                }
            }
        }
        close(&mut next);
        std::mem::swap(&mut cur, &mut next);
        if !cur.iter().any(|s| *s) {
            return false;
        }
    }
    cur[n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_star_forms() {
        let i = Ignore::parse("**/x.yaml\na/**\nb/**/c\n").unwrap();
        assert!(i.is_ignored("x.yaml", false));
        assert!(i.is_ignored("p/q/x.yaml", false));
        assert!(i.is_ignored("a/f.yaml", false));
        assert!(
            !i.is_ignored("a", true),
            "x/** matches what is inside x, not x"
        );
        assert!(i.is_ignored("b/c", false));
        assert!(i.is_ignored("b/1/2/c", false));
        assert!(!i.is_ignored("c", false));
    }

    #[test]
    fn classes_ranges_and_escapes() {
        let i = Ignore::parse("[a-c]1.yaml\n[^x]2.yaml\n\\!bang.yaml\n[x.yaml\nsp\\ \n").unwrap();
        assert!(i.is_ignored("b1.yaml", false));
        assert!(!i.is_ignored("d1.yaml", false));
        assert!(i.is_ignored("y2.yaml", false));
        assert!(!i.is_ignored("x2.yaml", false));
        assert!(i.is_ignored("!bang.yaml", false));
        assert!(
            i.is_ignored("[x.yaml", false),
            "an unclosed class is a literal"
        );
        assert!(
            i.is_ignored("sp ", false),
            "an escaped trailing space stays"
        );
    }

    #[test]
    fn crlf_and_case() {
        let i = Ignore::parse("A.yaml\r\n").unwrap();
        assert!(i.is_ignored("A.yaml", false));
        assert!(!i.is_ignored("a.yaml", false));
    }
}
