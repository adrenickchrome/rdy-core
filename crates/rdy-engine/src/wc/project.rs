//! The projection of a view into file texts (MS, FW-D10, FW-D14, FW-F2a): the files `rdy
//! checkout` writes. The walk is export's (the same names, the same `E_PATH_CLASH` refusal); the
//! entries leave out what lives only in the store: supporting blocks and their subtrees, pins,
//! rdy's own fields, store-only links (kept as each block's `hidden`). A reference the author
//! typed as a key is written back as that key.

use crate::error::*;
use crate::io::{BlockYaml, to_yaml_struct};
use crate::keys::key_of;
use crate::tree_io::FOLDER_FILE;
use crate::wc::layout::FileKind;
use crate::wc::state::HiddenLink;
use crate::{Materialized, ReadState, Scope};
use rdy_model::{Block, BlockId, FieldValue, Hash, RefMode, ref_spans, tree};
use rdy_schema::{
    Pack, STORE_ONLY_FIELDS, STORE_ONLY_LINK_KINDS, STORE_ONLY_TARGET_TYPES, TypeClass,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A view as files: what `rdy checkout` writes and what `base` records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projection {
    pub view: String,
    /// The view head's hash, or `canon:<canon head hash>` when the scope is canon.
    pub head: String,
    /// In walk order: a folder's file before what it holds.
    pub files: Vec<ProjectedFile>,
    /// Every written block, by id.
    pub blocks: BTreeMap<String, ProjectedBlock>,
    /// Every blob a written figure names, once, sorted by hash.
    pub blobs: Vec<ProjectedBlob>,
    /// The asked-for ids the scope does not hold.
    pub missing: Vec<String>,
}

/// One doc file or `_folder.yaml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedFile {
    /// The doc or folder the file describes.
    pub id: String,
    pub kind: FileKind,
    /// Relative to the root, `/`-separated.
    pub path: String,
    pub text: String,
}

/// A written block: its version, its file and the links the file does not show.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedBlock {
    pub version: String,
    pub file: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hidden: BTreeMap<String, Vec<HiddenLink>>,
}

/// A blob and its file name under `_blobs/`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedBlob {
    pub hash: String,
    pub name: String,
}

/// The file name of a blob under `_blobs/`: `<hash>.<ext>`, the extension from its media type
/// (`bin` for one rdy does not know).
pub fn blob_name(hash: &str, media_type: Option<&str>) -> String {
    let ext = match media_type {
        Some("image/png") => "png",
        Some("image/jpeg") => "jpg",
        Some("image/gif") => "gif",
        Some("image/svg+xml") => "svg",
        Some("image/webp") => "webp",
        Some("application/pdf") => "pdf",
        _ => "bin",
    };
    format!("{hash}.{ext}")
}

impl ReadState {
    /// wcco1: `scope` as files. With `ids`, only the files of those blocks (a doc or folder: its
    /// own file; any other block: its doc's file); an id the scope does not hold goes to
    /// `missing`.
    pub fn wc_project(
        &self,
        scope: &Scope,
        ids: Option<&[BlockId]>,
    ) -> Result<Projection, RdyError> {
        let m = self.materialized(scope)?;
        let (view, head) = match scope {
            Scope::View(v) => (v.clone(), self.view_head(v).map(|h| h.head.to_string())),
            _ => ("canon".to_string(), None),
        };
        let head = head.unwrap_or_else(|| {
            format!(
                "canon:{}",
                self.canon_head().map(|h| h.to_string()).unwrap_or_default()
            )
        });
        let mut missing = vec![];
        let wanted = ids.map(|ids| {
            let mut heads = BTreeSet::new();
            for id in ids {
                let head = m.get(id).and_then(|b| {
                    if tree::is_tree_type(&b.block_type) {
                        Some(b.id.clone())
                    } else {
                        m.doc_of(id).map(|d| d.id.clone())
                    }
                });
                match head {
                    Some(h) => {
                        heads.insert(h);
                    }
                    None => missing.push(id.to_string()),
                }
            }
            heads
        });
        let mut p = Projector {
            m: &m,
            pack: self.pack(),
            wanted,
            files: vec![],
            blocks: BTreeMap::new(),
            blobs: BTreeMap::new(),
        };
        p.level(None, 0, "")?;
        Ok(Projection {
            view,
            head,
            files: p.files,
            blocks: p.blocks,
            blobs: p
                .blobs
                .into_iter()
                .map(|(hash, name)| ProjectedBlob { hash, name })
                .collect(),
            missing,
        })
    }
}

struct Projector<'a> {
    m: &'a Materialized,
    pack: &'a Pack,
    /// The heads of the files to write; `None` for every file.
    wanted: Option<BTreeSet<BlockId>>,
    files: Vec<ProjectedFile>,
    blocks: BTreeMap<String, ProjectedBlock>,
    /// Hash → file name.
    blobs: BTreeMap<String, String>,
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

impl Projector<'_> {
    fn wants(&self, id: &BlockId) -> bool {
        self.wanted.as_ref().is_none_or(|w| w.contains(id))
    }

    fn supporting(&self, b: &Block) -> bool {
        self.pack.class_of(&b.block_type) == TypeClass::Supporting
    }

    /// The folders and docs under `parent`, written under `dir`, as `export_level` walks them.
    fn level(&mut self, parent: Option<&BlockId>, level: usize, dir: &str) -> Result<(), RdyError> {
        if level > tree::MAX_FOLDER_DEPTH {
            return Ok(());
        }
        let m = self.m;
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
                        "{other} and {} are both named {name:?} (ignoring case) under the same parent, so they cannot be checked out as files",
                        b.id
                    ),
                    "rename one of them, then check out again",
                ));
            }
            if self.supporting(b) {
                continue;
            }
            let rel = join(dir, name);
            if b.block_type == tree::FOLDER {
                if self.wants(&b.id) {
                    let path = format!("{rel}/{FOLDER_FILE}");
                    let head = self.entry(b, true, &path);
                    self.files.push(ProjectedFile {
                        id: b.id.to_string(),
                        kind: FileKind::Folder,
                        text: serde_yaml_ng::to_string(&head).expect("folder yaml"),
                        path,
                    });
                }
                self.level(Some(&b.id), level + 1, &rel)?;
                continue;
            }
            if !self.wants(&b.id) {
                continue;
            }
            let path = format!("{rel}.yaml");
            let mut list = vec![self.entry(b, true, &path)];
            let mut stack = m.children(Some(&b.id));
            stack.reverse();
            while let Some(c) = stack.pop() {
                if self.supporting(c) {
                    continue;
                }
                list.push(self.entry(c, false, &path));
                let mut kids = m.children(Some(&c.id));
                kids.reverse();
                stack.extend(kids);
            }
            self.files.push(ProjectedFile {
                id: b.id.to_string(),
                kind: FileKind::Doc,
                text: serde_yaml_ng::to_string(&list).expect("doc yaml"),
                path,
            });
        }
        Ok(())
    }

    /// Whether a link to `target` lives only in the store: the target's type in the scope, or,
    /// for a target the scope does not hold, the type whose prefix its id has.
    fn store_only_target(&self, target: &BlockId) -> bool {
        let ty = match self.m.get(target) {
            Some(t) => Some(t.block_type.as_str()),
            None => self.pack.type_for_prefix(target.prefix()),
        };
        ty.is_some_and(|t| STORE_ONLY_TARGET_TYPES.contains(&t))
    }

    /// The entry of `b` in the file at `file`, and its record in `blocks` (and `blobs`).
    fn entry(&mut self, b: &Block, head: bool, file: &str) -> BlockYaml {
        let mut c = b.clone();
        c.pins.clear();
        if tree::is_tree_type(&c.block_type) {
            c.fields.remove("name");
        }
        for f in STORE_ONLY_FIELDS {
            c.fields.remove(*f);
        }
        let mut hidden: BTreeMap<String, Vec<HiddenLink>> = BTreeMap::new();
        let mut emptied = vec![];
        for (kind, list) in c.links.iter_mut() {
            let all = STORE_ONLY_LINK_KINDS.contains(&kind.as_str());
            let (hide, keep): (Vec<_>, Vec<_>) = std::mem::take(list)
                .into_iter()
                .partition(|l| all || self.store_only_target(&l.target));
            if !hide.is_empty() {
                hidden.insert(
                    kind.clone(),
                    hide.into_iter()
                        .map(|l| HiddenLink {
                            target: l.target.to_string(),
                            part: l.part,
                        })
                        .collect(),
                );
                if keep.is_empty() {
                    emptied.push(kind.clone());
                }
            }
            *list = keep;
        }
        for kind in emptied {
            c.links.remove(&kind);
        }
        c.body = self.keys_back(&c.body, RefMode::for_body(&c.block_type));
        for v in c.fields.values_mut() {
            self.keys_back_value(v);
        }
        if b.block_type == "figure"
            && let Some(h) = b.field_str("blob").and_then(|s| s.parse::<Hash>().ok())
        {
            let h = h.to_string();
            let name = blob_name(&h, b.field_str("media_type"));
            self.blobs.entry(h).or_insert(name);
        }
        let version = self.m.version_of(&b.id).unwrap_or_else(|| b.version_hash());
        self.blocks.insert(
            b.id.to_string(),
            ProjectedBlock {
                version: version.to_string(),
                file: file.to_string(),
                hidden,
            },
        );
        let mut y = to_yaml_struct(&c);
        if head {
            y.parent = None;
        }
        y
    }

    fn keys_back_value(&self, v: &mut FieldValue) {
        match v {
            FieldValue::Str(s) => *s = self.keys_back(s, RefMode::Strict),
            FieldValue::List(items) => items.iter_mut().for_each(|i| self.keys_back_value(i)),
            FieldValue::Map(m) => m.values_mut().for_each(|i| self.keys_back_value(i)),
            _ => {}
        }
    }

    /// FW-F2a: `text` with each reference stored key-typed (`{{<id>|@key}}`) whose target holds
    /// a key in the scope written `{{<key>}}`; every other reference as stored.
    fn keys_back(&self, text: &str, mode: RefMode) -> String {
        if !text.contains("{{") {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for span in ref_spans(text, mode) {
            if !span.key_typed || span.display.is_some() {
                continue;
            }
            let Some(key) = BlockId::parse(&span.token)
                .ok()
                .and_then(|id| self.m.get(&id))
                .and_then(key_of)
            else {
                continue;
            };
            out.push_str(&text[last..span.range.start]);
            out.push_str(&format!("{{{{{key}}}}}"));
            last = span.range.end;
        }
        out.push_str(&text[last..]);
        out
    }
}
