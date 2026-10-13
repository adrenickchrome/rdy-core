//! Section addresses: `<doc>#<heading-slug>` names one section of a doc.

use crate::error::*;
use crate::materialize::Materialized;
use rdy_model::tree::{DOC, slug as slug_of};
use rdy_model::{Block, BlockId};
use std::collections::BTreeSet;

/// Splits `text` at its `#` into the target and the section slug. A text that starts with `[` is
/// a ref line, whose trailing words may hold a `#`; ids, keys, paths, versions and slugs never
/// do. An empty slug after the `#` is `E_BAD_INPUT`.
pub fn split_address(text: &str) -> Result<(&str, Option<&str>), RdyError> {
    let (target, slug) = match text.split_once('#') {
        Some((t, s)) if !text.trim_start().starts_with('[') => (t, Some(s)),
        _ => (text, None),
    };
    if slug == Some("") {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "a # needs a heading slug after it",
            "write <doc>#<heading-slug>, e.g. guide#set-up",
        ));
    }
    Ok((target, slug))
}

/// rfoc1: the section of `doc` that `slug` names. Sections are named by the slug of their title,
/// depth-first in child order; the 2nd, 3rd, … section with the same slug in this doc is named
/// `{slug}-2`, `{slug}-3`, …, skipping a name an earlier section already has. The first exact
/// match wins.
pub fn section_by_slug(m: &Materialized, doc: &Block, slug: &str) -> Result<BlockId, RdyError> {
    if doc.block_type != DOC {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!(
                "{} is a {}, not a doc: #<heading> reads a section of a doc",
                doc.id, doc.block_type
            ),
            "pass a doc's id, key or path before #",
        ));
    }
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut walk: Vec<&Block> = m.children(Some(&doc.id)).into_iter().rev().collect();
    while let Some(b) = walk.pop() {
        // a doc inside this doc has its own sections
        if b.block_type == DOC {
            continue;
        }
        if b.block_type == "section" {
            let base = slug_of(b.field_str("title").unwrap_or(""));
            let name = if taken.contains(&base) {
                (2..)
                    .map(|n| format!("{base}-{n}"))
                    .find(|c| !taken.contains(c))
                    .expect("an unbounded range has a free name")
            } else {
                base
            };
            taken.insert(name.clone());
            if name == slug {
                return Ok(b.id.clone());
            }
        }
        walk.extend(m.children(Some(&b.id)).into_iter().rev());
    }
    Err(RdyError::new(
        E_UNKNOWN_BLOCK,
        format!("no section #{slug} in {}", doc.id),
        "rdy_read the doc to see its section titles",
    ))
}
