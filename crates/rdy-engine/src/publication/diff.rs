//! Disclosure comparison (D-KDFZ3, D-0VCQ5, D-WDKWQ): what a candidate selection discloses that
//! a baseline selection of the same profile and audience did not. Both sides are [`Resolved`]
//! values the caller already holds; nothing here reads source history, looks up a previous
//! publication, or stores a baseline. Which baseline was last published is the hub's record.
//!
//! Names (D-Z8Q4N, D-3M81W): a doc or folder version that either side holds removed shows its
//! full id in place of its name on both sides, matched by that version's hash, never by id
//! alone; a version no removal names keeps its name. Digests and selection hashes stay the
//! recorded facts.

use super::references::{Classification, ReferenceRow};
use super::select::{
    AttachmentEntry, DocEntry, RequestSet, Resolved, SelectionEntry, invalid_tree, name_replaced,
};
use crate::error::*;
use crate::materialize::Materialized;
use rdy_model::redaction::BlockResidue;
use rdy_model::{BlockId, Hash, Op, tree};
use rdy_schema::publication::Audience;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

/// A doc both sides select whose digest differs: a child, order, supporting block, asset, path
/// or redaction change can cause it without a new version of the doc block itself. `redacted`
/// is true when either side's path had a name replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedDoc {
    pub id: BlockId,
    pub before_path: String,
    pub after_path: String,
    pub before_digest: Hash,
    pub after_digest: Hash,
    pub redacted: bool,
}

/// A selected folder (selected itself or kept as an ancestor) whose own entry or path differs, or
/// that only one side selects. A side that does not select the folder has `None` for both.
/// `redacted` is true when a side that selects it had a name in its path replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFolder {
    pub id: BlockId,
    pub before_version: Option<Hash>,
    pub after_version: Option<Hash>,
    pub before_path: Option<String>,
    pub after_path: Option<String>,
    pub redacted: bool,
}

/// A retained block that is no doc or folder and sits in no selected doc (D-P3YD5: a decision at
/// the project root or in a folder, or a block under one), whose version differs between the
/// sides or that only one side retains. A side that does not retain it has None. `redacted` is
/// true when a side that retains it holds a removed version of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedOutsideDoc {
    pub id: BlockId,
    pub before_version: Option<Hash>,
    pub after_version: Option<Hash>,
    pub redacted: bool,
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

/// The comparison of two resolutions of one profile. Doc, folder, outside-doc and attachment
/// arrays are sorted by id; `new_references` by (source, location, target, classification),
/// each row carrying the positive occurrence delta, and `new_reference_count` is the sum of
/// those deltas.
/// `redactions` is the sorted union of the removal requests both resolutions apply. It holds no
/// timestamp, actor, artifact, record id or authorization status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisclosureDiff {
    pub profile_id: Option<BlockId>,
    pub audience: Audience,
    pub before_profile_version: Option<Hash>,
    pub after_profile_version: Option<Hash>,
    pub before_selection_hash: Hash,
    pub after_selection_hash: Hash,
    pub before_selection_identity: Hash,
    pub after_selection_identity: Hash,
    pub redactions: Vec<Hash>,
    pub profile_changed: bool,
    pub added_docs: Vec<DocEntry>,
    pub removed_docs: Vec<DocEntry>,
    pub changed_docs: Vec<ChangedDoc>,
    pub changed_folders: Vec<ChangedFolder>,
    pub changed_outside_docs: Vec<ChangedOutsideDoc>,
    pub added_attachments: Vec<AttachmentEntry>,
    pub removed_attachments: Vec<AttachmentEntry>,
    pub changed_attachments: Vec<ChangedAttachment>,
    pub new_references: Vec<ReferenceRow>,
    pub new_reference_count: u64,
}

/// The removed versions either side holds, each with the request that removed it.
fn removed_versions(sides: [&Resolved; 2]) -> BTreeMap<Hash, Hash> {
    sides
        .iter()
        .flat_map(|r| r.entries.iter())
        .filter_map(|e| Some((e.version, e.redaction?)))
        .collect()
}

/// One side's docs and folders as the comparison shows them: each one's path, rebuilt by the
/// selected tree's own path rules after every doc and folder whose version is in `removed` is
/// read as that version's stub (so it is named by its id), and whether a name in that path was
/// replaced. A side that already holds a stub keeps it; only a supplied pre-removal side needs
/// the projection.
fn shown(
    r: &Resolved,
    removed: &BTreeMap<Hash, Hash>,
) -> Result<BTreeMap<BlockId, (String, bool)>, RdyError> {
    let ops: Vec<Op> =
        r.m.iter()
            .filter(|b| tree::is_tree_type(&b.block_type) && !b.is_redacted())
            .filter_map(|b| {
                let version = r.m.version_of(&b.id)?;
                let request = removed.get(&version)?;
                Some(Op::Update {
                    base: version,
                    block: BlockResidue::of(b).stub(*request),
                })
            })
            .collect();
    let m: Cow<'_, Materialized> = if ops.is_empty() {
        Cow::Borrowed(&r.m)
    } else {
        Cow::Owned(r.m.with(&ops))
    };
    let mut out = BTreeMap::new();
    for b in m.iter().filter(|b| tree::is_tree_type(&b.block_type)) {
        let path = m.path_of(&b.id).ok_or_else(invalid_tree)?;
        out.insert(b.id.clone(), (path, name_replaced(&m, &b.id)));
    }
    Ok(out)
}

/// `d` with its path and flag as `names` shows them.
fn shown_doc(
    d: &DocEntry,
    names: &BTreeMap<BlockId, (String, bool)>,
) -> Result<DocEntry, RdyError> {
    let (path, redacted) = names.get(&d.id).ok_or_else(invalid_tree)?;
    Ok(DocEntry {
        id: d.id.clone(),
        path: path.clone(),
        digest: d.digest,
        redacted: *redacted,
    })
}

/// One side's selected folders: each folder's own entry, its shown path and its flag.
fn folders<'r>(
    r: &'r Resolved,
    names: &BTreeMap<BlockId, (String, bool)>,
) -> Result<BTreeMap<&'r BlockId, (&'r SelectionEntry, String, bool)>, RdyError> {
    let mut out = BTreeMap::new();
    for e in r
        .entries
        .iter()
        .filter(|e| r.m.get(&e.id).is_some_and(|b| b.block_type == tree::FOLDER))
    {
        let (path, redacted) = names.get(&e.id).ok_or_else(invalid_tree)?;
        out.insert(&e.id, (e, path.clone(), *redacted));
    }
    Ok(out)
}

/// One side's retained blocks outside a doc: each entry whose block is no doc or folder and has
/// no doc in the selected state (D-P3YD5), with whether that side holds a removed version of it.
fn outside_docs(r: &Resolved) -> BTreeMap<&BlockId, (Hash, bool)> {
    r.entries
        .iter()
        .filter_map(|e| {
            let b = r.m.get(&e.id)?;
            (!tree::is_tree_type(&b.block_type) && r.m.doc_of(&e.id).is_none())
                .then(|| (&e.id, (e.version, b.is_redacted())))
        })
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

    // names: the removed versions of both sides, then each side's paths with those names
    // replaced, before any row is built
    let removed = removed_versions([before, after]);
    let b_names = shown(before, &removed)?;
    let a_names = shown(after, &removed)?;

    // docs, by id
    let b_docs: BTreeMap<&BlockId, &DocEntry> = before.docs.iter().map(|d| (&d.id, d)).collect();
    let a_docs: BTreeMap<&BlockId, &DocEntry> = after.docs.iter().map(|d| (&d.id, d)).collect();
    let mut added_docs = vec![];
    for (id, d) in &a_docs {
        if !b_docs.contains_key(*id) {
            added_docs.push(shown_doc(d, &a_names)?);
        }
    }
    let mut removed_docs = vec![];
    for (id, d) in &b_docs {
        if !a_docs.contains_key(*id) {
            removed_docs.push(shown_doc(d, &b_names)?);
        }
    }
    let mut changed_docs = vec![];
    for (id, a) in &a_docs {
        let Some(b) = b_docs.get(id) else { continue };
        if a.digest == b.digest {
            continue;
        }
        let (b, a) = (shown_doc(b, &b_names)?, shown_doc(a, &a_names)?);
        changed_docs.push(ChangedDoc {
            id: (*id).clone(),
            before_path: b.path,
            after_path: a.path,
            before_digest: b.digest,
            after_digest: a.digest,
            redacted: b.redacted || a.redacted,
        });
    }

    // folders, by id: own entry and shown path
    let b_folders = folders(before, &b_names)?;
    let a_folders = folders(after, &a_names)?;
    let folder_ids: BTreeSet<&BlockId> =
        b_folders.keys().chain(a_folders.keys()).copied().collect();
    let changed_folders = folder_ids
        .into_iter()
        .filter_map(|id| {
            let b = b_folders.get(id);
            let a = a_folders.get(id);
            if let (Some((be, bp, _)), Some((ae, ap, _))) = (b, a)
                && be == ae
                && bp == ap
            {
                return None;
            }
            Some(ChangedFolder {
                id: id.clone(),
                before_version: b.map(|(e, _, _)| e.version),
                after_version: a.map(|(e, _, _)| e.version),
                before_path: b.map(|(_, p, _)| p.clone()),
                after_path: a.map(|(_, p, _)| p.clone()),
                redacted: b.is_some_and(|(_, _, r)| *r) || a.is_some_and(|(_, _, r)| *r),
            })
        })
        .collect();

    // blocks outside a doc, by id: own version
    let b_outside = outside_docs(before);
    let a_outside = outside_docs(after);
    let outside_ids: BTreeSet<&BlockId> =
        b_outside.keys().chain(a_outside.keys()).copied().collect();
    let changed_outside_docs = outside_ids
        .into_iter()
        .filter_map(|id| {
            let b = b_outside.get(id);
            let a = a_outside.get(id);
            if let (Some((bv, _)), Some((av, _))) = (b, a)
                && bv == av
            {
                return None;
            }
            Some(ChangedOutsideDoc {
                id: id.clone(),
                before_version: b.map(|(v, _)| *v),
                after_version: a.map(|(v, _)| *v),
                redacted: b.is_some_and(|(_, r)| *r) || a.is_some_and(|(_, r)| *r),
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

    // the removal requests both resolutions apply, bounded as one list
    let mut requests = RequestSet::default();
    for request in before.redactions.iter().chain(&after.redactions) {
        requests.insert(*request)?;
    }

    Ok(DisclosureDiff {
        profile_id: after.profile.id.clone(),
        audience: after.profile.audience,
        before_profile_version: before.profile.version,
        after_profile_version: after.profile.version,
        before_selection_hash: before.selection_hash,
        after_selection_hash: after.selection_hash,
        before_selection_identity: before.selection_identity,
        after_selection_identity: after.selection_identity,
        redactions: requests.into_sorted(),
        profile_changed: before.profile.version != after.profile.version,
        added_docs,
        removed_docs,
        changed_docs,
        changed_folders,
        changed_outside_docs,
        added_attachments,
        removed_attachments,
        changed_attachments,
        new_references,
        new_reference_count,
    })
}
