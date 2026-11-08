//! Hidden planned blocks (D-B2AC3, TASK-1422): what a hiding profile leaves out of a selection
//! because its status is planned, counted per doc. This is report metadata only. It is kept
//! beside the selection, never inside it, so it enters no entry, graph, rendering state or
//! digest. The hidden id set is the one the planned filter already computed; no second status
//! resolver runs here.

use super::report::REPORT_ROWS;
use super::select::{Limits, limit};
use crate::error::*;
use crate::materialize::Materialized;
use rdy_model::{Block, BlockId, tree};
use std::collections::{BTreeMap, BTreeSet};

/// One doc of a hidden list. `hidden_blocks` counts the hidden blocks under the doc, its own
/// block excluded. `doc_hidden` is true when the doc block itself is in the list's hidden set.
/// `title` is the candidate's title; `redacted` is true, and `title` is the doc id, when a
/// removal replaced the title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HiddenRow {
    pub doc: BlockId,
    pub title: String,
    pub redacted: bool,
    pub hidden_blocks: u64,
    pub doc_hidden: bool,
}

/// A bounded list of docs with hidden planned blocks. `docs` and `blocks` cover every affected
/// doc; `rows` keeps the first [`REPORT_ROWS`] by doc id and `omitted_rows` counts the rest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HiddenList {
    pub docs: u64,
    pub blocks: u64,
    pub rows: Vec<HiddenRow>,
    pub omitted_rows: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DocFacts {
    title: String,
    redacted: bool,
    /// The doc block itself is hidden.
    hidden: bool,
}

/// The hidden planned blocks of one resolution, with the candidate's doc membership and the safe
/// title data of each doc that holds a hidden block or is hidden. It holds no hidden text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HiddenPlanned {
    /// Every hidden block id, docs included: the set two resolutions compare.
    ids: BTreeSet<BlockId>,
    /// Each hidden block that is no doc, with the doc it sits in.
    owner: BTreeMap<BlockId, BlockId>,
    docs: BTreeMap<BlockId, DocFacts>,
}

impl HiddenPlanned {
    /// The metadata of `hidden`, the ids the filter removed from `m`. The ids are already
    /// within the selection bound; each title is checked against the text bounds before it is
    /// copied.
    pub(crate) fn capture(
        m: &Materialized,
        hidden: &BTreeSet<BlockId>,
        l: &Limits,
    ) -> Result<HiddenPlanned, RdyError> {
        if hidden.len() > l.selection {
            return Err(limit("selection", l.selection, "blocks"));
        }
        let mut out = HiddenPlanned::default();
        let mut text = 0usize;
        for id in hidden {
            let Some(b) = m.get(id) else { continue };
            out.ids.insert(id.clone());
            let is_doc = b.block_type == tree::DOC;
            let Some(doc) = (if is_doc { Some(b) } else { m.doc_of(id) }) else {
                continue;
            };
            if !is_doc {
                out.owner.insert(id.clone(), doc.id.clone());
            }
            if !out.docs.contains_key(&doc.id) {
                let facts = facts(doc, hidden.contains(&doc.id), l, &mut text)?;
                out.docs.insert(doc.id.clone(), facts);
            }
        }
        Ok(out)
    }

    /// Every doc with a hidden block or hidden itself.
    pub fn full(&self) -> HiddenList {
        let mut counts: BTreeMap<&BlockId, (u64, bool)> = BTreeMap::new();
        for doc in self.owner.values() {
            counts.entry(doc).or_default().0 += 1;
        }
        for (id, d) in &self.docs {
            if d.hidden {
                counts.entry(id).or_default().1 = true;
            }
        }
        self.list(counts)
    }

    /// What this resolution hides that `before` did not, by id: a version change to a block
    /// hidden on both sides is no change. A doc hidden here and not there is listed even with no
    /// new hidden block under it, and its `doc_hidden` is true. A block counts under its doc in
    /// this resolution.
    pub fn newly(&self, before: &HiddenPlanned) -> HiddenList {
        let mut counts: BTreeMap<&BlockId, (u64, bool)> = BTreeMap::new();
        for (id, doc) in &self.owner {
            if !before.ids.contains(id) {
                counts.entry(doc).or_default().0 += 1;
            }
        }
        for (id, d) in &self.docs {
            if d.hidden && !before.ids.contains(id) {
                counts.entry(id).or_default().1 = true;
            }
        }
        self.list(counts)
    }

    fn list(&self, counts: BTreeMap<&BlockId, (u64, bool)>) -> HiddenList {
        let total = counts.len();
        let blocks = counts.values().map(|(n, _)| *n).sum();
        let rows = counts
            .into_iter()
            .take(REPORT_ROWS)
            .filter_map(|(id, (hidden_blocks, doc_hidden))| {
                let d = self.docs.get(id)?;
                Some(HiddenRow {
                    doc: id.clone(),
                    title: d.title.clone(),
                    redacted: d.redacted,
                    hidden_blocks,
                    doc_hidden,
                })
            })
            .collect::<Vec<_>>();
        HiddenList {
            docs: total as u64,
            blocks,
            omitted_rows: (total - rows.len()) as u64,
            rows,
        }
    }
}

/// The title facts of `doc`. A removed title shows the doc id and never the removed text.
fn facts(doc: &Block, hidden: bool, l: &Limits, text: &mut usize) -> Result<DocFacts, RdyError> {
    let redacted = doc.is_redacted();
    let shown: &str = if redacted {
        doc.id.as_str()
    } else {
        doc.field_str("title")
            .filter(|t| !t.is_empty())
            .or_else(|| doc.field_str("name").filter(|n| !n.is_empty()))
            .unwrap_or_else(|| doc.id.as_str())
    };
    if shown.len() > l.text_value {
        return Err(limit("text value", l.text_value, "bytes"));
    }
    *text = text.saturating_add(shown.len());
    if *text > l.text {
        return Err(limit("text", l.text, "bytes"));
    }
    Ok(DocFacts {
        title: shown.to_string(),
        redacted,
        hidden,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_model::{FieldValue, StateSnapshot};

    fn block(id: &str, ty: &str, parent: Option<&str>, title: Option<&str>) -> Block {
        Block {
            id: BlockId::parse(id).unwrap(),
            block_type: ty.into(),
            parent: parent.map(|p| BlockId::parse(p).unwrap()),
            order: "a".into(),
            fields: title
                .map(|t| ("title".to_string(), FieldValue::Str(t.into())))
                .into_iter()
                .collect(),
            body: String::new(),
            links: BTreeMap::new(),
            pins: BTreeMap::new(),
            read_identity: None,
        }
    }

    fn state(titles: &[&str]) -> (Materialized, BTreeSet<BlockId>) {
        let mut blocks = BTreeMap::new();
        let mut hidden = BTreeSet::new();
        for (i, t) in titles.iter().enumerate() {
            let id = format!("d-H{i:04}");
            let b = block(&id, "doc", None, Some(t));
            hidden.insert(b.id.clone());
            blocks.insert(b.id.clone(), b);
        }
        let m = Materialized::build(
            StateSnapshot {
                blocks: BTreeMap::new(),
            },
            blocks,
        );
        (m, hidden)
    }

    #[test]
    fn relc4_hidden_metadata_obeys_existing_report_and_selection_bounds() {
        let l = Limits {
            text_value: 4,
            text: 7,
            selection: 3,
            ..Limits::default()
        };
        // within every bound
        let (m, hidden) = state(&["abcd", "abc"]);
        assert_eq!(
            HiddenPlanned::capture(&m, &hidden, &l).unwrap().full().docs,
            2
        );
        // one title over the value bound is refused with the existing words
        let (m, hidden) = state(&["abcde"]);
        let e = HiddenPlanned::capture(&m, &hidden, &l).unwrap_err();
        assert_eq!(e.code, E_LIMIT);
        assert_eq!(
            e.message,
            "Publication text value exceeds the limit of 4 bytes."
        );
        assert_eq!(e.fix, "Reduce the selection or split the source project.");
        // all titles together over the text bound
        let (m, hidden) = state(&["abcd", "abcd"]);
        let e = HiddenPlanned::capture(&m, &hidden, &l).unwrap_err();
        assert_eq!(e.message, "Publication text exceeds the limit of 7 bytes.");
        // more hidden ids than the selection bound, refused before any title is read
        let (m, hidden) = state(&["a", "b", "c", "d"]);
        let e = HiddenPlanned::capture(&m, &hidden, &l).unwrap_err();
        assert_eq!(
            e.message,
            "Publication selection exceeds the limit of 3 blocks."
        );
        // the report keeps its own bound and reserve
        assert_eq!(super::super::report::REPORT_BYTES, 8_388_608);
        assert_eq!(super::super::report::REPORT_ROWS, 200);
    }
}
