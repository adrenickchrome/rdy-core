//! The "On this page" list: the sections of a doc that has enough of them to need one, and in a
//! review render the changes of the page (D-NYCMF).
use crate::page::Item;
use crate::review::Mark;
use serde::Serialize;

/// A doc with fewer sections than this has no section list.
const MIN_SECTIONS: usize = 3;
/// Sections nested deeper than this are in the page but not in the index.
const MAX_DEPTH: usize = 4;
/// The most characters of a change's label (O1).
pub(crate) const LABEL_MAX: usize = 40;

/// The word a section entry or a change of the page index carries (D-NYCMF).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Word {
    Edited,
    New,
    Removed,
}

impl From<Mark> for Word {
    fn from(m: Mark) -> Self {
        match m {
            Mark::Edited => Word::Edited,
            Mark::New => Word::New,
        }
    }
}

/// One entry of "Changes on this page": its word, its label, and where it jumps (`#<id>`).
#[derive(Clone, Debug, Serialize)]
pub struct Change {
    pub word: Word,
    /// Escaped HTML, written as it is: a code span of the block's text stays `<code>` (ruling
    /// task-2). Built by [`change_label_html`] or [`change_label_coded`].
    pub label: String,
    pub href: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Toc {
    /// The changes of the page in page order, for the "Changes on this page" group; empty
    /// outside a review render's doc page.
    pub changes: Vec<Change>,
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
    /// The word of the changes the section holds, in a review render.
    pub change: Option<Word>,
}

/// `text` cut as `home::cut_plain(text, LABEL_MAX)` cuts it.
pub(crate) fn change_label(text: &str) -> String {
    crate::home::cut_plain(text, LABEL_MAX)
}

/// A change's label from plain `text` (a heading, a name, the notice), as [`Change::label`]
/// holds it: [`change_label`], escaped.
pub(crate) fn change_label_html(text: &str) -> String {
    crate::escape(&change_label(text))
}

/// A change's label from characters with their code spans
/// ([`crate::site::Site::label_coded`]), as [`Change::label`] holds it: cut to [`LABEL_MAX`] as
/// the home card's summary is cut, escaped, each code span a `<code>` element
/// (`home::summary_html`, ruling task-2).
pub(crate) fn change_label_coded(chars: &[crate::changes::Ch]) -> String {
    crate::home::summary_html_of(chars, LABEL_MAX)
}

/// One item of a doc page in page order, as the section words read it.
pub(crate) struct Piece {
    pub depth: usize,
    pub section: bool,
    pub word: Option<Word>,
    /// The Removed lines shown just before the item.
    pub removed_before: usize,
}

/// The word of each section the index shows (depth at most MAX_DEPTH), in page order, by O3.
/// `removed_end` counts the Removed lines after the last item.
pub(crate) fn section_words(pieces: &[Piece], removed_end: usize) -> Vec<Option<Word>> {
    let mut words: Vec<Option<Word>> = vec![];
    // the shown sections that hold the current place: their depth and their index in `words`
    let mut open: Vec<(usize, usize)> = vec![];
    fn give(words: &mut [Option<Word>], open: &[(usize, usize)], w: Word) {
        for &(_, i) in open {
            words[i] = Some(match words[i] {
                Some(had) if had != w => Word::Edited,
                _ => w,
            });
        }
    }
    for p in pieces {
        if p.removed_before > 0 {
            give(&mut words, &open, Word::Removed);
        }
        while open.last().is_some_and(|&(d, _)| d >= p.depth) {
            open.pop();
        }
        if p.section && p.depth <= MAX_DEPTH {
            words.push(None);
            open.push((p.depth, words.len() - 1));
        }
        if let Some(w) = p.word {
            give(&mut words, &open, w);
        }
    }
    if removed_end > 0 {
        give(&mut words, &open, Word::Removed);
    }
    words
}

/// The index of a doc page: its sections (when it has at least MIN_SECTIONS), each with its
/// word, and `changes` as given.
pub fn build(items: &[Item], removed_end: usize, changes: Vec<Change>) -> Toc {
    let sections = || items.iter().filter(|i| i.block_type == "section");
    if sections().count() < MIN_SECTIONS {
        return Toc {
            changes,
            entries: vec![],
        };
    }
    let pieces: Vec<Piece> = items
        .iter()
        .map(|i| Piece {
            depth: i.depth,
            section: i.block_type == "section",
            word: i.change.map(Word::from),
            removed_before: i.removed_before.len(),
        })
        .collect();
    let words = section_words(&pieces, removed_end);
    let flat: Vec<TocEntry> = sections()
        .filter(|i| i.depth <= MAX_DEPTH)
        .zip(words)
        .map(|(i, change)| TocEntry {
            label: i
                .title
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "Untitled".into()),
            anchor: i.anchor.clone(),
            depth: i.depth,
            children: vec![],
            change,
        })
        .collect();
    Toc {
        changes,
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
            change: None,
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
    #[test]
    fn rvm2_a_label_is_cut_at_a_word_boundary_to_40_characters() {
        for (text, want) in [
            ("Short heading", "Short heading"),
            (
                "Every arrow in a chain counts toward the bound here",
                "Every arrow in a chain counts toward the…",
            ),
            (
                "Every arrow in a chain counts towards the bound",
                "Every arrow in a chain counts towards…",
            ),
            (
                "Ünïcödé wörds éverywhere ïn thïs hèading, ok then",
                "Ünïcödé wörds éverywhere ïn thïs…",
            ),
            (
                "Supercalifragilisticexpialidociousandmoremoremore",
                "Supercalifragilisticexpialidociousandmor…",
            ),
            (
                "A  heading\nwith   spaces that runs past forty characters",
                "A heading with spaces that runs past…",
            ),
        ] {
            assert_eq!(change_label(text), want, "{text}");
            assert!(
                change_label(text).chars().count() <= LABEL_MAX + 1,
                "{text}"
            );
        }
    }

    fn piece(depth: usize, section: bool, word: Option<Word>, removed_before: usize) -> Piece {
        Piece {
            depth,
            section,
            word,
            removed_before,
        }
    }

    fn sec(depth: usize) -> Piece {
        piece(depth, true, None, 0)
    }

    fn prose(depth: usize, word: Option<Word>) -> Piece {
        piece(depth, false, word, 0)
    }

    #[test]
    fn rvm2_section_words_take_one_word_or_edited() {
        let marked = [
            sec(1),
            prose(2, Some(Word::New)),
            sec(1),
            prose(2, Some(Word::Edited)),
            prose(2, Some(Word::New)),
            sec(1),
            prose(2, None),
            // S4's heading: the line shown before it is S3's
            piece(1, true, None, 1),
            prose(2, None),
        ];
        assert_eq!(
            section_words(&marked, 0),
            vec![
                Some(Word::New),
                Some(Word::Edited),
                Some(Word::Removed),
                None
            ]
        );
        // a page whose only change is the lede, which is never a piece
        let calm = [
            sec(1),
            prose(2, None),
            sec(1),
            prose(2, None),
            prose(2, None),
            sec(1),
            prose(2, None),
            sec(1),
            prose(2, None),
        ];
        assert_eq!(section_words(&calm, 0), vec![None; 4]);
    }

    #[test]
    fn rvm2_section_words_mark_parents_and_skip_deep_sections() {
        let pieces = [
            sec(1),
            sec(2),
            sec(3),
            sec(4),
            sec(5),
            prose(6, Some(Word::Edited)),
        ];
        assert_eq!(section_words(&pieces, 0), vec![Some(Word::Edited); 4]);
    }

    #[test]
    fn rvm2_section_words_place_removed_lines_where_they_show() {
        // a line before the first piece marks nothing; a line before S2's heading marks S1;
        // the lines at the end mark the sections that hold the last piece
        let pieces = [
            piece(1, true, None, 1),
            prose(2, None),
            piece(1, true, None, 1),
            prose(2, None),
            sec(1),
            prose(2, None),
        ];
        assert_eq!(
            section_words(&pieces, 1),
            vec![Some(Word::Removed), None, Some(Word::Removed)]
        );
        // a depth-1 prose after S1 is not in S1, nor are the lines after it
        let after = [sec(1), prose(2, None), prose(1, Some(Word::Edited))];
        assert_eq!(section_words(&after, 1), vec![None]);
        let between = [
            sec(1),
            prose(2, None),
            prose(1, Some(Word::New)),
            sec(1),
            prose(2, None),
        ];
        assert_eq!(section_words(&between, 0), vec![None, None]);
    }
}
