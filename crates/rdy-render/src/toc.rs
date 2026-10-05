//! The "On this page" list: the sections of a doc that has enough of them to need one.
use crate::page::Item;
use serde::Serialize;

/// A doc with fewer sections than this has no index.
const MIN_SECTIONS: usize = 3;
/// Sections nested deeper than this are in the page but not in the index.
const MAX_DEPTH: usize = 4;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Toc {
    /// The sections that have no section above them in the index.
    pub entries: Vec<TocEntry>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TocEntry {
    pub label: String,
    pub anchor: String,
    pub depth: usize,
    /// The sections below this one in the index.
    pub children: Vec<TocEntry>,
}

pub fn build(items: &[Item]) -> Toc {
    let sections = || items.iter().filter(|i| i.block_type == "section");
    if sections().count() < MIN_SECTIONS {
        return Toc::default();
    }
    let flat: Vec<TocEntry> = sections()
        .filter(|i| i.depth <= MAX_DEPTH)
        .map(|i| TocEntry {
            label: i
                .title
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "Untitled".into()),
            anchor: i.anchor.clone(),
            depth: i.depth,
            children: vec![],
        })
        .collect();
    Toc {
        entries: nest(flat),
    }
}

/// `flat` (in page order) as a tree: an entry goes under the nearest entry above it that is
/// less deep, whatever levels lie between.
fn nest(flat: Vec<TocEntry>) -> Vec<TocEntry> {
    // the open path from the root down, each with the entries that are done below it
    let mut stack: Vec<TocEntry> = vec![];
    let mut roots: Vec<TocEntry> = vec![];
    fn close(stack: &mut Vec<TocEntry>, roots: &mut Vec<TocEntry>) {
        if let Some(done) = stack.pop() {
            match stack.last_mut() {
                Some(parent) => parent.children.push(done),
                None => roots.push(done),
            }
        }
    }
    for e in flat {
        while stack.last().is_some_and(|top| top.depth >= e.depth) {
            close(&mut stack, &mut roots);
        }
        stack.push(e);
    }
    while !stack.is_empty() {
        close(&mut stack, &mut roots);
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(depth: usize) -> TocEntry {
        TocEntry {
            label: format!("d{depth}"),
            anchor: String::new(),
            depth,
            children: vec![],
        }
    }

    #[test]
    fn entries_nest_by_depth_even_when_a_level_is_skipped() {
        let t = nest(vec![e(1), e(3), e(3), e(2), e(1)]);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].children.len(), 3);
        assert_eq!(t[0].children[2].depth, 2);
        assert!(t[1].children.is_empty());
    }

    #[test]
    fn a_deeper_first_entry_is_a_root_and_a_shallower_one_after_it_is_too() {
        let t = nest(vec![e(2), e(1), e(2)]);
        assert_eq!(t.len(), 2);
        assert_eq!(t[1].children.len(), 1);
    }
}
