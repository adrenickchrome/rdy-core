//! Markdown import (D-XR09R, D-19BZD, D-H5MKZ): the deterministic map from a Markdown source
//! tree to prepared folders, docs and blocks, and the report of what it could not map.
//!
//! [`convert`] is pure. It takes logical relative paths and captured bytes, never a native path,
//! and reads no file, store or network. It infers nothing: no decision, status, author, relation
//! or audience comes from the words. Folders become folders and files become docs; headings
//! become sections, paragraphs, lists and other running text become prose, fenced and indented
//! code becomes code, and tables become tables. A local link becomes a reference when it names
//! exactly one included Markdown file (and heading); everything else that cannot map stays as its
//! literal source text with an [`Unmapped`] row.
//!
//! The prepared values carry no block id and no key: the importer gives the ids, and keys come
//! from each doc's and folder's path through the existing key routine. A reference names its
//! target by position ([`Target`]); [`Text::render`] writes it with the target's id.
//!
//! [`Rdy::preview_markdown`] and [`Rdy::import_markdown`] capture a local source directory
//! ([`files::capture`]), check the digest a preview gave, convert the captured bytes, and pass
//! the one candidate to the shared importer: the preview runs its checks without append, view
//! claim or briefing; the import stages and seals it once. The source is never read again after
//! the capture.
use crate::error::{E_BAD_INPUT, E_INTERNAL, E_LIMIT, E_VALIDATION, RdyError};
use crate::import_prepared::PreparedImport;
use crate::write::WriteResult;
use crate::{Materialized, Rdy, Scope, Session};
use rdy_model::{Block, BlockId, FieldValue, tree};
use rdy_schema::Pack;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub mod files;
mod parse;

/// Source regular files, including reported non-Markdown files.
pub const MAX_SOURCE_FILES: usize = 4_096;
/// Markdown bytes per file.
pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
/// Total captured Markdown and ignore bytes.
pub const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
/// Front matter per file: the bytes between its two fence lines.
pub const MAX_FRONT_MATTER_BYTES: usize = 65_536;
/// Generated blocks: folders, docs and their content.
pub const MAX_BLOCKS: usize = 100_000;
/// Parser nesting: open block and inline elements at once.
pub const MAX_NESTING: usize = 128;
/// Bytes of one logical relative path.
pub const MAX_PATH_BYTES: usize = 4_096;
/// `unmapped` rows; past this the import refuses rather than hide an omission.
pub const MAX_UNMAPPED: usize = 10_000;

/// One bound of a Markdown import: its table label and its limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bound {
    pub label: &'static str,
    pub limit: usize,
}

pub const SOURCE_FILES: Bound = Bound {
    label: "Source regular files, including reported non-Markdown files",
    limit: MAX_SOURCE_FILES,
};
pub const FILE_BYTES: Bound = Bound {
    label: "Markdown bytes per file",
    limit: MAX_FILE_BYTES,
};
pub const TOTAL_BYTES: Bound = Bound {
    label: "Total captured Markdown and ignore bytes",
    limit: MAX_TOTAL_BYTES,
};
pub const FRONT_MATTER_BYTES: Bound = Bound {
    label: "Front matter per file",
    limit: MAX_FRONT_MATTER_BYTES,
};
pub const BLOCKS: Bound = Bound {
    label: "Generated blocks",
    limit: MAX_BLOCKS,
};
pub const FOLDER_DEPTH: Bound = Bound {
    label: "Folder depth",
    limit: rdy_model::tree::MAX_FOLDER_DEPTH,
};
pub const NESTING: Bound = Bound {
    label: "Parser nesting",
    limit: MAX_NESTING,
};
pub const PATH_BYTES: Bound = Bound {
    label: "Relative path bytes",
    limit: MAX_PATH_BYTES,
};
pub const UNMAPPED_ROWS: Bound = Bound {
    label: "Unmapped rows",
    limit: MAX_UNMAPPED,
};

/// `E_LIMIT` for a Markdown import past `bound`.
pub fn limit_error(bound: Bound) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("Markdown import exceeds {}: {}", bound.label, bound.limit),
        "split the source into smaller import directories",
    )
}

/// One included Markdown file: its logical path under the source root (`/` between segments)
/// and the bytes captured from it. The caller bounds the file count and sizes ([`SOURCE_FILES`],
/// [`FILE_BYTES`], [`TOTAL_BYTES`]) while it captures them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// What a reference points at: a doc of [`Prepared::docs`], or one block of that doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Target {
    /// The index in [`Prepared::docs`].
    pub doc: usize,
    /// The index in that doc's [`PreparedDoc::blocks`] (always a section), or `None` for the doc.
    pub block: Option<usize>,
}

/// One piece of a text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    /// Text in its stored form: every source `{{` already escaped as `\{{`, so it binds to
    /// nothing.
    Literal(String),
    /// A reference to an imported block, with its display text in stored form (a leading `@`
    /// escaped as `\@`).
    Ref {
        target: Target,
        display: Option<String>,
    },
}

/// A body, title or table cell: literal text and references to imported blocks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Text {
    pub segments: Vec<Segment>,
}

impl Text {
    /// The stored text, each reference written as `{{id}}` or `{{id|display}}` with the id
    /// `id_of` gives its target.
    pub fn render(&self, id_of: &dyn Fn(&Target) -> String) -> String {
        let mut out = String::new();
        for s in &self.segments {
            match s {
                Segment::Literal(t) => out.push_str(t),
                Segment::Ref { target, display } => {
                    out.push_str("{{");
                    out.push_str(&id_of(target));
                    if let Some(d) = display {
                        out.push('|');
                        out.push_str(d);
                    }
                    out.push_str("}}");
                }
            }
        }
        out
    }

    /// The targets this text refers to, in order.
    pub fn targets(&self) -> impl Iterator<Item = &Target> {
        self.segments.iter().filter_map(|s| match s {
            Segment::Ref { target, .. } => Some(target),
            Segment::Literal(_) => None,
        })
    }

    pub(crate) fn literal(text: String) -> Text {
        Text {
            segments: if text.is_empty() {
                vec![]
            } else {
                vec![Segment::Literal(text)]
            },
        }
    }
}

/// A source directory: a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedFolder {
    /// The logical path of the directory.
    pub path: String,
    /// The index of its parent in [`Prepared::folders`]; `None` at the source root.
    pub parent: Option<usize>,
    /// Its rdy name: unique under its parent, ignoring case, among folders and docs.
    pub name: String,
}

/// A source Markdown file: a doc and its content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedDoc {
    /// The logical path of the file.
    pub path: String,
    /// The index of its folder in [`Prepared::folders`]; `None` at the source root.
    pub parent: Option<usize>,
    /// Its rdy name: unique under its parent, ignoring case, among folders and docs.
    pub name: String,
    /// From front matter, else the first heading, else the file name.
    pub title: Text,
    /// From front matter, else the pack's default doc type.
    pub doc_type: String,
    /// Its content blocks in source order; each block's parent comes before it.
    pub blocks: Vec<PreparedBlock>,
}

/// One content block of a doc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedBlock {
    /// The index of its section in the same doc's blocks; `None` for a direct child of the doc.
    pub parent: Option<usize>,
    pub content: PreparedContent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreparedContent {
    /// A heading: a section titled with its text.
    Section { title: Text },
    /// Running text: its inline Markdown kept as written.
    Prose { body: Text },
    /// Fenced or indented code; code reads no references, so its body is the source as written.
    Code { lang: Option<String>, body: String },
    /// A table: header cells and body rows.
    Table {
        columns: Vec<Text>,
        rows: Vec<Vec<Text>>,
    },
}

impl PreparedContent {
    /// The block type it maps to in the pack.
    pub fn block_type(&self) -> &'static str {
        match self {
            PreparedContent::Section { .. } => "section",
            PreparedContent::Prose { .. } => "prose",
            PreparedContent::Code { .. } => "code",
            PreparedContent::Table { .. } => "table",
        }
    }
}

/// What a Markdown import could not map (D-19BZD, D-2R9EY).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnmappedKind {
    Html,
    Image,
    Extension,
    NonMarkdownFile,
    Link,
    FrontMatter,
}

impl UnmappedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UnmappedKind::Html => "html",
            UnmappedKind::Image => "image",
            UnmappedKind::Extension => "extension",
            UnmappedKind::NonMarkdownFile => "non_markdown_file",
            UnmappedKind::Link => "link",
            UnmappedKind::FrontMatter => "front_matter",
        }
    }
}

/// One `unmapped` row: `{path, line, kind}`, `line` one-based in the source file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Unmapped {
    pub path: String,
    pub line: usize,
    pub kind: UnmappedKind,
}

/// The converted source: folders and docs in path order (UTF-8 bytes), and the rows of what
/// could not map, ordered by path, line and kind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prepared {
    pub folders: Vec<PreparedFolder>,
    pub docs: Vec<PreparedDoc>,
    pub unmapped: Vec<Unmapped>,
}

impl Prepared {
    /// Every block the import would create: folders, docs and content blocks.
    pub fn block_count(&self) -> usize {
        self.folders.len()
            + self.docs.len()
            + self.docs.iter().map(|d| d.blocks.len()).sum::<usize>()
    }
}

/// Converts the source into prepared blocks and its `unmapped` rows. `dirs` lists the included
/// directories (empty ones too; a file's parent directories are included without being listed),
/// and `files` the included Markdown files with their captured bytes, in any order.
///
/// Refuses, before building anything past the bound: a path over [`PATH_BYTES`] or a directory
/// deeper than [`FOLDER_DEPTH`] (`E_LIMIT`); a path that is not a plain relative path, or two
/// entries with one path (`E_BAD_INPUT`); bytes that are not UTF-8 (`E_FILE_FORMAT`, as the export
/// import's unreadable file); front matter over [`FRONT_MATTER_BYTES`], nesting over
/// [`NESTING`], more than [`BLOCKS`] blocks or more than [`UNMAPPED_ROWS`] rows (`E_LIMIT`); a
/// front-matter `doc_type` the pack does not hold (`E_DOC_TYPE`); and a pack with no default doc
/// type when a file names none.
pub fn convert(pack: &Pack, dirs: &[String], files: &[MarkdownFile]) -> Result<Prepared, RdyError> {
    parse::convert(pack, dirs, files)
}

/// `E_BAD_INPUT` unless `digest` has the form of a source digest: 64 lowercase hexadecimal
/// characters.
pub fn check_expected_source(digest: &str) -> Result<(), RdyError> {
    let ok = digest.len() == 64
        && digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if ok {
        return Ok(());
    }
    Err(RdyError::new(
        E_BAD_INPUT,
        "expected_source must be 64 lowercase hexadecimal characters",
        "use source_digest from a Markdown import preview",
    ))
}

/// One `files` row: an imported Markdown file's logical path, the key its doc takes, and the
/// blocks it creates (the doc and its content blocks), a count rather than an identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileRow {
    pub path: String,
    pub doc_key: String,
    pub blocks: usize,
}

/// A Markdown import preview: what the import would create, with no write receipt or change id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MarkdownPreview {
    /// Always `true`.
    pub dry_run: bool,
    pub source_digest: String,
    pub files: Vec<FileRow>,
    /// Every block the import would create: folders, docs and their content.
    pub blocks: usize,
    pub unmapped: Vec<Unmapped>,
}

/// A Markdown import: the import's write report, with the source digest, the files and the
/// `unmapped` rows.
#[derive(Clone, Debug, Serialize)]
pub struct MarkdownImported {
    #[serde(flatten)]
    pub write: WriteResult,
    pub source_digest: String,
    pub files: Vec<FileRow>,
    pub unmapped: Vec<Unmapped>,
}

/// A captured and converted source, decoded into the shared importer's input.
struct Candidate {
    prepared: PreparedImport,
    source_digest: String,
    /// Each doc's path, id and block count, in path order.
    docs: Vec<(String, BlockId, usize)>,
    blocks: usize,
    unmapped: Vec<Unmapped>,
}

impl Rdy {
    /// Previews the Markdown import of the source directory `dir` into `view` as `s`: the same
    /// capture, digest check, conversion and checks as [`Rdy::import_markdown`], against the
    /// view as it is, with no append, no view claim and no briefing registered.
    pub fn preview_markdown(
        &self,
        s: &Session,
        view: &str,
        dir: &Path,
        expected_source: Option<&str>,
    ) -> Result<MarkdownPreview, RdyError> {
        let c = self.markdown_candidate(s, view, dir, expected_source)?;
        let planned = self.plan_import(s, view, c.prepared)?;
        Ok(MarkdownPreview {
            dry_run: true,
            files: file_rows(&c.docs, &planned.keys),
            source_digest: c.source_digest,
            blocks: c.blocks,
            unmapped: c.unmapped,
        })
    }

    /// Imports the Markdown source directory `dir` into `view` as `s`: one capture, then (with
    /// `expected_source`) the digest check before anything else, then one candidate through the
    /// shared importer, which stages it in one batch and seals it once. Fresh blocks name the
    /// importing session as their author and importer.
    pub fn import_markdown(
        &mut self,
        s: &Session,
        view: &str,
        dir: &Path,
        expected_source: Option<&str>,
    ) -> Result<MarkdownImported, RdyError> {
        let c = self.markdown_candidate(s, view, dir, expected_source)?;
        let write = self.import_prepared(s, view, c.prepared)?;
        Ok(MarkdownImported {
            files: file_rows(&c.docs, &write.keys),
            write,
            source_digest: c.source_digest,
            unmapped: c.unmapped,
        })
    }

    /// The checks before the source is read (the digest's form, the store's write gate, the
    /// writer role and the view's access), then the capture, the digest check and the
    /// conversion into the importer's input.
    fn markdown_candidate(
        &self,
        s: &Session,
        view: &str,
        dir: &Path,
        expected_source: Option<&str>,
    ) -> Result<Candidate, RdyError> {
        if let Some(d) = expected_source {
            check_expected_source(d)?;
        }
        self.ensure_writable()?;
        self.check_import_access(s, view)?;
        let captured = files::capture(dir)?;
        if expected_source.is_some_and(|d| d != captured.source_digest) {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "the Markdown source changed after preview",
                "preview the source again and review the new report",
            ));
        }
        let mut prepared = convert(&self.pack, &captured.dirs, &captured.files)?;
        // D-2R9EY: a reported file has no source line
        let reported = captured.reported.iter().map(|path| Unmapped {
            path: path.clone(),
            line: 0,
            kind: UnmappedKind::NonMarkdownFile,
        });
        if prepared.unmapped.len() + captured.reported.len() > MAX_UNMAPPED {
            return Err(limit_error(UNMAPPED_ROWS));
        }
        prepared.unmapped.extend(reported);
        prepared.unmapped.sort_by(|a, b| {
            (a.path.as_bytes(), a.line, a.kind).cmp(&(b.path.as_bytes(), b.line, b.kind))
        });
        let existing = self.read().materialized(&Scope::View(view.to_string()))?;
        let (blocks, docs) = to_blocks(&self.pack, &prepared, &existing)?;
        let count = blocks.len();
        let label = std::fs::canonicalize(dir)
            .ok()
            .and_then(|c| c.file_name()?.to_str().map(str::to_string));
        Ok(Candidate {
            prepared: PreparedImport {
                keyed: (0..count).map(|_| vec![]).collect(),
                blocks,
                authors: Ok(None),
                blobs: vec![],
                blob_hashes: BTreeSet::new(),
                label,
            },
            source_digest: captured.source_digest,
            docs,
            blocks: count,
            unmapped: prepared.unmapped,
        })
    }
}

/// The `files` rows: each doc's path, its key from `keys` (by id) and its block count.
fn file_rows(docs: &[(String, BlockId, usize)], keys: &BTreeMap<String, String>) -> Vec<FileRow> {
    docs.iter()
        .map(|(path, id, blocks)| FileRow {
            path: path.clone(),
            doc_key: keys.get(&id.to_string()).cloned().unwrap_or_default(),
            blocks: *blocks,
        })
        .collect()
}

/// The prefix of `block_type` in `pack`, or the refusal of a type the pack does not define: the
/// import never guesses another type.
fn prefix_of(pack: &Pack, block_type: &str, path: &str) -> Result<String, RdyError> {
    pack.block_types
        .get(block_type)
        .map(|d| d.prefix.clone())
        .filter(|p| BlockId::parse(&format!("{p}-00000")).is_ok())
        .ok_or_else(|| {
            RdyError::new(
                E_VALIDATION,
                format!("{path}: unknown block type {block_type:?}"),
                "use a block type the project's pack defines",
            )
        })
}

/// Fresh ids: none the import already gave, none the view holds.
struct Ids<'a> {
    taken: BTreeSet<BlockId>,
    existing: &'a Materialized,
}

impl Ids<'_> {
    fn fresh(&mut self, prefix: &str) -> BlockId {
        loop {
            let id = BlockId::new(prefix);
            if self.existing.get(&id).is_none() && self.taken.insert(id.clone()) {
                return id;
            }
        }
    }
}

/// `n` order keys after `lo`, increasing.
fn orders(lo: Option<&str>, n: usize) -> Result<Vec<String>, RdyError> {
    rdy_model::order_run(lo, None, n).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("internal error: no order keys for the import: {e}"),
            "report this as a bug in rdy",
        )
    })
}

/// A block of `block_type` with `fields` and `body`, and no links, pins or foreign part.
fn block(
    id: BlockId,
    block_type: &str,
    parent: Option<BlockId>,
    order: String,
    fields: BTreeMap<String, FieldValue>,
    body: String,
) -> Block {
    Block {
        id,
        block_type: block_type.to_string(),
        parent,
        order,
        fields,
        body,
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

/// The prepared source as blocks with fresh ids, parents and order keys: folders and docs in
/// path order (a top-level one after the view's last top-level block), each doc's content in
/// source order. A reference is written with its target's id. Also returns each doc's path, id
/// and block count.
#[allow(clippy::type_complexity)]
fn to_blocks(
    pack: &Pack,
    p: &Prepared,
    existing: &Materialized,
) -> Result<(Vec<Block>, Vec<(String, BlockId, usize)>), RdyError> {
    let mut ids = Ids {
        taken: BTreeSet::new(),
        existing,
    };
    let folder_prefix = prefix_of(pack, tree::FOLDER, ".")?;
    let doc_prefix = prefix_of(pack, tree::DOC, ".")?;
    let folder_ids: Vec<BlockId> = p
        .folders
        .iter()
        .map(|_| ids.fresh(&folder_prefix))
        .collect();
    let doc_ids: Vec<BlockId> = p.docs.iter().map(|_| ids.fresh(&doc_prefix)).collect();
    let mut block_ids: Vec<Vec<BlockId>> = Vec::with_capacity(p.docs.len());
    for d in &p.docs {
        let mut these = Vec::with_capacity(d.blocks.len());
        for b in &d.blocks {
            these.push(ids.fresh(&prefix_of(pack, b.content.block_type(), &d.path)?));
        }
        block_ids.push(these);
    }
    let id_of = |t: &Target| -> String {
        match t.block {
            None => doc_ids[t.doc].to_string(),
            Some(b) => block_ids[t.doc][b].to_string(),
        }
    };
    // folders and docs: siblings in path order under each folder
    enum Node {
        Folder(usize),
        Doc(usize),
    }
    let mut tree_nodes: Vec<(&str, Option<usize>, Node)> = p
        .folders
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.as_str(), f.parent, Node::Folder(i)))
        .chain(
            p.docs
                .iter()
                .enumerate()
                .map(|(i, d)| (d.path.as_str(), d.parent, Node::Doc(i))),
        )
        .collect();
    tree_nodes.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut siblings: BTreeMap<Option<usize>, usize> = BTreeMap::new();
    for (_, parent, _) in &tree_nodes {
        *siblings.entry(*parent).or_default() += 1;
    }
    let last_top = existing
        .children(None)
        .into_iter()
        .map(|b| b.order.clone())
        .max();
    let mut keys: BTreeMap<Option<usize>, std::vec::IntoIter<String>> = BTreeMap::new();
    for (parent, n) in siblings {
        let lo = if parent.is_none() {
            last_top.as_deref()
        } else {
            None
        };
        keys.insert(parent, orders(lo, n)?.into_iter());
    }
    let mut out = Vec::with_capacity(p.block_count());
    let mut docs = Vec::with_capacity(p.docs.len());
    for (_, parent, node) in tree_nodes {
        let order = keys
            .get_mut(&parent)
            .and_then(Iterator::next)
            .expect("one key per sibling");
        let parent_id = parent.map(|i| folder_ids[i].clone());
        match node {
            Node::Folder(i) => {
                let fields = BTreeMap::from([(
                    "name".to_string(),
                    FieldValue::Str(p.folders[i].name.clone()),
                )]);
                out.push(block(
                    folder_ids[i].clone(),
                    tree::FOLDER,
                    parent_id,
                    order,
                    fields,
                    String::new(),
                ));
            }
            Node::Doc(i) => {
                let d = &p.docs[i];
                let fields = BTreeMap::from([
                    ("name".to_string(), FieldValue::Str(d.name.clone())),
                    ("title".to_string(), FieldValue::Str(d.title.render(&id_of))),
                    ("doc_type".to_string(), FieldValue::Str(d.doc_type.clone())),
                ]);
                out.push(block(
                    doc_ids[i].clone(),
                    tree::DOC,
                    parent_id,
                    order,
                    fields,
                    String::new(),
                ));
                docs.push((d.path.clone(), doc_ids[i].clone(), 1 + d.blocks.len()));
                content(d, &doc_ids[i], &block_ids[i], &id_of, &mut out)?;
            }
        }
    }
    Ok((out, docs))
}

/// The content blocks of `d` under the doc `doc`, with `ids` (one per block) and order keys per
/// parent in source order.
fn content(
    d: &PreparedDoc,
    doc: &BlockId,
    ids: &[BlockId],
    id_of: &dyn Fn(&Target) -> String,
    out: &mut Vec<Block>,
) -> Result<(), RdyError> {
    let mut siblings: BTreeMap<Option<usize>, usize> = BTreeMap::new();
    for b in &d.blocks {
        *siblings.entry(b.parent).or_default() += 1;
    }
    let mut keys: BTreeMap<Option<usize>, std::vec::IntoIter<String>> = BTreeMap::new();
    for (parent, n) in siblings {
        keys.insert(parent, orders(None, n)?.into_iter());
    }
    let cells = |row: &[Text]| {
        FieldValue::List(
            row.iter()
                .map(|t| FieldValue::Str(t.render(id_of)))
                .collect(),
        )
    };
    for (i, b) in d.blocks.iter().enumerate() {
        let order = keys
            .get_mut(&b.parent)
            .and_then(Iterator::next)
            .expect("one key per sibling");
        let parent = Some(b.parent.map_or_else(|| doc.clone(), |s| ids[s].clone()));
        let mut fields = BTreeMap::new();
        let mut body = String::new();
        match &b.content {
            PreparedContent::Section { title } => {
                fields.insert("title".to_string(), FieldValue::Str(title.render(id_of)));
            }
            PreparedContent::Prose { body: text } => body = text.render(id_of),
            PreparedContent::Code { lang, body: code } => {
                if let Some(l) = lang {
                    fields.insert("lang".to_string(), FieldValue::Str(l.clone()));
                }
                body = code.clone();
            }
            PreparedContent::Table { columns, rows } => {
                fields.insert("columns".to_string(), cells(columns));
                fields.insert(
                    "rows".to_string(),
                    FieldValue::List(rows.iter().map(|r| cells(r)).collect()),
                );
            }
        }
        out.push(block(
            ids[i].clone(),
            b.content.block_type(),
            parent,
            order,
            fields,
            body,
        ));
    }
    Ok(())
}
