//! The .rdyignore matcher (FW-X5): a subset of gitignore. Patterns are matched by walking a table
//! of reachable states over a path's segments and each segment's characters, never by recursive
//! backtracking, so a hostile pattern stays linear in the length of the path. A walk carries the
//! segment states from a directory to its entries (`IgnoreAt`), so deciding one entry costs one
//! segment step per pattern, whatever its depth.

use crate::error::*;
use std::io::Read;
use std::ops::Range;
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
    /// The length of an `IgnoreAt`: one slot per segment state of each anchored pattern.
    slots: usize,
}

/// Where the matcher stands in one directory: for each anchored pattern, the segment states
/// reachable after the directory's path. A walk carries it from a directory to its entries.
#[derive(Clone, Debug, Default)]
pub(crate) struct IgnoreAt {
    reach: Vec<bool>,
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
    /// An anchored pattern's first slot in an `IgnoreAt`.
    base: usize,
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
        let mut slots = 0;
        for raw in text.split('\n') {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line = trim_trailing_spaces(line);
            let Some(mut p) = Pattern::parse(line) else {
                continue;
            };
            if patterns.len() == MAX_IGNORE_PATTERNS {
                return Err(RdyError::new(
                    E_CONFIG,
                    format!(".rdyignore is over {MAX_IGNORE_PATTERNS} patterns"),
                    LIMIT_FIX,
                ));
            }
            if p.anchored {
                p.base = slots;
                slots += p.segs.len() + 1;
            }
            patterns.push(p);
        }
        Ok(Ignore { patterns, slots })
    }

    /// Whether `rel` (relative to the root, `/`-separated) is ignored. Each parent directory is
    /// checked first: a path under an ignored directory is ignored, and no pattern re-includes it.
    pub fn is_ignored(&self, rel: &str, is_dir: bool) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let segs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
        let mut at = self.root();
        for (k, name) in segs.iter().enumerate() {
            let last = k + 1 == segs.len();
            if self.ignores(&at, name, is_dir || !last) {
                return true;
            }
            if !last {
                at = self.enter(&at, name);
            }
        }
        false
    }

    /// The matcher's state at the root.
    pub(crate) fn root(&self) -> IgnoreAt {
        let mut reach = vec![false; self.slots];
        for p in self.patterns.iter().filter(|p| p.anchored) {
            let set = &mut reach[p.range()];
            set[0] = true;
            p.close(set);
        }
        IgnoreAt { reach }
    }

    /// Whether the entry `name` of the directory at `at` is ignored; the last pattern that
    /// matches decides. Only the entry is decided: a walk enters no ignored directory, so the
    /// entry's parents were decided (not ignored) on the way down.
    pub(crate) fn ignores(&self, at: &IgnoreAt, name: &str, is_dir: bool) -> bool {
        let mut scratch = Vec::new();
        self.patterns
            .iter()
            .rev()
            .find(|p| p.matches(at, name, is_dir, &mut scratch))
            .is_some_and(|p| !p.negate)
    }

    /// The matcher's state inside the directory `name` of the directory at `at`.
    pub(crate) fn enter(&self, at: &IgnoreAt, name: &str) -> IgnoreAt {
        let mut reach = vec![false; self.slots];
        for p in self.patterns.iter().filter(|p| p.anchored) {
            p.advance(&at.reach[p.range()], name, &mut reach[p.range()]);
        }
        IgnoreAt { reach }
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
            base: 0,
        })
    }

    /// An anchored pattern's slots in an `IgnoreAt`.
    fn range(&self) -> Range<usize> {
        self.base..self.base + self.segs.len() + 1
    }

    /// Whether the pattern matches the entry `name` of the directory at `at`.
    fn matches(&self, at: &IgnoreAt, name: &str, is_dir: bool, scratch: &mut Vec<bool>) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        if !self.anchored {
            let Some(Seg::Glob(toks)) = self.segs.first() else {
                return false;
            };
            return glob_match(toks, name);
        }
        let n = self.segs.len();
        scratch.clear();
        scratch.resize(n + 1, false);
        self.advance(&at.reach[self.range()], name, scratch);
        scratch[n]
    }

    /// A `**` reaches the segment after it without taking one, except as the last segment
    /// (`x/**`), where it matches one or more.
    fn close(&self, set: &mut [bool]) {
        for i in 0..self.segs.len().saturating_sub(1) {
            if set[i] && self.segs[i] == Seg::Any {
                set[i + 1] = true;
            }
        }
    }

    /// One path segment: the states reachable from `cur` (all false in `next` on entry).
    fn advance(&self, cur: &[bool], name: &str, next: &mut [bool]) {
        let n = self.segs.len();
        if !cur.contains(&true) {
            return;
        }
        count(n + 1);
        for i in 0..n {
            if !cur[i] {
                continue;
            }
            match &self.segs[i] {
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
        self.close(next);
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

// Matcher steps, counted in unit tests only: a guard that the work per entry stays bounded.
#[cfg(test)]
thread_local! {
    static STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count(n: usize) {
    STEPS.with(|s| s.set(s.get() + n as u64));
}

#[cfg(not(test))]
fn count(_: usize) {}

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
        count(n + 1);
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

    fn steps() -> u64 {
        STEPS.with(std::cell::Cell::get)
    }

    /// A pattern built to be slow, 1024 times, against a 32-deep tree: an entry a walk decides
    /// costs one step of each pattern whatever its depth (its parents were decided on the way
    /// down), and `is_ignored` on a deep path one step per segment.
    #[test]
    fn hostile_patterns_cost_one_step_per_entry() {
        const STARS: usize = 28;
        let text = format!("**/{}*b\n", "*a".repeat(STARS)).repeat(MAX_IGNORE_PATTERNS);
        assert!(text.len() <= MAX_IGNORE_BYTES);
        let ig = Ignore::parse(&text).unwrap();
        let t = tempfile::tempdir().unwrap();
        let folder = "a".repeat(27);
        let mut dir = t.path().to_path_buf();
        let mut rel = vec![];
        for i in 0..rdy_model::tree::MAX_FOLDER_DEPTH {
            dir.push(&folder);
            rel.push(folder.as_str());
            std::fs::create_dir(&dir).unwrap();
            let f = format!("id: fd-{i:05}\ntype: folder\norder: a1\nfields:\n  title: F\n");
            std::fs::write(dir.join("_folder.yaml"), f).unwrap();
        }
        for i in 0..20 {
            let d = format!("- id: d-{i:05}\n  type: doc\n  order: a1\n  fields:\n    title: T\n");
            std::fs::write(dir.join(format!("{}{i:02}.yaml", "a".repeat(24))), d).unwrap();
        }
        // one unit: every pattern's glob run once over the longest name (31 characters)
        let unit = (MAX_IGNORE_PATTERNS * (2 * STARS + 2 + 1) * (31 + 1)) as u64;
        let pack = rdy_schema::PackSet::with_builtin()
            .resolve("rdy-standard")
            .unwrap();

        let s = steps();
        let w = crate::tree_io::read_wc(t.path(), &ig, &pack);
        let wc = steps() - s;
        assert_eq!(w.files.len(), 52, "the walk reaches the deepest folder");
        let s = steps();
        let (loaded, _) = crate::tree_io::read_tree_with(t.path(), "docs", Some(&ig));
        let export = steps() - s;
        assert_eq!(
            loaded.len(),
            52,
            "the export walk reaches the deepest folder"
        );
        let deep = format!("{}/x.yaml", rel.join("/"));
        let s = steps();
        assert!(!ig.is_ignored(&deep, false));
        let one = steps() - s;

        // 32 folders: the directory, its _folder.yaml twice and the step into it; 20 docs
        assert!(wc <= 160 * unit, "working-copy walk: {} units", wc / unit);
        assert!(export <= 160 * unit, "export walk: {} units", export / unit);
        assert!(one <= 2 * 33 * unit, "one deep path: {} units", one / unit);
    }

    #[test]
    fn crlf_and_case() {
        let i = Ignore::parse("A.yaml\r\n").unwrap();
        assert!(i.is_ignored("A.yaml", false));
        assert!(!i.is_ignored("a.yaml", false));
    }
}
