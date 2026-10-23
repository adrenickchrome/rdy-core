//! Pages of a change-plan read (p-PZX3T): the items are cut at item boundaries to a token
//! budget, with the briefing's estimate (four bytes a token) and its strict check, and a cut
//! page carries a cursor that holds everything a later page needs. No cursor registry is kept.
//!
//! A cursor is `plan1:<offset>:<digest>:<selector>`: the offset of the next item in decimal, the
//! first [`DIGEST_HEX`] lowercase hex digits of the whole reply's digest, and the canonical
//! selector (the reply's scope text), last, so it may hold colons. The digest covers the whole
//! reply without `next`: scope, plan, every item with its answer, counts, code and notes, but
//! not the page budget. A later page recomputes the whole reply; a different digest is
//! `E_PAGE_STALE`.
use super::api::{PlanReply, Selector};
use crate::RdyError;
use crate::brief::{DIGEST_HEX, budget_bytes, parse_page_tail};
use crate::error::{E_BAD_INPUT, E_PAGE_STALE};
use rdy_model::Hash;

/// The prefix of a plan cursor.
pub const CURSOR_PREFIX: &str = "plan1:";
/// The most bytes a cursor may take.
pub const MAX_CURSOR_BYTES: usize = 4096;

/// A page that is not a cursor `rdy_plan` gave.
fn bad_cursor() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "page is not a cursor from rdy_plan's next",
        "pass the next of a cut rdy_plan, alone",
    )
}

/// A page of a plan that is not the one its cursor counted in.
fn stale() -> RdyError {
    RdyError::new(
        E_PAGE_STALE,
        "the change plan moved since its first page",
        "read the plan again without page",
    )
}

/// The offset, the digest and the selector of `cursor`. The byte limit is checked before
/// anything is parsed, and the selector's grammar again after.
pub(crate) fn parse(cursor: &str) -> Result<(usize, &str, Selector), RdyError> {
    if cursor.len() > MAX_CURSOR_BYTES {
        return Err(bad_cursor());
    }
    let rest = cursor.strip_prefix(CURSOR_PREFIX).ok_or_else(bad_cursor)?;
    let ([offset], digest, selector) = parse_page_tail::<1>(rest).ok_or_else(bad_cursor)?;
    let selector = Selector::parse(selector).ok_or_else(bad_cursor)?;
    Ok((offset, digest, selector))
}

/// The cursor of the page that starts at `offset` of the reply digested as `digest`.
pub(crate) fn cursor(offset: usize, digest: &str, selector: &str) -> String {
    format!("{CURSOR_PREFIX}{offset}:{digest}:{selector}")
}

/// The first [`DIGEST_HEX`] hex digits of the digest of `full` (a whole reply, with no `next`).
pub(crate) fn digest_hex(full: &PlanReply) -> String {
    debug_assert!(full.next.is_none());
    let bytes = serde_json::to_vec(full).expect("a plan reply serializes");
    Hash::of(&bytes).to_string()[..DIGEST_HEX].to_string()
}

/// The first page of `full` within `max` tokens.
pub fn first(full: PlanReply, max: usize) -> PlanReply {
    let digest = digest_hex(&full);
    cut(full, 0, &digest, max)
}

/// The page of `full` at `offset`, when `full` is still the reply its cursor's `digest` counted
/// in; else `E_PAGE_STALE`. An offset past the whole list is stale too.
pub(crate) fn continue_at(
    full: PlanReply,
    offset: usize,
    digest: &str,
    max: usize,
) -> Result<PlanReply, RdyError> {
    let now = digest_hex(&full);
    if now != digest || offset > full.items.len() {
        return Err(stale());
    }
    Ok(cut(full, offset, &now, max))
}

fn json_len<T: serde::Serialize>(v: &T) -> usize {
    serde_json::to_string(v)
        .expect("a plan reply serializes")
        .len()
}

/// The items of `full` from `offset`, as many as fit `max` tokens with the rest of the reply and
/// the cursor (which counts against the budget), and at least one while any remain.
fn cut(mut full: PlanReply, offset: usize, digest: &str, max: usize) -> PlanReply {
    let items = std::mem::take(&mut full.items);
    let total = items.len();
    let budget = budget_bytes(max);
    // the frame (every field but the items, with no `next`), measured once; each item adds its
    // own size and a comma after the first, and a cursor adds `,"next":` and its JSON string
    let frame = json_len(&full);
    let next_len = |end: usize| {
        if end < total {
            ",\"next\":".len() + json_len(&cursor(end, digest, &full.scope))
        } else {
            0
        }
    };
    let mut used = frame;
    let mut end = offset;
    for item in &items[offset.min(total)..] {
        let add = json_len(item) + usize::from(end > offset);
        if end > offset && used + add + next_len(end + 1) > budget {
            break;
        }
        used += add;
        end += 1;
    }
    full.next = (end < total).then(|| cursor(end, digest, &full.scope));
    full.items = items
        .into_iter()
        .skip(offset)
        .take(end.saturating_sub(offset))
        .collect();
    full
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plan cursor reuses the briefing's tail parser: one offset, the 16-digit digest, and
    /// the selector last (colons and all); a malformed offset, digest or selector is refused.
    #[test]
    fn cp8_cursor_tail_parses_exactly_one_offset() {
        let d = "0123456789abcdef";
        let c = format!("plan1:7:{d}:view:a:b");
        let (o, digest, sel) = parse(&c).unwrap();
        assert_eq!((o, digest), (7, d));
        assert_eq!(sel, Selector::View("a:b".into()));
        for bad in [
            format!("plan1::{d}:view:a"),
            format!("plan1:1,2:{d}:view:a"),
            format!("plan1:x:{d}:view:a"),
            format!("plan1:-1:{d}:view:a"),
            format!("plan1:1:{}:view:a", &d[..15]),
            format!("plan1:1:{}0:view:a", d),
            format!("plan1:1:{}:view:a", d.to_uppercase()),
            format!("plan1:1:{d}:"),
            format!("plan1:1:{d}:canon"),
            format!("plan1:1:{d}:view:"),
            format!("plan1:1:{d}:proposal:P-abcd"),
            format!("plan1:1:{d}:landing:abc"),
            format!("plan1:1:{d}:decision:nope"),
            format!("plan2:1:{d}:view:a"),
            format!("{}:1,0,0,0,0,0,0,0,0,0,0:{d}:view:a", "a".repeat(64)),
            format!("plan1:1:{d}:view:{}", "a".repeat(MAX_CURSOR_BYTES)),
        ] {
            let e = parse(&bad).unwrap_err();
            assert_eq!(e.code, E_BAD_INPUT, "{bad}");
            assert_eq!(e.message, "page is not a cursor from rdy_plan's next");
            assert_eq!(e.fix, "pass the next of a cut rdy_plan, alone");
        }
        let c = format!("plan1:0:{d}:proposal:P-abcd@2");
        let (_, _, sel) = parse(&c).unwrap();
        assert_eq!(sel, Selector::Proposal("P-abcd@2".into()));
        let c = format!("plan1:0:{d}:decision:D-AAAAA");
        let (_, _, sel) = parse(&c).unwrap();
        assert_eq!(sel, Selector::Decision("D-AAAAA".into()));
        let full = "f".repeat(64);
        let c = format!("plan1:0:{d}:landing:{full}");
        let (_, _, sel) = parse(&c).unwrap();
        assert_eq!(sel, Selector::Landing(full));
        assert_eq!(DIGEST_HEX, 16);
        // the parser takes exactly N offsets, for any N
        assert_eq!(
            parse_page_tail::<3>(&format!("1,2,3:{d}:s")),
            Some(([1, 2, 3], d, "s"))
        );
        assert_eq!(parse_page_tail::<3>(&format!("1,2:{d}:s")), None);
        assert_eq!(parse_page_tail::<3>(&format!("1,2,3,4:{d}:s")), None);
    }
}
