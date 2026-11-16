//! The owned rows of the publication review (D-RDH1C, D-9BWMV, D-0C4TB, D-MCH70, D-BND1J,
//! D-5WDC2, D-EGFYD, D-9PG33, D-62KA3, D-YKZNK): what a
//! reviewer reads about a candidate. The row builders fill them from two
//! [`super::review_side::PublicationReviewSide`] views; one projection writes them for chat, the
//! CLI and the adapter. They add to the raw comparison ([`super::DisclosureDiff`]) and the
//! selection types, which keep their fields and callers.
//!
//! Every text here is plain: a code span keeps its backticks, and no generated HTML enters a row.
//! A title is `None` only when a removal removed it. No row fact enters a selection hash, a
//! selection identity or a doc digest (D-B2AC3).

use super::select::AttachmentVariant;
use rdy_model::BlockId;
use serde::{Serialize, Serializer};

/// A dark image's row writes `variant` as `dark`.
impl Serialize for AttachmentVariant {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// What happened to a block, a doc or a decision between the sides. Movement alone is `moved`;
/// a decision has no `moved` kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewChange {
    Added,
    Removed,
    Updated,
    Moved,
}

/// One word-diff operation, as the word engine names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffOp {
    Same,
    Del,
    Add,
}

/// One piece of a short diff: its operation and its plain text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiffPiece {
    pub op: DiffOp,
    pub text: String,
}

/// A changed doc's counts, complete before any list is cut. Decisions count apart from ordinary
/// blocks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ReviewCounts {
    pub added: u64,
    pub removed: u64,
    pub updated: u64,
    pub moved: u64,
    pub sections_added: u64,
    pub sections_removed: u64,
    pub figures_added: u64,
    pub figures_removed: u64,
    pub figures_changed: u64,
    pub decisions_added: u64,
    pub decisions_removed: u64,
    pub decisions_updated: u64,
}

impl ReviewCounts {
    /// The doc's changed blocks plus its changed decisions (D-6T9WJ). A section and a figure are
    /// blocks already counted, so their own counts stay out.
    pub fn changes_total(&self) -> u64 {
        self.added
            + self.removed
            + self.updated
            + self.moved
            + self.decisions_added
            + self.decisions_removed
            + self.decisions_updated
    }
}

impl DocSize {
    /// The `changes_total` of an added or removed doc (D-6T9WJ): 1 for the doc, plus its blocks
    /// and its decisions.
    pub fn changes_total(&self) -> u64 {
        1 + self.blocks + self.decisions
    }
}

/// One change in a changed doc. `what` lists the changed parts of an update, in part order, with
/// `place` for a counted move; it is empty for every other kind. `section` is the label of the
/// nearest section (the baseline's, for a removed block), and `section_id` that section's id.
/// `before_section` and `before_section_id` name the nearest section that held the block on the
/// baseline's side, for a `moved` row and an `updated` row with `place` only (D-EGFYD, D-9PG33);
/// each id is `None` where its label is. `excerpt` and `diff` are `None` for removed content
/// (`redacted`) and where a side has no reader text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChangeRow {
    pub id: BlockId,
    pub block_type: String,
    pub change: ReviewChange,
    pub label: String,
    pub section: Option<String>,
    pub section_id: Option<BlockId>,
    pub before_section: Option<String>,
    pub before_section_id: Option<BlockId>,
    pub what: Vec<String>,
    pub excerpt: Option<String>,
    pub diff: Option<Vec<DiffPiece>>,
    pub redacted: bool,
}

/// One decision row. `what` lists `title`, `statement` and `status` for an update, and is empty
/// for an added or removed decision. `status_from` is `None` for an added decision and
/// `status_to` for a removed one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DecisionRow {
    pub id: BlockId,
    pub title: String,
    pub change: ReviewChange,
    pub status_from: Option<String>,
    pub status_to: Option<String>,
    pub what: Vec<String>,
    pub excerpt: Option<String>,
}

/// One section of a doc's outline (D-EGFYD, D-9PG33): its block id, its label, the shown blocks
/// whose nearest section it is, and the change rows whose block sits in it. A nested section
/// counts as one block of its parent and has its own row. A redacted section's label reads
/// "Content removed."; its id stays.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutlineRow {
    pub id: BlockId,
    pub label: String,
    pub blocks: u64,
    pub changes: u64,
}

/// What a changed doc adds to its comparison row. `title` and `page` are the candidate's;
/// `before_title` is the baseline's title when it differs, else `None`; `before_page` is the
/// baseline's page (D-RDH1C: each side's render names its page). A page is `None` when that
/// side's site gives the doc none. `folder` is the candidate's top folder title (D-5WDC2), and
/// `folder_redacted` is true only when a removal removed it (D-62KA3). `position` is the row's
/// place in the review's one reading order of doc rows (D-EGFYD). `outline` is the candidate's,
/// with each section's change rows counted. `record_only` is true when the digest changed and
/// every count is zero.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChangedDocReview {
    pub id: BlockId,
    pub title: Option<String>,
    pub before_title: Option<String>,
    pub page: Option<String>,
    pub before_page: Option<String>,
    pub folder: Option<String>,
    pub folder_redacted: bool,
    pub position: u64,
    pub outline: Vec<OutlineRow>,
    pub omitted_outline: u64,
    pub counts: ReviewCounts,
    pub record_only: bool,
    pub changes: Vec<ChangeRow>,
    pub omitted_changes: u64,
    pub decisions: Vec<DecisionRow>,
    pub omitted_decisions: u64,
}

impl ChangedDocReview {
    /// The row's `changes_total` (D-6T9WJ): its counts' changed blocks and decisions, over every
    /// change, also those a cap leaves out of the lists. A record-only row has none.
    pub fn changes_total(&self) -> u64 {
        self.counts.changes_total()
    }
}

/// The size of a doc as its side's view shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DocSize {
    pub sections: u64,
    pub blocks: u64,
    pub decisions: u64,
    pub figures: u64,
}

/// One attachment image of a doc: its figure's label, and `variant` for a dark image only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FigureEntry {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<AttachmentVariant>,
}

/// What an added or included doc shows. `folder` names its top folder, `None` for a root doc or
/// when a removal removed its title; `folder_redacted` is true only in the second case
/// (D-62KA3). `position` is the row's place in its reading order of doc rows (D-EGFYD). Each
/// outline row counts every block of its section as a change, because each block is added.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DocSummary {
    pub id: BlockId,
    pub title: Option<String>,
    pub page: Option<String>,
    pub folder: Option<String>,
    pub folder_redacted: bool,
    pub position: u64,
    pub size: DocSize,
    pub lede: Option<String>,
    pub outline: Vec<OutlineRow>,
    pub omitted_outline: u64,
    pub decision_titles: Vec<String>,
    pub omitted_decision_titles: u64,
    pub figures: Vec<FigureEntry>,
    pub omitted_figures: u64,
}

/// What a removed doc adds to its comparison row, from the baseline: its title, page, top
/// folder (D-5WDC2) and size. `position` is its place in the review's one reading order of doc
/// rows (D-EGFYD).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RemovedDocReview {
    pub id: BlockId,
    pub title: Option<String>,
    pub before_page: Option<String>,
    pub folder: Option<String>,
    pub folder_redacted: bool,
    pub position: u64,
    pub size: DocSize,
}

/// The totals of a first publication: the sums of the rows' sizes, with the selected docs and
/// folders.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct IncludedTotals {
    pub docs: u64,
    pub folders: u64,
    pub sections: u64,
    pub blocks: u64,
    pub decisions: u64,
    pub figures: u64,
}

/// A first publication's review: every selected doc in the added-doc form. `docs` and `folders`
/// equal their totals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Included {
    pub docs: u64,
    pub folders: u64,
    pub rows: Vec<DocSummary>,
    pub omitted_rows: u64,
    pub totals: IncludedTotals,
}

/// What a changed folder adds: the candidate's title, else the baseline's for a removed folder,
/// and `before_title` when the baseline's differs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FolderReview {
    pub id: BlockId,
    pub title: Option<String>,
    pub before_title: Option<String>,
}

/// What a row outside docs adds. A version change with no changed part keeps its row with
/// `change` `None`, an empty `what` and `record_only` true; it counts nothing (D-BND1J). A
/// decision row with a change also carries its decision fields; a record-only row has none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutsideReview {
    pub id: BlockId,
    pub block_type: String,
    pub label: String,
    pub change: Option<ReviewChange>,
    pub what: Vec<String>,
    pub record_only: bool,
    pub decision: Option<DecisionRow>,
}

/// The labels an attachment row adds: its figure's label and its home doc's title. A changed
/// attachment gives both home doc titles instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AttachmentLabels {
    pub label: String,
    pub home_doc_title: Option<String>,
    pub before_home_doc_title: Option<String>,
    pub after_home_doc_title: Option<String>,
}

/// The labels a reference row adds, read from the candidate; `target_label` is `None` for a
/// missing target. `source_doc_redacted` is true only when a removal removed the title of the
/// source's doc, so a `None` title with false means the source sits outside docs (D-62KA3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReferenceLabels {
    pub source_label: String,
    pub source_doc_title: Option<String>,
    pub source_doc_redacted: bool,
    pub target_label: Option<String>,
}

/// A decision outside the selection that a reference names: its id and its title.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DroppedDecision {
    pub id: BlockId,
    pub title: String,
}

/// The dropped decisions, in id order. They count as neither excluded nor missing references.
/// `count` counts every one, also the rows a cut leaves out (D-YKZNK).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DroppedDecisions {
    pub count: u64,
    pub rows: Vec<DroppedDecision>,
    pub omitted_rows: u64,
}

/// The labels of one reference report, read from the candidate: one for each row of its
/// `rows` and of its `links_outside`, in their order, and its dropped decisions with their
/// titles. The report's status and counts stay as they are.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ReferenceReview {
    pub rows: Vec<ReferenceLabels>,
    pub links_outside: Vec<ReferenceLabels>,
    pub dropped_decisions: DroppedDecisions,
}

/// The totals of a comparison, over the complete comparison before any row is cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DisclosureTotals {
    pub docs_changed: u64,
    pub docs_added: u64,
    pub docs_removed: u64,
    pub docs_record_only: u64,
    pub blocks_added: u64,
    pub blocks_removed: u64,
    pub blocks_updated: u64,
    pub blocks_moved: u64,
    pub sections_added: u64,
    pub sections_removed: u64,
    pub figures_added: u64,
    pub figures_removed: u64,
    pub figures_changed: u64,
    pub decisions_added: u64,
    pub decisions_removed: u64,
    pub decisions_updated: u64,
    pub new_references: u64,
}

/// The review of a comparison, before any projection cuts an outer list: the totals over the
/// complete comparison, and every changed, added and removed doc row and every row outside docs.
/// The inner lists are cut already, by the limits both projections share: a changed doc's
/// changes by its share of the change budget, its decisions by their limit. Doc rows follow each
/// side's reading order. The rows outside docs, the folder rows and the attachment labels follow
/// the comparison's own order: one for each row of its `changed_outside_docs`,
/// `changed_folders`, `added_attachments`, `removed_attachments` and `changed_attachments`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DisclosureReview {
    pub totals: DisclosureTotals,
    pub changed_docs: Vec<ChangedDocReview>,
    pub added_docs: Vec<DocSummary>,
    pub removed_docs: Vec<RemovedDocReview>,
    pub changed_outside_docs: Vec<OutsideReview>,
    pub changed_folders: Vec<FolderReview>,
    pub added_attachments: Vec<AttachmentLabels>,
    pub removed_attachments: Vec<AttachmentLabels>,
    pub changed_attachments: Vec<AttachmentLabels>,
}
