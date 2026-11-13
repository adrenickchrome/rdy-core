//! The paragraphs and lists of a prose body, and the parts of a list item (D-7M4TR): after a
//! blank line, a line indented past an item's marker starts a new paragraph of the item; a line
//! indented past the marker that starts with three or more backticks or tildes opens fenced code,
//! which the same fence (at least as long) closes, or the end of the item. A fence outside a list
//! item stays text.
//!
//! This is the one reading of a body every reader shares: the renderer shows a body from
//! [`parse_body`], and [`crate::ref_spans`] reads no reference inside the byte ranges
//! [`code_ranges`] gives, so a write, a validation, the graph and every page agree that list code
//! is literal.
use std::ops::Range;

/// The most lists a body nests.
pub const MAX_DEPTH: usize = 6;

/// A top-level part of a body: a paragraph, or a list.
#[derive(Debug)]
pub enum Block {
    Para(String),
    List(List),
}

/// A list: numbered or not, its first number, its marker's indent and its items.
#[derive(Debug)]
pub struct List {
    pub ordered: bool,
    pub start: u32,
    pub indent: usize,
    pub items: Vec<Item>,
}

/// A list item: its parts in the order they are written (D-7M4TR). The first part is the
/// marker line's paragraph.
#[derive(Debug)]
pub struct Item {
    pub parts: Vec<Part>,
}

/// One part of a list item (D-7M4TR): a paragraph of spans, fenced code (literal text: no
/// reference, no markup), or a nested list.
#[derive(Debug)]
pub enum Part {
    Para(String),
    Code { lang: Option<String>, text: String },
    List(List),
}

impl Item {
    fn new(text: &str) -> Item {
        Item {
            parts: vec![Part::Para(text.to_string())],
        }
    }

    /// Adds a line to the paragraph that ends the item, or starts one when the item ends with
    /// another part.
    fn push_line(&mut self, line: &str) {
        match self.parts.last_mut() {
            Some(Part::Para(t)) => {
                t.push('\n');
                t.push_str(line);
            }
            _ => self.parts.push(Part::Para(line.to_string())),
        }
    }
}

/// A line's indent: leading spaces, a tab counting as 4.
fn indent(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// `line` with up to `n` columns of its indent removed (a tab counting as 4).
fn dedent(line: &str, n: usize) -> &str {
    let mut cols = 0;
    for (i, c) in line.char_indices() {
        let w = match c {
            ' ' => 1,
            '\t' => 4,
            _ => return &line[i..],
        };
        if cols + w > n {
            return &line[i..];
        }
        cols += w;
    }
    ""
}

/// A marker line: its indent, whether it is numbered, its number and its text.
fn marker(line: &str) -> Option<(usize, bool, u32, &str)> {
    let rest = line.trim_start_matches([' ', '\t']);
    let (ordered, number, after) = if let Some(r) = rest.strip_prefix(['-', '*']) {
        (false, 1, r)
    } else {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if !(1..=3).contains(&digits) {
            return None;
        }
        let r = rest[digits..].strip_prefix('.')?;
        (true, rest[..digits].parse().ok()?, r)
    };
    if !after.starts_with([' ', '\t']) {
        return None;
    }
    let text = after.trim();
    (!text.is_empty()).then_some((indent(line), ordered, number, text))
}

/// An opening code fence (D-7M4TR rule 2): three or more backticks or tildes, then an optional
/// language. Its character, its length and its language. A backtick fence whose info holds a
/// backtick is no fence (it would read as a code span).
fn fence_open(line: &str) -> Option<(char, usize, Option<String>)> {
    let rest = line.trim_start_matches([' ', '\t']);
    let ch = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = rest.chars().take_while(|c| *c == ch).count();
    if len < 3 {
        return None;
    }
    let info = rest[len..].trim();
    if ch == '`' && info.contains('`') {
        return None;
    }
    let lang = info.split_whitespace().next().map(String::from);
    Some((ch, len, lang))
}

/// Whether `line` closes a fence of `ch` that opened with `len` characters: the same character,
/// at least as many, and nothing else on the line.
fn fence_close(line: &str, ch: char, len: usize) -> bool {
    let rest = line.trim();
    rest.chars().count() >= len && rest.chars().all(|c| c == ch)
}

/// Fenced code being read in a list item.
struct OpenCode {
    ch: char,
    len: usize,
    /// The opening fence line's indent, which each code line drops.
    indent: usize,
    lang: Option<String>,
    lines: Vec<String>,
    /// The bytes of the body the code covers: from its opening fence line to the last line it
    /// took (its closing fence, when it has one).
    span: Range<usize>,
}

impl OpenCode {
    /// The code as a part: a fence with no closing line keeps no blank line at its end.
    fn into_part(mut self) -> Part {
        while self.lines.last().is_some_and(|l| l.trim().is_empty()) {
            self.lines.pop();
        }
        Part::Code {
            lang: self.lang,
            text: self.lines.join("\n"),
        }
    }
}

pub fn parse_body(text: &str) -> Vec<Block> {
    parse(text, &mut vec![])
}

/// The byte ranges of `text` that hold fenced code in a list item (D-7M4TR), each from its
/// opening fence line to its closing fence line, or to the last line of the item when no line
/// closes it. No reference is read in these ranges. A text with no fence has none.
pub fn code_ranges(text: &str) -> Vec<Range<usize>> {
    let mut out = vec![];
    if text.contains("```") || text.contains("~~~") {
        parse(text, &mut out);
    }
    out
}

/// The blocks of `text`, with the byte range of each fenced code in a list item added to `spans`.
fn parse(text: &str, spans: &mut Vec<Range<usize>>) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    // where each line ends in `text`: `lines` borrows `text`, so its offset is the pointer's
    let end_of = |l: &str| l.as_ptr() as usize - text.as_ptr() as usize + l.len();
    // the first line at or after each line that is not blank: one reverse pass, not a scan per
    // blank line
    let mut next_nonblank = vec![lines.len(); lines.len() + 1];
    for (i, l) in lines.iter().enumerate().rev() {
        next_nonblank[i] = if l.trim().is_empty() {
            next_nonblank[i + 1]
        } else {
            i
        };
    }
    let mut out: Vec<Block> = vec![];
    let mut para: Vec<&str> = vec![];
    let mut stack: Vec<List> = vec![];
    // the fenced code of the innermost list's last item, while it is open
    let mut code: Option<OpenCode> = None;
    // a blank line came before this line inside a list: an indented line starts a new paragraph
    let mut after_blank = false;
    fn flush(para: &mut Vec<&str>, out: &mut Vec<Block>) {
        if !para.is_empty() {
            out.push(Block::Para(para.join("\n")));
            para.clear();
        }
    }
    fn close_one(stack: &mut Vec<List>, out: &mut Vec<Block>) {
        if let Some(l) = stack.pop() {
            match stack.last_mut().and_then(|p| p.items.last_mut()) {
                Some(item) => item.parts.push(Part::List(l)),
                None => out.push(Block::List(l)),
            }
        }
    }
    fn close_all(stack: &mut Vec<List>, out: &mut Vec<Block>) {
        while !stack.is_empty() {
            close_one(stack, out);
        }
    }
    /// Ends the open code: it becomes the last part of the innermost list's last item.
    fn end_code(code: &mut Option<OpenCode>, stack: &mut [List], spans: &mut Vec<Range<usize>>) {
        if let Some(c) = code.take() {
            spans.push(c.span.clone());
            if let Some(item) = stack.last_mut().and_then(|l| l.items.last_mut()) {
                item.parts.push(c.into_part());
            }
        }
    }
    let open = |indent: usize, ordered: bool, start: u32, text: &str| List {
        ordered,
        start,
        indent,
        items: vec![Item::new(text)],
    };
    for (i, line) in lines.iter().enumerate() {
        if let Some(c) = code.as_mut() {
            // the code is the item's until a line closes it or a line ends the item
            let item_indent = stack.last().map_or(0, |l| l.indent);
            if line.trim().is_empty() {
                c.lines.push(String::new());
                continue;
            }
            if indent(line) > item_indent {
                c.span.end = end_of(line);
                if fence_close(line, c.ch, c.len) {
                    end_code(&mut code, &mut stack, spans);
                } else {
                    let kept = dedent(line, c.indent).to_string();
                    c.lines.push(kept);
                }
                continue;
            }
            // a line no deeper than the item's marker ends the item, and the code with it
            end_code(&mut code, &mut stack, spans);
            after_blank = false;
        }
        if line.trim().is_empty() {
            flush(&mut para, &mut out);
            if !stack.is_empty() {
                let next = lines.get(next_nonblank[i + 1]);
                // the next line belongs to a list that stays open: a marker, or a line indented
                // past an open list's marker
                let keep = next.is_some_and(|n| {
                    marker(n).is_some() || stack.iter().any(|l| indent(n) > l.indent)
                });
                if keep {
                    after_blank = true;
                } else {
                    close_all(&mut stack, &mut out);
                }
            }
            continue;
        }
        if let Some((ind, ordered, number, item)) = marker(line) {
            after_blank = false;
            flush(&mut para, &mut out);
            while stack.last().is_some_and(|l| l.indent > ind) {
                close_one(&mut stack, &mut out);
            }
            let new_item = Item::new(item);
            let depth = stack.len();
            match stack.last_mut() {
                Some(l) if l.indent == ind && l.ordered == ordered => l.items.push(new_item),
                Some(l) if l.indent == ind => {
                    close_one(&mut stack, &mut out);
                    stack.push(open(ind, ordered, number, item));
                }
                // deeper than a list may nest: a sibling of the innermost list's items
                Some(l) if depth >= MAX_DEPTH => l.items.push(new_item),
                _ => stack.push(open(ind, ordered, number, item)),
            }
            continue;
        }
        let ind = indent(line);
        let fence = fence_open(line);
        // a new part after a blank line, or a fence, belongs to the innermost item whose marker
        // the line is indented past (D-7M4TR rule 1): the lists it is not inside end
        if (after_blank || fence.is_some()) && stack.iter().any(|l| ind > l.indent) {
            while stack.last().is_some_and(|l| l.indent >= ind) {
                close_one(&mut stack, &mut out);
            }
        }
        let blank_before = std::mem::take(&mut after_blank);
        match stack.last_mut() {
            Some(l) if ind > l.indent => {
                let Some(it) = l.items.last_mut() else {
                    continue;
                };
                if let Some((ch, len, lang)) = fence {
                    code = Some(OpenCode {
                        ch,
                        len,
                        indent: ind,
                        lang,
                        lines: vec![],
                        span: end_of(line) - line.len()..end_of(line),
                    });
                } else if blank_before {
                    it.parts.push(Part::Para(line.trim().to_string()));
                } else {
                    it.push_line(line.trim());
                }
            }
            _ => {
                close_all(&mut stack, &mut out);
                para.push(line);
            }
        }
    }
    end_code(&mut code, &mut stack, spans);
    flush(&mut para, &mut out);
    close_all(&mut stack, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RefMode, ref_spans};

    fn tokens(text: &str) -> Vec<String> {
        ref_spans(text, RefMode::Strict)
            .into_iter()
            .map(|s| s.token)
            .collect()
    }

    #[test]
    fn rfr3_list_code_holds_no_reference() {
        // closed, unclosed (the item ends it) and tilde fences in items: their refs are text
        let body = "1. {{a}}\n   ```\n   {{d-ZZZZZ}}\n   ```\n2. {{b}}\n   ~~~\n   {{$x}}\n3. {{c}}\n   ```\n   {{K}}";
        assert_eq!(tokens(body), ["a", "b", "c"]);
        assert_eq!(code_ranges(body).len(), 3);
        for r in code_ranges(body) {
            let s = &body[r];
            assert!(s.trim_start().starts_with(['`', '~']), "{s:?}");
        }
        // a fence outside a list item stays text, so its ref is read
        assert_eq!(tokens("```\n{{d-ZZZZZ}}\n```"), ["d-ZZZZZ"]);
        assert!(code_ranges("```\n{{d-ZZZZZ}}\n```").is_empty());
        // a `{{` before the code whose `}}` is in it is no reference
        assert!(tokens("- a {{\n  ```\n  x}}\n  ```").is_empty());
        // other modes are unchanged: a diagram reads id-shaped tokens anywhere
        let diagram = ref_spans("- a\n  ```\n  {{d-ZZZZZ}}\n  ```", RefMode::IdShaped);
        assert_eq!(diagram.len(), 1);
    }

    #[test]
    fn rfr3_code_ranges_match_the_parsed_code() {
        let body =
            "- a\n\n  ```sh\n  x {{y}}\n\n  ```\n- b\n  ~~~~\n  z\n  ~~~\n  ~~~~~\n\n```\ntop\n```";
        let ranges = code_ranges(body);
        let mut codes = vec![];
        for b in parse_body(body) {
            if let Block::List(l) = b {
                for it in l.items {
                    for p in it.parts {
                        if let Part::Code { text, .. } = p {
                            codes.push(text);
                        }
                    }
                }
            }
        }
        assert_eq!(codes, ["x {{y}}", "z\n~~~"]);
        assert_eq!(
            ranges.iter().map(|r| &body[r.clone()]).collect::<Vec<_>>(),
            ["  ```sh\n  x {{y}}\n\n  ```", "  ~~~~\n  z\n  ~~~\n  ~~~~~"]
        );
    }
}
