//! Path globs for pack areas (glob1).
//!
//! A glob is a `/`-separated list of segments. A segment is `**` (any number of path segments,
//! including none) or a run of literal name characters, `*` (zero or more name characters inside
//! one segment) and `?` (exactly one name character). Matching is iterative and polynomial:
//! no recursion, no backtracking blow-up.

/// The longest pattern, in bytes.
pub const MAX_GLOB_LEN: usize = 256;
/// The most segments a path may have and still match: the matcher's own guard.
pub const MAX_PATH_SEGMENTS: usize = 64;

/// Why a pattern was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GlobError {
    #[error("a path glob cannot be empty")]
    Empty,
    #[error("a path glob is at most {} bytes; this one is {len}", MAX_GLOB_LEN)]
    TooLong { len: usize },
    #[error(
        "character {ch:?} at byte {at} is not allowed in a path glob: use A-Z, a-z, 0-9, '.', '_', '-', '/', '*', '?'"
    )]
    BadChar { ch: char, at: usize },
    #[error("a path glob has no leading, trailing or doubled '/'")]
    BadSlash,
    #[error("'**' must be a whole segment (as in 'a/**/b'); '***' is not allowed")]
    BadDoubleStar,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Lit(u8),
    Star,
    Q,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    /// `**`
    Any,
    Pat(Vec<Tok>),
}

/// A compiled path glob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glob {
    pattern: String,
    segments: Vec<Segment>,
}

impl Glob {
    /// Validate and compile `pattern`.
    pub fn compile(pattern: &str) -> Result<Glob, GlobError> {
        if pattern.is_empty() {
            return Err(GlobError::Empty);
        }
        if pattern.len() > MAX_GLOB_LEN {
            return Err(GlobError::TooLong { len: pattern.len() });
        }
        for (at, ch) in pattern.char_indices() {
            if !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '/' | '*' | '?')) {
                return Err(GlobError::BadChar { ch, at });
            }
        }
        let mut segments = Vec::new();
        for seg in pattern.split('/') {
            if seg.is_empty() {
                return Err(GlobError::BadSlash);
            }
            if seg == "**" {
                segments.push(Segment::Any);
            } else if seg.contains("**") {
                return Err(GlobError::BadDoubleStar);
            } else {
                segments.push(Segment::Pat(
                    seg.bytes()
                        .map(|b| match b {
                            b'*' => Tok::Star,
                            b'?' => Tok::Q,
                            b => Tok::Lit(b),
                        })
                        .collect(),
                ));
            }
        }
        Ok(Glob {
            pattern: pattern.to_string(),
            segments,
        })
    }

    /// The pattern as written.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Whole-path match.
    pub fn matches(&self, path: &str) -> bool {
        match self.prefix_matches(path, &mut 0) {
            Some(row) => row.last().copied().unwrap_or(false),
            None => false,
        }
    }

    /// Whole-path match, or a match of a proper prefix of the path ending at a `/`.
    pub fn covers(&self, path: &str) -> bool {
        self.covers_counting(path).0
    }

    /// [`Glob::covers`], and the matcher's step count: one per table cell and one per scan of a
    /// name character. It stays within `(pattern bytes + segments + 1) x (path bytes + segments
    /// + 1)` whatever the stars: the test hook that pins the matcher as polynomial.
    #[doc(hidden)]
    pub fn covers_counting(&self, path: &str) -> (bool, u64) {
        let mut steps = 0;
        let covered = match self.prefix_matches(path, &mut steps) {
            Some(row) => row.iter().skip(1).any(|&m| m),
            None => false,
        };
        (covered, steps)
    }

    /// `row[k]` is whether the pattern matches the first `k` segments of `path`. `None` for a path
    /// that is empty, has an empty segment, or has more than `MAX_PATH_SEGMENTS` segments.
    fn prefix_matches(&self, path: &str, steps: &mut u64) -> Option<Vec<bool>> {
        if path.is_empty() {
            return None;
        }
        let segs: Vec<&str> = path.split('/').collect();
        if segs.len() > MAX_PATH_SEGMENTS || segs.iter().any(|s| s.is_empty()) {
            return None;
        }
        let n = segs.len();
        // prev[j]: the first i pattern segments match the first j path segments.
        let mut prev = vec![false; n + 1];
        prev[0] = true;
        for seg in &self.segments {
            let mut next = vec![false; n + 1];
            match seg {
                Segment::Any => {
                    for j in 0..=n {
                        *steps += 1;
                        next[j] = prev[j] || (j > 0 && next[j - 1]);
                    }
                }
                Segment::Pat(toks) => {
                    for j in 1..=n {
                        *steps += 1;
                        next[j] = prev[j - 1] && segment_matches(toks, segs[j - 1], steps);
                    }
                }
            }
            prev = next;
        }
        Some(prev)
    }
}

/// `*`/`?` matching of one segment: the standard single-backtrack-point scan, O(pattern x name).
/// `?` is one character. Tree names are ASCII, so the common case scans bytes without allocating.
fn segment_matches(toks: &[Tok], name: &str, steps: &mut u64) -> bool {
    if name.is_ascii() {
        scan(toks, name.as_bytes(), |lit, b| lit == *b, steps)
    } else {
        let chars: Vec<char> = name.chars().collect();
        scan(toks, &chars, |lit, c| char::from(lit) == *c, steps)
    }
}

fn scan<T>(toks: &[Tok], name: &[T], lit_eq: impl Fn(u8, &T) -> bool, steps: &mut u64) -> bool {
    let (mut t, mut s) = (0usize, 0usize);
    let mut backtrack: Option<(usize, usize)> = None;
    while s < name.len() {
        *steps += 1;
        match toks.get(t) {
            Some(Tok::Star) => {
                backtrack = Some((t, s));
                t += 1;
            }
            Some(Tok::Q) => {
                t += 1;
                s += 1;
            }
            Some(Tok::Lit(b)) if lit_eq(*b, &name[s]) => {
                t += 1;
                s += 1;
            }
            _ => match backtrack {
                Some((bt, bs)) => {
                    backtrack = Some((bt, bs + 1));
                    t = bt + 1;
                    s = bs + 1;
                }
                None => return false,
            },
        }
    }
    toks[t..].iter().all(|tok| *tok == Tok::Star)
}
