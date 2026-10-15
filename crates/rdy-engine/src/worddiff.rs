//! A deterministic word diff with a cost cap.
//!
//! The site marks the removed and added words of each changed block with
//! these pieces (D-W40KZ). There is one diff engine (ra-R7PM6): `rdy_diff`
//! (M3f) reuses this module and serializes each [`Piece`] as `{op, text}`,
//! with `op` one of `same`, `del` or `add` (p-BJYG0).
//!
//! The rules (closed):
//!
//! 1. **Tokens.** With [`Unit::Words`], a token is a maximal run of word
//!    characters (`char::is_alphanumeric()` or `_`), a maximal run of
//!    whitespace, or any other character on its own. An apostrophe (`'` or
//!    `’`) between two word characters joins the word run. With
//!    [`Unit::Lines`], a token is one line with its `\n`; the last line may
//!    lack one. The tokens joined give the text back.
//! 2. **The unit falls back.** With [`Unit::Words`], when either side has
//!    more than [`MAX_WORD_CHARS`] characters, the diff runs with
//!    [`Unit::Lines`].
//! 3. **The edit script** is Myers' shortest edit script over the tokens.
//!    Each step keeps only the furthest-reaching array of width `2d + 1`, so
//!    memory is bounded by [`MAX_EDITS`]. On a tie the path takes the
//!    deletion first.
//! 4. **The cap.** When the script needs more than [`MAX_EDITS`] edits, the
//!    result is `[Del(old), Add(new)]`.
//! 5. **Cleanup, in this order.** (a) Adjacent tokens of one op join into one
//!    piece. (b) A `Same` piece made only of whitespace, with a changed piece
//!    on both sides, joins the change: its text goes into both the `Del` text
//!    and the `Add` text around it. (c) In each run of changed pieces, all
//!    `Del` text comes first as one piece, then all `Add` text as one piece.
//! 6. **Edges.** Both sides empty give `[]`. An empty old side gives
//!    `[Add(new)]`. An empty new side gives `[Del(old)]`. Equal sides give
//!    `[Same(text)]`.
//! 7. **The identity.** The text of every `Same` and `Del` piece, in order,
//!    is `old`. The text of every `Same` and `Add` piece, in order, is `new`.
//!    No piece is empty. Two neighbours never have the same op.
//!
//! The same pair always gives the same pieces: nothing here hashes or
//! depends on the clock.

use serde::Serialize;

/// What a piece does to the old text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    /// The text is on both sides.
    Same,
    /// The text is only on the old side.
    Del,
    /// The text is only on the new side.
    Add,
}

/// One run of text with its op.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Piece {
    pub op: Op,
    pub text: String,
}

/// The token unit of a diff (rule 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Words,
    Lines,
}

/// Above this many characters on either side, a word diff runs by lines (rule 2).
pub const MAX_WORD_CHARS: usize = 20_000;

/// Above this many edits, the diff replaces the whole text (rule 4).
pub const MAX_EDITS: usize = 1_000;

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_apostrophe(c: char) -> bool {
    c == '\'' || c == '\u{2019}'
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Other,
}

/// Splits `text` into tokens of `unit` (rule 1). The tokens joined give `text`.
pub fn tokens(text: &str, unit: Unit) -> Vec<&str> {
    match unit {
        Unit::Lines => text.split_inclusive('\n').collect(),
        Unit::Words => word_tokens(text),
    }
}

fn word_tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut start = 0;
    let mut run: Option<Class> = None;
    let mut prev_word = false;
    while let Some((i, c)) = chars.next() {
        let class = if is_word(c) {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else if is_apostrophe(c)
            && prev_word
            && run == Some(Class::Word)
            && chars.peek().is_some_and(|&(_, n)| is_word(n))
        {
            Class::Word
        } else {
            Class::Other
        };
        let joins = run == Some(class) && class != Class::Other;
        if !joins {
            if run.is_some() {
                out.push(&text[start..i]);
            }
            start = i;
            run = Some(class);
        }
        prev_word = is_word(c);
    }
    if run.is_some() {
        out.push(&text[start..]);
    }
    out
}

/// The word diff of `old` and `new` (rules 1 to 7).
pub fn diff(old: &str, new: &str, unit: Unit) -> Vec<Piece> {
    match (old.is_empty(), new.is_empty()) {
        (true, true) => return Vec::new(),
        (true, false) => return vec![piece(Op::Add, new)],
        (false, true) => return vec![piece(Op::Del, old)],
        (false, false) if old == new => return vec![piece(Op::Same, old)],
        (false, false) => {}
    }
    let too_long = |s: &str| s.chars().nth(MAX_WORD_CHARS).is_some();
    let unit = if unit == Unit::Words && (too_long(old) || too_long(new)) {
        Unit::Lines
    } else {
        unit
    };
    let a = tokens(old, unit);
    let b = tokens(new, unit);
    match script(&a, &b) {
        Some(steps) => cleanup(join(&a, &b, &steps)),
        None => vec![piece(Op::Del, old), piece(Op::Add, new)],
    }
}

fn piece(op: Op, text: &str) -> Piece {
    Piece {
        op,
        text: text.to_owned(),
    }
}

/// Myers' shortest edit script (rule 3), or `None` past [`MAX_EDITS`] (rule 4).
/// `trace[d]` is the furthest-reaching array after `d` edits, indexed `k + d`.
fn script(a: &[&str], b: &[&str]) -> Option<Vec<Op>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    if n.abs_diff(m) > MAX_EDITS {
        return None;
    }
    let mut trace: Vec<Vec<isize>> = Vec::new();
    for d in 0..=MAX_EDITS as isize {
        let mut v = vec![0isize; (2 * d + 1) as usize];
        let mut done = false;
        for k in (-d..=d).step_by(2) {
            let mut x = if d == 0 {
                0
            } else {
                let prev = &trace[(d - 1) as usize];
                let at = |k: isize| prev[(k + d - 1) as usize];
                if takes_insertion(k, d, at) {
                    at(k + 1)
                } else {
                    at(k - 1) + 1
                }
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[(k + d) as usize] = x;
            if x >= n && y >= m {
                done = true;
            }
        }
        trace.push(v);
        if done {
            return Some(backtrack(&trace, n, m));
        }
    }
    None
}

/// Whether diagonal `k` at step `d` comes down from `k + 1` (an insertion).
/// On a tie it comes across from `k - 1` (a deletion).
fn takes_insertion(k: isize, d: isize, at: impl Fn(isize) -> isize) -> bool {
    k == -d || (k != d && at(k - 1) < at(k + 1))
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize) -> Vec<Op> {
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..trace.len() as isize).rev() {
        let prev = &trace[(d - 1) as usize];
        let at = |k: isize| prev[(k + d - 1) as usize];
        let k = x - y;
        let insert = takes_insertion(k, d, at);
        let pk = if insert { k + 1 } else { k - 1 };
        let px = at(pk);
        let py = px - pk;
        let (mx, my) = if insert { (px, py + 1) } else { (px + 1, py) };
        while x > mx && y > my {
            ops.push(Op::Same);
            x -= 1;
            y -= 1;
        }
        ops.push(if insert { Op::Add } else { Op::Del });
        (x, y) = (px, py);
    }
    while x > 0 && y > 0 {
        ops.push(Op::Same);
        x -= 1;
        y -= 1;
    }
    ops.reverse();
    ops
}

/// Rule 5 (a): adjacent tokens of one op join into one piece.
fn join(a: &[&str], b: &[&str], steps: &[Op]) -> Vec<Piece> {
    let (mut i, mut j) = (0, 0);
    let mut out: Vec<Piece> = Vec::new();
    for &op in steps {
        let tok = match op {
            Op::Same => {
                j += 1;
                i += 1;
                a[i - 1]
            }
            Op::Del => {
                i += 1;
                a[i - 1]
            }
            Op::Add => {
                j += 1;
                b[j - 1]
            }
        };
        match out.last_mut() {
            Some(last) if last.op == op => last.text.push_str(tok),
            _ => out.push(piece(op, tok)),
        }
    }
    out
}

/// Rule 5 (b) and (c).
fn cleanup(pieces: Vec<Piece>) -> Vec<Piece> {
    let last = pieces.len().saturating_sub(1);
    let mut out = Vec::new();
    let (mut del, mut add) = (String::new(), String::new());
    for (i, p) in pieces.into_iter().enumerate() {
        match p.op {
            Op::Del => del.push_str(&p.text),
            Op::Add => add.push_str(&p.text),
            // Pieces from rule 5 (a) alternate, so both neighbours of an
            // inner `Same` piece are changed pieces.
            Op::Same if i != 0 && i != last && p.text.chars().all(char::is_whitespace) => {
                del.push_str(&p.text);
                add.push_str(&p.text);
            }
            Op::Same => {
                flush(&mut out, &mut del, &mut add);
                out.push(p);
            }
        }
    }
    flush(&mut out, &mut del, &mut add);
    out
}

fn flush(out: &mut Vec<Piece>, del: &mut String, add: &mut String) {
    if !del.is_empty() {
        out.push(Piece {
            op: Op::Del,
            text: std::mem::take(del),
        });
    }
    if !add.is_empty() {
        out.push(Piece {
            op: Op::Add,
            text: std::mem::take(add),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(p: &[Piece]) -> String {
        p.iter()
            .map(|p| match p.op {
                Op::Same => p.text.clone(),
                Op::Del => format!("[-{}-]", p.text),
                Op::Add => format!("{{+{}+}}", p.text),
            })
            .collect()
    }

    fn sides(p: &[Piece]) -> (String, String) {
        let pick = |keep: Op| {
            p.iter()
                .filter(|x| x.op == Op::Same || x.op == keep)
                .map(|x| x.text.as_str())
                .collect()
        };
        (pick(Op::Del), pick(Op::Add))
    }

    /// A fixed pseudo-random generator, so the test needs no crate and repeats exactly.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn text(&mut self) -> String {
            const WORDS: [&str; 8] = ["a", "rule", "the", "agent's", "größe", ",", "  ", "\n"];
            (0..self.next() % 30)
                .map(|_| WORDS[(self.next() % 8) as usize])
                .collect::<Vec<_>>()
                .join(" ")
        }
    }

    #[test]
    fn sr1_a_one_word_edit_marks_one_word() {
        assert_eq!(
            show(&diff("Where to start.", "Where to begin.", Unit::Words)),
            "Where to [-start-]{+begin+}."
        );
        assert_eq!(
            show(&diff("Old wording.", "New wording.", Unit::Words)),
            "[-Old-]{+New+} wording."
        );
    }

    #[test]
    fn sr1_the_pieces_rebuild_both_sides() {
        let mut g = Lcg(0x7372_3031);
        for _ in 0..500 {
            let (a, b) = (g.text(), g.text());
            let p = diff(&a, &b, Unit::Words);
            assert_eq!(sides(&p), (a.clone(), b.clone()), "{a:?} -> {b:?}");
            assert!(p.iter().all(|x| !x.text.is_empty()));
            assert!(p.windows(2).all(|w| w[0].op != w[1].op));
        }
    }

    #[test]
    fn sr1_deletions_come_before_insertions() {
        assert_eq!(
            show(&diff("one two three", "one four three", Unit::Words)),
            "one [-two-]{+four+} three"
        );
        let p = diff("keep x keep", "keep y z keep", Unit::Words);
        let first_change = p.iter().position(|x| x.op != Op::Same).unwrap();
        assert_eq!(p[first_change].op, Op::Del, "{}", show(&p));
    }

    #[test]
    fn sr1_whitespace_between_changes_joins_them() {
        assert_eq!(
            show(&diff("a b c", "x y c", Unit::Words)),
            "[-a b-]{+x y+} c"
        );
    }

    #[test]
    fn sr1_new_and_removed_text_is_whole() {
        assert_eq!(
            diff("", "all new", Unit::Words),
            vec![Piece {
                op: Op::Add,
                text: "all new".into()
            }]
        );
        assert_eq!(
            diff("all gone", "", Unit::Words),
            vec![Piece {
                op: Op::Del,
                text: "all gone".into()
            }]
        );
        assert_eq!(
            diff("same", "same", Unit::Words),
            vec![Piece {
                op: Op::Same,
                text: "same".into()
            }]
        );
        assert!(diff("", "", Unit::Words).is_empty());
    }

    #[test]
    fn sr1_a_long_text_falls_back_to_lines() {
        let old = format!("{}\nkeep\n", "x ".repeat(MAX_WORD_CHARS));
        let new = format!("{}\nkeep\n", "y ".repeat(MAX_WORD_CHARS));
        let p = diff(&old, &new, Unit::Words);
        assert_eq!(
            p.last().unwrap(),
            &Piece {
                op: Op::Same,
                text: "keep\n".into()
            }
        );
        assert_eq!(sides(&p), (old, new));
    }

    #[test]
    fn sr1_too_many_edits_replace_the_whole_text() {
        let old: String = (0..1_500).map(|i| format!("a{i} ")).collect();
        let new: String = (0..1_500).map(|i| format!("b{i} ")).collect();
        let p = diff(&old, &new, Unit::Words);
        assert_eq!(
            p,
            vec![
                Piece {
                    op: Op::Del,
                    text: old
                },
                Piece {
                    op: Op::Add,
                    text: new
                }
            ]
        );
    }

    #[test]
    fn sr1_the_cap_starts_after_max_edits() {
        let lines = |n: usize| -> String { (0..n).map(|i| format!("a{i}\n")).collect() };
        let at_cap = format!("keep\n{}", lines(MAX_EDITS));
        let p = diff(&at_cap, "keep\n", Unit::Lines);
        assert_eq!(
            p,
            vec![
                Piece {
                    op: Op::Same,
                    text: "keep\n".into()
                },
                Piece {
                    op: Op::Del,
                    text: lines(MAX_EDITS)
                }
            ]
        );
        let past_cap = format!("keep\n{}", lines(MAX_EDITS + 1));
        let p = diff(&past_cap, "keep\n", Unit::Lines);
        assert_eq!(
            p,
            vec![
                Piece {
                    op: Op::Del,
                    text: past_cap
                },
                Piece {
                    op: Op::Add,
                    text: "keep\n".into()
                }
            ]
        );
    }

    #[test]
    fn sr1_the_same_input_gives_the_same_pieces() {
        let (a, b) = (
            "the rule holds; the rule bends",
            "a rule bends; the rule holds",
        );
        assert_eq!(diff(a, b, Unit::Words), diff(a, b, Unit::Words));
    }

    #[test]
    fn sr1_the_script_is_the_shortest() {
        fn lcs(a: &[&str], b: &[&str]) -> usize {
            let mut row = vec![0; b.len() + 1];
            for x in a {
                let mut diag = 0;
                for (j, y) in b.iter().enumerate() {
                    let up = row[j + 1];
                    row[j + 1] = if x == y { diag + 1 } else { up.max(row[j]) };
                    diag = up;
                }
            }
            row[b.len()]
        }
        let mut g = Lcg(0x7372_3032);
        for _ in 0..500 {
            let (a, b) = (g.text(), g.text());
            let (ta, tb) = (tokens(&a, Unit::Words), tokens(&b, Unit::Words));
            let edits = script(&ta, &tb)
                .unwrap()
                .iter()
                .filter(|op| **op != Op::Same)
                .count();
            assert_eq!(
                edits,
                ta.len() + tb.len() - 2 * lcs(&ta, &tb),
                "{a:?} -> {b:?}"
            );
        }
    }

    #[test]
    fn sr1_words_keep_apostrophes_and_unicode() {
        assert_eq!(
            tokens("the agent's größe, ok", Unit::Words),
            ["the", " ", "agent's", " ", "größe", ",", " ", "ok"]
        );
        assert_eq!(tokens("a\nb", Unit::Lines), ["a\n", "b"]);
    }
}
