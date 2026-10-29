//! O12 (D-ZWP7Q, D-A8J01): owner entries live in the policy's `owners`. The most specific entry
//! wins, and replaces.
use crate::Materialized;
use crate::paths::resolve_in;
use rdy_model::BlockId;
use rdy_model::tree::FOLDER;
use rdy_schema::policy::{OwnerEntry, PolicyFields};

/// The block an entry's `path` names in `m`: a text that parses as a block id names that block
/// when `m` holds it, else nothing; any other text resolves through folder and doc names.
fn named_block(m: &Materialized, path: &str) -> Option<BlockId> {
    match BlockId::parse(path) {
        Ok(id) => m.get(&id).map(|_| id),
        Err(_) => resolve_in(m, path).ok(),
    }
}

/// O12: the most specific entry for `block` in `m`, with the path text it came from.
///
/// An entry that resolves to `block` wins; else one that resolves to `block`'s doc; else one that
/// resolves to the deepest folder above `block`. When two entries resolve to one block, the first
/// in the list wins. An entry that names no block is skipped. None when `m` does not hold `block`
/// or no entry covers it.
pub fn owner_entry<'a>(
    m: &Materialized,
    p: &'a PolicyFields,
    block: &BlockId,
) -> Option<&'a OwnerEntry> {
    let b = m.get(block)?;
    if p.owners.is_empty() {
        return None;
    }
    let doc = m.doc_of(block).map(|d| d.id.clone());
    // the folders above `block`, nearest (deepest) first; bounded by the block count, as
    // `Materialized::path_of` is, so a parent loop in a damaged store ends
    let mut folders: Vec<BlockId> = vec![];
    let mut cur = b.parent.as_ref().and_then(|id| m.get(id));
    for _ in 0..=m.len() {
        let Some(node) = cur else { break };
        if node.block_type == FOLDER {
            folders.push(node.id.clone());
        }
        cur = node.parent.as_ref().and_then(|id| m.get(id));
    }
    let mut doc_hit: Option<&'a OwnerEntry> = None;
    // (index in `folders`, entry): the deepest folder hit so far
    let mut folder_hit: Option<(usize, &'a OwnerEntry)> = None;
    for entry in &p.owners {
        let Some(target) = named_block(m, &entry.path) else {
            continue;
        };
        if target == *block {
            return Some(entry);
        }
        if doc.as_ref() == Some(&target) {
            doc_hit.get_or_insert(entry);
            continue;
        }
        if let Some(depth) = folders.iter().position(|f| *f == target)
            && folder_hit.is_none_or(|(d, _)| depth < d)
        {
            folder_hit = Some((depth, entry));
        }
    }
    doc_hit.or(folder_hit.map(|(_, e)| e))
}
