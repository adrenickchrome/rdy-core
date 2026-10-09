//! Duplicate definition: two blocks define the same name, or have the same normative text.
//!
//! A group is the blocks that share one normalized name, or one normalized text of at least the
//! pack's `duplicate_text_min_words` words. In flight, a group is its live writes plus the canon
//! blocks no change writes; every pair of distinct blocks with an in-flight side is a tension.
//! In canon (FW-CH6, FW-CH6a) every pair of canon blocks in a group is one: a duplicate is a
//! standing condition, so two duplicates that both land keep their vkey until one is renamed or
//! removed.
use super::{
    DetectCx, DetectScope, Detector, Found, SideRef, TensionKind, Write, normalize_name,
    normalize_text,
};
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

/// The keys to examine: every key of the scope, or in flight with `seeds` those of the seeds'
/// live writes. The canon scope always examines every canon key.
fn keys(cx: &DetectCx<'_>, seeds: Option<&BTreeSet<BlockId>>) -> Result<BTreeSet<Key>, RdyError> {
    let min_words = cx.st.pack().duplicate_text_min_words();
    let mut out = BTreeSet::new();
    let mut add = |b: &rdy_model::Block| {
        if cx.defines(b)
            && let Some(n) = b.field_str("name")
        {
            out.insert(Key::Name(normalize_name(n)));
        }
        if cx.is_normative(b)
            && let Some(h) = normalize_text(&b.body, min_words)
        {
            out.insert(Key::Text(h));
        }
    };
    match (seeds, cx.scope()) {
        (None, _) => {
            for w in cx.writes.live() {
                if let Some(b) = w.new.as_deref() {
                    add(b);
                }
            }
        }
        (Some(seeds), DetectScope::Inflight) => {
            for s in seeds {
                for w in cx.writes.live_on(s) {
                    if let Some(b) = w.new.as_deref() {
                        add(b);
                    }
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

impl Detector for DuplicateDefinition {
    fn kind(&self) -> TensionKind {
        TensionKind::DuplicateDefinition
    }

    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        let ix = cx.canon_index();
        let mut seen: BTreeSet<(BlockId, BlockId)> = BTreeSet::new();
        // names before texts: two blocks that share both are reported once, as a name
        let mut keys: Vec<Key> = keys(cx, seeds)?.into_iter().collect();
        keys.sort_by_key(|k| matches!(k, Key::Text(_)));
        for key in keys {
            let (canon, writes, what) = match &key {
                Key::Name(n) => (
                    ix.names.get(n),
                    cx.writes.named(n),
                    format!("both define \"{n}\""),
                ),
                Key::Text(h) => (
                    ix.texts.get(h),
                    cx.writes.texted(h),
                    "have the same text".to_string(),
                ),
            };
            let canon = canon.map_or(&[][..], Vec::as_slice);
            if !can_pair(canon, &writes) {
                continue;
            }
            let group = members(cx, canon, writes)?;
            for (i, x) in group.iter().enumerate() {
                for y in &group[i + 1..] {
                    let anchored = cx.scope() == DetectScope::Canon
                        || [x, y].iter().any(|m| matches!(m.side, SideRef::Change(_)));
                    if !anchored || !seen.insert((x.block.clone(), y.block.clone())) {
                        continue;
                    }
                    out.push(Found {
                        kind: TensionKind::DuplicateDefinition,
                        summary: format!("{} and {} {what}", x.block, y.block),
                        stable: cx.stable(&x.block)? || cx.stable(&y.block)?,
                        involved: [x, y]
                            .into_iter()
                            .map(|m| cx.involved(m.side.clone(), &m.block, m.version))
                            .collect::<Result<_, _>>()?,
                    });
                }
            }
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
