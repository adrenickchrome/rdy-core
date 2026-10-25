//! Copy (D-BRF4P as amended by D-E8P8J): the Markdown a page's Copy control puts on the
//! clipboard.
//!
//! A page carries one [`CopyPayload`]: the Markdown of each block it shows, once, in the order it
//! shows them, built by the export's writer ([`copy_parts`]) from the same HTML the page shows.
//! A toolbar names a [`CopyRange`] of it: one block, a section (its heading and every block under
//! it) or the whole page (its title, its lede and every block). A link target stays a slot
//! ([`CopyPart::Url`]) until a reader copies: `copy.js` resolves it against the page's own URL
//! and writes it with the export's link-target escape ([`link_target`]), so a copy holds absolute
//! links and no id. [`CopyPayload::markdown`] is what `copy.js` does, for the tests.
use crate::print::md::{CopyPart, copy_parts};
use crate::print::{Blob, ir};
use rdy_model::Hash;
use serde::Serialize;
use std::collections::BTreeMap;

/// The Markdown of every block a page shows, once, in page order. A block that writes nothing
/// has no entry, so no range holds an empty block.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CopyPayload {
    pub blocks: Vec<Vec<CopyPart>>,
}

/// The blocks `start..end` of a page's [`CopyPayload`] (`end` exclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CopyRange {
    pub start: usize,
    pub end: usize,
}

impl CopyRange {
    /// The range, or none when it holds no block (that toolbar gets no Copy control).
    pub(crate) fn of(start: usize, end: usize) -> Option<CopyRange> {
        (start < end).then_some(CopyRange { start, end })
    }
}

impl CopyPayload {
    /// The payload as the page carries it: JSON, which the template escapes as text.
    pub fn json(&self) -> String {
        serde_json::to_string(self).expect("a payload is JSON")
    }

    /// Adds the Markdown of `html` (a piece of the page's own HTML, on site page `page`) as one
    /// block, unless it writes nothing; the payload's length after it.
    pub(crate) fn push_html(&mut self, html: &str, page: &str) -> usize {
        let (blocks, files) = ir::from_html_with_figures(html, page);
        // a figure's file as the page names it: `assets/<hash>.<extension>`
        let mut blobs: BTreeMap<Hash, Blob> = BTreeMap::new();
        for file in files {
            let hash = file
                .strip_prefix("assets/")
                .and_then(|f| f.split_once('.'))
                .and_then(|(stem, _)| stem.parse::<Hash>().ok());
            if let Some(h) = hash {
                blobs.entry(h).or_insert(Blob {
                    media_type: String::new(),
                    bytes: vec![],
                    file,
                });
            }
        }
        // the writer has no error of its own today: a block it refuses writes nothing, so its
        // toolbar has no Copy control
        match copy_parts(&blocks, page, &blobs) {
            Ok(parts) if !parts.is_empty() => self.blocks.push(parts),
            _ => {}
        }
        self.blocks.len()
    }

    /// The Markdown of `range`, as `copy.js` writes it: the blocks joined by one blank line, each
    /// link target `resolve`d (an absolute URL) and written with [`link_target`]. None when the
    /// range is not one of this payload's or a target does not resolve: nothing is copied.
    pub fn markdown(
        &self,
        range: CopyRange,
        resolve: &mut dyn FnMut(&str) -> Option<String>,
    ) -> Option<String> {
        if range.start >= range.end {
            return None;
        }
        let blocks = self.blocks.get(range.start..range.end)?;
        let mut out = String::new();
        for (i, block) in blocks.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            for part in block {
                match part {
                    CopyPart::Text(t) => out.push_str(t),
                    CopyPart::Url(u) => out.push_str(&link_target(&resolve(u)?)),
                }
            }
        }
        Some(out)
    }
}

/// An absolute link target as the export writes one inside `(…)`: space, `(`, `)`, `<` and `>`
/// percent-encoded, CR, LF and tab dropped, and a `&` that opens a character reference written
/// as `&amp;`. `copy.js` writes the same.
pub fn link_target(resolved: &str) -> String {
    crate::print::md::url(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dgm9_payload_ranges_join_blocks_with_one_blank_line() {
        let mut p = CopyPayload::default();
        assert_eq!(p.push_html("<h1>Title</h1>", "a/b.html"), 1);
        assert_eq!(
            p.push_html("<p></p>", "a/b.html"),
            1,
            "an empty block is not stored"
        );
        assert_eq!(
            p.push_html("<p>See <a href=\"c.html#x\">c</a>.</p>", "a/b.html"),
            2
        );
        let md = p.markdown(CopyRange { start: 0, end: 2 }, &mut |u| {
            Some(format!("file:///s/a/{u}"))
        });
        assert_eq!(
            md.as_deref(),
            Some("# Title\n\nSee [c](file:///s/a/c.html#x).\n")
        );
        assert_eq!(
            p.markdown(CopyRange { start: 1, end: 3 }, &mut |u| Some(u.into())),
            None
        );
        assert_eq!(
            p.markdown(CopyRange { start: 1, end: 1 }, &mut |u| Some(u.into())),
            None
        );
        assert_eq!(
            p.markdown(CopyRange { start: 1, end: 2 }, &mut |_| None),
            None
        );
        assert_eq!(CopyRange::of(2, 2), None);
    }
}
