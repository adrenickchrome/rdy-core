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
use crate::error::{E_LIMIT, RdyError};
use rdy_schema::Pack;
use serde::Serialize;

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
