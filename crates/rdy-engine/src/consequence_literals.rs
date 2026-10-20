//! Changed literals and their evidence in dependent text (D-CFC6V, D-SES4J, D-EGXAS).
//!
//! A literal is a number with its unit, a code literal or an entity a text names. A literal that
//! an old text holds and its new text lacks is a changed literal. A dependent that states a
//! changed literal is evidence that it needs an update.
//!
//! - A number is `-?[0-9]+(\.[0-9]+)?` with the boundaries of the value check, then zero or one
//!   ASCII space, then a maximal run of 1 to 24 Unicode letters or one `%`. Its identity is the
//!   normalized number and the unit's exact bytes: `030.0 days` is `30 days`; `day` is not `days`.
//! - A code literal is the trimmed text of a pair of backticks on one line, 2 to 64 characters
//!   ([`code_literal_spans`]). A dependent states it inside or outside backticks, bounded by the
//!   start or end of its text or a character that is not alphanumeric.
//! - An entity is an entity block that the text names by name or alias, as the mention scan finds
//!   it ([`Lexicon::mention_spans`]).
//!
//! References are masked with `'\0'`s of equal length; only an explicit display text shows,
//! at its own bytes. A plain reference never reads as its target's value. A target's sources are
//! read whole; a dependent's are read up to [`EVIDENCE_BYTES`] in all.
//!
//! The briefing and supersede replies are this module's callers; until they call it, only its
//! tests do.
#![cfg_attr(not(test), allow(dead_code))]

use crate::lexicon::{Lexicon, count_scan, is_enum_field};
use crate::search::window;
use crate::tensions::value::{
    MAX_UNIT_CHARS, number_literal_spans, stated_number_spans, value_number,
};
use rdy_model::decision::DECISION;
use rdy_model::entity::{
    MAX_NAME_WORDS, VALUE, code_literal_spans, lines_at, source_field, value_text, visit_leaves,
    visit_text_sources, words,
};
use rdy_model::{Block, BlockId, RefMode, RefSpan, ref_spans};
use rdy_schema::Pack;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::{ControlFlow, Range};

/// The most changed or covered literals one set holds; later ones are not inspected.
pub(crate) const MAX_LITERALS: usize = 32;
/// The most literal spellings and evidence rows one result shows; the rest are counted.
pub(crate) const MAX_SHOWN: usize = 8;
/// The bytes of a dependent's text read for evidence, across its sources in order.
pub(crate) const EVIDENCE_BYTES: usize = 16_384;
/// The fields a decision pair, or a covered decision, reads, in this order.
const DECISION_FIELDS: [&str; 2] = ["title", "statement"];

/// One place a dependent states a literal: the literal as first written, the source it is in,
/// the line (from one), the byte span in that stored line and the line's search excerpt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LiteralMatch {
    pub value: String,
    pub field: String,
    pub line: usize,
    pub span: [usize; 2],
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum LiteralIdentity {
    Number { number: String, unit: String },
    Code(String),
    Entity(BlockId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Literal {
    identity: LiteralIdentity,
    text: String,
}

/// Distinct literals in text order, at most [`MAX_LITERALS`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LiteralSet(Vec<Literal>);

impl LiteralSet {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
}

/// What one dependent states of a literal set: the first [`MAX_SHOWN`] spellings inspected and
/// how many more, the first [`MAX_SHOWN`] rows in (field, line, start, end, literal) order and
/// how many more, and whether the dependent's text went past [`EVIDENCE_BYTES`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Evidence {
    pub values: Vec<String>,
    pub values_omitted: usize,
    pub matched: Vec<LiteralMatch>,
    pub matched_omitted: usize,
    pub cut: bool,
}

/// The literals `old` holds and `new` lacks; with no `new` (a revoke), all of `old`'s.
pub(crate) fn changed(
    pack: &Pack,
    old: &Block,
    new: Option<&Block>,
    old_names: &Lexicon,
    new_names: &Lexicon,
    decision_pair: bool,
) -> LiteralSet {
    let new: Vec<&Block> = new.into_iter().collect();
    changed_many(pack, old, &new, old_names, new_names, decision_pair)
}

/// D-EGXAS: the literals that each baseline (the view's base, then the write's start; either
/// may be absent) holds and `new` lacks, in baseline order, each once with its first spelling,
/// then one cap. A literal `new` holds is in neither set.
pub(crate) fn changed_from(
    pack: &Pack,
    old: &[(&Block, &Lexicon)],
    new: Option<&Block>,
    new_names: &Lexicon,
    decision_pair: bool,
) -> LiteralSet {
    let mut union = Distinct::default();
    for (b, names) in old {
        count_scan();
        scan_target(pack, b, names, decision_pair, &mut |id, text| {
            union.add(id, text)
        });
    }
    let new: Vec<&Block> = new.into_iter().collect();
    removed(pack, union.0, &new, new_names, decision_pair)
}

/// The literals `old` holds and at least one of `new` lacks; an empty `new` removes them all.
pub(crate) fn changed_many(
    pack: &Pack,
    old: &Block,
    new: &[&Block],
    old_names: &Lexicon,
    new_names: &Lexicon,
    decision_pair: bool,
) -> LiteralSet {
    let mut lits = Distinct::default();
    count_scan();
    scan_target(pack, old, old_names, decision_pair, &mut |id, text| {
        lits.add(id, text)
    });
    removed(pack, lits.0, new, new_names, decision_pair)
}

/// The literals `target`'s text holds: a decision's title, then its statement; another block's
/// normal sources.
pub(crate) fn covered(pack: &Pack, target: &Block, names: &Lexicon) -> LiteralSet {
    let mut lits = Distinct::default();
    count_scan();
    let decision = target.block_type == DECISION;
    scan_target(pack, target, names, decision, &mut |id, text| {
        lits.add(id, text)
    });
    lits.0.truncate(MAX_LITERALS);
    LiteralSet(lits.0)
}

/// Where `dependent` states each of `literals`, within its first [`EVIDENCE_BYTES`].
pub(crate) fn evidence(
    pack: &Pack,
    literals: &LiteralSet,
    dependent: &Block,
    names: &Lexicon,
) -> Evidence {
    scan_dependent(pack, literals, dependent, names).0
}

/// The literals `dependent` states, each once, in `literals`' order, at most [`MAX_SHOWN`], and
/// whether its text was cut: [`evidence`]'s scan.
pub(crate) fn states(
    pack: &Pack,
    literals: &LiteralSet,
    dependent: &Block,
    names: &Lexicon,
) -> (Vec<String>, bool) {
    let (ev, stated) = scan_dependent(pack, literals, dependent, names);
    let out = literals
        .0
        .iter()
        .enumerate()
        .filter(|(i, _)| stated & (1 << i) != 0)
        .map(|(_, l)| l.text.clone())
        .take(MAX_SHOWN)
        .collect();
    (out, ev.cut)
}

/// Literals in first-occurrence order, each identity once with its first spelling.
#[derive(Default)]
struct Distinct(Vec<Literal>, BTreeSet<LiteralIdentity>);

impl Distinct {
    fn add(&mut self, identity: LiteralIdentity, text: &str) {
        if self.1.insert(identity.clone()) {
            self.0.push(Literal {
                identity,
                text: text.to_string(),
            });
        }
    }
}

/// `lits` that at least one of `new` lacks (all of them for an empty `new`), at most
/// [`MAX_LITERALS`]. Each of `new` is scanned once.
fn removed(
    pack: &Pack,
    lits: Vec<Literal>,
    new: &[&Block],
    names: &Lexicon,
    decision_pair: bool,
) -> LiteralSet {
    let mut gone = vec![new.is_empty(); lits.len()];
    if !new.is_empty() && !lits.is_empty() {
        let index: BTreeMap<&LiteralIdentity, usize> = lits
            .iter()
            .enumerate()
            .map(|(i, l)| (&l.identity, i))
            .collect();
        for b in new {
            count_scan();
            let mut held = vec![false; lits.len()];
            scan_target(pack, b, names, decision_pair, &mut |id, _| {
                if let Some(&i) = index.get(&id) {
                    held[i] = true;
                }
            });
            for (g, h) in gone.iter_mut().zip(held) {
                *g |= !h;
            }
        }
    }
    LiteralSet(
        lits.into_iter()
            .zip(gone)
            .filter(|(_, g)| *g)
            .map(|(l, _)| l)
            .take(MAX_LITERALS)
            .collect(),
    )
}

/// Calls `found` with each literal of `b`'s text and its spelling, in the closed order: for a
/// value (not in a decision pair), its number and unit first; then each source in order, and
/// within one by start byte, then number, code, entity, then entity id. `decision` reads a
/// decision's title, then its statement, only. Duplicates are passed on.
fn scan_target(
    pack: &Pack,
    b: &Block,
    names: &Lexicon,
    decision: bool,
    found: &mut dyn FnMut(LiteralIdentity, &str),
) {
    let is_value = !decision && b.block_type == VALUE;
    if is_value && let (Some((number, unit)), Some(text)) = (value_number(b), value_text(b)) {
        found(LiteralIdentity::Number { number, unit }, &text);
    }
    let mut source = |text: &str, mode: RefMode| scan_source(text, mode, &b.id, names, found);
    if decision {
        for f in DECISION_FIELDS {
            if let Some(v) = b.fields.get(f)
                && !is_enum_field(pack, &b.block_type, f)
            {
                let _ = visit_leaves(f, v, &mut |_, text, mode| {
                    source(text, mode);
                    ControlFlow::Continue(())
                });
            }
        }
        return;
    }
    visit_text_sources(b, &mut |name, text, mode| {
        let field = source_field(name);
        let value_part = is_value && (field == "value" || field == "unit");
        if !value_part && !is_enum_field(pack, &b.block_type, field) {
            source(text, mode);
        }
        ControlFlow::Continue(())
    });
}

/// The literals of one source of a target, in the closed order, each identity once at its first
/// place: sorting every occurrence and keeping the first of each gives the same list. Only
/// distinct identities are held, never an (occurrence, entity) pair.
fn scan_source(
    text: &str,
    mode: RefMode,
    own: &BlockId,
    names: &Lexicon,
    found: &mut dyn FnMut(LiteralIdentity, &str),
) {
    let masked = mask(text, mode);
    // (start, kind, identity, range); every scan gives its places in start order, so the first
    // place an identity is offered is its first
    let mut cands: Vec<(usize, u8, LiteralIdentity, Range<usize>)> = vec![];
    let mut held: BTreeSet<LiteralIdentity> = BTreeSet::new();
    for n in number_literal_spans(&masked) {
        let identity = LiteralIdentity::Number {
            number: n.number,
            unit: masked[n.unit].to_string(),
        };
        if held.insert(identity.clone()) {
            cands.push((n.span.start, 0, identity, n.span));
        }
    }
    // backticks pair on stored lines: a newline that a masked reference hides still ends one
    for (at, line) in lines_at(text) {
        for r in code_literal_spans(&masked[at..at + line.len()]) {
            let r = at + r.start..at + r.end;
            let identity = LiteralIdentity::Code(masked[r.clone()].to_string());
            if held.insert(identity.clone()) {
                cands.push((r.start, 1, identity, r));
            }
        }
    }
    let mut entities: BTreeSet<BlockId> = BTreeSet::new();
    names.visit_mention_spans(&masked, own, &mut |id, r| {
        if !entities.contains(id) {
            entities.insert(id.clone());
            cands.push((r.start, 2, LiteralIdentity::Entity(id.clone()), r));
        }
    });
    cands.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    for (_, _, identity, r) in cands {
        found(identity, &text[r]);
    }
}

/// `text` with each reference under `mode` replaced by `'\0'`s of equal length, except its
/// explicit display text, which stays at its own bytes. A key-typed mark (`@key`) is no display
/// text; an escaped `\@` shows from the `@`.
fn mask(text: &str, mode: RefMode) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let nuls = |out: &mut String, n: usize| out.extend(std::iter::repeat_n('\0', n));
    for span in ref_spans(text, mode) {
        out.push_str(&text[at..span.range.start]);
        match display_range(text, &span) {
            Some(d) => {
                nuls(&mut out, d.start - span.range.start);
                out.push_str(&text[d.clone()]);
                nuls(&mut out, span.range.end - d.end);
            }
            None => nuls(&mut out, span.range.len()),
        }
        at = span.range.end;
    }
    out.push_str(&text[at..]);
    out
}

/// The bytes of `text` that supply `span`'s display text, if it shows one.
fn display_range(text: &str, span: &RefSpan) -> Option<Range<usize>> {
    if span.key_typed || span.display.is_none() {
        return None;
    }
    let inner_start = span.range.start + 2;
    let inner = &text[inner_start..span.range.end - 2];
    let bar = inner.find('|')?;
    let raw = &inner[bar + 1..];
    let mut start = inner_start + bar + 1 + (raw.len() - raw.trim_start().len());
    let end = start + raw.trim().len();
    if span.at_escaped {
        // the `\` of `\@` supplies no character
        start += 1;
    }
    Some(start..end)
}

/// The start of a reference that `text`, a cut prefix, opens and does not close: past the
/// boundary it may close, so nothing from there is read.
fn unclosed_ref(text: &str, mode: RefMode) -> Option<usize> {
    if mode == RefMode::Off {
        return None;
    }
    let mut pos = ref_spans(text, mode).last().map_or(0, |s| s.range.end);
    while let Some(found) = text[pos..].find("{{") {
        let start = pos + found;
        if text[..start].ends_with('\\') {
            pos = start + 2;
            continue;
        }
        return Some(start);
    }
    None
}

/// In a cut prefix, the byte from which a name match is not settled. The mention scan takes,
/// at each word it visits, the longest known phrase of up to [`MAX_NAME_WORDS`] words. Where
/// that window reaches past the words the prefix holds whole (no masked reference ends it
/// first), the rest of the text could change the choice, but only when the choice uses the cut
/// word, or when some known phrase is longer than the whole words from there and starts with
/// them (and with what the cut word begins with). A match that starts at or after the first
/// such word is not accepted; every match before it is the full text's.
fn settled_before(scan: &str, names: &Lexicon) -> usize {
    let ws = words(scan);
    let n = ws.len();
    let Some(last) = ws.last() else {
        return scan.len();
    };
    let tail = &scan[last.end..];
    // the last word is whole when a character that cannot join it follows it in the prefix
    let mut t = tail.chars();
    let last_whole = !matches!((t.next(), t.next()), (None, _) | (Some('-' | '.'), None));
    // the words the prefix holds whole
    let k = if last_whole { n } else { n - 1 };
    // the first word whose window can reach past them
    let zone = (k + 1).saturating_sub(MAX_NAME_WORDS);
    // the phrases the scan takes that can cover a word from `zone` on: first word, word count
    let mut taken: BTreeMap<usize, usize> = BTreeMap::new();
    names.visit_phrase_spans(scan, &mut |r| {
        let first = ws.partition_point(|w| w.start < r.start);
        if first + MAX_NAME_WORDS > zone {
            let last = ws.partition_point(|w| w.end < r.end);
            taken.insert(first, last + 1 - first);
        }
    });
    // whether the gap before word `j` (the prefix's tail when `j` is `n`) holds a masked reference
    let breaks = |j: usize| {
        let to = ws.get(j).map_or(scan.len(), |w| w.start);
        scan[ws[j - 1].end..to].contains('\0')
    };
    // the first word the scan visits from `zone` on
    let mut i = zone;
    if let Some((&first, &len)) = taken.range(..zone).next_back() {
        i = i.max(first + len);
    }
    while i < n {
        let len = taken.get(&i).copied();
        if len.is_some_and(|len| i + len > k) {
            return ws[i].start;
        }
        if !(i + 1..=k).any(breaks) {
            let mut p = ws[i..k]
                .iter()
                .map(|w| w.folded.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let open = if last_whole {
                p.push(' ');
                names.extends_known(&p)
            } else {
                // what the cut word begins with: each form its folding can take
                if i < k {
                    p.push(' ');
                }
                let cut = &ws[n - 1];
                cut_word_folds(&scan[cut.start..cut.end], &cut.folded)
                    .iter()
                    .any(|f| names.extends_known(&format!("{p}{f}")))
            };
            if open {
                return ws[i].start;
            }
        }
        i += len.unwrap_or(1);
    }
    scan.len()
}

/// The folded forms that cut word `raw` (folded as `folded` where it ends the word) can begin
/// with once the rest of the text joins it. Folding is per character except at a final sigma:
/// a `Σ` followed in the word only by case-ignorable characters folds to `ς` where the word
/// ends and to `σ` where a cased letter follows. One next character decides every such `Σ`, so
/// no continuation gives a third form. A continuation folds after these forms.
fn cut_word_folds(raw: &str, folded: &str) -> Vec<String> {
    let mut joined = format!("{raw}a").to_lowercase();
    joined.pop();
    if joined == folded {
        vec![joined]
    } else {
        vec![folded.to_string(), joined]
    }
}

/// Whether text can state `unit` under the number grammar: one `%`, or 1 to
/// [`MAX_UNIT_CHARS`] letters.
fn text_unit(unit: &str) -> bool {
    unit == "%"
        || ((1..=MAX_UNIT_CHARS).contains(&unit.chars().count())
            && unit.chars().all(char::is_alphabetic))
}

/// The largest char boundary of `text` at or below `at`.
fn floor_boundary(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The line of `text` that starts at byte `at`, as [`str::lines`] gives it.
fn line_from(text: &str, at: usize) -> &str {
    let rest = &text[at..];
    match rest.find('\n') {
        Some(i) => rest[..i].strip_suffix('\r').unwrap_or(&rest[..i]),
        None => rest,
    }
}

/// The byte each stored line of `stored` starts at. They come from the stored text, never a
/// masked copy, which hides a newline inside a reference.
fn line_starts(stored: &str) -> Vec<usize> {
    let mut lines = vec![0];
    lines.extend(stored.match_indices('\n').map(|(i, _)| i + 1));
    lines
}

/// The end of stored line `l`'s text, before its `\n` or `\r\n`, as [`str::lines`] ends it.
fn line_end(stored: &str, lines: &[usize], l: usize) -> usize {
    match lines.get(l + 1) {
        Some(&next) if next - 1 > lines[l] && stored.as_bytes()[next - 2] == b'\r' => next - 2,
        Some(&next) => next - 1,
        None => stored.len(),
    }
}

/// One evidence row before its excerpt: the source's position among the visited sources and the
/// byte its line starts at, to find the stored line again.
struct Row {
    field: String,
    line: usize,
    start: usize,
    end: usize,
    lit: usize,
    source: usize,
    line_at: usize,
}

impl Row {
    fn key(&self) -> (&str, usize, usize, usize, usize) {
        (&self.field, self.line, self.start, self.end, self.lit)
    }
}

/// The first [`MAX_SHOWN`] rows in key order, and how many others there were.
#[derive(Default)]
struct Rows {
    kept: Vec<Row>,
    omitted: usize,
}

impl Rows {
    #[allow(clippy::too_many_arguments)]
    fn offer(
        &mut self,
        field: &str,
        line: usize,
        start: usize,
        end: usize,
        lit: usize,
        source: usize,
        line_at: usize,
    ) {
        let row = || Row {
            field: field.to_string(),
            line,
            start,
            end,
            lit,
            source,
            line_at,
        };
        if self.kept.len() < MAX_SHOWN {
            self.kept.push(row());
            return;
        }
        let key = (field, line, start, end, lit);
        if let Some((i, _)) = self
            .kept
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.key().cmp(&b.1.key()))
            .filter(|(_, worst)| key < worst.key())
        {
            self.kept[i] = row();
        }
        self.omitted += 1;
    }
}

/// What one dependent source is matched against: the literals by kind with their index.
struct Wanted<'a> {
    numbers: Vec<(usize, &'a str, &'a str)>,
    codes: Vec<(usize, &'a str)>,
    entities: BTreeMap<&'a BlockId, usize>,
}

impl<'a> Wanted<'a> {
    fn of(set: &'a LiteralSet) -> Wanted<'a> {
        let mut w = Wanted {
            numbers: vec![],
            codes: vec![],
            entities: BTreeMap::new(),
        };
        for (i, l) in set.0.iter().enumerate() {
            match &l.identity {
                LiteralIdentity::Number { number, unit } => w.numbers.push((i, number, unit)),
                LiteralIdentity::Code(c) => w.codes.push((i, c)),
                LiteralIdentity::Entity(e) => {
                    w.entities.insert(e, i);
                }
            }
        }
        w
    }
}

/// [`evidence`] and the literals stated (bit `i` for literal `i`).
fn scan_dependent(
    pack: &Pack,
    literals: &LiteralSet,
    dep: &Block,
    names: &Lexicon,
) -> (Evidence, u64) {
    count_scan();
    let wanted = Wanted::of(literals);
    let mut rows = Rows::default();
    let mut stated: u64 = 0;
    let mut cut = false;
    let mut remaining = EVIDENCE_BYTES;
    let mut ordinal = 0;
    visit_text_sources(dep, &mut |name, text, mode| {
        let source = ordinal;
        ordinal += 1;
        if is_enum_field(pack, &dep.block_type, source_field(name)) {
            return ControlFlow::Continue(());
        }
        if remaining == 0 {
            cut = true;
            return ControlFlow::Break(());
        }
        let here_cut = text.len() > remaining;
        let k = if here_cut {
            floor_boundary(text, remaining)
        } else {
            text.len()
        };
        remaining = remaining.saturating_sub(text.len());
        cut |= here_cut;
        let mut hit = |line: usize, start: usize, end: usize, lit: usize, line_at: usize| {
            stated |= 1 << lit;
            rows.offer(name, line, start, end, lit, source, line_at);
        };
        scan_prefix(
            &text[..k],
            mode,
            here_cut,
            &wanted,
            &dep.id,
            names,
            &mut hit,
        );
        if here_cut {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    // the excerpts of the kept rows, from their stored lines
    let mut kept = rows.kept;
    kept.sort_by(|a, b| a.key().cmp(&b.key()));
    let mut texts: Vec<Option<String>> = vec![None; kept.len()];
    if let Some(last) = kept.iter().map(|r| r.source).max() {
        let mut ordinal = 0;
        visit_text_sources(dep, &mut |_, text, _| {
            for (r, t) in kept.iter().zip(texts.iter_mut()) {
                if r.source == ordinal {
                    *t = Some(window(line_from(text, r.line_at), r.start));
                }
            }
            ordinal += 1;
            if ordinal > last {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
    }
    let matched = kept
        .into_iter()
        .zip(texts)
        .map(|(r, t)| LiteralMatch {
            value: literals.0[r.lit].text.clone(),
            field: r.field,
            line: r.line + 1,
            span: [r.start, r.end],
            text: t.unwrap_or_default(),
        })
        .collect();
    let values: Vec<String> = literals
        .0
        .iter()
        .take(MAX_SHOWN)
        .map(|l| l.text.clone())
        .collect();
    let ev = Evidence {
        values_omitted: literals.0.len() - values.len(),
        values,
        matched,
        matched_omitted: rows.omitted,
        cut,
    };
    (ev, stated)
}

/// Matches `prefix`, the read part of one dependent source, against `wanted`, calling `hit`
/// with (line from zero, start, end in that line, literal, line start in the source) for each
/// row. In a `cut` prefix no token that the cut could change is accepted.
fn scan_prefix(
    prefix: &str,
    mode: RefMode,
    cut: bool,
    wanted: &Wanted<'_>,
    own: &BlockId,
    names: &Lexicon,
    hit: &mut dyn FnMut(usize, usize, usize, usize, usize),
) {
    let masked = mask(prefix, mode);
    let end = match cut.then(|| unclosed_ref(prefix, mode)).flatten() {
        Some(at) => at,
        None => masked.len(),
    };
    let scan = &masked[..end];
    // the masked copy has the stored bytes' positions; its lines are the stored text's
    let stored = &prefix[..end];
    // a token that reaches the end of a cut prefix may go on past it
    let whole = |r: &Range<usize>| !cut || r.end < end;
    let lines = line_starts(stored);
    let line_of = |at: usize| lines.partition_point(|&s| s <= at) - 1;
    let mut row = |r: Range<usize>, lit: usize| {
        let l = line_of(r.start);
        hit(l, r.start - lines[l], r.end - lines[l], lit, lines[l]);
    };
    let number = |number: &str, unit: &str| {
        wanted
            .numbers
            .iter()
            .find(|(_, n, u)| *n == number && *u == unit)
            .map(|&(lit, _, _)| lit)
    };
    // a unit that text can hold is matched by the text grammar; for those the exact-unit scan of
    // the value check gives the same numbers. Another unit (a value's `MB/s`) is matched exactly.
    let general = wanted.numbers.iter().any(|(_, _, u)| text_unit(u));
    let mut exact: Vec<&str> = wanted
        .numbers
        .iter()
        .map(|(_, _, u)| *u)
        .filter(|u| !text_unit(u))
        .collect();
    exact.sort_unstable();
    exact.dedup();
    // a number and its unit stay on one stored line
    let numbers = if general || !exact.is_empty() {
        lines.len()
    } else {
        0
    };
    for l in 0..numbers {
        let at = lines[l];
        let line = &scan[at..line_end(stored, &lines, l)];
        let shift = |r: Range<usize>| at + r.start..at + r.end;
        if general {
            for n in number_literal_spans(line) {
                let r = shift(n.span);
                if whole(&r)
                    && let Some(lit) = number(&n.number, &line[n.unit])
                {
                    row(r, lit);
                }
            }
        }
        for unit in &exact {
            for (n, r) in stated_number_spans(line, unit) {
                // an exact unit never reads into a masked reference
                if line[r.clone()].contains('\0') {
                    continue;
                }
                let r = shift(r);
                if whole(&r)
                    && let Some(lit) = number(&n, unit)
                {
                    row(r, lit);
                }
            }
        }
    }
    for &(lit, code) in &wanted.codes {
        let mut from = 0;
        while let Some(f) = scan[from..].find(code) {
            let start = from + f;
            let r = start..start + code.len();
            let before = scan[..r.start].chars().next_back();
            let after = scan[r.end..].chars().next();
            if before.is_none_or(|c| !c.is_alphanumeric())
                && after.is_none_or(|c| !c.is_alphanumeric())
                && whole(&r)
            {
                row(r, lit);
            }
            from = start + scan[start..].chars().next().map_or(1, char::len_utf8);
        }
    }
    if !wanted.entities.is_empty() {
        let settled = if cut {
            settled_before(scan, names)
        } else {
            scan.len()
        };
        // each occurrence is filtered by the wanted identities as it is found
        names.visit_mention_spans(scan, own, &mut |id, r| {
            let Some(&lit) = wanted.entities.get(id) else {
                return;
            };
            if r.start >= settled {
                return;
            }
            // one row per nonempty fragment on each stored line
            let mut l = line_of(r.start);
            loop {
                let ls = lines[l];
                let (fs, fe) = (r.start.max(ls), r.end.min(line_end(stored, &lines, l)));
                if fs < fe {
                    hit(l, fs - ls, fe - ls, lit, ls);
                }
                match lines.get(l + 1) {
                    Some(&next) if next < r.end => l += 1,
                    _ => break,
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::{Mention, text_in, text_scans};
    use crate::search::window;
    use crate::tensions::value::{stated_number_spans, stated_numbers};
    use rdy_model::FieldValue;
    use rdy_model::entity::{code_literal_spans, code_literals, text_sources, visit_text_sources};
    use rdy_schema::PackSet;
    use std::collections::BTreeSet;
    use std::ops::ControlFlow;

    fn pack() -> Pack {
        PackSet::with_builtin().resolve("rdy-standard").unwrap()
    }

    fn id(s: &str) -> BlockId {
        BlockId::parse(s).unwrap()
    }

    fn s(t: &str) -> FieldValue {
        FieldValue::Str(t.to_string())
    }

    fn blk(i: &str, ty: &str, body: &str, fields: Vec<(&str, FieldValue)>) -> Block {
        Block {
            id: id(i),
            block_type: ty.to_string(),
            parent: None,
            order: "a1".to_string(),
            fields: fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            body: body.to_string(),
            links: Default::default(),
            pins: Default::default(),
            read_identity: None,
        }
    }

    fn req(i: &str, body: &str) -> Block {
        blk(i, "requirement", body, vec![])
    }

    fn named(i: &str, name: &str, aliases: &[&str]) -> Block {
        let mut fields = vec![("name", s(name))];
        if !aliases.is_empty() {
            fields.push((
                "aliases",
                FieldValue::List(aliases.iter().map(|a| s(a)).collect()),
            ));
        }
        blk(i, "component", "", fields)
    }

    fn value(i: &str, number: &str, unit: &str) -> Block {
        blk(
            i,
            "value",
            "",
            vec![
                ("name", s("Retention")),
                ("value", s(number)),
                ("unit", s(unit)),
            ],
        )
    }

    fn lex(p: &Pack, blocks: &[Block]) -> Lexicon {
        Lexicon::build(p, blocks.iter())
    }

    fn spell(set: &LiteralSet) -> Vec<String> {
        set.0.iter().map(|l| l.text.clone()).collect()
    }

    fn ident(set: &LiteralSet) -> Vec<String> {
        set.0
            .iter()
            .map(|l| match &l.identity {
                LiteralIdentity::Number { number, unit } => format!("{number}|{unit}"),
                LiteralIdentity::Code(c) => format!("`{c}`"),
                LiteralIdentity::Entity(e) => e.to_string(),
            })
            .collect()
    }

    fn cov(p: &Pack, body: &str) -> Vec<String> {
        ident(&covered(p, &req("r-00001", body), &lex(p, &[])))
    }

    fn list(n: std::ops::RangeInclusive<usize>, unit: &str) -> String {
        n.map(|i| format!("{i} {unit}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// (field, line, span, value) of each retained row.
    fn rows(ev: &Evidence) -> Vec<(String, usize, [usize; 2], String)> {
        ev.matched
            .iter()
            .map(|m| (m.field.clone(), m.line, m.span, m.value.clone()))
            .collect()
    }

    fn row(
        field: &str,
        line: usize,
        span: [usize; 2],
        value: &str,
    ) -> (String, usize, [usize; 2], String) {
        (field.to_string(), line, span, value.to_string())
    }

    #[test]
    fn sdc1_numbers_use_value_grammar_and_exact_units() {
        let p = pack();
        // normalization: 030.0 days is 30 days, and the first spelling stays
        let target = req("r-00001", "Keep 030.0 days, then 30 days.");
        let set = covered(&p, &target, &lex(&p, &[]));
        assert_eq!(ident(&set), ["30|days"]);
        assert_eq!(spell(&set), ["030.0 days"]);
        // negatives, zero (-0 is 0) and a leading plus, which is not part of the number
        assert_eq!(
            cov(&p, "Drift -5 ms, -0 s, 0 s, +3 s."),
            ["-5|ms", "0|s", "3|s"]
        );
        // boundaries: no letter, digit or `.` before; no letter or digit after
        assert_eq!(
            cov(&p, "v5 ms, x30 days, 1.5.6 ms, 30 days2, 9%x, (6 ms)."),
            ["6|ms"]
        );
        // percent with or without one space; one ASCII space at most; no space
        assert_eq!(
            cov(&p, "50% and 200 % and 7  days and 8days"),
            ["50|%", "200|%", "8|days"]
        );
        // a unit of 24 letters counts; one of 25 does not, nor does its prefix
        let (u24, u25) = ("a".repeat(24), "b".repeat(25));
        assert_eq!(
            cov(&p, &format!("1 {u24} and 2 {u25}.")),
            [format!("1|{u24}")]
        );
        // Unicode letters; the unit is exact bytes: no plural or case conversion
        assert_eq!(
            cov(&p, "5 mètres, 3 秒, 1 day, 2 days, 2 Days, 1 days"),
            ["5|mètres", "3|秒", "1|day", "2|days", "2|Days", "1|days"]
        );
        // a bare number is no literal
        assert!(cov(&p, "Version 42, then 7.").is_empty());
        // a value's unit outside the text grammar is matched exactly, as the value check does
        let rate = value("v-00002", "5", "MB/s");
        let lits = covered(&p, &rate, &lex(&p, &[]));
        assert_eq!(ident(&lits), ["5|MB/s"]);
        let dep = req("r-00002", "at 5 MB/s, not 5 MB/sx or 5 MB");
        assert_eq!(
            rows(&evidence(&p, &lits, &dep, &lex(&p, &[]))),
            [row("body", 1, [3, 9], "5 MB/s")]
        );
        // a dependent states the identity only in the exact unit
        let lits = covered(&p, &req("r-00001", "Keep 30 days"), &lex(&p, &[]));
        let dep = req("r-00002", "30 day, 30 Days, 30days, 30.00 days");
        let ev = evidence(&p, &lits, &dep, &lex(&p, &[]));
        assert_eq!(
            rows(&ev),
            [
                row("body", 1, [17, 23], "30 days"),
                row("body", 1, [25, 35], "30 days")
            ]
        );
    }

    #[test]
    fn sdc1_code_and_entities_use_existing_identity_rules() {
        let p = pack();
        let (c64, c65) = ("c".repeat(64), "d".repeat(65));
        assert_eq!(
            cov(
                &p,
                &format!("`a` `ab` `{c64}` `{c65}` `x y` `{{{{v-00001}}}}` and ` unpaired")
            ),
            ["`ab`".to_string(), format!("`{c64}`"), "`x y`".to_string()]
        );
        // a dependent holds the code inside or outside backticks, bounded by non-alphanumerics
        let lits = covered(&p, &req("r-00001", "Set `max_retries`."), &lex(&p, &[]));
        let dep = req(
            "r-00002",
            "max_retries=3; Xmax_retries; max_retriesY; `max_retries`; MAX_RETRIES",
        );
        let ev = evidence(&p, &lits, &dep, &lex(&p, &[]));
        assert_eq!(
            rows(&ev),
            [
                row("body", 1, [0, 11], "max_retries"),
                row("body", 1, [44, 55], "max_retries")
            ]
        );

        // aliases, ambiguous names in id order, and no match by key alone
        let mut ledger = named("m-0000A", "Ledger Service", &["Ledger Svc"]);
        ledger.fields.insert("key".into(), s("ledger"));
        let names = lex(
            &p,
            &[
                ledger.clone(),
                named("m-0000C", "Gateway", &[]),
                named("m-0000B", "Gateway", &[]),
            ],
        );
        let target = req(
            "r-00001",
            "The Ledger Svc calls the Gateway; the ledger is old.",
        );
        let set = covered(&p, &target, &names);
        assert_eq!(ident(&set), ["m-0000A", "m-0000B", "m-0000C"]);
        assert_eq!(spell(&set), ["Ledger Svc", "Gateway", "Gateway"]);
        // an entity never names itself
        let mut own = ledger.clone();
        own.body = "Ledger Service is the ledger.".into();
        assert!(covered(&p, &own, &names).is_empty());

        // the same entity renamed between the texts keeps its identity
        let old_names = lex(&p, &[named("m-0000A", "Ledger Service", &[])]);
        let new_names = lex(&p, &[named("m-0000A", "Payments Core", &[])]);
        let old = req("r-00001", "Ledger Service posts.");
        let new = req("r-00001", "Payments Core posts.");
        assert!(
            changed(&p, &old, Some(&new), &old_names, &new_names, false)
                .0
                .is_empty()
        );
        let gone = req("r-00001", "Nothing posts.");
        assert_eq!(
            ident(&changed(
                &p,
                &old,
                Some(&gone),
                &old_names,
                &new_names,
                false
            )),
            ["m-0000A"]
        );
        // each side is read under its own names: the new name under the old names is unknown
        assert_eq!(
            ident(&changed(
                &p,
                &old,
                Some(&new),
                &old_names,
                &old_names,
                false
            )),
            ["m-0000A"]
        );
    }

    #[test]
    fn sdc1_sources_match_mentions_and_preserve_display_spans() {
        let p = pack();
        let names = lex(
            &p,
            &[
                named("m-0000A", "Ledger Service", &[]),
                value("v-00001", "30", "days"),
            ],
        );
        let lits = covered(
            &p,
            &req("r-00001", "Keep 30 days with the Ledger Service."),
            &names,
        );
        assert_eq!(ident(&lits), ["30|days", "m-0000A"]);
        let line2 = "see {{v-00001}} and {{v-00001|30 days}} and {{v-00001|\\@30 days}} and {{m-0000A|@key}}";
        let body = format!("Line one\n{line2}\né→ 30 days\nuses Ledger\nService daily");
        let mut extra = std::collections::BTreeMap::new();
        extra.insert("k".to_string(), s("30 days"));
        let dep = blk(
            "r-00002",
            "requirement",
            &body,
            vec![
                (
                    "steps",
                    FieldValue::List(vec![
                        s("wait 30 days"),
                        FieldValue::List(vec![s("then 30 days")]),
                    ]),
                ),
                ("extra", FieldValue::Map(extra)),
                ("kind", s("30 days")),
                ("priority", s("30 days")),
            ],
        );
        let ev = evidence(&p, &lits, &dep, &names);
        let shown = line2.find("|30 days").unwrap() + 1;
        let escaped = line2.find("\\@30 days").unwrap() + 2;
        assert_eq!(
            rows(&ev),
            [
                row("body", 2, [shown, shown + 7], "30 days"),
                row("body", 2, [escaped, escaped + 7], "30 days"),
                row("body", 3, [6, 13], "30 days"),
                row("body", 4, [5, 11], "Ledger Service"),
                row("body", 5, [0, 7], "Ledger Service"),
                row("extra.k", 1, [0, 7], "30 days"),
                row("steps[0]", 1, [5, 12], "30 days"),
                row("steps[1][0]", 1, [5, 12], "30 days"),
            ]
        );
        assert_eq!(ev.matched_omitted, 0);
        assert_eq!(ev.matched[0].text, line2);
        assert_eq!(ev.matched[3].text, "uses Ledger");
        assert!(!ev.cut);
        // display text keeps its own bytes; the `\` of `\@` and a key mark do not show
        assert_eq!(
            mask(
                "a {{v-00001| x y }} {{v-00001|\\@z}} {{v-00001|@key}}",
                RefMode::Strict
            ),
            format!(
                "a {}x y{} {}@z{} {}",
                "\0".repeat(11),
                "\0".repeat(3),
                "\0".repeat(11),
                "\0".repeat(2),
                "\0".repeat(16)
            )
        );
        // the mention scan finds the same two-line name
        assert_eq!(
            names.mentions(&dep),
            [Mention {
                entity: "m-0000A".into(),
                field: "body".into(),
                text: "Ledger\nService".into(),
            }]
        );
        // a code body and a diagram body are not read; a diagram's alt is, its lang is not
        let code = blk("cd-00001", "code", "30 days", vec![("lang", s("30 days"))]);
        assert!(evidence(&p, &lits, &code, &names).matched.is_empty());
        let diagram = blk(
            "dg-00001",
            "diagram",
            "A[30 days]",
            vec![("alt", s("30 days")), ("lang", s("mermaid"))],
        );
        assert_eq!(
            rows(&evidence(&p, &lits, &diagram, &names)),
            [row("alt", 1, [0, 7], "30 days")]
        );
    }

    #[test]
    fn sdc1_changed_literals_are_removed_distinct_and_ordered() {
        let p = pack();
        let n = lex(&p, &[]);
        let old = req(
            "r-00001",
            "Retain 30 days; set `max_retries`; 30 days again; 7 days, 7 days; then 14 days.",
        );
        let new = req("r-00001", "Retain 30 days.");
        assert_eq!(
            spell(&changed(&p, &old, Some(&new), &n, &n, false)),
            ["max_retries", "7 days", "14 days"]
        );
        // a decision pair reads the title, then the statement, and nothing else
        let d_old = blk(
            "D-00001",
            "decision",
            "90 days",
            vec![
                ("statement", s("Use `fsync` per write")),
                ("title", s("Keep 7 days")),
                ("why", s("12 days")),
            ],
        );
        let d_new = blk(
            "D-00001",
            "decision",
            "",
            vec![("statement", s("Batch writes")), ("title", s("Keep it"))],
        );
        assert_eq!(
            spell(&changed(&p, &d_old, Some(&d_new), &n, &n, true)),
            ["7 days", "fsync"]
        );
        // a value's number and unit are one literal; the same number written another way stays
        let v_old = value("v-00001", "30", "days");
        assert_eq!(
            spell(&changed(
                &p,
                &v_old,
                Some(&value("v-00001", "14", "days")),
                &n,
                &n,
                false
            )),
            ["30 days"]
        );
        assert!(
            changed(
                &p,
                &v_old,
                Some(&value("v-00001", "030", "days")),
                &n,
                &n,
                false
            )
            .0
            .is_empty()
        );
        assert_eq!(
            spell(&changed(
                &p,
                &value("v-00001", "50", "%"),
                None,
                &n,
                &n,
                false
            )),
            ["50%"]
        );
        // the 33rd removed identity is not inspected
        let many = req("r-00001", &list(1..=40, "s"));
        let set = changed(&p, &many, None, &n, &n, false);
        assert_eq!(set.len(), 32);
        assert_eq!(spell(&set).last().unwrap(), "32 s");
        // two replacements: removed by either counts once; an empty slice removes all
        let old = req("r-00001", "7 days and 14 days");
        let a = req("r-00002", "7 days");
        let b = req("r-00003", "14 days");
        let both = req("r-00004", "14 days, 7 days");
        assert_eq!(
            spell(&changed_many(&p, &old, &[&a, &b], &n, &n, false)),
            ["7 days", "14 days"]
        );
        assert!(
            changed_many(&p, &old, &[&both, &both], &n, &n, false)
                .0
                .is_empty()
        );
        assert_eq!(
            spell(&changed_many(&p, &old, &[&a, &a], &n, &n, false)),
            ["14 days"]
        );
        assert_eq!(
            spell(&changed_many(&p, &old, &[], &n, &n, false)),
            ["7 days", "14 days"]
        );
        // one scan per block passed
        let before = text_scans();
        changed_many(&p, &old, &[&a, &b], &n, &n, false);
        assert_eq!(text_scans() - before, 3);
    }

    #[test]
    fn sdc1_dependent_budget_is_16384_bytes_before_allocation() {
        let p = pack();
        let n = lex(&p, &[]);
        let lits = covered(&p, &req("r-00001", "Keep 30 days"), &n);
        let two =
            |a: &str, b: &str| blk("r-00009", "requirement", "", vec![("a", s(a)), ("b", s(b))]);
        let a = format!("{}\n", "x".repeat(8000));
        // b's "30 days" ends at byte 16,384 of the budget
        let fill = 16_384 - a.len() - " 30 days".len();
        let at = |b: &str| (two(&a, b), fill + 1);

        // 16,384 bytes: complete, matched
        let (dep, start) = at(&format!("{} 30 days", "y".repeat(fill)));
        let ev = evidence(&p, &lits, &dep, &n);
        assert_eq!(rows(&ev), [row("b", 1, [start, start + 7], "30 days")]);
        assert!(!ev.cut);
        // 16,383 bytes: complete, matched
        let dep = two(&a, &format!("{} 30 days", "y".repeat(fill - 1)));
        let ev = evidence(&p, &lits, &dep, &n);
        assert_eq!(ev.matched.len(), 1);
        assert!(!ev.cut);
        // 16,385 bytes: the token "dayss" is cut at the boundary and is not accepted
        let (dep, _) = at(&format!("{} 30 dayss", "y".repeat(fill)));
        let ev = evidence(&p, &lits, &dep, &n);
        assert!(ev.matched.is_empty());
        assert!(ev.cut);
        // a match that ends before the boundary in a cut source counts, with the cut flag
        let dep = two(&a, &format!("{} 30 days and more", "y".repeat(fill - 1)));
        let ev = evidence(&p, &lits, &dep, &n);
        assert_eq!(ev.matched.len(), 1);
        assert!(ev.cut);
        // a code point across the boundary rounds down
        let dep = two(
            &a,
            &format!("{}é 30 days", "z".repeat(16_384 - a.len() - 1)),
        );
        let ev = evidence(&p, &lits, &dep, &n);
        assert!(ev.matched.is_empty());
        assert!(ev.cut);
        // a reference cut by the boundary stays masked; a complete huge one is masked
        let huge = format!("{{{{{}}}}}", "30 days ".repeat(3000));
        let ev = evidence(&p, &lits, &two(&a, &huge), &n);
        assert!(ev.matched.is_empty());
        assert!(ev.cut);
        let whole = format!("{{{{{}}}}}", "30 days ".repeat(1000));
        let ev = evidence(&p, &lits, &two(&whole, "ok 30 days"), &n);
        assert_eq!(rows(&ev), [row("b", 1, [3, 10], "30 days")]);
        assert!(!ev.cut);
        // a full first field leaves the second unread; an empty one adds no bytes
        let full = "x".repeat(16_384);
        let ev = evidence(&p, &lits, &two(&full, "30 days"), &n);
        assert!(ev.matched.is_empty());
        assert!(ev.cut);
        let ev = evidence(&p, &lits, &two(&full, ""), &n);
        assert!(!ev.cut);
        // an entity name cut by the boundary is not taken for a shorter name
        let names = lex(
            &p,
            &[
                named("m-0000E", "Ledger", &[]),
                named("m-0000F", "Ledger Service", &[]),
            ],
        );
        let lits = covered(&p, &req("r-00001", "the Ledger posts"), &names);
        assert_eq!(ident(&lits), ["m-0000E"]);
        let tail = "pay Ledger Serv";
        let body = format!("{} {tail}ice now", "x".repeat(16_384 - tail.len() - 1));
        let ev = evidence(&p, &lits, &req("r-00002", &body), &names);
        assert!(ev.matched.is_empty());
        assert!(ev.cut);
        // a masked reference after the name settles it before the boundary
        let tail = "pay Ledger {{v-00001}} and more wor";
        let body = format!("{} {tail}ds", "x".repeat(16_384 - tail.len() - 1));
        let ev = evidence(&p, &lits, &req("r-00002", &body), &names);
        assert_eq!(ev.matched.len(), 1);
        assert!(ev.cut);
        // one scan per dependent
        let before = text_scans();
        evidence(&p, &lits, &req("r-00002", &body), &names);
        assert_eq!(text_scans() - before, 1);
    }

    #[test]
    fn sdc1_evidence_caps_rows_and_counts_omissions() {
        let p = pack();
        let n = lex(&p, &[]);
        let lits = covered(&p, &req("r-00001", &list(1..=32, "s")), &n);
        let ev = evidence(&p, &lits, &req("r-00002", "nothing"), &n);
        assert_eq!(
            ev.values,
            (1..=8).map(|i| format!("{i} s")).collect::<Vec<_>>()
        );
        assert_eq!(ev.values_omitted, 24);
        assert!(ev.matched.is_empty());
        assert_eq!(ev.matched_omitted, 0);

        let lits = covered(&p, &req("r-00001", "Keep 7 days and 9 s and `7 days`"), &n);
        assert_eq!(ident(&lits), ["7|days", "9|s", "`7 days`"]);
        let dep = blk(
            "r-00002",
            "requirement",
            "7 days 7 days\n9 s\n7 days",
            vec![
                ("zeta", s("7 days\n7 days\n7 days")),
                ("alpha", s("9 s and 7 days")),
            ],
        );
        let ev = evidence(&p, &lits, &dep, &n);
        assert_eq!(
            rows(&ev),
            [
                row("alpha", 1, [0, 3], "9 s"),
                row("alpha", 1, [8, 14], "7 days"),
                row("alpha", 1, [8, 14], "7 days"),
                row("body", 1, [0, 6], "7 days"),
                row("body", 1, [0, 6], "7 days"),
                row("body", 1, [7, 13], "7 days"),
                row("body", 1, [7, 13], "7 days"),
                row("body", 2, [0, 3], "9 s"),
            ]
        );
        // 16 rows in all: 3 in alpha, 7 in the body, 6 in zeta
        assert_eq!(ev.matched_omitted, 8);
        assert_eq!(ev.values, ["7 days", "9 s", "7 days"]);
        assert_eq!(ev.values_omitted, 0);
    }

    #[test]
    fn sdc1_excerpts_use_search_windows() {
        let p = pack();
        let n = lex(&p, &[]);
        let lits = covered(&p, &req("r-00001", "Keep 30 days"), &n);
        let line = format!("{} 30 days {}", "é".repeat(300), "ü".repeat(300));
        let ev = evidence(&p, &lits, &req("r-00002", &line), &n);
        assert_eq!(rows(&ev), [row("body", 1, [601, 608], "30 days")]);
        assert_eq!(ev.matched[0].text, window(&line, 601));
        assert_eq!(ev.matched[0].text.chars().count(), 242);
        assert!(ev.matched[0].text.starts_with('…'));
        // a line longer than the budget: the excerpt and span come from the uncut stored line
        let long = format!("{} 30 days {}", "é".repeat(300), "ü".repeat(9000));
        let ev = evidence(&p, &lits, &req("r-00002", &format!("intro\n{long}")), &n);
        assert!(ev.cut);
        assert_eq!(rows(&ev), [row("body", 2, [601, 608], "30 days")]);
        assert_eq!(ev.matched[0].text, window(&long, 601));
    }

    #[test]
    fn sdc1_states_are_unique_in_covered_text_order() {
        let p = pack();
        let n = lex(&p, &[]);
        let lits = covered(&p, &req("r-00001", &list(1..=10, "s")), &n);
        let dep = req(
            "r-00002",
            "10 s, 9 s, 9 s, 3 s, 1 s, 2 s, 4 s, 5 s, 6 s, 7 s, 8 s",
        );
        assert_eq!(
            states(&p, &lits, &dep, &n),
            ((1..=8).map(|i| format!("{i} s")).collect::<Vec<_>>(), false)
        );
        // a plain reference to a value is not its rendered value
        let v = value("v-00001", "30", "days");
        let names = lex(&p, std::slice::from_ref(&v));
        let lits = covered(&p, &v, &names);
        assert_eq!(spell(&lits), ["30 days"]);
        let dep = req("r-00002", "Retention is {{v-00001}}.");
        assert_eq!(states(&p, &lits, &dep, &names), (vec![], false));
        // a cut prefix keeps its cut flag with no match, and with one
        let dep = req("r-00002", &"x".repeat(20_000));
        assert_eq!(states(&p, &lits, &dep, &names), (vec![], true));
        let dep = req("r-00002", &format!("30 days {}", "x".repeat(20_000)));
        assert_eq!(
            states(&p, &lits, &dep, &names),
            (vec!["30 days".to_string()], true)
        );
        // states reads the dependent once
        let before = text_scans();
        states(&p, &lits, &dep, &names);
        assert_eq!(text_scans() - before, 1);
    }

    #[test]
    fn sdc1_covered_decision_reads_only_title_and_statement() {
        let p = pack();
        let n = lex(&p, &[]);
        let d = blk(
            "D-00001",
            "decision",
            "Body says 7 days",
            vec![
                ("statement", s("Use `fsync` within 14 days")),
                ("title", s("Keep 30 days")),
                ("why", s("90 days")),
                ("options", FieldValue::List(vec![s("7 days"), s("90 days")])),
            ],
        );
        assert_eq!(spell(&covered(&p, &d, &n)), ["30 days", "fsync", "14 days"]);
        // a nondecision reads its normal sources: the body, then the fields by name
        let r = blk(
            "r-00001",
            "requirement",
            "7 days",
            vec![("why", s("90 days"))],
        );
        assert_eq!(spell(&covered(&p, &r, &n)), ["7 days", "90 days"]);
        let before = text_scans();
        covered(&p, &r, &n);
        assert_eq!(text_scans() - before, 1);
    }

    #[test]
    fn sdc1_write_union_deduplicates_before_its_single_cap() {
        let p = pack();
        let n = lex(&p, &[]);
        let base = req("r-00001", "Keep 30 days");
        let start = req("r-00001", "Keep 14 days");
        let fin = req("r-00001", "Keep 7 days");
        let union = |old: &[(&Block, &Lexicon)], new: Option<&Block>| {
            spell(&changed_from(&p, old, new, &n, false))
        };
        assert_eq!(
            union(&[(&base, &n), (&start, &n)], Some(&fin)),
            ["30 days", "14 days"]
        );
        // D-EGXAS: a literal the final text holds is in neither set
        let fin30 = req("r-00001", "Keep 30 days");
        assert_eq!(
            union(&[(&base, &n), (&start, &n)], Some(&fin30)),
            ["14 days"]
        );
        // shared identities once, base first, first spelling kept
        let b2 = req("r-00001", "030 days and 5 s");
        let s2 = req("r-00001", "6 s and 30 days");
        let none = req("r-00001", "none");
        assert_eq!(
            union(&[(&b2, &n), (&s2, &n)], Some(&none)),
            ["030 days", "5 s", "6 s"]
        );
        assert_eq!(
            union(&[(&start, &n), (&base, &n)], Some(&fin)),
            ["14 days", "30 days"]
        );
        // one cap after the removal, the union and deduplication
        let b40 = req("r-00001", &list(1..=40, "s"));
        let f8 = req("r-00001", &list(1..=8, "s"));
        let got = union(&[(&b40, &n)], Some(&f8));
        assert_eq!(got, (9..=40).map(|i| format!("{i} s")).collect::<Vec<_>>());
        let b20 = req("r-00001", &list(1..=20, "s"));
        let s20 = req("r-00001", &list(21..=40, "s"));
        assert_eq!(
            union(&[(&b20, &n), (&s20, &n)], Some(&none)),
            (1..=32).map(|i| format!("{i} s")).collect::<Vec<_>>()
        );
        let b30 = req("r-00001", &list(1..=30, "s"));
        let s33 = req("r-00001", &list(1..=33, "s"));
        assert_eq!(
            union(&[(&b30, &n), (&s33, &n)], Some(&none)),
            (1..=32).map(|i| format!("{i} s")).collect::<Vec<_>>()
        );
        // an absent baseline contributes nothing; no final text removes every baseline literal
        assert!(union(&[], Some(&fin)).is_empty());
        assert_eq!(union(&[(&start, &n)], Some(&fin)), ["14 days"]);
        assert_eq!(union(&[(&base, &n)], None), ["30 days"]);
        // one scan per block passed
        let before = text_scans();
        union(&[(&base, &n), (&start, &n)], Some(&fin));
        assert_eq!(text_scans() - before, 3);
    }

    /// A global allocator that keeps the live bytes each thread holds, and their peak, so a test
    /// can bound what one call allocates. Each count is per thread: parallel tests do not mix.
    mod live_alloc {
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;

        pub struct Live;

        thread_local! {
            static LIVE: Cell<isize> = const { Cell::new(0) };
            static PEAK: Cell<isize> = const { Cell::new(0) };
        }

        fn grow(n: usize) {
            let _ = LIVE.try_with(|l| {
                let now = l.get().saturating_add(n as isize);
                l.set(now);
                let _ = PEAK.try_with(|p| p.set(p.get().max(now)));
            });
        }

        fn shrink(n: usize) {
            let _ = LIVE.try_with(|l| l.set(l.get().saturating_sub(n as isize)));
        }

        // SAFETY: every call goes to `System` with the caller's arguments unchanged.
        #[allow(unsafe_code)]
        unsafe impl GlobalAlloc for Live {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                grow(layout.size());
                // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
                unsafe { System.alloc(layout) }
            }
            unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
                grow(layout.size());
                // SAFETY: the caller's layout, as `GlobalAlloc::alloc_zeroed` requires.
                unsafe { System.alloc_zeroed(layout) }
            }
            unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
                shrink(layout.size());
                // SAFETY: `ptr` came from `System` with `layout` (every allocation is System's).
                unsafe { System.dealloc(ptr, layout) }
            }
            unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
                shrink(layout.size());
                grow(new_size);
                // SAFETY: `ptr` came from `System` with `layout`; the caller's new size.
                unsafe { System.realloc(ptr, layout, new_size) }
            }
        }

        /// `f`'s result and the most bytes above the start this thread held while it ran.
        pub fn peak_above_start<T>(f: impl FnOnce() -> T) -> (T, usize) {
            let start = LIVE.with(Cell::get);
            PEAK.with(|p| p.set(start));
            let out = f();
            (out, (PEAK.with(Cell::get) - start).max(0) as usize)
        }
    }

    #[global_allocator]
    static ALLOC: live_alloc::Live = live_alloc::Live;

    #[test]
    fn sdc1_fix1_hidden_newlines_keep_stored_line_coordinates() {
        let p = pack();
        let n = lex(&p, &[]);
        let lits = covered(&p, &req("r-00001", "Keep 30 days"), &n);
        // a newline inside a reference, before its display text, still ends stored line 1
        let dep = req("r-00002", "{{v-00001|\n30 days}}");
        let ev = evidence(&p, &lits, &dep, &n);
        assert_eq!(rows(&ev), [row("body", 2, [0, 7], "30 days")]);
        assert_eq!(ev.matched[0].text, "30 days}}");
        // a first line shorter than the masked one: no slice past the stored line
        let dep = req(
            "r-00002",
            &format!("{}{{{{v-00001|\n30 days}}}}", "x".repeat(300)),
        );
        let ev = evidence(&p, &lits, &dep, &n);
        assert_eq!(rows(&ev), [row("body", 2, [0, 7], "30 days")]);
        assert_eq!(ev.matched[0].text, "30 days}}");
        // a target's backticks pair on stored lines, not across a hidden newline
        let target = req("r-00001", "`ab` `q {{v-00001|\nx}} `cd`");
        assert_eq!(cov(&p, &target.body), ["`ab`", "`cd`"]);
    }

    #[test]
    fn sdc1_fix1_ambiguous_names_are_matched_without_occurrence_pairs() {
        let p = pack();
        // 256 entities share one name: each occurrence names all of them
        let blocks: Vec<Block> = (0..256)
            .map(|i| named(&format!("m-{i:05X}"), "Gateway", &[]))
            .collect();
        let names = lex(&p, &blocks);
        let text = "Gateway ".repeat(2048);
        assert_eq!(text.len(), EVIDENCE_BYTES);
        let target = req("r-00001", &text);
        let (lits, peak) = live_alloc::peak_above_start(|| covered(&p, &target, &names));
        assert_eq!(lits.len(), MAX_LITERALS);
        // 524,288 (id, range) pairs would hold 20 MiB; the text itself is 16 KiB
        assert!(peak < 1 << 20, "covered held {peak} bytes");
        let (_, peak) =
            live_alloc::peak_above_start(|| changed(&p, &target, None, &names, &names, false));
        assert!(peak < 1 << 20, "changed held {peak} bytes");
        let dep = req("r-00002", &text);
        let (ev, peak) = live_alloc::peak_above_start(|| evidence(&p, &lits, &dep, &names));
        assert!(peak < 1 << 20, "evidence held {peak} bytes");
        assert!(!ev.cut);
        assert_eq!(ev.matched.len(), MAX_SHOWN);
        // every occurrence states each of the 32 identities: one row each
        assert_eq!(ev.matched_omitted, 2048 * MAX_LITERALS - MAX_SHOWN);
        assert_eq!(rows(&ev)[0], row("body", 1, [0, 7], "Gateway"));
        assert_eq!(rows(&ev)[7], row("body", 1, [0, 7], "Gateway"));
        let (st, peak) = live_alloc::peak_above_start(|| states(&p, &lits, &dep, &names));
        assert!(peak < 1 << 20, "states held {peak} bytes");
        assert_eq!(st, (vec!["Gateway".to_string(); MAX_SHOWN], false));
        // the mention scan keeps its first-found result without the pairs
        let (m, peak) = live_alloc::peak_above_start(|| names.mentions(&dep));
        assert!(peak < 1 << 20, "mentions held {peak} bytes");
        assert_eq!(m.len(), 256);
        assert_eq!(m[0].entity, "m-00000");
        assert_eq!(m[255].entity, "m-000FF");
    }

    #[test]
    fn sdc1_fix1_settled_names_before_a_cut_are_kept() {
        let p = pack();
        // the only name is complete and nothing longer can start with it: it is kept
        let gate = lex(&p, &[named("m-0000G", "Gateway", &[])]);
        let lits = covered(&p, &req("r-00001", "the Gateway"), &gate);
        let dep = req("r-00002", &format!("Gateway {}", "x".repeat(20_000)));
        let ev = evidence(&p, &lits, &dep, &gate);
        assert_eq!(rows(&ev), [row("body", 1, [0, 7], "Gateway")]);
        assert_eq!(ev.matched_omitted, 0);
        assert!(ev.cut);
        assert_eq!(
            states(&p, &lits, &dep, &gate),
            (vec!["Gateway".to_string()], true)
        );
        // a cut word that no known name can continue does not unsettle the name before it
        let names = lex(
            &p,
            &[
                named("m-0000E", "Ledger", &[]),
                named("m-0000F", "Ledger Service", &[]),
            ],
        );
        let lits = covered(&p, &req("r-00001", "the Ledger posts"), &names);
        let at_cut = |tail: &str, rest: &str| {
            let body = format!(
                "{} {tail}{rest}",
                "x".repeat(EVIDENCE_BYTES - tail.len() - 1)
            );
            evidence(&p, &lits, &req("r-00002", &body), &names)
        };
        let ev = at_cut("pay Ledger now", "here");
        assert_eq!(ev.matched.len(), 1);
        assert!(ev.cut);
        // a longer known name could follow a whole last word: not settled
        let ev = at_cut("pay Ledger ", "Service now");
        assert!(ev.matched.is_empty());
        assert!(ev.cut);
        // the cut word could still be the longer name's: not settled
        let ev = at_cut("pay Ledger Se", "rvice now");
        assert!(ev.matched.is_empty());
        assert!(ev.cut);
        // the name itself is the cut word: not settled
        let ev = at_cut("pay Ledg", "er now");
        assert!(ev.matched.is_empty());
        assert!(ev.cut);
    }

    #[test]
    fn sdc1_fix2_stable_cut_word_characters_keep_settled_names() {
        let p = pack();
        let names = lex(
            &p,
            &[
                named("m-0000E", "Ledger", &[]),
                named("m-0000F", "Ledger Service", &[]),
            ],
        );
        let lits = covered(&p, &req("r-00001", "the Ledger posts"), &names);
        let at_cut = |tail: &str, rest: &str| {
            let body = format!(
                "{} {tail}{rest}",
                "x".repeat(EVIDENCE_BYTES - tail.len() - 1)
            );
            assert_eq!(body.len() - rest.len(), EVIDENCE_BYTES);
            req("r-00002", &body)
        };
        // no continuation of `n` gives `Service`: Ledger is the full text's match
        let dep = at_cut("pay Ledger n", "ow");
        let ev = evidence(&p, &lits, &dep, &names);
        assert_eq!(rows(&ev), [row("body", 1, [16376, 16382], "Ledger")]);
        assert_eq!(ev.matched_omitted, 0);
        assert!(ev.cut);
        assert_eq!(
            states(&p, &lits, &dep, &names),
            (vec!["Ledger".to_string()], true)
        );
        // a cut word that a known name can still continue keeps refusing
        let dep = at_cut("pay Ledger S", "ervice");
        let ev = evidence(&p, &lits, &dep, &names);
        assert!(ev.matched.is_empty(), "{:?}", rows(&ev));
        assert!(ev.cut);
        assert_eq!(states(&p, &lits, &dep, &names), (vec![], true));
        // a final sigma folds as `ς` in the prefix and as `σ` once a cased letter follows it,
        // even across a case-ignorable mark: both forms are checked
        for (name, tail) in [
            ("Ledger ΑΣΒ", "pay Ledger ΑΣ"),
            ("Ledger ΑΣ\u{0345}Β", "pay Ledger ΑΣ\u{0345}"),
        ] {
            let names = lex(
                &p,
                &[named("m-0000E", "Ledger", &[]), named("m-0000F", name, &[])],
            );
            let lits = covered(&p, &req("r-00001", "the Ledger posts"), &names);
            let dep = at_cut(tail, "Β now");
            let ev = evidence(&p, &lits, &dep, &names);
            assert!(ev.matched.is_empty(), "{name:?}: {:?}", rows(&ev));
            assert!(ev.cut);
            assert_eq!(states(&p, &lits, &dep, &names), (vec![], true), "{name:?}");
            // the full text does take the longer name there
            let full = covered(&p, &dep, &names);
            assert!(
                spell(&full).contains(&name.to_string()),
                "{:?}",
                spell(&full)
            );
        }
    }

    #[test]
    fn sdc1_fix1_exact_units_stay_on_one_stored_line() {
        let p = pack();
        let n = lex(&p, &[]);
        // a unit with a newline is never stated across two stored lines
        let lits = covered(&p, &value("v-00002", "5", "MB/\ns"), &n);
        assert_eq!(ident(&lits), ["5|MB/\ns"]);
        let ev = evidence(&p, &lits, &req("r-00002", "5 MB/\ns"), &n);
        assert!(ev.matched.is_empty(), "{:?}", rows(&ev));
        // an exact unit never reads into a masked reference
        let lits = covered(&p, &value("v-00002", "5", "MB\0"), &n);
        assert_eq!(ident(&lits), ["5|MB\0"]);
        let ev = evidence(&p, &lits, &req("r-00002", "5 MB{{v-00001}}"), &n);
        assert!(ev.matched.is_empty(), "{:?}", rows(&ev));
        // a unit on one line still matches there, on its own stored line
        let lits = covered(&p, &value("v-00002", "5", "MB/s"), &n);
        let ev = evidence(&p, &lits, &req("r-00002", "first\nat 5 MB/s\r\nend"), &n);
        assert_eq!(rows(&ev), [row("body", 2, [3, 9], "5 MB/s")]);
        assert_eq!(ev.matched[0].text, "at 5 MB/s");
    }

    #[test]
    fn sdc1_existing_scanners_keep_their_results() {
        let p = pack();
        // code literals
        for text in [
            "a `b c` d `ee`\r\n`ff` `g",
            "`{{x}}` `\0zz` `` `é√` y\n\n`hh`",
            "no ticks",
        ] {
            let spans: Vec<String> = code_literal_spans(text)
                .into_iter()
                .map(|r| text[r].to_string())
                .collect();
            assert_eq!(code_literals(text), spans, "{text:?}");
        }
        assert_eq!(
            code_literals("a `b c` d `ee`\r\n`ff` `g"),
            ["b c", "ee", "ff"]
        );
        // stated numbers
        for (text, unit) in [
            ("Timeouts are 30 s, then 45s, then 2 min.", "s"),
            ("Up to 05.50 ms or 5.5ms; not v5 ms; not 1.5.6 ms", "ms"),
            ("50% of 200 %", "%"),
            ("10 msec", "ms"),
            ("-0 s and +3 s", "s"),
            ("é 5 s", ""),
        ] {
            let spans: Vec<String> = stated_number_spans(text, unit)
                .into_iter()
                .map(|(n, _)| n)
                .collect();
            assert_eq!(stated_numbers(text, unit), spans, "{text:?}");
        }
        assert_eq!(
            stated_number_spans("é Up to 05.50 ms", "ms"),
            [("5.5".to_string(), 9..17)]
        );
        // mentions
        let names = lex(
            &p,
            &[
                named("m-0000A", "Ledger Service", &["Ledger Svc"]),
                named("m-0000B", "Gateway", &[]),
                named("m-0000C", "Gateway", &[]),
            ],
        );
        let dep = blk(
            "m-0000B",
            "component",
            "The Gateway calls the Ledger Svc, then {{m-0000A|Ledger Service}}.",
            vec![
                ("name", s("Gateway")),
                (
                    "steps",
                    FieldValue::List(vec![s("Ledger Service"), s("Gateway")]),
                ),
            ],
        );
        let mut seen = BTreeSet::new();
        let mut want = vec![];
        for (field, text) in text_in(&p, &dep) {
            for (e, r) in names.mention_spans(&text, &dep.id) {
                if seen.insert(e.clone()) {
                    want.push(Mention {
                        entity: e.to_string(),
                        field: field.clone(),
                        text: text[r].to_string(),
                    });
                }
            }
        }
        assert_eq!(names.mentions(&dep), want);
        assert_eq!(
            names.mentions(&dep),
            [
                Mention {
                    entity: "m-0000C".into(),
                    field: "body".into(),
                    text: "Gateway".into()
                },
                Mention {
                    entity: "m-0000A".into(),
                    field: "body".into(),
                    text: "Ledger Svc".into()
                },
            ]
        );
        // text sources: visitation in the existing order and labels
        let mut cells = std::collections::BTreeMap::new();
        cells.insert("a".to_string(), s("x {{m-0000A}} y"));
        let b = blk(
            "r-00001",
            "requirement",
            "Body",
            vec![
                ("status", s("stable")),
                ("cells", FieldValue::Map(cells)),
                (
                    "steps",
                    FieldValue::List(vec![s("one"), s(""), FieldValue::Int(3)]),
                ),
            ],
        );
        let mut seen = vec![];
        visit_text_sources(&b, &mut |name, text, _| {
            seen.push((name.to_string(), text.to_string()));
            ControlFlow::Continue(())
        });
        assert_eq!(
            seen,
            [
                ("body".to_string(), "Body".to_string()),
                ("cells.a".to_string(), "x {{m-0000A}} y".to_string()),
                ("steps[0]".to_string(), "one".to_string()),
            ]
        );
        assert_eq!(
            text_sources(&b),
            [
                ("body".to_string(), "Body".to_string()),
                (
                    "cells.a".to_string(),
                    "x \0\0\0\0\0\0\0\0\0\0\0 y".to_string()
                ),
                ("steps[0]".to_string(), "one".to_string()),
            ]
        );
        // a visitor that breaks stops before the next source
        let mut count = 0;
        visit_text_sources(&b, &mut |_, _, _| {
            count += 1;
            ControlFlow::Break(())
        });
        assert_eq!(count, 1);
    }
}
