//! Disclosure comparison (D-KDFZ3, D-0VCQ5, D-WDKWQ): what a candidate selection discloses that
//! a baseline selection of the same profile and audience did not. Both sides are [`Resolved`]
//! values the caller already holds; nothing here reads source history, looks up a previous
//! publication, or stores a baseline. Which baseline was last published is the hub's record.

use super::references::{Classification, ReferenceRow};
use super::select::{AttachmentEntry, DocEntry, Resolved, SelectionEntry};
use crate::error::*;
use rdy_model::{BlockId, Hash, tree};
use rdy_schema::publication::Audience;
use std::collections::{BTreeMap, BTreeSet};

/// A doc both sides select whose digest differs: a child, order, supporting block, asset, path
/// or redaction change can cause it without a new version of the doc block itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedDoc {
    pub id: BlockId,
    pub before_path: String,
    pub after_path: String,
    pub before_digest: Hash,
    pub after_digest: Hash,
}

/// A selected folder (selected itself or kept as an ancestor) whose own entry or path differs, or
/// that only one side selects. A side that does not select the folder has `None` for both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFolder {
    pub id: BlockId,
    pub before_version: Option<Hash>,
    pub after_version: Option<Hash>,
    pub before_path: Option<String>,
    pub after_path: Option<String>,
}

/// An attachment both sides hold whose blob, home doc or availability differs. Only ids, hashes
/// and booleans: never bytes, captions or source paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedAttachment {
    pub block: BlockId,
    pub before_home_doc: BlockId,
    pub after_home_doc: BlockId,
    pub before_blob: Hash,
    pub after_blob: Hash,
    pub before_available: bool,
    pub after_available: bool,
}

/// The comparison of two resolutions of one profile. Doc, folder and attachment arrays are
/// sorted by id; `new_references` by (source, location, target, classification), each row
/// carrying the positive occurrence delta, and `new_reference_count` is the sum of those deltas.
/// It holds no timestamp, actor, artifact, record id or authorization status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisclosureDiff {
    pub profile_id: Option<BlockId>,
    pub audience: Audience,
    pub before_profile_version: Option<Hash>,
    pub after_profile_version: Option<Hash>,
    pub before_selection_hash: Hash,
    pub after_selection_hash: Hash,
    pub profile_changed: bool,
    pub added_docs: Vec<DocEntry>,
    pub removed_docs: Vec<DocEntry>,
    pub changed_docs: Vec<ChangedDoc>,
    pub changed_folders: Vec<ChangedFolder>,
    pub added_attachments: Vec<AttachmentEntry>,
    pub removed_attachments: Vec<AttachmentEntry>,
    pub changed_attachments: Vec<ChangedAttachment>,
    pub new_references: Vec<ReferenceRow>,
    pub new_reference_count: u64,
}

/// One side's selected folders: each folder's own entry and its path in the selected tree.
fn folders(r: &Resolved) -> BTreeMap<&BlockId, (&SelectionEntry, Option<String>)> {
    r.entries
        .iter()
        .filter(|e| r.m.get(&e.id).is_some_and(|b| b.block_type == tree::FOLDER))
        .map(|e| (&e.id, (e, r.m.path_of(&e.id))))
        .collect()
}

type RefKey<'a> = (&'a BlockId, &'a str, &'a str, Classification);

fn ref_key(r: &ReferenceRow) -> RefKey<'_> {
    (&r.source, &r.location, &r.target, r.classification)
}

/// What `after` discloses relative to `before`. Both must resolve the same profile id (two
/// implicit profiles are the same) with the same audience; the profile revision may differ.
pub fn disclosure_diff(before: &Resolved, after: &Resolved) -> Result<DisclosureDiff, RdyError> {
    if before.profile.id != after.profile.id {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "Disclosure comparison requires the same publication profile.",
            "Choose a baseline that contains the candidate profile id.",
        ));
    }
    if before.profile.audience != after.profile.audience {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "Disclosure comparison requires the same audience.",
            "Choose a baseline with the same audience as the candidate profile.",
        ));
    }

    // docs, by id
    let b_docs: BTreeMap<&BlockId, &DocEntry> = before.docs.iter().map(|d| (&d.id, d)).collect();
    let a_docs: BTreeMap<&BlockId, &DocEntry> = after.docs.iter().map(|d| (&d.id, d)).collect();
    let added_docs = a_docs
        .iter()
        .filter(|(id, _)| !b_docs.contains_key(*id))
        .map(|(_, d)| (*d).clone())
        .collect();
    let removed_docs = b_docs
        .iter()
        .filter(|(id, _)| !a_docs.contains_key(*id))
        .map(|(_, d)| (*d).clone())
        .collect();
    let changed_docs = a_docs
        .iter()
        .filter_map(|(id, a)| {
            let b = b_docs.get(id)?;
            (a.digest != b.digest).then(|| ChangedDoc {
                id: (*id).clone(),
                before_path: b.path.clone(),
                after_path: a.path.clone(),
                before_digest: b.digest,
                after_digest: a.digest,
            })
        })
        .collect();

    // folders, by id: own entry and path
    let b_folders = folders(before);
    let a_folders = folders(after);
    let folder_ids: BTreeSet<&BlockId> =
        b_folders.keys().chain(a_folders.keys()).copied().collect();
    let changed_folders = folder_ids
        .into_iter()
        .filter_map(|id| {
            let b = b_folders.get(id);
            let a = a_folders.get(id);
            if let (Some((be, bp)), Some((ae, ap))) = (b, a)
                && be == ae
                && bp == ap
            {
                return None;
            }
            Some(ChangedFolder {
                id: id.clone(),
                before_version: b.map(|(e, _)| e.version),
                after_version: a.map(|(e, _)| e.version),
                before_path: b.and_then(|(_, p)| p.clone()),
                after_path: a.and_then(|(_, p)| p.clone()),
            })
        })
        .collect();

    // attachments, by block id
    let b_atts: BTreeMap<&BlockId, &AttachmentEntry> =
        before.attachments.iter().map(|a| (&a.block, a)).collect();
    let a_atts: BTreeMap<&BlockId, &AttachmentEntry> =
        after.attachments.iter().map(|a| (&a.block, a)).collect();
    let added_attachments = a_atts
        .iter()
        .filter(|(id, _)| !b_atts.contains_key(*id))
        .map(|(_, a)| (*a).clone())
        .collect();
    let removed_attachments = b_atts
        .iter()
        .filter(|(id, _)| !a_atts.contains_key(*id))
        .map(|(_, a)| (*a).clone())
        .collect();
    let changed_attachments = a_atts
        .iter()
        .filter_map(|(id, a)| {
            let b = b_atts.get(id)?;
            (a.blob != b.blob || a.home_doc != b.home_doc || a.available != b.available).then(
                || ChangedAttachment {
                    block: (*id).clone(),
                    before_home_doc: b.home_doc.clone(),
                    after_home_doc: a.home_doc.clone(),
                    before_blob: b.blob,
                    after_blob: a.blob,
                    before_available: b.available,
                    after_available: a.available,
                },
            )
        })
        .collect();

    // references: positive occurrence deltas per (source, location, target, classification)
    let b_refs: BTreeMap<RefKey<'_>, u64> = before
        .references
        .rows
        .iter()
        .map(|r| (ref_key(r), r.occurrences))
        .collect();
    let mut new_references: Vec<ReferenceRow> = after
        .references
        .rows
        .iter()
        .filter_map(|r| {
            let was = b_refs.get(&ref_key(r)).copied().unwrap_or(0);
            (r.occurrences > was).then(|| ReferenceRow {
                occurrences: r.occurrences - was,
                ..r.clone()
            })
        })
        .collect();
    new_references.sort_by(|x, y| ref_key(x).cmp(&ref_key(y)));
    let new_reference_count = new_references
        .iter()
        .fold(0u64, |n, r| n.saturating_add(r.occurrences));

    Ok(DisclosureDiff {
        profile_id: after.profile.id.clone(),
        audience: after.profile.audience,
        before_profile_version: before.profile.version,
        after_profile_version: after.profile.version,
        before_selection_hash: before.selection_hash,
        after_selection_hash: after.selection_hash,
        profile_changed: before.profile.version != after.profile.version,
        added_docs,
        removed_docs,
        changed_docs,
        changed_folders,
        added_attachments,
        removed_attachments,
        changed_attachments,
        new_references,
        new_reference_count,
    })
}
