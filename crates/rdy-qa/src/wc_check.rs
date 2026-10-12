//! Spec §15.3 invariant 7: a working copy checked out from a view holds what `rdy export` of
//! that view writes, less what lives only in the store (the metadata split: supporting blocks
//! and their subtrees, version pins, rdy's own fields, store-only links). Only people edit the
//! working copy (FW-V2), so the check runs on a checkout that nobody has touched since. This
//! module is the QA gate's; the product's `rdy check` lint of a working copy is
//! `rdy_engine::wc_check`.
//!
//! Invariant 7 reads: the working copy equals the export of its view *less the metadata split*.
//! A byte-for-byte compare with the whole export fails by design, because the export holds
//! supporting blocks, pins and rdy's own fields that the checkout never shows. The orchestrator
//! accepted this reading on 2026-10-12 (task 2 ruling).
//!
//! The split is applied here to the parsed export, and not by calling the checkout's projection:
//! a projection checked against itself proves nothing. The lists of what is store-only are this
//! module's own, written from the spec's "The metadata split" (the working-copy page) and the
//! M2f-b plan, and not taken from the product's `STORE_ONLY_*` constants. A test in
//! `invariants_wc.rs` pins the product's constants to these, so a field or link kind the product
//! wrongly marks store-only fails there instead of vanishing on both sides of the comparison.
use crate::daemon::Served;
use crate::invariants::Violation;
use rdy_schema::{Pack, PackSet};
use rdy_store::Store;
use serde_yaml_ng::{Mapping, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Fields rdy sets and keeps in the store: a working-copy file never shows them.
pub const STORE_ONLY_FIELDS: &[&str] = &["key_auto"];

/// Link kinds that live only in the store (justification links).
pub const STORE_ONLY_LINK_KINDS: &[&str] = &["decided_by", "implements"];

/// A link of any other kind whose target has one of these types is store-only too.
pub const STORE_ONLY_TARGET_TYPES: &[&str] = &["decision", "question"];

/// The supporting block types of the standard pack: never written into the tree, with their
/// subtrees. The spec also lists reviews, tensions and intents as supporting; those are store
/// records and not block types, so no export entry has them.
pub const SUPPORTING_TYPES: &[&str] = &["decision", "question", "rationale"];

fn violation(rule: &'static str, detail: impl Into<String>) -> Violation {
    Violation {
        rule,
        block: None,
        detail: detail.into(),
    }
}

/// Every file under `root` as (path relative to `root` with `/`, bytes), skipping the entries
/// the working copy keeps for itself: a dot entry anywhere on the path, a `.theirs.yaml` file,
/// and (when `skip_blobs`) the top-level `_blobs` directory.
fn files(root: &Path, skip_blobs: bool) -> BTreeMap<String, Vec<u8>> {
    fn walk(
        dir: &Path,
        rel: &str,
        top: bool,
        skip_blobs: bool,
        out: &mut BTreeMap<String, Vec<u8>>,
    ) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.ends_with(".theirs.yaml") {
                continue;
            }
            if top && skip_blobs && name == "_blobs" {
                continue;
            }
            let path = entry.path();
            let rel = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if path.is_dir() {
                walk(&path, &rel, false, skip_blobs, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.insert(rel, bytes);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, "", true, skip_blobs, &mut out);
    out
}

/// The content hashes of the files directly under `dir`, as a set (a file's name differs
/// between an export and a checkout, by its extension, so the content is what is compared).
fn blob_hashes(dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut out: Vec<String> = read
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| std::fs::read(e.path()).ok())
        .map(|bytes| Store::blob_hash(&bytes).to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// The type of the block an id names: the type of its entry in the export, else the type whose
/// prefix the id has.
fn type_of(id: &str, types: &BTreeMap<String, String>, pack: &Pack) -> Option<String> {
    types.get(id).cloned().or_else(|| {
        id.split_once('-')
            .and_then(|(prefix, _)| pack.type_for_prefix(prefix))
            .map(str::to_string)
    })
}

/// Takes the store-only parts (the lists above) out of one exported entry. `None` for an entry the working copy
/// does not show: a supporting block, or anything under one.
fn split_entry(
    mut e: Value,
    types: &BTreeMap<String, String>,
    pack: &Pack,
    hidden: &mut BTreeSet<String>,
) -> Option<Value> {
    let id = text(&e, "id")?;
    let parent_hidden = text(&e, "parent").is_some_and(|p| hidden.contains(&p));
    let supporting = text(&e, "type").is_some_and(|t| SUPPORTING_TYPES.contains(&t.as_str()));
    if parent_hidden || supporting {
        hidden.insert(id);
        return None;
    }
    let map = e.as_mapping_mut()?;
    map.remove("pins");
    if let Some(Value::Mapping(fields)) = map.get_mut("fields") {
        for f in STORE_ONLY_FIELDS {
            fields.remove(*f);
        }
    }
    if let Some(Value::Mapping(links)) = map.get_mut("links") {
        let mut kept = Mapping::new();
        for (kind, list) in links.iter() {
            let kind_name = kind.as_str().unwrap_or_default();
            if STORE_ONLY_LINK_KINDS.contains(&kind_name) {
                continue;
            }
            let Value::Sequence(list) = list else {
                kept.insert(kind.clone(), list.clone());
                continue;
            };
            let shown: Vec<Value> = list
                .iter()
                .filter(|l| {
                    let target = text(l, "target").unwrap_or_default();
                    !type_of(&target, types, pack)
                        .is_some_and(|t| STORE_ONLY_TARGET_TYPES.contains(&t.as_str()))
                })
                .cloned()
                .collect();
            if !shown.is_empty() {
                kept.insert(kind.clone(), Value::Sequence(shown));
            }
        }
        if kept.is_empty() {
            map.remove("links");
        } else {
            map.insert("links".into(), Value::Mapping(kept));
        }
    }
    Some(e)
}

/// What the working copy shows of one exported file, or `None` when it shows nothing of it.
fn shown(file: &str, types: &BTreeMap<String, String>, pack: &Pack) -> Option<String> {
    let value: Value = serde_yaml_ng::from_str(file).ok()?;
    let mut hidden = BTreeSet::new();
    match value {
        Value::Sequence(list) => {
            let kept: Vec<Value> = list
                .into_iter()
                .filter_map(|e| split_entry(e, types, pack, &mut hidden))
                .collect();
            (!kept.is_empty()).then(|| serde_yaml_ng::to_string(&kept).expect("yaml"))
        }
        one => split_entry(one, types, pack, &mut hidden)
            .map(|e| serde_yaml_ng::to_string(&e).expect("yaml")),
    }
}

/// Runs `rdy export --scope view:<view> --out <tmp>` as the person and compares it with
/// `checkout`: every file under `<tmp>/docs/`, less the metadata split, with the same path
/// under `checkout` (bytes equal; rule `wc_tree`), and the content hashes under `<tmp>/blobs/` with those under
/// `checkout/_blobs/` (rule `wc_blobs`). A failed export is one `wc_export` violation.
pub fn matches_export(s: &Served, view: &str, checkout: &Path) -> Result<(), Vec<Violation>> {
    let tmp = tempfile::tempdir().expect("a temp dir for the export");
    let out = tmp.path().join("export");
    let scope = format!("view:{view}");
    let o = s.rdy(&["export", "--scope", &scope, "--out", &out.to_string_lossy()]);
    if !o.status.success() {
        return Err(vec![violation(
            "wc_export",
            format!(
                "rdy export failed: {}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
        )]);
    }
    let mut found = vec![];
    let pack = PackSet::with_builtin()
        .resolve("rdy-standard")
        .expect("the standard pack resolves");
    let raw = files(&out.join("docs"), false);
    let mut types = BTreeMap::new();
    for bytes in raw.values() {
        let entries = match serde_yaml_ng::from_slice::<Value>(bytes) {
            Ok(Value::Sequence(list)) => list,
            Ok(one) => vec![one],
            Err(_) => vec![],
        };
        for e in entries {
            if let (Some(id), Some(t)) = (text(&e, "id"), text(&e, "type")) {
                types.insert(id, t);
            }
        }
    }
    let exported: BTreeMap<String, Vec<u8>> = raw
        .iter()
        .filter_map(|(path, bytes)| {
            let file = String::from_utf8_lossy(bytes);
            shown(&file, &types, &pack).map(|t| (path.clone(), t.into_bytes()))
        })
        .collect();
    let kept = files(checkout, true);
    for (path, bytes) in &exported {
        match kept.get(path) {
            None => found.push(violation("wc_tree", format!("{path}: only in the export"))),
            Some(mine) if mine != bytes => found.push(violation(
                "wc_tree",
                format!("{path}: the working copy differs from the export"),
            )),
            Some(_) => {}
        }
    }
    for path in kept.keys().filter(|p| !exported.contains_key(*p)) {
        found.push(violation(
            "wc_tree",
            format!("{path}: only in the working copy"),
        ));
    }
    let (theirs, mine) = (
        blob_hashes(&out.join("blobs")),
        blob_hashes(&checkout.join("_blobs")),
    );
    for hash in theirs.iter().filter(|h| !mine.contains(h)) {
        found.push(violation(
            "wc_blobs",
            format!("blob {hash}: only in the export"),
        ));
    }
    for hash in mine.iter().filter(|h| !theirs.contains(h)) {
        found.push(violation(
            "wc_blobs",
            format!("blob {hash}: only in the working copy"),
        ));
    }
    if found.is_empty() { Ok(()) } else { Err(found) }
}
