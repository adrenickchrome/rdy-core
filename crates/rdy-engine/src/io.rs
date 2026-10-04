use crate::Materialized;
use crate::error::*;
use crate::query::field_json;
use crate::tree_io::{self, FOLDER_FILE};
use crate::write::{LinkInput, OpInput, WriteRequest, WriteResult};
use crate::{Rdy, Scope, Session};
use rdy_model::{Block, BlockId, Change, FieldValue, Hash, Link, Op, tree};
use rdy_schema::{has_errors, validate_block};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Serialize, Deserialize)]
pub(crate) struct LinkYaml {
    target: BlockId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    part: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct BlockYaml {
    pub(crate) id: BlockId,
    #[serde(rename = "type")]
    pub(crate) block_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent: Option<BlockId>,
    pub(crate) order: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) fields: BTreeMap<String, serde_yaml_ng::Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) body: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) links: BTreeMap<String, Vec<LinkYaml>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) pins: BTreeMap<BlockId, String>,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: u32,
    pack: String,
    scope: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExportSummary {
    pub docs: usize,
    pub folders: usize,
    pub blocks: usize,
    pub blobs: usize,
    pub warnings: Vec<String>,
}

/// Writes the folders and docs under `parent` into `dir`: a folder is a directory holding
/// `_folder.yaml`, a doc is `<name>.yaml`. Neither file records the block's name or parent.
fn export_level(
    m: &Materialized,
    parent: Option<&BlockId>,
    level: usize,
    dir: &Path,
    summary: &mut ExportSummary,
    blobs: &mut BTreeMap<Hash, BlockId>,
) -> Result<(), RdyError> {
    // `level` folders are above this one; a damaged store may nest deeper than writes allow,
    // and what lies below the limit is left out (the caller reports the missing blocks)
    if level > tree::MAX_FOLDER_DEPTH {
        return Ok(());
    }
    let names = m.names_under(parent);
    let mut used: BTreeMap<String, &BlockId> = BTreeMap::new();
    for b in m.children(parent) {
        let Some(name) = names.get(&b.id) else {
            continue;
        };
        if let Some(other) = used.insert(name.to_ascii_lowercase(), &b.id) {
            return Err(RdyError::new(
                E_PATH_CLASH,
                format!(
                    "{other} and {} are both named {name:?} (ignoring case) under the same parent, so they cannot be exported as files",
                    b.id
                ),
                "rename one of them, then export again",
            ));
        }
        let mut head = to_yaml_struct(b);
        head.parent = None;
        head.fields.remove("name");
        summary.blocks += 1;
        if b.block_type == tree::FOLDER {
            let sub = dir.join(name);
            std::fs::create_dir_all(&sub).map_err(io_err)?;
            std::fs::write(
                sub.join(FOLDER_FILE),
                serde_yaml_ng::to_string(&head).expect("folder yaml"),
            )
            .map_err(io_err)?;
            summary.folders += 1;
            export_level(m, Some(&b.id), level + 1, &sub, summary, blobs)?;
            continue;
        }
        if b.field_str("name").is_none() {
            summary.warnings.push(format!(
                "doc {} has no stored name: exported as {name}.yaml; importing it stores that name, so its version hash changes once",
                b.id
            ));
        }
        let mut list = vec![head];
        let mut stack = m.children(Some(&b.id));
        stack.reverse();
        while let Some(c) = stack.pop() {
            list.push(to_yaml_struct(c));
            if let Some(h) = c.field_str("blob").and_then(|s| s.parse::<Hash>().ok()) {
                blobs.entry(h).or_insert_with(|| c.id.clone());
            }
            let mut kids = m.children(Some(&c.id));
            kids.reverse();
            stack.extend(kids);
        }
        summary.blocks += list.len() - 1;
        summary.docs += 1;
        std::fs::create_dir_all(dir).map_err(io_err)?;
        std::fs::write(
            dir.join(format!("{name}.yaml")),
            serde_yaml_ng::to_string(&list).expect("doc yaml"),
        )
        .map_err(io_err)?;
    }
    Ok(())
}

/// The M1 flat layout: `docs/*.yaml`, one list of blocks per doc.
fn read_format_1(dir: &Path) -> Result<Vec<Block>, RdyError> {
    let mut files: Vec<_> = std::fs::read_dir(dir.join("docs"))
        .map_err(io_err)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| !is_hidden(p) && p.extension().is_some_and(|x| x == "yaml"))
        .collect();
    files.sort();
    // A format-1 export is flat: a directory under docs/ is a format-2 tree, and reading it as
    // format 1 would silently import nothing.
    if let Some(d) = std::fs::read_dir(dir.join("docs"))
        .map_err(io_err)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !is_hidden(p))
        .min()
    {
        return Err(RdyError::new(
            E_FILE_FORMAT,
            format!(
                "{}: a format-1 export is flat, but docs/ holds a directory (a format-2 tree?)",
                d.display()
            ),
            "set `format: 2` in rdy-export.yaml, or re-export",
        ));
    }
    let mut blocks: Vec<Block> = vec![];
    for f in files {
        refuse_symlink(&f)?;
        let text = std::fs::read_to_string(&f).map_err(io_err)?;
        let list: Vec<BlockYaml> = serde_yaml_ng::from_str(&text).map_err(|e| {
            RdyError::new(
                E_BAD_INPUT,
                format!("{}: {e}", f.display()),
                "fix or re-export the file",
            )
        })?;
        for y in list {
            blocks.push(from_yaml_struct(y)?);
        }
    }
    Ok(blocks)
}

/// A format-1 export from M1 has docs without a name: give each the name readers already use,
/// de-duplicated against names under the same parent in the import and in the view.
fn assign_missing_names(blocks: &mut [Block], existing: &Materialized) {
    let mut taken: BTreeMap<Option<BlockId>, BTreeSet<String>> = BTreeMap::new();
    for b in blocks.iter().filter(|b| tree::is_tree_type(&b.block_type)) {
        let set = taken.entry(b.parent.clone()).or_insert_with(|| {
            existing
                .names_under(b.parent.as_ref())
                .into_values()
                .map(|n| n.to_ascii_lowercase())
                .collect()
        });
        if let Some(n) = b.field_str("name") {
            set.insert(n.to_ascii_lowercase());
        }
    }
    let mut nameless: Vec<usize> = (0..blocks.len())
        .filter(|&i| blocks[i].block_type == tree::DOC && blocks[i].field_str("name").is_none())
        .collect();
    nameless.sort_by(|&a, &b| blocks[a].id.cmp(&blocks[b].id));
    for i in nameless {
        let set = taken.entry(blocks[i].parent.clone()).or_default();
        let n = tree::dedupe(
            &tree::default_name(blocks[i].field_str("title"), &blocks[i].id),
            set,
        );
        set.insert(n.to_ascii_lowercase());
        blocks[i].fields.insert("name".into(), FieldValue::Str(n));
    }
}

/// Refuses an import whose docs or folders share a name (ignoring case) with each other or with
/// what the view already holds under the same parent: export would overwrite one with the other.
fn check_import_names(blocks: &[Block], existing: &Materialized) -> Result<(), RdyError> {
    let mut seen: BTreeMap<(Option<BlockId>, String), BlockId> = BTreeMap::new();
    for b in blocks.iter().filter(|b| tree::is_tree_type(&b.block_type)) {
        let Some(name) = b.field_str("name") else {
            continue;
        };
        let key = (b.parent.clone(), name.to_ascii_lowercase());
        let mut other = seen.insert(key.clone(), b.id.clone());
        if other.is_none() {
            other = existing
                .named(b.parent.as_ref(), &key.1)
                .into_iter()
                .find(|id| *id != b.id);
        }
        if let Some(other) = other {
            return Err(RdyError::new(
                E_PATH_CLASH,
                format!(
                    "{} and {other} are both named {name:?} (ignoring case) under the same parent",
                    b.id
                ),
                "rename one of them in the export files, or import into a fresh view",
            ));
        }
    }
    Ok(())
}

fn io_err(e: std::io::Error) -> RdyError {
    RdyError::new(E_IO, e.to_string(), "check the path and permissions")
}

pub(crate) fn to_yaml_struct(b: &Block) -> BlockYaml {
    BlockYaml {
        id: b.id.clone(),
        block_type: b.block_type.clone(),
        parent: b.parent.clone(),
        order: b.order.clone(),
        fields: b
            .fields
            .iter()
            .map(|(k, v)| (k.clone(), v.to_yaml()))
            .collect(),
        body: b.body.clone(),
        links: b
            .links
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.iter()
                        .map(|l| LinkYaml {
                            target: l.target.clone(),
                            part: l.part.clone(),
                        })
                        .collect(),
                )
            })
            .collect(),
        pins: b
            .pins
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect(),
    }
}

pub(crate) fn from_yaml_struct(y: BlockYaml) -> Result<Block, RdyError> {
    let bad = |m: String| RdyError::new(E_BAD_INPUT, m, "fix the YAML and retry");
    Ok(Block {
        id: y.id,
        block_type: y.block_type,
        parent: y.parent,
        order: y.order,
        fields: y
            .fields
            .iter()
            .map(|(k, v)| {
                Ok((
                    k.clone(),
                    FieldValue::from_yaml(v).map_err(|e| bad(format!("field {k}: {e}")))?,
                ))
            })
            .collect::<Result<_, RdyError>>()?,
        body: y.body,
        links: y
            .links
            .into_iter()
            .map(|(k, v)| {
                (
                    k,
                    v.into_iter()
                        .map(|l| Link {
                            target: l.target,
                            part: l.part,
                        })
                        .collect(),
                )
            })
            .collect(),
        pins: y
            .pins
            .into_iter()
            .map(|(k, v)| Ok((k, v.parse::<Hash>().map_err(|e| bad(e.to_string()))?)))
            .collect::<Result<_, RdyError>>()?,
    })
}

/// Dotfiles (e.g. `.DS_Store`) are never part of an export.
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

/// sym1: import never follows a symbolic link out of the export.
fn refuse_symlink(path: &Path) -> Result<(), RdyError> {
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(RdyError::new(
            E_FILE_FORMAT,
            format!(
                "{} is a symbolic link; rdy does not follow links in an export",
                path.display()
            ),
            "replace the link with the file it points to",
        ));
    }
    Ok(())
}

pub fn block_to_yaml(b: &Block) -> String {
    serde_yaml_ng::to_string(&to_yaml_struct(b)).expect("block yaml")
}

pub fn block_from_yaml(text: &str) -> Result<Block, RdyError> {
    let y: BlockYaml = serde_yaml_ng::from_str(text)
        .map_err(|e| RdyError::new(E_BAD_INPUT, e.to_string(), "fix the YAML and retry"))?;
    from_yaml_struct(y)
}

impl Rdy {
    pub fn export(&mut self, scope: &Scope, out: &Path) -> Result<ExportSummary, RdyError> {
        if out.exists() && std::fs::read_dir(out).map_err(io_err)?.next().is_some() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("{} is not empty", out.display()),
                "choose an empty directory (or one that does not exist yet) so no stale files mix into the export",
            ));
        }
        let name = out.file_name().ok_or_else(|| {
            RdyError::new(
                E_BAD_INPUT,
                format!("{} does not name a directory", out.display()),
                "pass a directory path such as ./export",
            )
        })?;
        let parent = out
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).map_err(io_err)?;
        // everything is written to a staging directory beside `out` and renamed into place at
        // the end: a failure while writing leaves nothing behind; if the final rename itself
        // fails, the staging directory is cleared by the next export
        let staging = parent.join(format!(".{}.rdy-export-partial", name.to_string_lossy()));
        if staging.exists() {
            std::fs::remove_dir_all(&staging).map_err(io_err)?;
        }
        let summary = match self.write_export(scope, &staging) {
            Ok(summary) => summary,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(e);
            }
        };
        // POSIX rename atomically replaces an empty directory
        std::fs::rename(&staging, out).map_err(io_err)?;
        Ok(summary)
    }

    fn write_export(&mut self, scope: &Scope, out: &Path) -> Result<ExportSummary, RdyError> {
        let m = self.materialized(scope)?.clone();
        std::fs::create_dir_all(out.join("docs")).map_err(io_err)?;
        std::fs::create_dir_all(out.join("blobs")).map_err(io_err)?;
        let mut summary = ExportSummary {
            docs: 0,
            folders: 0,
            blocks: 0,
            blobs: 0,
            warnings: vec![],
        };
        // each blob, with the first figure that names it
        let mut blobs = BTreeMap::new();
        export_level(&m, None, 0, &out.join("docs"), &mut summary, &mut blobs)?;
        if summary.blocks != m.blocks.len() {
            let missing = m.blocks.len() - summary.blocks.min(m.blocks.len());
            return Err(RdyError::new(
                E_IO,
                format!(
                    "{missing} blocks are not inside any folder or doc reachable from the top level (or lie more than {} folders deep), so they cannot be exported",
                    tree::MAX_FOLDER_DEPTH
                ),
                "run rdy fsck; move or delete those blocks, then export again",
            ));
        }
        for (h, figure) in &blobs {
            let not_found = || {
                RdyError::new(
                    E_NOT_FOUND,
                    format!("{figure} names blob {h}, which is not a blob in the store"),
                    format!(
                        "attach the asset again (rdy_asset) and point {figure} at it, or delete {figure}"
                    ),
                )
            };
            if !self.store.has_blob(h)? {
                return Err(not_found());
            }
            let bytes = self.store.get_blob(h).map_err(|e| match e {
                rdy_store::StoreError::NotFound(_) => not_found(),
                other => other.into(),
            })?;
            std::fs::write(out.join("blobs").join(h.to_string()), bytes).map_err(io_err)?;
            summary.blobs += 1;
        }
        let scope_name = match scope {
            Scope::Canon => "canon".to_string(),
            Scope::View(v) if v.contains(':') => format!("view:{v}"),
            Scope::View(v) => v.clone(),
            Scope::At(h) => format!("landing:{h}"),
        };
        let manifest = Manifest {
            format: 2,
            pack: self.pack.name.clone(),
            scope: scope_name,
        };
        std::fs::write(
            out.join("rdy-export.yaml"),
            serde_yaml_ng::to_string(&manifest).expect("manifest"),
        )
        .map_err(io_err)?;
        Ok(summary)
    }

    pub fn import(&mut self, s: &Session, view: &str, dir: &Path) -> Result<WriteResult, RdyError> {
        if !s.role.can_land() {
            return Err(RdyError::new(
                E_PERMISSION,
                "import can carry accepted decisions, so it needs an approver",
                "run the import as an approver session",
            ));
        }
        crate::write::writable_view(view)?;
        let manifest_path = dir.join("rdy-export.yaml");
        let manifest_text = std::fs::read_to_string(&manifest_path).map_err(|e| {
            RdyError::new(
                E_BAD_INPUT,
                format!("{}: {e}", manifest_path.display()),
                "point import at a directory made by rdy export",
            )
        })?;
        let manifest: Manifest = serde_yaml_ng::from_str(&manifest_text).map_err(|e| {
            RdyError::new(
                E_BAD_INPUT,
                format!("rdy-export.yaml: {e}"),
                "re-export the project",
            )
        })?;
        if !(1..=2).contains(&manifest.format) {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("unsupported export format {}", manifest.format),
                "this rdy reads export formats 1 and 2",
            ));
        }
        if !self.lineage.contains(&manifest.pack) {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!(
                    "the export was made with pack {:?}; this project uses {:?}, which does not extend it",
                    manifest.pack, self.pack.name
                ),
                format!(
                    "import into a project whose pack is {} or extends it",
                    manifest.pack
                ),
            ));
        }
        let blob_dir = dir.join("blobs");
        let mut blob_bytes = vec![];
        let mut imported_blobs = BTreeSet::new();
        if blob_dir.exists() {
            for entry in std::fs::read_dir(&blob_dir).map_err(io_err)? {
                let path = entry.map_err(io_err)?.path();
                refuse_symlink(&path)?;
                if !path.is_file() || is_hidden(&path) {
                    continue;
                }
                let bytes = std::fs::read(&path).map_err(io_err)?;
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                let got = rdy_store::Store::blob_hash(&bytes);
                if got.to_string() != name {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        format!("blob {name} hashes to {got}"),
                        "the export is corrupt; export it again",
                    ));
                }
                imported_blobs.insert(got);
                blob_bytes.push(bytes);
            }
        }
        let mut blocks: Vec<Block> = match manifest.format {
            1 => read_format_1(dir)?,
            2 => {
                let (loaded, diags) = tree_io::read_tree(&dir.join("docs"), "docs");
                if let Some(d) = diags
                    .iter()
                    .find(|d| d.severity == rdy_schema::Severity::Error)
                {
                    return Err(d.to_error());
                }
                loaded.into_iter().map(|l| l.block).collect()
            }
            _ => unreachable!("format checked above"),
        };
        let scope = Scope::View(view.to_string());
        let existing = self.materialized(&scope)?.clone();
        // duplicates first: a duplicated doc would otherwise read as a name clash with itself
        let mut ids = BTreeSet::new();
        for b in &blocks {
            if !ids.insert(&b.id) {
                return Err(RdyError::new(
                    E_ALREADY_EXISTS,
                    format!("block {} appears more than once in the import", b.id),
                    "remove the duplicate from the export files",
                ));
            }
        }
        drop(ids);
        assign_missing_names(&mut blocks, &existing);
        check_import_names(&blocks, &existing)?;
        let incoming: BTreeMap<BlockId, &Block> =
            blocks.iter().map(|b| (b.id.clone(), b)).collect();
        let parent_of = |id: &BlockId| -> Option<Option<BlockId>> {
            incoming
                .get(id)
                .map(|b| b.parent.clone())
                .or_else(|| existing.get(id).map(|b| b.parent.clone()))
        };
        for b in &blocks {
            if let Some(p) = &b.parent
                && parent_of(p).is_none()
            {
                return Err(RdyError::new(
                    E_UNKNOWN_BLOCK,
                    format!(
                        "{} has parent {p}, which is in neither the import nor the view",
                        b.id
                    ),
                    "export the whole project",
                ));
            }
            let mut seen = BTreeSet::from([b.id.clone()]);
            let mut folders = usize::from(b.block_type == tree::FOLDER);
            let mut cur = b.parent.clone();
            while let Some(c) = cur {
                if !seen.insert(c.clone()) {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        format!("{} is part of a parent cycle", b.id),
                        "fix the parent fields in the export",
                    ));
                }
                let is_folder = incoming
                    .get(&c)
                    .copied()
                    .or_else(|| existing.get(&c))
                    .is_some_and(|p| p.block_type == tree::FOLDER);
                folders += usize::from(is_folder);
                if folders > tree::MAX_FOLDER_DEPTH {
                    break;
                }
                cur = parent_of(&c).flatten();
            }
            if folders > tree::MAX_FOLDER_DEPTH {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!(
                        "{} sits {folders} folders deep (at most {} allowed)",
                        b.id,
                        tree::MAX_FOLDER_DEPTH
                    ),
                    format!(
                        "keep folders at most {} deep: move it higher in the export",
                        tree::MAX_FOLDER_DEPTH
                    ),
                ));
            }
        }
        let mut issues = vec![];
        for b in &blocks {
            if existing.get(&b.id).is_some() {
                return Err(RdyError::new(
                    E_ALREADY_EXISTS,
                    format!("block {} already exists in view {view}", b.id),
                    "import into a fresh project or view",
                ));
            }
            let exists = |id: &BlockId| incoming.contains_key(id) || existing.get(id).is_some();
            for t in b.ref_tokens() {
                match BlockId::parse(&t) {
                    Ok(id) if exists(&id) => {}
                    _ => {
                        return Err(RdyError::new(
                            E_UNKNOWN_REF,
                            format!(
                                "{} references {{{{{t}}}}}, which is not in the import",
                                b.id
                            ),
                            "export the whole project, or remove the reference",
                        ));
                    }
                }
            }
            for l in b.links.values().flatten() {
                if !exists(&l.target) {
                    return Err(RdyError::new(
                        E_UNKNOWN_REF,
                        format!("{} links to {}, which is not in the import", b.id, l.target),
                        "export the whole project",
                    ));
                }
            }
            // blob2: an asset block's blob must be in the export's blobs/ or a blob in this
            // project; non-asset types may have a field named blob that is not an asset
            let is_asset = self
                .pack
                .block_types
                .get(&b.block_type)
                .is_some_and(|d| d.traits.contains(&rdy_schema::Trait::Asset));
            if is_asset && let Some(v) = b.fields.get("blob") {
                let known = match v.as_str().and_then(|s| s.parse::<Hash>().ok()) {
                    Some(h) => imported_blobs.contains(&h) || self.store.has_blob(&h)?,
                    None => false,
                };
                if !known {
                    return Err(RdyError::new(
                        E_UNKNOWN_REF,
                        format!(
                            "{} names blob {}, which is neither in the export's blobs/ nor a blob in this project",
                            b.id,
                            v.as_str().unwrap_or("(not a string)")
                        ),
                        "export the whole project again: rdy export copies every figure's blob",
                    ));
                }
            }
            let parent = b
                .parent
                .as_ref()
                .and_then(|p| incoming.get(p).copied().or_else(|| existing.get(p)));
            issues.extend(validate_block(&self.pack, b, parent));
        }
        if has_errors(&issues) {
            let mut e = RdyError::new(
                E_VALIDATION,
                "the import does not validate against this project's pack",
                "fix the listed blocks or use a compatible pack",
            );
            e.issues = issues;
            return Err(e);
        }
        // blobs go in only once the import is known to be valid, so a refused import stores nothing
        for bytes in &blob_bytes {
            self.store.put_blob(bytes)?;
        }
        let ops: Vec<Op> = blocks
            .into_iter()
            .map(|block| Op::Create { block })
            .collect();
        let change = Change {
            view: view.into(),
            ops: ops.clone(),
            deps: vec![],
            provenance: s.provenance(),
            justification: None,
            message: format!("import from {}", dir.display()),
        };
        let id = self.store.commit_to_view(view, change)?;
        self.update_cache(view, &ops);
        let created = ops
            .iter()
            .enumerate()
            .map(|(i, op)| (format!("op{i}"), op.block_id().to_string()))
            .collect();
        Ok(WriteResult {
            change: id.to_string(),
            created,
            names: BTreeMap::new(),
            warnings: vec![],
        })
    }

    pub fn edit_block(
        &mut self,
        s: &Session,
        view: &str,
        edited: Block,
    ) -> Result<WriteResult, RdyError> {
        let scope = Scope::View(view.to_string());
        let cur = self.block(&scope, &edited.id)?;
        if cur == edited {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("nothing changed in {}", edited.id),
                "change a field, the body or the links, or cancel the edit",
            ));
        }
        // edit1: pins are rdy's record of what a block was checked against; an edit carries
        // fields, body and links only, so a pin change would commit an update that changes nothing
        if cur.pins != edited.pins {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("the edit changes the pins of {}", edited.id),
                "pins are set by rdy; remove the pins change",
            ));
        }
        if cur.block_type != edited.block_type
            || cur.parent != edited.parent
            || cur.order != edited.order
        {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "type, parent and order cannot be changed by edit",
                "use a move op to change parent or order",
            ));
        }
        let fields = edited
            .fields
            .iter()
            .filter(|(k, v)| cur.fields.get(*k) != Some(*v))
            .map(|(k, v)| (k.clone(), field_json(v)))
            .collect();
        let unset = cur
            .fields
            .keys()
            .filter(|k| !edited.fields.contains_key(*k))
            .cloned()
            .collect();
        let links = (cur.links != edited.links).then(|| {
            edited
                .links
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.iter()
                            .map(|l| LinkInput {
                                target: l.target.to_string(),
                                part: l.part.clone(),
                            })
                            .collect(),
                    )
                })
                .collect()
        });
        let op = OpInput::Update {
            id: edited.id.to_string(),
            base: Some(cur.version_hash().to_string()),
            fields,
            unset,
            body: (cur.body != edited.body).then(|| edited.body.clone()),
            links,
        };
        self.write(
            s,
            WriteRequest {
                view: view.into(),
                ops: vec![op],
                justification: None,
                message: format!("edit {}", edited.id),
            },
        )
    }
}
