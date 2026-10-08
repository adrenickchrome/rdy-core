//! Duplicate definition: two blocks define the same name, or have the same normative text.
//!
//! A group is the blocks that share one normalized name, or one normalized text of at least the
//! pack's `duplicate_text_min_words` words. In flight, a group is its live writes plus the canon
//! blocks no change writes; every pair of distinct blocks with an in-flight side is a tension.
//! In canon (FW-CH6, FW-CH6a) every pair of canon blocks in a group is one: a duplicate is a
//! standing condition, so two duplicates that both land keep their vkey until one is renamed or
//! removed.
use super::{
    DetectCx, DetectScope, Detector, Found, Involved, SideRef, TensionKind, Write, normalize_name,
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

/// The keys to examine: every key of the scope, or with `seeds` those of the seeds' blocks (their
/// live writes in flight, canon's version in the canon scope).
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
        (Some(seeds), DetectScope::Canon) => {
            let m = cx.canon_m()?;
            for s in seeds {
                if let Some(b) = m.get(s) {
                    add(b);
                }
            }
        }
    }
    if seeds.is_none() && cx.scope() == DetectScope::Canon {
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
            let group = members(cx, canon.map_or(&[], Vec::as_slice), writes)?;
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
                            .map(|m| Involved {
                                side: m.side.clone(),
                                block: m.block.clone(),
                                version: m.version,
                            })
                            .collect(),
                    });
                }
            }
        }
        Ok(())
    }
}
