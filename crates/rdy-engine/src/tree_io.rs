//! The format-2 docs tree (addendum B §3), read with positions: shared by import and `rdy check`.
//! Working-copy mode (`read_wc`) reads a working tree: membership, untracked paths, and entries
//! that leave out `id`, `order` and `parent`.
use crate::error::*;
use crate::io::{BlockYaml, KeyedLink, from_yaml_struct};
use crate::wc::ignore::{Ignore, IgnoreAt};
use crate::wc::layout::{BLOBS_DIR, FileKind, MAX_MEMBER_BYTES, THEIRS_SUFFIX, Untracked, WcFile};
use crate::yaml_pos::{Positions, Seg, positions};
use rdy_model::{Block, BlockId, FieldValue, Hash, tree};
use rdy_schema::{Pack, Severity};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
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
            "E_ORDER_KEY" => "E_ORDER_KEY",
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

/// The keys of a block entry.
const BLOCK_KEYS: [&str; 8] = [
    "id", "type", "parent", "order", "fields", "body", "links", "pins",
];

/// The keys of a link object.
const LINK_OBJECT_KEYS: [&str; 2] = ["target", "part"];

/// A key an entry does not take (FW-HD1, ruling Q2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyFault {
    /// The keys and indexes from the entry to the mapping that holds the key: empty for the
    /// entry itself, `["links", kind, "0"]` for the first object of a link kind.
    pub path: Vec<String>,
    pub key: String,
    /// The known key it is closest to.
    pub near: Option<&'static str>,
    /// The link kind, when the key sits in a link object.
    pub in_link: Option<String>,
}

impl KeyFault {
    /// Where the key sits, in words: `block` for the entry itself.
    pub(crate) fn place(&self, block: &str) -> String {
        match &self.in_link {
            Some(kind) => format!("a {kind} link"),
            None => block.to_string(),
        }
    }

    pub(crate) fn fix(&self) -> String {
        match (self.near, &self.in_link) {
            (Some(y), _) => format!("did you mean \"{y}\"? rename it"),
            (None, Some(_)) => "remove it; a link takes target and part".to_string(),
            (None, None) => {
                "remove it, or use id, type, parent, order, fields, body, links or pins".to_string()
            }
        }
    }

    /// The path of the key in a YAML text, from the root of the entry.
    pub(crate) fn segs(&self) -> Vec<Seg> {
        let mut v: Vec<Seg> = self
            .path
            .iter()
            .enumerate()
            .map(|(i, p)| match (i, p.parse::<usize>()) {
                (2, Ok(n)) => Seg::Index(n),
                _ => Seg::Key(p.clone()),
            })
            .collect();
        v.push(Seg::Key(self.key.clone()));
        v
    }
}

fn key_name(k: &serde_yaml_ng::Value) -> String {
    match k {
        serde_yaml_ng::Value::String(s) => s.clone(),
        other => serde_yaml_ng::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

/// Every key of a block entry that the entry does not take, in file order: the entry's own keys
/// against `BLOCK_KEYS`, and the keys of each object in `links.<kind>` against `target` and `part`.
/// Nothing inside `fields` or `pins` (the pack checks fields).
pub(crate) fn key_faults(value: &serde_yaml_ng::Value) -> Vec<KeyFault> {
    use serde_yaml_ng::Value;
    let Value::Mapping(m) = value else {
        return vec![];
    };
    let mut out = vec![];
    for (k, v) in m {
        let name = key_name(k);
        if !BLOCK_KEYS.contains(&name.as_str()) {
            out.push(KeyFault {
                near: crate::did_you_mean::nearest(&name, &BLOCK_KEYS),
                path: vec![],
                key: name,
                in_link: None,
            });
        } else if name == "links"
            && let Value::Mapping(kinds) = v
        {
            for (kind, list) in kinds {
                let kind = key_name(kind);
                let Value::Sequence(items) = list else {
                    continue;
                };
                for (i, item) in items.iter().enumerate() {
                    let Value::Mapping(o) = item else {
                        continue;
                    };
                    for ok in o.keys() {
                        let ok = key_name(ok);
                        if !LINK_OBJECT_KEYS.contains(&ok.as_str()) {
                            out.push(KeyFault {
                                near: crate::did_you_mean::nearest(&ok, &LINK_OBJECT_KEYS),
                                path: vec!["links".into(), kind.clone(), i.to_string()],
                                key: ok,
                                in_link: Some(kind.clone()),
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

/// One block read from a file, with where it was found.
pub(crate) struct Loaded {
    pub block: Block,
    pub file: String,
    pub at: (usize, usize),
    pub keys: BTreeMap<String, (usize, usize)>,
    /// lkey1: link targets written as keys, resolved by import and check.
    pub keyed: Vec<KeyedLink>,
    /// Working-copy mode: the entry gave no id, and the reader assigned a fresh one.
    pub new: bool,
    /// Working-copy mode: the entry gave an `order` (else `block.order` is empty).
    pub order_given: bool,
    /// The entry's index in its file.
    pub seq: usize,
}

#[derive(Default)]
struct Reader<'a> {
    loaded: Vec<Loaded>,
    diags: Vec<Diagnostic>,
    /// The text of the file being read: `order` is quoted back from it (ord4).
    text: String,
    /// Paths to skip, relative to the root read.
    ignore: Option<&'a Ignore>,
    /// Working-copy mode: the pack whose prefixes give new entries their ids.
    wc: Option<&'a Pack>,
    /// Working-copy mode: ids in use, which a fresh id avoids.
    taken: BTreeSet<BlockId>,
    files: BTreeMap<String, WcFile>,
    untracked: Vec<Untracked>,
    theirs: Vec<String>,
    blobs: Vec<String>,
}

/// Reads the tree under `root`; `shown` is how `root` appears in messages (e.g. "docs").
pub(crate) fn read_tree(root: &Path, shown: &str) -> (Vec<Loaded>, Vec<Diagnostic>) {
    read_tree_with(root, shown, None)
}

/// `read_tree`, skipping the paths `ignore` matches relative to `root` (R-P5).
pub(crate) fn read_tree_with(
    root: &Path,
    shown: &str,
    ignore: Option<&Ignore>,
) -> (Vec<Loaded>, Vec<Diagnostic>) {
    let mut r = Reader {
        ignore,
        ..Reader::default()
    };
    let at = r.ignore_root();
    r.dir(root, shown, None, &at);
    (r.loaded, r.diags)
}

/// What the working-copy reader found: the blocks of the member files, the members, and the
/// paths it does not read. Paths are relative to the root.
pub(crate) struct WcRead {
    pub loaded: Vec<Loaded>,
    pub files: BTreeMap<String, WcFile>,
    pub untracked: Vec<Untracked>,
    pub theirs: Vec<String>,
    pub blobs: Vec<String>,
    pub diags: Vec<Diagnostic>,
}

/// How many times the working-copy reader reads the tree again when a fresh id it drew turns
/// out to be written in a later file.
const WC_READ_TRIES: usize = 3;

/// Reads the working tree under `root` (spec §12.1, `D-7BKK5`), in working-copy mode.
pub(crate) fn read_wc(root: &Path, ignore: &Ignore, pack: &Pack) -> WcRead {
    let mut reserved = BTreeSet::new();
    let mut tries = 0;
    loop {
        let mut r = Reader {
            ignore: Some(ignore),
            wc: Some(pack),
            taken: reserved.clone(),
            ..Reader::default()
        };
        let at = r.ignore_root();
        r.wc_dir(root, "", None, 0, &at);
        tries += 1;
        // a fresh id is never one the tree holds: one drawn before a later file named it is
        // drawn again on a second read, which avoids every id the first one saw
        let given: BTreeSet<BlockId> = r
            .loaded
            .iter()
            .filter(|l| !l.new)
            .map(|l| l.block.id.clone())
            .collect();
        let clash = r
            .loaded
            .iter()
            .any(|l| l.new && given.contains(&l.block.id));
        if !clash || tries == WC_READ_TRIES {
            return WcRead {
                loaded: r.loaded,
                files: r.files,
                untracked: r.untracked,
                theirs: r.theirs,
                blobs: r.blobs,
                diags: r.diags,
            };
        }
        reserved.extend(given);
    }
}

/// `rel` under `dir` (the root is "").
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// Why a file the scan met is not read as a member, from its metadata (never opened).
fn member_problem(meta: &std::fs::Metadata) -> Option<&'static str> {
    let ft = meta.file_type();
    if ft.is_symlink() {
        Some(SYMLINK_REASON)
    } else if !ft.is_file() && !ft.is_dir() {
        Some(NOT_REGULAR)
    } else {
        None
    }
}

const SYMLINK_REASON: &str = "symbolic link: rdy does not follow links";
const NOT_REGULAR: &str = "not a regular file";
const NOT_UTF8: &str = "the name is not UTF-8";
const NOT_DOC: &str = "not a doc file: the first entry is not type: doc";

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

/// How YAML read a value, for a message: `a number`, `a boolean`, `null`, `a list`, `a mapping` or `a string`.
fn yaml_kind(v: &serde_yaml_ng::Value) -> &'static str {
    use serde_yaml_ng::Value;
    match v {
        Value::Number(_) => "a number",
        Value::Bool(_) => "a boolean",
        Value::Null => "null",
        Value::Sequence(_) => "a list",
        Value::String(_) => "a string",
        Value::Mapping(_) | Value::Tagged(_) => "a mapping",
    }
}

fn hidden(name: &str) -> bool {
    name.starts_with('.')
}

pub(crate) fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// sym1: a link could read outside the tree or loop; it is refused, never followed.
pub(crate) fn link_diag(shown: &str) -> Diagnostic {
    Diagnostic::error(
        shown,
        (1, 1),
        E_FILE_FORMAT,
        format!("{shown} is a symbolic link; rdy does not follow links in an export"),
        "replace the link with the file or folder it points to",
    )
}

impl Reader<'_> {
    /// One directory of an export tree; `at` is the ignore matcher's state in it.
    fn dir(&mut self, dir: &Path, shown: &str, parent: Option<&BlockId>, at: &IgnoreAt) {
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
            if self.ignore.is_some() {
                let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
                if self.ignored(at, &fname, is_dir) {
                    continue;
                }
            }
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
                    let inner = self.enter(at, &fname);
                    self.dir(&path, &here, Some(&id), &inner);
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
        let parsed = serde_yaml_ng::from_str(&text);
        self.text = text;
        match parsed {
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

    /// One block entry at node path `at` in a file. In working-copy mode an entry with no `id`
    /// gets a fresh one with its type's prefix, and one with no `order` an empty order.
    fn entry(
        &mut self,
        mut value: serde_yaml_ng::Value,
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
                [Seg::Key(f), Seg::Key(kind), Seg::Index(i), Seg::Key(k)] if f == "links" => {
                    keys.insert(format!("links.{kind}.{i}.{k}"), *xy);
                }
                _ => {}
            }
        }
        if !self.check_keys(&value, &keys, here, shown) {
            return None;
        }
        let (mut new, mut order_given) = (false, true);
        if let Some(pack) = self.wc
            && let serde_yaml_ng::Value::Mapping(m) = &mut value
        {
            if !m.contains_key("id") {
                let t = m
                    .get("type")
                    .and_then(serde_yaml_ng::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let prefix = pack
                    .block_types
                    .get(&t)
                    .map(|d| d.prefix.as_str())
                    .filter(|p| BlockId::parse(&format!("{p}-00000")).is_ok());
                let Some(prefix) = prefix else {
                    let at = keys.get("type").copied().unwrap_or(here);
                    self.diags.push(Diagnostic::error(
                        shown,
                        at,
                        E_VALIDATION,
                        format!("unknown block type \"{t}\""),
                        "use a block type the project's pack defines",
                    ));
                    return None;
                };
                let id = self.fresh_id(prefix);
                m.insert("id".into(), id.to_string().into());
                new = true;
            }
            if !m.contains_key("order") {
                m.insert("order".into(), String::new().into());
                order_given = false;
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
            Ok((block, keyed)) => {
                if self.wc.is_some() && !new {
                    self.taken.insert(block.id.clone());
                }
                Some(Loaded {
                    block,
                    file: shown.into(),
                    at: here,
                    keys,
                    keyed,
                    new,
                    order_given,
                    seq: 0,
                })
            }
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

    /// ykey1, ord4: names the key at fault in an entry mapping, in the order the checks run. Each
    /// failure is a diagnostic at that key; false when any was raised (the entry is skipped).
    fn check_keys(
        &mut self,
        value: &serde_yaml_ng::Value,
        keys: &BTreeMap<String, (usize, usize)>,
        here: (usize, usize),
        shown: &str,
    ) -> bool {
        use serde_yaml_ng::Value;
        let Value::Mapping(m) = value else {
            return true; // not a mapping: from_value says so
        };
        let entries: Vec<(String, &Value)> = m
            .iter()
            .map(|(k, v)| {
                let name = match k {
                    Value::String(s) => s.clone(),
                    other => serde_yaml_ng::to_string(other)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                };
                (name, v)
            })
            .collect();
        let at = |k: &str| keys.get(k).copied().unwrap_or(here);
        let mut ok = true;
        for f in key_faults(value) {
            ok = false;
            let lookup = f.path.iter().chain([&f.key]).cloned().collect::<Vec<_>>();
            self.diags.push(Diagnostic::error(
                shown,
                at(&lookup.join(".")),
                E_FILE_FORMAT,
                format!("unknown key \"{}\" in {}", f.key, f.place("a block entry")),
                f.fix(),
            ));
        }
        for (k, v) in &entries {
            if ["id", "type", "parent", "body"].contains(&k.as_str())
                && !matches!(v, Value::String(_))
            {
                ok = false;
                self.diags.push(Diagnostic::error(
                    shown,
                    at(k),
                    E_FILE_FORMAT,
                    format!("\"{k}\" must be a string, not {}", yaml_kind(v)),
                    "quote the value",
                ));
            }
        }
        for (k, v) in &entries {
            if k == "order" && !matches!(v, Value::String(_)) {
                ok = false;
                let key_at = at(k);
                let raw = self.order_raw(key_at, v);
                self.diags.push(Diagnostic::error(
                    shown,
                    key_at,
                    "E_ORDER_KEY",
                    format!(
                        "order {raw} is not a string: YAML reads it as {}",
                        yaml_kind(v)
                    ),
                    format!("quote it: order: '{raw}'"),
                ));
            }
        }
        for (k, v) in &entries {
            if ["fields", "links", "pins"].contains(&k.as_str()) && !matches!(v, Value::Mapping(_))
            {
                ok = false;
                self.diags.push(Diagnostic::error(
                    shown,
                    at(k),
                    E_FILE_FORMAT,
                    format!("\"{k}\" must be a mapping, not {}", yaml_kind(v)),
                    format!("write {k} as key: value lines"),
                ));
            }
        }
        // a working-copy entry may leave out id and order: the reader assigns them
        let (required, fix): (&[&str], &str) = if self.wc.is_some() {
            (&["type"], "each entry needs a type")
        } else {
            (
                &["id", "type", "order"],
                "each entry needs id, type and order, as rdy export writes them",
            )
        };
        for k in required {
            if !entries.iter().any(|(n, _)| n == k) {
                ok = false;
                self.diags.push(Diagnostic::error(
                    shown,
                    here,
                    E_FILE_FORMAT,
                    format!("the entry has no \"{k}\""),
                    fix,
                ));
            }
        }
        ok
    }

    /// A fresh id with `prefix` that no block read so far holds.
    fn fresh_id(&mut self, prefix: &str) -> BlockId {
        loop {
            let id = BlockId::new(prefix);
            if self.taken.insert(id.clone()) {
                return id;
            }
        }
    }

    /// The text of an `order` value as written: from its column to the end of its line, without a
    /// trailing ` #…` comment. `key_at` is where the key `order` starts. Falls back to the value's
    /// own reading when the line is not the usual `order: value`.
    fn order_raw(&self, key_at: (usize, usize), v: &serde_yaml_ng::Value) -> String {
        let fallback = || {
            serde_yaml_ng::to_string(v)
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        let Some(line) = self.text.lines().nth(key_at.0.saturating_sub(1)) else {
            return fallback();
        };
        let rest: String = line.chars().skip(key_at.1.saturating_sub(1)).collect();
        let rest = rest.trim_start_matches(['"', '\'']);
        let Some(rest) = rest.strip_prefix("order") else {
            return fallback();
        };
        let rest = rest.trim_start_matches(['"', '\'']).trim_start();
        let Some(rest) = rest.strip_prefix(':') else {
            return fallback();
        };
        let value = match rest.find(" #") {
            Some(i) => &rest[..i],
            None => rest,
        };
        let value = value.trim();
        if value.is_empty() {
            fallback()
        } else {
            value.to_string()
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
        self.doc_items(items, &pos, shown, name, parent);
    }

    /// The entries of a doc file, the doc first. Returns the doc's id when its entry loaded.
    fn doc_items(
        &mut self,
        items: Vec<serde_yaml_ng::Value>,
        pos: &Positions,
        shown: &str,
        name: &str,
        parent: Option<&BlockId>,
    ) -> Option<BlockId> {
        let mut head: Option<BlockId> = None;
        let mut inner: Vec<Loaded> = vec![];
        for (i, v) in items.into_iter().enumerate() {
            if i > 0 && head.is_none() && self.wc.is_some() {
                return None; // the doc's entry did not load: its diagnostics say why
            }
            let Some(mut l) = self.entry(v, pos, vec![Seg::Index(i)], shown) else {
                continue;
            };
            l.seq = i;
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
                    return None;
                }
                if !self.forbid_fs_fields(&l, "doc") {
                    return None;
                }
                l.block.parent = parent.cloned();
                l.block
                    .fields
                    .insert("name".into(), FieldValue::Str(name.into()));
                head = Some(l.block.id.clone());
                self.loaded.push(l);
                continue;
            }
            // a new entry with no parent sits under the file's doc
            if self.wc.is_some() && l.block.parent.is_none() {
                l.block.parent = head.clone();
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
        if let Some(head) = &head {
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
                        Some(c) if c == head => {
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
        head
    }

    // ---- working-copy mode (spec §12.1, D-7BKK5) ----

    fn untrack(&mut self, path: String, reason: impl Into<String>) {
        self.untracked.push(Untracked {
            path,
            reason: reason.into(),
        });
    }

    /// The ignore matcher's state at the root read.
    fn ignore_root(&self) -> IgnoreAt {
        self.ignore.map(Ignore::root).unwrap_or_default()
    }

    /// Whether the entry `name` of the directory at `at` is ignored. The walks decide each entry
    /// only: they enter no ignored directory, so its parents are not ignored.
    fn ignored(&self, at: &IgnoreAt, name: &str, is_dir: bool) -> bool {
        self.ignore.is_some_and(|i| i.ignores(at, name, is_dir))
    }

    /// The ignore matcher's state inside the directory `name` of the directory at `at`.
    fn enter(&self, at: &IgnoreAt, name: &str) -> IgnoreAt {
        self.ignore.map(|i| i.enter(at, name)).unwrap_or_default()
    }

    /// The entries of `dir`, sorted by name; a directory that cannot be read is a diagnostic.
    fn listing(&mut self, dir: &Path, rel: &str) -> Option<Vec<std::path::PathBuf>> {
        match std::fs::read_dir(dir) {
            Ok(rd) => {
                let mut entries: Vec<_> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
                entries.sort();
                Some(entries)
            }
            Err(e) => {
                let shown = if rel.is_empty() { "." } else { rel };
                self.diags.push(Diagnostic::error(
                    shown,
                    (1, 1),
                    E_FILE_FORMAT,
                    format!("cannot read the directory: {e}"),
                    "check the directory's permissions",
                ));
                None
            }
        }
    }

    /// One directory of the working tree: the root (`rel` empty, `depth` 0) or a folder's.
    /// Checks run in the spec's order, and every type check uses `symlink_metadata` before any
    /// open, so a link is never followed and a FIFO never blocks the scan.
    fn wc_dir(
        &mut self,
        dir: &Path,
        rel: &str,
        parent: Option<&BlockId>,
        depth: usize,
        at: &IgnoreAt,
    ) {
        let Some(entries) = self.listing(dir, rel) else {
            return;
        };
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for path in entries {
            let Some(raw) = path.file_name() else {
                continue;
            };
            let lossy = raw.to_string_lossy();
            if hidden(&lossy) {
                continue;
            }
            let here = join(rel, &lossy);
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    self.untrack(here, format!("cannot read: {e}"));
                    continue;
                }
            };
            if self.ignored(at, &lossy, meta.is_dir()) {
                continue;
            }
            if let Some(why) = member_problem(&meta) {
                self.untrack(here, why);
                continue;
            }
            let Some(fname) = raw.to_str() else {
                self.untrack(here, NOT_UTF8);
                continue;
            };
            if meta.is_dir() {
                if rel.is_empty() && fname == BLOBS_DIR {
                    let inner = self.enter(at, fname);
                    self.wc_blobs(&path, &here, &inner);
                } else {
                    self.wc_folder(&path, &here, parent, depth + 1, at, &mut seen);
                }
            } else if fname.ends_with(THEIRS_SUFFIX) {
                self.theirs.push(here);
            } else if fname == FOLDER_FILE {
                continue; // read with its directory
            } else if let Some(stem) = fname.strip_suffix(".yaml") {
                self.wc_doc_file(&path, &here, stem, parent, meta.len(), &mut seen);
            }
            // other files (README.md, code) are not part of the working tree
        }
    }

    /// `_blobs/` at the root: its regular files are figures, never docs.
    fn wc_blobs(&mut self, dir: &Path, rel: &str, at: &IgnoreAt) {
        let Some(entries) = self.listing(dir, rel) else {
            return;
        };
        for path in entries {
            let Some(raw) = path.file_name() else {
                continue;
            };
            let lossy = raw.to_string_lossy();
            if hidden(&lossy) {
                continue;
            }
            let here = join(rel, &lossy);
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    self.untrack(here, format!("cannot read: {e}"));
                    continue;
                }
            };
            if self.ignored(at, &lossy, meta.is_dir()) || meta.is_dir() {
                continue;
            }
            if let Some(why) = member_problem(&meta) {
                self.untrack(here, why);
            } else if raw.to_str().is_none() {
                self.untrack(here, NOT_UTF8);
            } else {
                self.blobs.push(here);
            }
        }
    }

    /// A case-insensitive clash with a name already read in this directory is `E_PATH_CLASH`.
    fn claim(&mut self, seen: &mut BTreeMap<String, String>, name: &str, here: &str) -> bool {
        if let Some(other) = seen.insert(name.to_ascii_lowercase(), here.to_string()) {
            self.diags.push(Diagnostic::error(
                here,
                (1, 1),
                E_PATH_CLASH,
                format!("{here} and {other} have the same name ignoring case"),
                "rename one of them",
            ));
            return false;
        }
        true
    }

    /// A directory: a folder when it holds a `_folder.yaml` that is one folder entry; skipped
    /// with its contents when it holds none; untracked when that file is not one.
    /// `at` is the ignore matcher's state in the directory that holds `dir`.
    fn wc_folder(
        &mut self,
        dir: &Path,
        here: &str,
        parent: Option<&BlockId>,
        depth: usize,
        at: &IgnoreAt,
        seen: &mut BTreeMap<String, String>,
    ) {
        let name = here.rsplit('/').next().unwrap_or(here);
        let ff = dir.join(FOLDER_FILE);
        let ff_rel = format!("{here}/{FOLDER_FILE}");
        let Ok(meta) = std::fs::symlink_metadata(&ff) else {
            return; // no _folder.yaml: not a folder, and nothing under it is read
        };
        let inner = self.enter(at, name);
        if self.ignored(&inner, FOLDER_FILE, meta.is_dir()) {
            return;
        }
        if depth > tree::MAX_FOLDER_DEPTH {
            self.untrack(
                here.to_string(),
                format!(
                    "too deep: at most {} nested folders",
                    tree::MAX_FOLDER_DEPTH
                ),
            );
            return;
        }
        if let Some(why) = member_problem(&meta) {
            self.untrack(ff_rel, why);
            return;
        }
        if !meta.is_file() {
            self.untrack(ff_rel, NOT_REGULAR);
            return;
        }
        if meta.len() > MAX_MEMBER_BYTES as u64 {
            self.untrack(ff_rel, "over 16 MiB");
            return;
        }
        if !self.claim(seen, name, here) {
            return;
        }
        self.check_name(name, here);
        let before = self.diags.len();
        match self.folder_file(&ff, &ff_rel, name, parent) {
            Some(id) => {
                let new_head = self.loaded.last().is_some_and(|l| l.new);
                let hash = Hash::of(self.text.as_bytes());
                self.files.insert(
                    ff_rel,
                    WcFile {
                        kind: FileKind::Folder,
                        head: id.clone(),
                        new_head,
                        hash,
                    },
                );
                self.wc_dir(dir, here, Some(&id), depth, &inner);
            }
            None => {
                let reason = self
                    .diags
                    .get(before)
                    .map(|d| d.message.clone())
                    .unwrap_or_else(|| "not a folder entry".into());
                self.diags.truncate(before);
                self.untrack(ff_rel, reason);
            }
        }
    }

    /// A `*.yaml` file: a doc member when it is a YAML list whose first entry has `type: doc`;
    /// else untracked. Problems in later entries are diagnostics. When the doc entry itself does
    /// not load, the file stays a member under the id it gives (its blocks are not read and the
    /// diagnostics say why), or is untracked when it gives no id that parses.
    fn wc_doc_file(
        &mut self,
        path: &Path,
        here: &str,
        stem: &str,
        parent: Option<&BlockId>,
        len: u64,
        seen: &mut BTreeMap<String, String>,
    ) {
        if len > MAX_MEMBER_BYTES as u64 {
            self.untrack(here.to_string(), "over 16 MiB");
            return;
        }
        let before = self.diags.len();
        let Some((value, pos)) = self.parse(path, here) else {
            let reason = self
                .diags
                .get(before)
                .map(|d| d.message.clone())
                .unwrap_or_else(|| "cannot read the file".into());
            self.diags.truncate(before);
            self.untrack(here.to_string(), reason);
            return;
        };
        let items = match value {
            serde_yaml_ng::Value::Sequence(items)
                if items
                    .first()
                    .and_then(|e| e.get("type"))
                    .and_then(serde_yaml_ng::Value::as_str)
                    == Some(tree::DOC) =>
            {
                items
            }
            _ => {
                self.untrack(here.to_string(), NOT_DOC);
                return;
            }
        };
        if !self.claim(seen, stem, here) {
            return;
        }
        self.check_name(stem, here);
        let hash = Hash::of(self.text.as_bytes());
        let start = self.loaded.len();
        let before = self.diags.len();
        // the id the doc entry gives, read on its own: it keeps the file a member when the rest
        // of the entry does not load (a typo key, a parent or a name field)
        let given = items
            .first()
            .and_then(|e| e.get("id"))
            .and_then(serde_yaml_ng::Value::as_str)
            .and_then(|s| BlockId::parse(s).ok());
        let (head, new_head) = match self.doc_items(items, &pos, here, stem, parent) {
            Some(head) => (head, self.loaded.get(start).is_some_and(|l| l.new)),
            // the doc entry did not load: its diagnostics stay, so status shows the file as
            // changed, add stages it and the commit's lint names the problem
            None => match given {
                Some(id) => {
                    self.taken.insert(id.clone());
                    (id, false)
                }
                // no id to keep: untracked, as a _folder.yaml that does not load is
                None => {
                    let reason = self
                        .diags
                        .get(before)
                        .map(|d| d.message.clone())
                        .unwrap_or_else(|| "the doc entry does not load".into());
                    self.diags.truncate(before);
                    self.untrack(here.to_string(), reason);
                    return;
                }
            },
        };
        self.files.insert(
            here.to_string(),
            WcFile {
                kind: FileKind::Doc,
                head,
                new_head,
                hash,
            },
        );
    }
}
