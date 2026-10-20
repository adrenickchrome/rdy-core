//! Value disagreement (FW-CH2, D-S46DE, D-SES4J): a block that describes a `value` entity states
//! a different number in the value's unit.
//!
//! A value takes part when it has a unit and a decimal `value`. A block describes it when it
//! ties to it (a reference, a `describes` link or an `affects` link). A stated number counts
//! when the unit text follows it directly or after one space, exactly as written: there are no
//! unit aliases and no conversion. One tension per (describing block, value), involving both; its summary shows the value as written.
//! In flight it is reported when either side is a live write; in canon (FW-CH6a) it is a
//! standing condition, so it stays open after its last change lands, until the numbers agree or
//! a tie goes. It is advisory in draft content and blocking in stable content.
use super::{DetectCx, DetectScope, Detector, Found, TensionKind};
use crate::RdyError;
use crate::lexicon::is_tie;
use rdy_model::entity::{VALUE, text_sources};
use rdy_model::{Block, BlockId};
use std::collections::BTreeSet;

/// This kind's detector.
pub struct ValueDisagreement;

/// A decimal as written in a value's `value` field or in text: `-?[0-9]+(\.[0-9]+)?`, with an
/// optional leading `+` dropped, in its normal form: no leading zeros in the whole part (one `0`
/// stays), no trailing zeros in the fraction (and no `.` when nothing is left), and `-0` is `0`.
/// `None` when `s` is not a decimal.
fn normalize_decimal(s: &str) -> Option<String> {
    let s = s.strip_prefix('+').unwrap_or(s);
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (s, None),
    };
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || frac.is_some_and(|f| !digits(f)) {
        return None;
    }
    let whole = match whole.trim_start_matches('0') {
        "" => "0",
        w => w,
    };
    let frac = frac.map_or("", |f| f.trim_end_matches('0'));
    let mut out = String::new();
    if neg && (whole != "0" || !frac.is_empty()) {
        out.push('-');
    }
    out.push_str(whole);
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    Some(out)
}

/// The end of the digits that start at `i` in `cs` (`i` when there are none).
fn digits_end(cs: &[char], mut i: usize) -> usize {
    while i < cs.len() && cs[i].is_ascii_digit() {
        i += 1;
    }
    i
}

/// The numbers `text` states in `unit`, normalized, in text order. A number is
/// `-?[0-9]+(\.[0-9]+)?`, not preceded by a letter, a digit or `.`; it is followed by nothing or
/// one space, then the unit text exactly (case-sensitive), then the end of the text or a
/// character that is not a letter or a digit.
pub fn stated_numbers(text: &str, unit: &str) -> Vec<String> {
    stated_number_spans(text, unit)
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

/// [`stated_numbers`] with the byte range of each: from the number's first character to the
/// end of its unit.
pub fn stated_number_spans(text: &str, unit: &str) -> Vec<(String, std::ops::Range<usize>)> {
    let unit: Vec<char> = unit.chars().collect();
    if unit.is_empty() {
        return vec![];
    }
    let unit_end = |cs: &[char], q: usize| {
        let u_end = q + unit.len();
        cs.get(q..u_end)
            .is_some_and(|w| w == &unit[..])
            .then_some(u_end)
    };
    number_spans(text, &unit_end)
        .into_iter()
        .map(|n| (n.number, n.span))
        .collect()
}

/// The most characters a unit read from text has (D-SES4J): a longer run of letters is no unit.
pub(crate) const MAX_UNIT_CHARS: usize = 24;

/// A number stated with a unit in text: the normalized number, the unit's byte range and the
/// byte range from the number's first character to the end of the unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NumberSpan {
    pub number: String,
    pub unit: std::ops::Range<usize>,
    pub span: std::ops::Range<usize>,
}

/// Every number `text` states with some unit, in text order: the grammar and boundaries of
/// [`stated_numbers`], with the unit a maximal run of 1 to [`MAX_UNIT_CHARS`] Unicode letters
/// or one `%`. A bare number gives nothing.
pub(crate) fn number_literal_spans(text: &str) -> Vec<NumberSpan> {
    let unit_end = |cs: &[char], q: usize| {
        if cs.get(q) == Some(&'%') {
            return Some(q + 1);
        }
        let run = cs
            .get(q..)
            .unwrap_or_default()
            .iter()
            .take_while(|c| c.is_alphabetic())
            .count();
        (1..=MAX_UNIT_CHARS).contains(&run).then_some(q + run)
    };
    number_spans(text, &unit_end)
}

/// The numbers of `text` that a unit follows: a number is `-?[0-9]+(\.[0-9]+)?`, not preceded by
/// a letter, a digit or `.`; then nothing or one space; then a unit, where `unit_end` gives the
/// end of a unit that starts at character `q`; then the end of the text or a character that is
/// not a letter or a digit. The longest number is tried first, then the one without its
/// fraction.
fn number_spans(text: &str, unit_end: &dyn Fn(&[char], usize) -> Option<usize>) -> Vec<NumberSpan> {
    let (bytes, cs): (Vec<usize>, Vec<char>) = text.char_indices().unzip();
    let byte = |i: usize| bytes.get(i).copied().unwrap_or(text.len());
    let mut out = vec![];
    let mut i = 0;
    while i < cs.len() {
        if i > 0 && (cs[i - 1].is_alphanumeric() || cs[i - 1] == '.') {
            i += 1;
            continue;
        }
        let start = i;
        let mut p = i;
        if cs[p] == '-' {
            p += 1;
        }
        let whole_end = digits_end(&cs, p);
        if whole_end == p {
            i += 1;
            continue;
        }
        // the longest number first, then the one without its fraction
        let mut ends = vec![];
        if cs.get(whole_end) == Some(&'.') {
            let frac_end = digits_end(&cs, whole_end + 1);
            if frac_end > whole_end + 1 {
                ends.push(frac_end);
            }
        }
        ends.push(whole_end);
        let hit = ends.into_iter().find_map(|end| {
            let mut q = end;
            if cs.get(q) == Some(&' ') {
                q += 1;
            }
            let u_end = unit_end(&cs, q)?;
            let bounded = cs.get(u_end).is_none_or(|c| !c.is_alphanumeric());
            bounded.then_some((end, q, u_end))
        });
        match hit {
            Some((end, q, u_end)) => {
                let n: String = cs[start..end].iter().collect();
                if let Some(number) = normalize_decimal(&n) {
                    out.push(NumberSpan {
                        number,
                        unit: byte(q)..byte(u_end),
                        span: byte(start)..byte(u_end),
                    });
                }
                i = u_end;
            }
            None => i += 1,
        }
    }
    out
}

/// A value's number and unit when it takes part in the comparison: its `value` field
/// normalized and its trimmed unit (`None` for any other block, an empty unit or a `value`
/// that is not a decimal).
pub(crate) fn value_number(b: &Block) -> Option<(String, String)> {
    participant(b).map(|p| (p.number, p.unit))
}

/// The normal form of a value's `value` field: the trimmed field must be exactly
/// `-?[0-9]+(\.[0-9]+)?`, so a leading `+` (which `normalize_decimal` drops for numbers read from
/// text) refuses it.
fn field_decimal(field: &str) -> Option<String> {
    let field = field.trim();
    if field.starts_with('+') {
        return None;
    }
    normalize_decimal(field)
}

/// What a value that takes part gives the comparison.
struct Participant {
    unit: String,
    /// The `value` field, trimmed, as written.
    written: String,
    /// Its normal form, for the comparison only.
    number: String,
}

/// A value's unit (trimmed), its field as written and its normalized number, when it takes part.
fn participant(b: &Block) -> Option<Participant> {
    if b.block_type != VALUE {
        return None;
    }
    let unit = b.field_str("unit")?.trim();
    if unit.is_empty() {
        return None;
    }
    let written = b.field_str("value")?.trim();
    let number = field_decimal(written)?;
    Some(Participant {
        unit: unit.to_string(),
        written: written.to_string(),
        number,
    })
}

/// The first number `y` states in `unit` that differs from `number`.
fn first_difference(y: &Block, unit: &str, number: &str) -> Option<String> {
    text_sources(y)
        .iter()
        .flat_map(|(_, text)| stated_numbers(text, unit))
        .find(|n| n != number)
}

impl ValueDisagreement {
    /// The (describing block, value) pairs that `touched` blocks can be part of: a touched value
    /// with every block that ties to it, and a touched block with every value it ties to.
    fn pairs(
        cx: &DetectCx<'_>,
        touched: impl IntoIterator<Item = BlockId>,
    ) -> Result<BTreeSet<(BlockId, BlockId)>, RdyError> {
        let m = cx.inflight_m()?;
        let g = cx.inflight_graph()?;
        let mut out = BTreeSet::new();
        for t in touched {
            let Some(b) = m.get(&t) else {
                continue;
            };
            if participant(b).is_some() {
                for e in g.incoming(&t) {
                    if is_tie(&e.kind) && e.from != t && m.get(&e.from).is_some() {
                        out.insert((e.from.clone(), t.clone()));
                    }
                }
            }
            for e in g.outgoing(&t) {
                if is_tie(&e.kind)
                    && e.to != t
                    && m.get(&e.to).is_some_and(|v| participant(v).is_some())
                {
                    out.insert((t.clone(), e.to.clone()));
                }
            }
        }
        Ok(out)
    }
}

impl Detector for ValueDisagreement {
    fn kind(&self) -> TensionKind {
        TensionKind::ValueDisagreement
    }

    /// A change on a value reaches every block that ties to it; a change on a block reaches the
    /// values it ties to.
    fn reach(
        &self,
        cx: &DetectCx<'_>,
        seeds: &BTreeSet<BlockId>,
    ) -> Result<BTreeSet<BlockId>, RdyError> {
        let m = cx.inflight_m()?;
        let g = cx.inflight_graph()?;
        let mut out = BTreeSet::new();
        for s in seeds {
            let Some(b) = m.get(s) else {
                continue;
            };
            if participant(b).is_some() {
                out.extend(
                    g.incoming(s)
                        .iter()
                        .filter(|e| is_tie(&e.kind))
                        .map(|e| e.from.clone()),
                );
            }
            out.extend(
                g.outgoing(s)
                    .iter()
                    .filter(|e| is_tie(&e.kind))
                    .filter(|e| m.get(&e.to).is_some_and(|v| participant(v).is_some()))
                    .map(|e| e.to.clone()),
            );
        }
        Ok(out)
    }

    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        let pairs = match (cx.scope(), seeds) {
            // the canon part is always a full run: every canon value with its describers
            (DetectScope::Canon, _) => {
                let ids: Vec<BlockId> = cx
                    .canon_m()?
                    .iter()
                    .filter(|b| b.block_type == VALUE)
                    .map(|b| b.id.clone())
                    .collect();
                Self::pairs(cx, ids)?
            }
            (DetectScope::Inflight, Some(seeds)) => Self::pairs(cx, seeds.iter().cloned())?,
            (DetectScope::Inflight, None) => {
                let ids: BTreeSet<BlockId> = cx.writes.live().map(|w| w.block.clone()).collect();
                Self::pairs(cx, ids)?
            }
        };
        let m = cx.inflight_m()?;
        for (y, v) in pairs {
            let (Some(yb), Some(vb)) = (m.get(&y), m.get(&v)) else {
                continue;
            };
            let Some(Participant {
                unit,
                written,
                number,
            }) = participant(vb)
            else {
                continue;
            };
            let Some(stated) = first_difference(yb, &unit, &number) else {
                continue;
            };
            let (Some((ys, yv)), Some((vs, vv))) = (cx.side_at(&y)?, cx.side_at(&v)?) else {
                continue;
            };
            let name = vb.field_str("name").unwrap_or_default();
            out.push(Found {
                kind: TensionKind::ValueDisagreement,
                summary: format!(
                    "{y} states {stated} {unit} for {name} ({v}), which is {written} {unit}"
                ),
                stable: cx.stable(&y)?,
                involved: vec![cx.involved(ys, &y, yv)?, cx.involved(vs, &v, vv)?],
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cc5_numbers_in_the_unit_only() {
        assert_eq!(
            stated_numbers("Timeouts are 30 s, then 45s, then 2 min.", "s"),
            vec!["30", "45"]
        );
        assert_eq!(
            stated_numbers("Up to 05.50 ms or 5.5ms; not v5 ms; not 1.5.6 ms", "ms"),
            vec!["5.5", "5.5"]
        );
        assert_eq!(stated_numbers("50% of 200 %", "%"), vec!["50", "200"]);
        assert_eq!(stated_numbers("10 msec", "ms"), Vec::<String>::new());
        assert_eq!(stated_numbers("-0 s and +3 s", "s"), vec!["0", "3"]);
    }

    #[test]
    fn cc5_fix1_a_value_field_is_a_strict_decimal() {
        assert_eq!(field_decimal("5"), Some("5".into()));
        assert_eq!(field_decimal(" -05.50 "), Some("-5.5".into()));
        assert_eq!(field_decimal("+5"), None);
        assert_eq!(field_decimal("+-5"), None);
        assert_eq!(field_decimal("5."), None);
        assert_eq!(field_decimal(""), None);
    }
}
