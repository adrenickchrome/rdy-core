//! Publications (D-48NYM, D-0VCQ5, D-KDFZ3): a publication profile, an ordinary versioned block,
//! selects whole docs and folders of one source snapshot. [`resolve`] turns a profile into that
//! bounded selection: the selected state, its induced graph, the selection and doc digests, the
//! attachments, and the check of every reference that leaves the selection. It produces no
//! artifact and authorizes no disclosure; output callers ask [`Resolved::require_output`].

mod diff;
mod references;
mod report;
mod select;

pub use diff::{ChangedAttachment, ChangedDoc, ChangedFolder, DisclosureDiff, disclosure_diff};
pub use rdy_schema::publication::Audience;
pub use references::{CheckStatus, Classification, ReferenceReport, ReferenceRow};
pub use report::{REPORT_BYTES, REPORT_ROWS, check};
#[allow(
    unused_imports,
    reason = "the holder take calls it for every landing snapshot it takes"
)]
pub(crate) use select::check_snapshot_limits;
pub use select::{
    AttachmentEntry, DocEntry, IMPLICIT_ORG, Limits, PublicationWarning, Resolved, ResolvedProfile,
    SelectionEntry, blob_reads_for_tests, limits, load_assets, resolve,
};
#[cfg(feature = "test-support")]
pub use select::{
    LimitsGuard, check_snapshot_limits_for_tests, override_limits_for_tests, resolve_in_for_tests,
};
pub(crate) use select::{check_block_limits, limit};
