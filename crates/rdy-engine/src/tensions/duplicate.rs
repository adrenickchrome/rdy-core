//! Duplicate definition: blocks that define the same name, or have the same normative text.
//!
//! A group is the blocks that share one normalized name, or one normalized text of at least the
//! pack's `duplicate_text_min_words` words. A decision's text is its `statement`, never its body
//! (founder ruling 2026-10-08: register boilerplate such as "Disposition: … / Trigger: …" makes
//! bodies match across unrelated decisions); a decision without a statement has no text. Any
//! other normative block's text is its body. In flight, a group is its live writes plus the canon
//! blocks no change writes. FW-TN2: a group of two or more members is ONE tension whose involved
//! items are every member, so a membership change (a member added, edited away or removed)
//! gives a new id and vkey and voids a dismissal. In flight a group is kept when a member has a
//! change side; in canon (FW-CH6, FW-CH6a) a group of canon blocks is a standing condition, so
//! duplicates that land keep their vkey until one is renamed or removed. A text group whose
//! members all define one name is that name group's tension, reported once, as the name.
use super::{DetectCx, DetectScope, Detector, Found, SideRef, TensionKind, Write, normalize_name};
use crate::RdyError;
use rdy_model::{BlockId, Hash};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// This kind's detector.
pub struct DuplicateDefinition;

/// One block of a group: the side that holds it and its version there.
struct Member {
    block: BlockId,
    side: SideRef,
    version: Option<Hash>,
}

/// The group key: a normalized name or a normalized text.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Name(String),
    Text(Hash),
}

/// The members of a group: the live `writes` (one per block, the one that holds the block's
/// current version when there is one) and the `canon` blocks that no change writes.
fn members(
    cx: &DetectCx<'_>,
    canon: &[BlockId],
    writes: Vec<Arc<Write>>,
) -> Result<Vec<Member>, RdyError> {
    let mut by_block: BTreeMap<BlockId, Vec<Arc<Write>>> = BTreeMap::new();
    for w in writes {
        by_block.entry(w.block.clone()).or_default().push(w);
    }
    let mut out = vec![];
    for (block, ws) in by_block {
        let current = cx.inflight_m()?.version_of(&block);
        let w = ws.iter().find(|w| w.version == current).unwrap_or(&ws[0]);
        out.push(Member {
            block,
            side: SideRef::Change(w.change),
            version: w.version,
        });
    }
    for id in canon {
        if !cx.writes.all_on(id).is_empty() {
            continue;
        }
        if let Some((side, v)) = cx.canon_side(id)? {
            out.push(Member {
                block: id.clone(),
                side,
                version: Some(v),
            });
        }
    }
    out.sort_by(|a, b| a.block.cmp(&b.block));
    Ok(out)
}

/// Whether a group can hold two distinct blocks: an upper bound on `members`, from the index
/// alone. Most canon keys are singletons; `members` reads the store for every canon block, so a
/// group that cannot form a pair must not reach it.
fn can_pair(canon: &[BlockId], writes: &[Arc<Write>]) -> bool {
    let mut blocks: BTreeSet<&BlockId> = canon.iter().collect();
    blocks.extend(writes.iter().map(|w| &w.block));
    blocks.len() >= 2
}

/// The keys of block `b`: its normalized name (when it defines one) and its text key.
fn keys_of(cx: &DetectCx<'_>, b: &rdy_model::Block, out: &mut BTreeSet<Key>) {
    if cx.defines(b)
        && let Some(n) = b.field_str("name")
    {
        out.insert(Key::Name(normalize_name(n)));
    }
    if let Some(h) = cx.text_key(b) {
        out.insert(Key::Text(h));
    }
}

/// The keys to examine: every key of the scope, or in flight with `seeds` every key a seed had
/// or has: those of every write on a seed (live or not) and of its canon version. The canon
/// scope always examines every canon key.
fn keys(cx: &DetectCx<'_>, seeds: Option<&BTreeSet<BlockId>>) -> Result<BTreeSet<Key>, RdyError> {
    let mut out = BTreeSet::new();
    match (seeds, cx.scope()) {
        (None, _) => {
            for w in cx.writes.live() {
                if let Some(b) = w.new.as_deref() {
                    keys_of(cx, b, &mut out);
                }
            }
        }
        (Some(seeds), DetectScope::Inflight) => {
            let canon = cx.canon_m()?;
            for s in seeds {
                for w in cx.writes.all_on(s) {
                    if let Some(b) = w.new.as_deref() {
                        keys_of(cx, b, &mut out);
                    }
                }
                if let Some(b) = canon.get(s) {
                    keys_of(cx, b, &mut out);
                }
            }
        }
        // the canon part is always a full run (no seeds)
        (Some(_), DetectScope::Canon) => {}
    }
    if cx.scope() == DetectScope::Canon {
        let ix = cx.canon_index();
        out.extend(ix.names.keys().cloned().map(Key::Name));
        out.extend(ix.texts.keys().copied().map(Key::Text));
    }
    Ok(out)
}

/// The index entries of `key`: canon's blocks and the live writes.
fn entries<'c>(cx: &'c DetectCx<'_>, key: &Key) -> (&'c [BlockId], Vec<Arc<Write>>) {
    let ix = cx.canon_index();
    let (canon, writes) = match key {
        Key::Name(n) => (ix.names.get(n), cx.writes.named(n)),
        Key::Text(h) => (ix.texts.get(h), cx.writes.texted(h)),
    };
    (canon.map_or(&[][..], Vec::as_slice), writes)
}

/// How many members a summary names before "and N more".
const NAMED: usize = 5;

/// The members of a group as a summary names them: `A and B`, `A, B and C`, or the first
/// [`NAMED`] and `and N more`.
fn listed(group: &[Member]) -> String {
    let ids: Vec<String> = group.iter().map(|m| m.block.to_string()).collect();
    if ids.len() > NAMED {
        return format!("{} and {} more", ids[..NAMED].join(", "), ids.len() - NAMED);
    }
    match ids.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => ids.join(""),
    }
}

/// A text clipped to `n` characters, with an ellipsis when cut.
fn clipped(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

impl Detector for DuplicateDefinition {
    fn kind(&self) -> TensionKind {
        TensionKind::DuplicateDefinition
    }

    /// FW-TN2: a write on a seed can change the membership of every group the seed was or is
    /// in, so the carry re-detects around every block of those groups (from the index alone).
    fn reach(
        &self,
        cx: &DetectCx<'_>,
        seeds: &BTreeSet<BlockId>,
    ) -> Result<BTreeSet<BlockId>, RdyError> {
        let mut out = BTreeSet::new();
        for key in keys(cx, Some(seeds))? {
            let (canon, writes) = entries(cx, &key);
            out.extend(canon.iter().cloned());
            out.extend(writes.iter().map(|w| w.block.clone()));
        }
        Ok(out)
    }

    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        // names before texts: a text group whose members all define one name is reported once,
        // as that name
        let mut keys: Vec<Key> = keys(cx, seeds)?.into_iter().collect();
        keys.sort_by_key(|k| matches!(k, Key::Text(_)));
        for key in keys {
            let (canon, writes) = entries(cx, &key);
            if !can_pair(canon, &writes) {
                continue;
            }
            let group = members(cx, canon, writes.clone())?;
            if group.len() < 2 {
                continue;
            }
            let anchored = cx.scope() == DetectScope::Canon
                || group.iter().any(|m| matches!(m.side, SideRef::Change(_)));
            if !anchored {
                continue;
            }
            let what = match &key {
                Key::Name(n) => {
                    let verb = if group.len() == 2 { "both" } else { "all" };
                    format!("{verb} define \"{}\"", clipped(n, 80))
                }
                Key::Text(_) => {
                    let canon_m = cx.canon_m()?;
                    let mut names = BTreeSet::new();
                    let mut text = None;
                    for m in &group {
                        // a member's block: its write's, or canon's
                        let b = match m.side {
                            SideRef::Change(c) => writes
                                .iter()
                                .find(|w| w.change == c && w.block == m.block)
                                .and_then(|w| w.new.as_deref()),
                            _ => canon_m.get(&m.block),
                        };
                        names.insert(
                            b.filter(|b| cx.defines(b))
                                .and_then(|b| b.field_str("name"))
                                .map(normalize_name),
                        );
                        if text.is_none() {
                            text = b.and_then(|b| cx.duplicate_text(b)).map(normalize_name);
                        }
                    }
                    if names.len() == 1 && names.iter().all(Option::is_some) {
                        continue;
                    }
                    format!(
                        "have the same text \"{}\"",
                        clipped(&text.unwrap_or_default(), 80)
                    )
                }
            };
            let mut stable = false;
            for m in &group {
                if cx.stable(&m.block)? {
                    stable = true;
                    break;
                }
            }
            out.push(Found {
                kind: TensionKind::DuplicateDefinition,
                summary: format!("{} {what}", listed(&group)),
                stable,
                involved: group
                    .iter()
                    .map(|m| cx.involved(m.side.clone(), &m.block, m.version))
                    .collect::<Result<_, _>>()?,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(change: &[u8], block: &BlockId) -> Arc<Write> {
        Arc::new(Write {
            change: Hash::of(change),
            block: block.clone(),
            base: None,
            version: None,
            new: None,
            live: true,
        })
    }

    #[test]
    fn can_pair_needs_two_distinct_blocks_before_any_store_read() {
        let (a, b) = (BlockId::new("blk"), BlockId::new("blk"));
        assert!(!can_pair(&[], &[]), "empty group");
        assert!(!can_pair(std::slice::from_ref(&a), &[]), "one canon block");
        assert!(!can_pair(&[], &[write(b"1", &a)]), "one written block");
        assert!(
            !can_pair(
                std::slice::from_ref(&a),
                &[write(b"1", &a), write(b"2", &a)]
            ),
            "one block, however many writes and canon entries"
        );
        assert!(can_pair(&[a.clone(), b.clone()], &[]), "two canon blocks");
        assert!(
            can_pair(std::slice::from_ref(&a), &[write(b"1", &b)]),
            "canon and a write"
        );
        assert!(
            can_pair(&[], &[write(b"1", &a), write(b"2", &b)]),
            "two writes"
        );
    }
}
