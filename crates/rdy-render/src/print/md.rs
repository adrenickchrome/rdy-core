//! The print as a tree of Markdown files (D-K7ZFK, O28, O29).
//!
//! One file per page of the print, at the page's site path with `.md` for `.html`; each folder is
//! its folder's `index.md`; the glossary is `glossary.md`; each figure is a file under `assets/`
//! with the site's name for it. The title part is `index.md` at the root. Every file starts with
//! front matter, then a `# <title>` line. A reference is a relative link to a file and an anchor;
//! a block's id appears in no file but in the front matter of its folder or doc (O4). With
//! [`Layout::Mkdocs`] the tree sits under `docs/` and `mkdocs.yml` names the tree as `nav`.
//!
//! The tree is written to a staging directory beside `out` and renamed into place, so `out` holds
//! the whole tree or nothing. The module makes portable `std::fs` calls only (D-1RXEA).
use super::{Href, PBlock, PageKind, Print, PrintDoc, PrintSummary, Run};
use crate::llms::md_escape;
use rdy_engine::{E_BAD_INPUT, E_IO, RdyError};
use rdy_model::Hash;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// How the tree is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// The files of the print, directly under `out`.
    Plain,
    /// The files under `<out>/docs/`, and `<out>/mkdocs.yml` with the tree as `nav`.
    Mkdocs,
}

/// A list item's content starts in this column or later, so a block inside it keeps its
/// indentation in every reader (CommonMark needs the content's column, Python-Markdown four
/// spaces).
const ITEM_COLUMN: usize = 4;

/// The `.md` path of a site page.
fn md_path(page: &str) -> String {
    format!("{}.md", page.strip_suffix(".html").unwrap_or(page))
}

/// The path to `to` (a path in the tree) from the file `from` (a path in the tree).
fn relative(from: &str, to: &str) -> String {
    let mut dir: Vec<&str> = from.split('/').collect();
    dir.pop();
    let target: Vec<&str> = to.split('/').collect();
    let common = dir
        .iter()
        .zip(&target[..target.len() - 1])
        .take_while(|(a, b)| a == b)
        .count();
    let ups = "../".repeat(dir.len() - common);
    format!("{ups}{}", target[common..].join("/"))
}

/// What a page needs to write its blocks.
struct Cx<'a> {
    /// The `.md` path of the file being written.
    file: &'a str,
    /// The figures of the print.
    blobs: &'a BTreeMap<Hash, super::Blob>,
}

/// A link target as Markdown reads it inside `(…)`.
fn url(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            '\n' | '\r' | '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// A piece of code as an inline code span: its delimiter is longer than any backtick run inside.
fn code_span(s: &str) -> String {
    let delim = "`".repeat(longest_backticks(s) + 1);
    if s.starts_with('`') || s.ends_with('`') {
        format!("{delim} {s} {delim}")
    } else {
        format!("{delim}{s}{delim}")
    }
}

fn longest_backticks(s: &str) -> usize {
    let (mut best, mut run) = (0, 0);
    for c in s.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        best = best.max(run);
    }
    best
}

impl Cx<'_> {
    /// The words of one run's piece of a line, in the run's look.
    fn piece(&self, run: &Run, piece: &str, line_start: bool) -> String {
        let core = piece.trim();
        if core.is_empty() {
            return piece.to_string();
        }
        let lead = &piece[..piece.len() - piece.trim_start().len()];
        let trail = &piece[piece.trim_end().len()..];
        let plain = !(run.code || run.bold || run.italic || run.link.is_some());
        let mut out = if run.code {
            code_span(core)
        } else {
            md_escape(core, line_start && plain)
        };
        out = match (run.bold, run.italic) {
            (true, true) => format!("***{out}***"),
            (true, false) => format!("**{out}**"),
            (false, true) => format!("*{out}*"),
            (false, false) => out,
        };
        if let Some(href) = &run.link {
            let target = match href {
                Href::External(u) => url(u),
                Href::Page { page, anchor } => {
                    let mut t = relative(self.file, &md_path(page));
                    if let Some(a) = anchor {
                        t.push('#');
                        t.push_str(a);
                    }
                    url(&t)
                }
            };
            out = format!("[{out}]({target})");
        }
        format!("{lead}{out}{trail}")
    }

    /// Runs as one piece of Markdown text; a line break in a run is `br`.
    fn inline(&self, runs: &[Run], br: &str, line_start: bool) -> String {
        let mut out = String::new();
        let mut start = line_start;
        for run in runs {
            for (i, piece) in run.text.split('\n').enumerate() {
                if i > 0 {
                    out.push_str(br);
                    start = br.contains('\n');
                }
                if piece.is_empty() {
                    continue;
                }
                out.push_str(&self.piece(run, piece, start));
                start = false;
            }
        }
        out
    }

    /// The lines of `blocks`, one blank line between blocks.
    fn blocks(&self, blocks: &[PBlock]) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        for b in blocks {
            let lines = self.block(b);
            if lines.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(String::new());
            }
            out.extend(lines);
        }
        out
    }

    fn block(&self, b: &PBlock) -> Vec<String> {
        match b {
            PBlock::Heading {
                level,
                anchor,
                runs,
            } => {
                let mut text = self.inline(runs, " ", false);
                if text.ends_with('#') {
                    text.pop();
                    text.push_str("\\#");
                }
                let hashes = "#".repeat(usize::from((*level).clamp(1, 6)));
                let line = format!("{hashes} {text}");
                anchored(anchor, true, vec![line.trim_end().to_string()])
            }
            PBlock::Para { anchor, runs } => {
                let text = self.inline(runs, "\\\n", true);
                let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
                anchored(anchor, true, lines)
            }
            PBlock::List {
                ordered,
                start,
                items,
            } => self.list(*ordered, *start, items),
            PBlock::Table { anchor, head, rows } => anchored(anchor, false, self.table(head, rows)),
            PBlock::Code { anchor, lang, text } => {
                let fence = "`".repeat((longest_backticks(text) + 1).max(3));
                let info = lang
                    .as_deref()
                    .filter(|l| !l.chars().any(|c| c.is_whitespace() || c == '`'))
                    .unwrap_or("");
                let mut lines = vec![format!("{fence}{info}")];
                if !text.is_empty() {
                    lines.extend(text.split('\n').map(str::to_string));
                }
                lines.push(fence);
                anchored(anchor, false, lines)
            }
            PBlock::Figure {
                anchor,
                blob,
                alt,
                caption,
            } => {
                let mut lines = vec![];
                let file = blob.and_then(|h| self.blobs.get(&h)).map(|b| &b.file);
                let alt_text = alt.split_whitespace().collect::<Vec<_>>().join(" ");
                if let Some(file) = file {
                    lines.push(format!(
                        "![{}]({})",
                        md_escape(&alt_text, false),
                        url(&relative(self.file, file))
                    ));
                }
                let caption = self.inline(caption, "\\\n", true);
                if file.is_none() && caption.is_empty() && !alt_text.is_empty() {
                    lines.push(md_escape(&alt_text, true));
                }
                if !caption.is_empty() {
                    if !lines.is_empty() {
                        lines.push(String::new());
                    }
                    lines.extend(caption.split('\n').map(str::to_string));
                }
                anchored(anchor, false, lines)
            }
            PBlock::Callout {
                anchor,
                label,
                blocks,
            } => {
                let mut lines = vec![];
                let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
                let inner = self.blocks(blocks);
                if !label.is_empty() {
                    lines.push(format!("> **{}**", md_escape(&label, false)));
                    if !inner.is_empty() {
                        lines.push(">".to_string());
                    }
                }
                lines.extend(inner.into_iter().map(|l| {
                    if l.is_empty() {
                        ">".to_string()
                    } else {
                        format!("> {l}")
                    }
                }));
                anchored(anchor, false, lines)
            }
        }
    }

    fn list(&self, ordered: bool, start: u32, items: &[Vec<PBlock>]) -> Vec<String> {
        let mut out = vec![];
        for (i, item) in items.iter().enumerate() {
            let mark = if ordered {
                format!("{}.", u64::from(start) + i as u64)
            } else {
                "-".to_string()
            };
            let width = (mark.len() + 1).max(ITEM_COLUMN);
            let marker = format!("{mark:<width$}");
            let mut lines = self.blocks(item).into_iter();
            match lines.next() {
                Some(first) => out.push(format!("{marker}{first}")),
                None => out.push(mark),
            }
            let pad = " ".repeat(width);
            out.extend(lines.map(|l| if l.is_empty() { l } else { format!("{pad}{l}") }));
        }
        out
    }

    /// A pipe table: a cell writes `|` as `\|` and joins its lines with one space (O28 (e)).
    fn table(&self, head: &[Vec<Run>], rows: &[Vec<Vec<Run>>]) -> Vec<String> {
        let cols = rows
            .iter()
            .map(Vec::len)
            .chain([head.len()])
            .max()
            .unwrap_or(0);
        if cols == 0 {
            return vec![];
        }
        let row = |cells: &[Vec<Run>]| {
            let mut texts: Vec<String> = cells
                .iter()
                .map(|c| self.inline(c, " ", false).replace('|', "\\|"))
                .collect();
            texts.resize(cols, String::new());
            format!("| {} |", texts.join(" | "))
        };
        let mut lines = vec![row(head), format!("|{}", " --- |".repeat(cols))];
        lines.extend(rows.iter().map(|r| row(r)));
        lines
    }
}

/// `lines` after the line that names their anchor. A heading and a paragraph follow it directly;
/// any other block follows a blank line, so a reader that needs the blank line finds it.
fn anchored(anchor: &Option<String>, adjacent: bool, lines: Vec<String>) -> Vec<String> {
    let Some(a) = anchor else { return lines };
    let mut out = vec![format!("<a id=\"{}\"></a>", a.replace('"', "&quot;"))];
    if !lines.is_empty() && !adjacent {
        out.push(String::new());
    }
    out.extend(lines);
    out
}

/// A string as YAML reads it: a JSON string (O28 (c)).
fn json(s: &str) -> String {
    serde_json::to_string(s).expect("a string is JSON")
}

/// One line of text with the Markdown it would otherwise open escaped.
fn text_line(s: &str, line_start: bool) -> String {
    md_escape(
        &s.split_whitespace().collect::<Vec<_>>().join(" "),
        line_start,
    )
}

/// The file of a folder, doc or the glossary.
fn page_file(p: &Print, d: &PrintDoc) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("title: {}\n", json(&d.title)));
    if d.kind != PageKind::Glossary {
        out.push_str(&format!("rdy_id: {}\n", d.id));
    }
    out.push_str("---\n\n");
    out.push_str(&format!("# {}\n", text_line(&d.title, false)));
    let file = md_path(&d.page);
    let cx = Cx {
        file: &file,
        blobs: &p.blobs,
    };
    let lines = cx.blocks(&d.blocks);
    if !lines.is_empty() {
        out.push('\n');
        out.push_str(&lines.join("\n"));
        out.push('\n');
    }
    out
}

/// The title part (O2, O28 (a)).
fn title_file(p: &Print) -> String {
    let mut out = format!(
        "---\ntitle: {}\n---\n\n# {}\n",
        json(&p.title),
        text_line(&p.title, false)
    );
    if let Some(d) = p
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        let paras: Vec<String> = d
            .split("\n\n")
            .map(|para| {
                para.lines()
                    .map(|l| md_escape(l.trim(), true))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect();
        out.push('\n');
        out.push_str(&paras.join("\n\n"));
        out.push('\n');
    }
    out.push_str(&format!("\nVersion: {}\n", text_line(&p.label, false)));
    out.push_str(&format!("\nScope: {}\n", text_line(&p.scope, false)));
    out
}

/// `mkdocs.yml`: the site's name, then the tree as `nav` (O28 (d)).
fn mkdocs_yml(p: &Print) -> String {
    let mut out = format!("site_name: {}\nnav:\n", json(&p.title));
    out.push_str(&format!("  - {}: index.md\n", json(&p.title)));
    for d in &p.docs {
        let pad = " ".repeat(2 + 4 * d.depth);
        let (title, path) = (json(&d.title), md_path(&d.page));
        match d.kind {
            PageKind::Folder => {
                let inner = " ".repeat(2 + 4 * (d.depth + 1));
                out.push_str(&format!("{pad}- {title}:\n{inner}- {title}: {path}\n"));
            }
            PageKind::Doc => out.push_str(&format!("{pad}- {title}: {path}\n")),
            PageKind::Glossary => out.push_str("  - Glossary: glossary.md\n"),
        }
    }
    out
}

fn io(path: &Path, e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("{}: {e}", path.display()),
        "check the output directory",
    )
}

/// Writes one file, durably.
fn put(path: &Path, bytes: &[u8]) -> Result<(), RdyError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    }
    let mut file = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| io(path, e))?;
    file.write_all(bytes).map_err(|e| io(path, e))?;
    file.sync_all().map_err(|e| io(path, e))
}

/// The refusals of O29 and the Markdown export's own. `Ok(true)` when `out` is an empty
/// directory, which the rename replaces.
fn check_out(out: &Path, parent: &Path, name: &std::ffi::OsStr) -> Result<bool, RdyError> {
    if !parent.exists() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{} does not exist", parent.display()),
            "create the directory first, or pick another path",
        ));
    }
    // a file in `.rdy` can break the store: refuse `out` there and below it. The check reads the
    // resolved path, so a symlink or `..` does not pass
    let resolved = std::fs::canonicalize(parent).map_err(|e| io(parent, e))?;
    let in_rdy = |s: &std::ffi::OsStr| s == ".rdy";
    if resolved.iter().any(in_rdy) || in_rdy(name) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{} is inside the project .rdy directory", out.display()),
            "export to a path outside .rdy",
        ));
    }
    match std::fs::symlink_metadata(out) {
        Ok(md) if md.is_dir() => {
            if std::fs::read_dir(out)
                .map_err(|e| io(out, e))?
                .next()
                .is_some()
            {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("{} is not empty", out.display()),
                    "choose an empty directory (or one that does not exist yet) so no stale files mix into the export",
                ));
            }
            Ok(true)
        }
        Ok(_) => Err(RdyError::new(
            E_BAD_INPUT,
            format!(
                "{} is a file: a Markdown export writes a directory",
                out.display()
            ),
            "pass a directory path such as ./docs-md",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io(out, e)),
    }
}

/// Writes `p` as a Markdown tree at `out`: a directory that does not exist or is empty. The
/// same print gives the same bytes (O15).
pub fn write_markdown(p: &Print, out: &Path, layout: Layout) -> Result<PrintSummary, RdyError> {
    let name = out.file_name().ok_or_else(|| {
        RdyError::new(
            E_BAD_INPUT,
            format!("{} does not name a directory", out.display()),
            "pass a directory path such as ./docs-md",
        )
    })?;
    let parent = out
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let replaces_empty = check_out(out, parent, name)?;
    // everything is written to a staging directory beside `out` and renamed into place at the
    // end: a failure leaves nothing behind but, at worst, the staging directory, which the next
    // export clears
    let staging = parent.join(format!(".{}.rdy-export-partial", name.to_string_lossy()));
    if let Ok(md) = std::fs::symlink_metadata(&staging) {
        let cleared = if md.is_dir() {
            std::fs::remove_dir_all(&staging)
        } else {
            std::fs::remove_file(&staging)
        };
        cleared.map_err(|e| io(&staging, e))?;
    }
    let written = std::fs::create_dir(&staging)
        .map_err(|e| io(&staging, e))
        .and_then(|()| write_tree(p, &staging, layout));
    let bytes = match written {
        Ok(bytes) => bytes,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    // renaming over an empty directory works on POSIX only: remove it first
    let placed = (if replaces_empty {
        std::fs::remove_dir(out)
    } else {
        Ok(())
    })
    .and_then(|()| std::fs::rename(&staging, out));
    if let Err(e) = placed {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(io(out, e));
    }
    Ok(PrintSummary {
        out: out.display().to_string(),
        format: "md",
        docs: p.docs.iter().filter(|d| d.kind == PageKind::Doc).count(),
        folders: p.docs.iter().filter(|d| d.kind == PageKind::Folder).count(),
        assets: p.blobs.len(),
        pages: None,
        bytes,
        warnings: p.warnings.clone(),
    })
}

/// Writes the files of the tree under `root`; the bytes written.
fn write_tree(p: &Print, root: &Path, layout: Layout) -> Result<u64, RdyError> {
    let base = match layout {
        Layout::Plain => root.to_path_buf(),
        Layout::Mkdocs => root.join("docs"),
    };
    let mut bytes = 0u64;
    let mut write = |path: std::path::PathBuf, data: &[u8]| -> Result<(), RdyError> {
        bytes += data.len() as u64;
        put(&path, data)
    };
    write(base.join("index.md"), title_file(p).as_bytes())?;
    for d in &p.docs {
        write(base.join(md_path(&d.page)), page_file(p, d).as_bytes())?;
    }
    for blob in p.blobs.values() {
        write(base.join(&blob.file), &blob.bytes)?;
    }
    if layout == Layout::Mkdocs {
        write(root.join("mkdocs.yml"), mkdocs_yml(p).as_bytes())?;
    }
    Ok(bytes)
}
