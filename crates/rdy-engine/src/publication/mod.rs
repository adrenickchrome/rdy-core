//! Publications (D-48NYM, D-0VCQ5, D-KDFZ3): a publication profile, an ordinary versioned block,
//! selects whole docs and folders of one source snapshot. [`resolve`] turns a profile into that
//! bounded selection: the selected state, its induced graph, the selection and doc digests, the
//! attachments, and the check of every reference that leaves the selection. It produces no
//! artifact and authorizes no disclosure; output callers ask [`Resolved::require_output`].
//! [`resolve_inspection`] makes the same selection in a view or a proposal version too, for the
//! live site and the publication check (D-CWC91); durable output keeps [`resolve`].
//! [`resolve_source`] makes the same selection through a [`PublicationSource`], with no store
//! open.
//!
//! The publication review reads each side through one seam ([`PublicationReviewSide`], D-W4YAE)
//! and fills owned rows ([`review_model`]). A reader view ([`ReaderReviewSide`]) reads a resolved
//! side; its compared parts are authored values ([`ReaderPartValue`], D-MCH70). An added, removed
//! or included doc reads its own side for its summary ([`doc_reviews`]). One JSON form
//! ([`review_json`]) writes the review for chat, the CLI and the adapter.

mod diff;
mod hidden;
mod references;
mod release;
mod report;
mod review_changes;
mod review_docs;
mod review_json;
mod review_labels;
mod review_limits;
pub mod review_model;
mod review_side;
mod select;
mod source;

pub use diff::{
    ChangedAttachment, ChangedDoc, ChangedFolder, ChangedOutsideDoc, DisclosureDiff,
    disclosure_diff,
};
pub use hidden::{HiddenList, HiddenPlanned, HiddenRow};
pub use rdy_schema::publication::Audience;
pub use references::{CheckStatus, Classification, ReferenceReport, ReferenceRow};
pub use report::{REPORT_BYTES, REPORT_ROWS, check, check_reserving, reference_summary};
pub use review_changes::{DIFF_MAX, EXCERPT_MAX, changed_doc_reviews, excerpt, short_diff};
pub use review_docs::{
    DECISION_TITLES_MAX, DocReviews, FIGURES_MAX, OUTLINE_MAX, doc_reviews, doc_summary, included,
};
pub use review_json::{PublicationReview, ReviewJson, review_json};
pub use review_labels::{
    DECISIONS_MAX, dropped_decisions, first_attachments, reference_labels, reference_review,
};
pub use review_limits::{
    ADAPTER_REVIEW_BYTES, CHANGE_BUDGET, CHANGES_MAX, ReviewProjection, change_shares, compact_len,
    disclosure_review,
};
pub use review_side::{
    BlockParts, DecisionFacts, PartName, PublicationReviewSide, ReaderPartValue, ReaderReviewSide,
    ReferenceLookup, ReviewRemovals, SelectionFacts, ShownBlock, TreeFacts, change_label,
    changed_parts, decision_title, text_fields,
};
#[allow(
    unused_imports,
    reason = "the baseline view of the sync client hashes its parts with the same encoder"
)]
pub(crate) use review_side::{CanonicalNumber, rfc8785};
pub use select::{
    AttachmentEntry, AttachmentVariant, DocEntry, IMPLICIT_ORG, Limits, ProfileFacts,
    PublicationWarning, Resolved, ResolvedProfile, SelectionEntry, ShownStatus,
    blob_reads_for_tests, check_stable_selector, limits, load_assets, load_inspection_assets,
    load_source_assets, profile_at, profile_facts, resolve, resolve_inspection, resolve_revision,
    resolve_source, selection_resolutions_for_tests,
};
#[cfg(feature = "test-support")]
pub use select::{
    LimitsGuard, check_snapshot_limits_for_tests, override_limits_for_tests, resolve_in_for_tests,
};
#[allow(
    unused_imports,
    reason = "the holder take calls it for every landing snapshot it takes"
)]
pub(crate) use select::{SnapshotBudget, check_snapshot_limits};
pub(crate) use select::{check_block_limits, limit};
pub use source::{PublicationSource, ReadPublicationSource};
