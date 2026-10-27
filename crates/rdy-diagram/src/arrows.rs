//! The arrow cost of the line families: class, state and sequence (D-VACA9, D-R7C3W).
//!
//! Their pinned parsers read one line at a time. This recognizer reads the lines the same way,
//! with the same precedence of rules, and keeps only what costs: class relations, state
//! transitions and sequence messages, each with whether it has a label. It builds no node,
//! graph or tree: each line is a byte slice, read once, and the only state is a few counters.
//! A line a parser skips costs nothing, and so does every line once a parser would stop with an
//! error: the count never invents a syntax error. A relation, a transition and a message each
//! have a rank span of one.
//!
//! Where a parser lowercases a whole line before it matches a keyword, this recognizer matches
//! ASCII case only. They differ only on a character whose lowercase is ASCII (the Kelvin sign),
//! and there the recognizer counts at least as many arrows as the parser reads.

use crate::{
    ARROW_CAP, Family, LABELLED_ARROW_WEIGHT, after_front_matter, before_comment,
    description_colon, is_state_description, is_state_transition, keyword, opens_state_block,
    state_note,
};

/// What a source's arrows cost, saturated at `MAX_ARROWS + 1`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tally {
    /// The cost: each arrow's rank span, times the label weight when it has a label.
    pub cost: usize,
    /// How many arrows have a label, for the parity pin.
    #[cfg(test)]
    pub labelled: usize,
}

impl Tally {
    /// Adds `times` arrows of rank span `span`, each with a label or not.
    pub(crate) fn add(&mut self, span: usize, labelled: bool, times: usize) {
        let weight = if labelled { LABELLED_ARROW_WEIGHT } else { 1 };
        let one = span.saturating_mul(weight).min(ARROW_CAP);
        let all = one.saturating_mul(times).min(ARROW_CAP);
        self.cost = self.cost.saturating_add(all).min(ARROW_CAP);
        #[cfg(test)]
        if labelled {
            self.labelled = self.labelled.saturating_add(times).min(ARROW_CAP);
        }
    }
}

/// What the arrows of `src`, a class, state or sequence drawing input, cost.
pub(crate) fn measure(src: &str, family: Family) -> usize {
    tally(src, family).cost
}

/// The arrow tally of `src` in `family`; nothing for the other families.
pub(crate) fn tally(src: &str, family: Family) -> Tally {
    let mut t = Tally::default();
    match family {
        Family::Class => class(src, &mut t),
        Family::State => state(src, &mut t),
        Family::Sequence => sequence(src, &mut t),
        Family::Flowchart | Family::Unknown => {}
    }
    t
}

/// Whether `line` starts with the ASCII `prefix`, in any case; the text after it.
fn starts_ci<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let head = line.as_bytes().get(..prefix.len())?;
    // an ASCII prefix that matches ends on a character boundary
    head.eq_ignore_ascii_case(prefix.as_bytes())
        .then(|| &line[prefix.len()..])
}

/// The lines after the class and sequence parsers' front matter: when the first line, trimmed,
/// is `---`, everything through the next line whose trimmed text is `---`. Then the header: the
/// first line that is not blank or a `%%` comment, which `header` must accept, else the parser
/// stops and there are no lines.
fn after_header(src: &str, header: impl Fn(&str) -> bool) -> Option<impl Iterator<Item = &str>> {
    let mut lines = src.lines().peekable();
    if lines.peek().is_some_and(|l| l.trim() == "---") {
        lines.next();
        for line in lines.by_ref() {
            if line.trim() == "---" {
                break;
            }
        }
    }
    for line in lines.by_ref() {
        let text = line.trim();
        if text.is_empty() || text.starts_with("%%") {
            continue;
        }
        return header(text).then_some(lines);
    }
    None
}

// --- class --------------------------------------------------------------------------------------

/// The class parser's lines (`class/mod.rs`): outside a class body, in order, a direction, a
/// namespace opening, a class declaration or body opening, a standalone annotation, a relation,
/// an inline member; any other line is skipped. A body runs to the line `}`.
fn class(src: &str, t: &mut Tally) {
    let header = |l: &str| starts_ci(l, "classdiagram").is_some();
    let Some(lines) = after_header(src, header) else {
        return;
    };
    let mut body = false;
    for line in lines {
        let text = line.trim();
        if text.is_empty() || text.starts_with("%%") {
            continue;
        }
        if text == "}" {
            body = false;
            continue;
        }
        if body || keyword(text, "direction").is_some() {
            continue;
        }
        if let Some(rest) = keyword(text, "namespace")
            && rest.trim().ends_with('{')
        {
            continue;
        }
        if let Some(rest) = keyword(text, "class") {
            // `class Name {` opens a body when the name reads
            if let Some(name) = rest.trim().strip_suffix('{') {
                body = class_name(name.trim()).is_some();
            }
            continue;
        }
        if annotation_statement(text) {
            continue;
        }
        if let Some(labelled) = class_relation(text) {
            t.add(1, labelled, 1);
        }
    }
}

/// The length of the class name at the start of `s`: a backtick name that is not empty, or a
/// run of letters, digits, `_`, `.` and `-`.
fn class_name(s: &str) -> Option<usize> {
    if let Some(rest) = s.strip_prefix('`') {
        let end = rest.find('`')?;
        return (end > 0).then_some(end + 2);
    }
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || matches!(c, '_' | '.' | '-')))
        .map_or(s.len(), |(i, _)| i);
    (end > 0).then_some(end)
}

/// Whether a line is `<<annotation>> Name`: a nonempty annotation, then exactly one class name.
fn annotation_statement(text: &str) -> bool {
    let Some(start) = text.strip_prefix("<<") else {
        return false;
    };
    let Some(end) = start.find(">>") else {
        return false;
    };
    let name = start[end + 2..].trim();
    !start[..end].trim().is_empty() && class_name(name) == Some(name.len())
}

/// A relation line (`try_parse_relation`): a source name, an optional quoted cardinality, a
/// connector, an optional quoted cardinality, a target name, then nothing. A label after the
/// first `:` outside quotes is split off first. Whether it has a label or a cardinality with
/// text, if it is a relation.
fn class_relation(text: &str) -> Option<bool> {
    let (relation, label) = split_relation_label(text);
    let from = relation.trim_start();
    let rest = &from[class_name(from)?..];
    let (tail, rest) = cardinality(rest)?;
    let rest = class_connector(rest.trim_start())?;
    let (head, rest) = cardinality(rest)?;
    let to = rest.trim();
    if class_name(to)? != to.len() {
        return None;
    }
    Some(label || tail || head)
}

/// A relation line and whether its label after the first `:` outside quotes has text.
fn split_relation_label(line: &str) -> (&str, bool) {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => return (&line[..i], !line[i + 1..].trim().is_empty()),
            _ => {}
        }
    }
    (line, false)
}

/// An optional quoted cardinality: whether it has text, and what follows it. An unclosed quote
/// is no relation.
fn cardinality(input: &str) -> Option<(bool, &str)> {
    let trimmed = input.trim_start();
    let Some(rest) = trimmed.strip_prefix('"') else {
        return Some((false, trimmed));
    };
    let end = rest.find('"')?;
    Some((!rest[..end].trim().is_empty(), &rest[end + 1..]))
}

/// A class connector, contiguous: a left marker (none, `<|`, `()`, `*`, `o` or `<`), a line (`--`
/// or `..`) and a right marker (none, `|>`, `()`, `*`, `o` or `>`). What follows it.
fn class_connector(input: &str) -> Option<&str> {
    let rest = ["<|", "()", "*", "o", "<"]
        .iter()
        .find_map(|m| input.strip_prefix(m))
        .unwrap_or(input);
    let rest = rest
        .strip_prefix("--")
        .or_else(|| rest.strip_prefix(".."))?;
    Some(
        ["|>", "()", "*", "o", ">"]
            .iter()
            .find_map(|m| rest.strip_prefix(m))
            .unwrap_or(rest),
    )
}

// --- state --------------------------------------------------------------------------------------

/// The state parser's lines (`state/mod.rs`), each trimmed and cut at its first `%%`. In order:
/// `}` closes a block (at the top level it ends the parse), a region divider, a discarded
/// directive, a classDef, a style, a class apply, a note (a multi-line one runs to `end note`), a
/// direction, a transition, a description, a state declaration (which opens a block when it ends
/// with `{`). Only a transition costs.
fn state(src: &str, t: &mut Tally) {
    let mut lines = after_front_matter(src).lines();
    for line in lines.by_ref() {
        let text = line.trim();
        if text.is_empty() || text.starts_with("%%") {
            continue;
        }
        // the header; the parser stops at any other line
        if starts_ci(text, "statediagram").is_none() {
            return;
        }
        break;
    }
    let mut depth: usize = 0;
    while let Some(line) = lines.next() {
        let text = before_comment(line.trim());
        if text.is_empty() {
            continue;
        }
        if text == "}" {
            if depth == 0 {
                return;
            }
            depth -= 1;
            continue;
        }
        if text == "--" || discarded(text) || classdef(text) || style(text) || class_apply(text) {
            continue;
        }
        match state_note(text) {
            Some(true) => {
                for line in lines.by_ref() {
                    if before_comment(line.trim()).eq_ignore_ascii_case("end note") {
                        break;
                    }
                }
                continue;
            }
            Some(false) => continue,
            None => {}
        }
        if keyword(text, "direction").is_some() {
            continue;
        }
        if is_state_transition(text) {
            t.add(1, transition_labelled(text), 1);
            continue;
        }
        if starts_ci(text, "state ").is_none() && is_state_description(text) {
            continue;
        }
        if opens_state_block(text) {
            depth += 1;
        }
    }
}

/// Whether a transition's label, after the first description colon of its target, has text.
fn transition_labelled(text: &str) -> bool {
    let Some(arrow) = text.find("-->") else {
        return false;
    };
    let rest = text[arrow + 3..].trim();
    description_colon(rest).is_some_and(|colon| !rest[colon + 1..].trim().is_empty())
}

/// A directive the state parser discards: `click `, `accTitle` or `accDescr`, in any case.
fn discarded(text: &str) -> bool {
    ["click ", "acctitle", "accdescr"]
        .iter()
        .any(|p| starts_ci(text, p).is_some())
}

/// The style statements' keyword (`graph/style.rs`): in any case, and not followed by a
/// letter, a digit or `_`. The text after it.
fn style_keyword<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let rest = starts_ci(text, word)?;
    (!rest
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_'))
    .then_some(rest)
}

/// `classDef <names> ...` with at least one class name.
fn classdef(text: &str) -> bool {
    let Some(rest) = style_keyword(text, "classDef") else {
        return false;
    };
    rest.trim_start()
        .split(char::is_whitespace)
        .next()
        .is_some_and(|names| names.split(',').any(|n| !n.is_empty()))
}

/// `style <id> ...`.
fn style(text: &str) -> bool {
    style_keyword(text, "style").is_some_and(|rest| !rest.trim_start().is_empty())
}

/// `class <ids> <class>`: not classDef, at least one id before the last white space and a class
/// after it.
fn class_apply(text: &str) -> bool {
    if style_keyword(text, "classDef").is_some() {
        return false;
    }
    let Some(rest) = style_keyword(text, "class") else {
        return false;
    };
    let rest = rest.trim_start();
    let Some(last) = rest.rfind(char::is_whitespace) else {
        return false;
    };
    let ids = rest[..last].trim();
    !rest[last..].trim().is_empty() && ids.split(',').any(|id| !id.trim().is_empty())
}

// --- sequence -----------------------------------------------------------------------------------

/// The message forms, in the order the parser searches for them (mmdflux 2.6.1
/// `sequence/mod.rs:575`).
const MESSAGES: [&str; 8] = ["-->>", "->>", "-->", "->", "--x", "-x", "--)", "-)"];

/// The sequence parser's lines (`sequence/mod.rs`), each trimmed. In order: autonumber, title,
/// a participant box and its body, a block start, a divider, `end`, a created participant, a
/// participant or actor, a destroy, an activation, a note, a message. One message a line.
fn sequence(src: &str, t: &mut Tally) {
    let header = |l: &str| l.eq_ignore_ascii_case("sequencediagram");
    let Some(mut lines) = after_header(src, header) else {
        return;
    };
    while let Some(line) = lines.next() {
        let text = line.trim();
        if text.is_empty() || text.starts_with("%%") {
            continue;
        }
        if autonumber(text) || title(text) {
            continue;
        }
        if keyword(text, "box").is_some() {
            // the box holds only participants until its `end`; anything else stops the parse
            loop {
                let Some(line) = lines.next() else {
                    return;
                };
                let text = line.trim();
                if text.is_empty() || text.starts_with("%%") || participant(text) {
                    continue;
                }
                if text.eq_ignore_ascii_case("end") {
                    break;
                }
                return;
            }
            continue;
        }
        let block = ["loop", "alt", "opt", "par", "critical", "break"];
        let divider = ["else", "and", "option"];
        if block
            .iter()
            .chain(&divider)
            .any(|k| keyword(text, k).is_some())
            || text.eq_ignore_ascii_case("end")
            || created(text)
            || participant(text)
            || destroyed(text)
            || activation(text)
            || sequence_note(text)
        {
            continue;
        }
        if let Some(labelled) = message(text) {
            t.add(1, labelled, 1);
        }
    }
}

/// `autonumber`, `autonumber off`, or `autonumber <start> [<step>]` with whole numbers.
fn autonumber(text: &str) -> bool {
    let Some(rest) = keyword(text, "autonumber") else {
        return false;
    };
    let rest = rest.trim();
    if rest.is_empty() || rest.eq_ignore_ascii_case("off") {
        return true;
    }
    let mut parts = rest.split_whitespace();
    let number = |p: Option<&str>| p.is_some_and(|p| p.parse::<usize>().is_ok());
    if !number(parts.next()) {
        return false;
    }
    match parts.next() {
        None => true,
        step => number(step) && parts.next().is_none(),
    }
}

/// `title <text>` or `title:<text>`, the text not empty.
fn title(text: &str) -> bool {
    keyword(text, "title")
        .or_else(|| starts_ci(text, "title:"))
        .is_some_and(|t| !t.trim().is_empty())
}

/// `participant <id>` or `actor <id>`, a space or a tab after the keyword.
fn participant(text: &str) -> bool {
    ["participant ", "participant\t", "actor ", "actor\t"]
        .iter()
        .find_map(|p| starts_ci(text, p))
        .is_some_and(|rest| !rest.trim().is_empty())
}

/// `create` and a participant.
fn created(text: &str) -> bool {
    ["create ", "create\t"]
        .iter()
        .find_map(|p| starts_ci(text, p))
        .is_some_and(|rest| participant(rest.trim()))
}

/// `destroy <id>`.
fn destroyed(text: &str) -> bool {
    ["destroy ", "destroy\t"]
        .iter()
        .find_map(|p| starts_ci(text, p))
        .is_some_and(|rest| !rest.trim().is_empty())
}

/// `activate <id>` or `deactivate <id>`.
fn activation(text: &str) -> bool {
    ["activate ", "activate\t", "deactivate ", "deactivate\t"]
        .iter()
        .find_map(|p| starts_ci(text, p))
        .is_some_and(|rest| !rest.trim().is_empty())
}

/// `Note left of`, `Note right of` or `Note over`, then participants, a `:` and text.
fn sequence_note(text: &str) -> bool {
    let Some(rest) = ["note left of ", "note right of ", "note over "]
        .iter()
        .find_map(|p| starts_ci(text, p))
    else {
        return false;
    };
    let Some(colon) = rest.find(':') else {
        return false;
    };
    let ids = rest[..colon].trim();
    !ids.is_empty()
        && !rest[colon + 1..].trim().is_empty()
        && ids.split(',').any(|id| !id.trim().is_empty())
}

/// A message (`try_parse_message`): the first form, in [`MESSAGES`] order, found anywhere in
/// the line with a sender before it and a receiver after it (after a `+` or `-` activation
/// marker, before the first `:`). Whether its text after that `:` is not empty, if it is one.
fn message(text: &str) -> Option<bool> {
    for form in MESSAGES {
        let Some(at) = text.find(form) else {
            continue;
        };
        if text[..at].trim().is_empty() {
            continue;
        }
        let rest = text[at + form.len()..].trim();
        let rest = rest
            .strip_prefix('+')
            .or_else(|| rest.strip_prefix('-'))
            .unwrap_or(rest);
        let (to, label) = match rest.find(':') {
            Some(colon) => (&rest[..colon], !rest[colon + 1..].trim().is_empty()),
            None => (rest, false),
        };
        if to.trim().is_empty() {
            continue;
        }
        return Some(label);
    }
    None
}
