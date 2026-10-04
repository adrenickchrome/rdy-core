//! The format-2 docs tree (addendum B §3), read with positions: shared by import and `rdy check`.
use crate::error::*;
use crate::io::{BlockYaml, from_yaml_struct};
use crate::yaml_pos::{Positions, Seg, positions};
use rdy_model::{Block, BlockId, FieldValue, tree};
use rdy_schema::Severity;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

pub const FOLDER_FILE: &str = "_folder.yaml";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub code: String,
    pub message: String,
    pub fix: String,
    pub severity: Severity,
}

impl Diagnostic {
    pub(crate) fn error(
        file: &str,
        at: (usize, usize),
        code: &str,
        message: impl Into<String>,
        fix: impl Into<String>,
    ) -> Diagnostic {
        Diagnostic {
            file: file.into(),
            line: at.0,
            column: at.1,
            code: code.into(),
            message: message.into(),
            fix: fix.into(),
            severity: Severity::Error,
        }
    }

    pub(crate) fn to_error(&self) -> RdyError {
        let code: &'static str = match self.code.as_str() {
            "E_BAD_NAME" => E_BAD_NAME,
            "E_PATH_CLASH" => E_PATH_CLASH,
            _ => E_FILE_FORMAT,
        };
        RdyError::new(
            code,
            format!(
                "{}:{}:{}: {}",
                self.file, self.line, self.column, self.message
            ),
            self.fix.clone(),
        )
    }
}

/// One block read from a file, with where it was found.
pub(crate) struct Loaded {
    pub block: Block,
    pub file: String,
    pub at: (usize, usize),
    pub keys: BTreeMap<String, (usize, usize)>,
}

#[derive(Default)]
struct Reader {
    loaded: Vec<Loaded>,
    diags: Vec<Diagnostic>,
}

/// Reads the tree under `root`; `shown` is how `root` appears in messages (e.g. "docs").
pub(crate) fn read_tree(root: &Path, shown: &str) -> (Vec<Loaded>, Vec<Diagnostic>) {
    let mut r = Reader::default();
    r.dir(root, shown, None);
    (r.loaded, r.diags)
}

/// Reads one doc file or `_folder.yaml` as if it were at the top level.
pub(crate) fn read_file(path: &Path, shown: &str) -> (Vec<Loaded>, Vec<Diagnostic>) {
    let mut r = Reader::default();
    let fname = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if fname == FOLDER_FILE {
        let dir_name = path
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        r.folder_file(path, shown, &dir_name, None);
    } else if let Some(stem) = fname.strip_suffix(".yaml") {
        r.check_name(stem, shown);
        r.doc_file(path, shown, stem, None);
    } else {
        r.diags.push(Diagnostic::error(
            shown,
            (1, 1),
            E_FILE_FORMAT,
            "not a .yaml file",
            "check a doc file, a _folder.yaml or a directory",
        ));
    }
    (r.loaded, r.diags)
}

fn hidden(name: &str) -> bool {
    name.starts_with('.')
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// sym1: a link could read outside the tree or loop; it is refused, never followed.
fn link_diag(shown: &str) -> Diagnostic {
    Diagnostic::error(
        shown,
        (1, 1),
        E_FILE_FORMAT,
        format!("{shown} is a symbolic link; rdy does not follow links in an export"),
        "replace the link with the file or folder it points to",
    )
}

impl Reader {
    fn dir(&mut self, dir: &Path, shown: &str, parent: Option<&BlockId>) {
        let mut entries: Vec<_> = match std::fs::read_dir(dir) {
            Ok(rd) => rd.filter_map(Result::ok).map(|e| e.path()).collect(),
            Err(e) => {
                self.diags.push(Diagnostic::error(
                    shown,
                    (1, 1),
                    E_FILE_FORMAT,
                    format!("cannot read the directory: {e}"),
                    "check the path",
                ));
                return;
            }
        };
        entries.sort();
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for path in entries {
            let Some(fname) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
                self.diags.push(Diagnostic::error(
                    &format!(
                        "{shown}/{}",
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    ),
                    (1, 1),
                    E_BAD_NAME,
                    "the name is not valid UTF-8",
                    "rename it",
                ));
                continue;
            };
            if hidden(&fname) {
                continue;
            }
            let here = format!("{shown}/{fname}");
            if is_symlink(&path) {
                self.diags.push(link_diag(&here));
                continue;
            }
            let name = if path.is_dir() {
                fname.clone()
            } else if fname == FOLDER_FILE {
                continue;
            } else if let Some(stem) = fname.strip_suffix(".yaml") {
                stem.to_string()
            } else {
                continue; // README.md and other non-YAML files are not part of the tree
            };
            if let Some(other) = seen.insert(name.to_ascii_lowercase(), here.clone()) {
                self.diags.push(Diagnostic::error(
                    &here,
                    (1, 1),
                    E_PATH_CLASH,
                    format!("{here} and {other} have the same name ignoring case"),
                    "rename one of them",
                ));
                continue;
            }
            self.check_name(&name, &here);
            if path.is_dir() {
                let ff = path.join(FOLDER_FILE);
                let ff_shown = format!("{here}/{FOLDER_FILE}");
                if is_symlink(&ff) {
                    self.diags.push(link_diag(&ff_shown));
                    continue;
                }
                if !ff.exists() {
                    self.diags.push(Diagnostic::error(
                        &ff_shown,
                        (1, 1),
                        E_FILE_FORMAT,
                        format!("folder {here} has no {FOLDER_FILE}"),
                        "add _folder.yaml with the folder's id, type: folder and order",
                    ));
                    continue;
                }
                if let Some(id) = self.folder_file(&ff, &ff_shown, &name, parent) {
                    self.dir(&path, &here, Some(&id));
                }
            } else {
                self.doc_file(&path, &here, &name, parent);
            }
        }
    }

    fn check_name(&mut self, name: &str, shown: &str) {
        if let Some(why) = tree::name_problem(name) {
            let id = BlockId::parse("d-00000").expect("valid id");
            self.diags.push(Diagnostic::error(
                shown,
                (1, 1),
                E_BAD_NAME,
                format!("{name:?} is not a valid name: {why}"),
                format!(
                    "rename it, e.g. to {:?}",
                    tree::default_name(Some(name), &id)
                ),
            ));
        }
    }

    fn parse(&mut self, path: &Path, shown: &str) -> Option<(serde_yaml_ng::Value, Positions)> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                self.diags.push(Diagnostic::error(
                    shown,
                    (1, 1),
                    E_FILE_FORMAT,
                    format!("cannot read the file: {e}"),
                    "check the file",
                ));
                return None;
            }
        };
        let pos = match positions(&text) {
            Ok(p) => p,
            Err((line, col, msg)) => {
                self.diags.push(Diagnostic::error(
                    shown,
                    (line, col),
                    E_FILE_FORMAT,
                    format!("YAML syntax: {msg}"),
                    "fix the YAML syntax",
                ));
                return None;
            }
        };
        match serde_yaml_ng::from_str(&text) {
            Ok(v) => Some((v, pos)),
            Err(e) => {
                let at = e
                    .location()
                    .map(|l| (l.line(), l.column()))
                    .unwrap_or((1, 1));
                self.diags.push(Diagnostic::error(
                    shown,
                    at,
                    E_FILE_FORMAT,
                    format!("YAML: {e}"),
                    "fix the YAML",
                ));
                None
            }
        }
    }

    /// One block entry at node path `at` in a file.
    fn entry(
        &mut self,
        value: serde_yaml_ng::Value,
        pos: &Positions,
        at: Vec<Seg>,
        shown: &str,
    ) -> Option<Loaded> {
        let here = pos.get(&at).copied().unwrap_or((1, 1));
        let mut keys = BTreeMap::new();
        for (p, xy) in pos.range(at.clone()..) {
            if !p.starts_with(&at) {
                break;
            }
            match &p[at.len()..] {
                [Seg::Key(k)] => {
                    keys.insert(k.clone(), *xy);
                }
                [Seg::Key(f), Seg::Key(k)] if f == "fields" => {
                    keys.insert(format!("fields.{k}"), *xy);
                }
                _ => {}
            }
        }
        let y: BlockYaml = match serde_yaml_ng::from_value(value) {
            Ok(y) => y,
            Err(e) => {
                self.diags.push(Diagnostic::error(
                    shown,
                    here,
                    E_FILE_FORMAT,
                    format!("not a block entry: {e}"),
                    "each entry needs id, type and order, as rdy export writes them",
                ));
                return None;
            }
        };
        match from_yaml_struct(y) {
            Ok(block) => Some(Loaded {
                block,
                file: shown.into(),
                at: here,
                keys,
            }),
            Err(e) => {
                self.diags.push(Diagnostic::error(
                    shown,
                    here,
                    E_FILE_FORMAT,
                    e.message,
                    e.fix,
                ));
                None
            }
        }
    }

    /// The file system holds a folder's or doc's name and parent: neither may be written in the file.
    fn forbid_fs_fields(&mut self, l: &Loaded, what: &str) -> bool {
        let mut ok = true;
        if l.block.parent.is_some() {
            let at = l.keys.get("parent").copied().unwrap_or(l.at);
            self.diags.push(Diagnostic::error(
                &l.file,
                at,
                E_FILE_FORMAT,
                format!("a {what} entry must not have parent: its directory is its parent"),
                "remove the parent line; move the file to change the parent",
            ));
            ok = false;
        }
        if l.block.fields.contains_key("name") {
            let at = l.keys.get("fields.name").copied().unwrap_or(l.at);
            self.diags.push(Diagnostic::error(&l.file, at, E_FILE_FORMAT, format!("a {what} entry must not have a name field: its file or directory name is its name"), "remove the name field; rename the file or directory to rename it"));
            ok = false;
        }
        ok
    }

    fn folder_file(
        &mut self,
        path: &Path,
        shown: &str,
        name: &str,
        parent: Option<&BlockId>,
    ) -> Option<BlockId> {
        let (value, pos) = self.parse(path, shown)?;
        if !value.is_mapping() {
            self.diags.push(Diagnostic::error(
                shown,
                (1, 1),
                E_FILE_FORMAT,
                "_folder.yaml holds one block entry (a mapping)",
                "write the folder's id, type: folder and order",
            ));
            return None;
        }
        let mut l = self.entry(value, &pos, vec![], shown)?;
        if l.block.block_type != tree::FOLDER {
            let at = l.keys.get("type").copied().unwrap_or(l.at);
            self.diags.push(Diagnostic::error(
                shown,
                at,
                E_FILE_FORMAT,
                "_folder.yaml must describe a folder (type: folder)",
                "set type: folder",
            ));
            return None;
        }
        if !self.forbid_fs_fields(&l, "folder") {
            return None;
        }
        l.block.parent = parent.cloned();
        l.block
            .fields
            .insert("name".into(), FieldValue::Str(name.into()));
        let id = l.block.id.clone();
        self.loaded.push(l);
        Some(id)
    }

    fn doc_file(&mut self, path: &Path, shown: &str, name: &str, parent: Option<&BlockId>) {
        let Some((value, pos)) = self.parse(path, shown) else {
            return;
        };
        let Some(items) = value.as_sequence().cloned() else {
            self.diags.push(Diagnostic::error(
                shown,
                (1, 1),
                E_FILE_FORMAT,
                "a doc file is a list of block entries, the doc first",
                "write the doc's entry, then the blocks inside it",
            ));
            return;
        };
        if items.is_empty() {
            self.diags.push(Diagnostic::error(
                shown,
                (1, 1),
                E_FILE_FORMAT,
                "the doc file is empty",
                "write the doc's entry first",
            ));
            return;
        }
        let mut head: Option<BlockId> = None;
        let mut inner: Vec<Loaded> = vec![];
        for (i, v) in items.into_iter().enumerate() {
            let Some(mut l) = self.entry(v, &pos, vec![Seg::Index(i)], shown) else {
                continue;
            };
            if i == 0 {
                if l.block.block_type != tree::DOC {
                    let at = l.keys.get("type").copied().unwrap_or(l.at);
                    self.diags.push(Diagnostic::error(
                        shown,
                        at,
                        E_FILE_FORMAT,
                        "the first entry of a doc file must be the doc (type: doc)",
                        "put the doc's entry first",
                    ));
                    return;
                }
                if !self.forbid_fs_fields(&l, "doc") {
                    return;
                }
                l.block.parent = parent.cloned();
                l.block
                    .fields
                    .insert("name".into(), FieldValue::Str(name.into()));
                head = Some(l.block.id.clone());
                self.loaded.push(l);
                continue;
            }
            if tree::is_tree_type(&l.block.block_type) {
                let at = l.keys.get("type").copied().unwrap_or(l.at);
                self.diags.push(Diagnostic::error(
                    shown,
                    at,
                    E_FILE_FORMAT,
                    format!(
                        "{} is a {}, but a doc file holds one doc; put folders and other docs in their own files",
                        l.block.id, l.block.block_type
                    ),
                    "move the entry to its own file (a doc) or directory (a folder)",
                ));
                continue;
            }
            inner.push(l);
        }
        // every block after the doc sits inside it: its parents stay in this file and end at the doc
        if let Some(head) = head {
            let parents: BTreeMap<&BlockId, Option<&BlockId>> = inner
                .iter()
                .map(|l| (&l.block.id, l.block.parent.as_ref()))
                .collect();
            for l in &inner {
                let Some(p) = l.block.parent.as_ref() else {
                    continue; // validation reports the missing parent (E_ORPHAN)
                };
                let mut cur = Some(p);
                let mut inside = false;
                for _ in 0..=parents.len() {
                    match cur {
                        Some(c) if *c == head => {
                            inside = true;
                            break;
                        }
                        Some(c) => cur = parents.get(c).copied().flatten(),
                        None => break,
                    }
                }
                if !inside {
                    let at = l.keys.get("parent").copied().unwrap_or(l.at);
                    self.diags.push(Diagnostic::error(
                        shown,
                        at,
                        E_FILE_FORMAT,
                        format!(
                            "{} has parent {p}, which does not lead to this file's doc {head}: a doc file holds only its doc and the blocks inside it",
                            l.block.id
                        ),
                        format!("set parent to {head} or a block inside it in this file; a block of another doc belongs in that doc's file"),
                    ));
                }
            }
        }
        self.loaded.extend(inner);
    }
}
