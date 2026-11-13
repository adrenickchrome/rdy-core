//! One doc as one Markdown file (D-ESHXM, D-6PR3Y, D-H1FRY): `rdy export --format md --layout
//! single --out <file> [--assets <dir>]`.
//!
//! The file is written from the print model, as every print format is, read without captions,
//! labels or pills ([`super::ir`]'s single reading). It has no front matter, anchor line,
//! generated-by line or title part: `# <doc title>`, then the doc's blocks with GitHub Flavored
//! Markdown's minimal escapes ([`super::md::gfm_escape`]). A reference, or a link to the doc's
//! own page with a fragment, is a `#slug` link when its target is in the doc and outside every
//! collapsed section; any other reference is its label, and a warning names it. Each figure's
//! image is a file of the `--assets` folder named by the SHA-256 of its bytes.
//!
//! The order of the writes (D-6PR3Y rule 5): both paths are checked first, then the assets
//! folder is made and the images written, and the Markdown file is created last, new, so it
//! never replaces a file. A failure removes only the files and folders this call made. The
//! module makes portable `std::fs` calls only (D-1RXEA).
use super::{Blob, Href, PBlock, PageKind, Print, PrintDoc, PrintSummary, Run};
use rdy_engine::{E_ALREADY_EXISTS, E_BAD_INPUT, E_IO, RdyError, dump::place_file};
use rdy_model::Hash;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

/// The layout's name, as the CLI, the tools and the reply name it.
pub const LAYOUT: &str = "single";

// The texts of the layout's refusals and warning (D-ESHXM, D-6PR3Y): each `<...>` is filled in.

/// The selection holds `<n>` docs, not one (D-ESHXM rule 2).
pub const SINGLE_MD_DOCS_MESSAGE: &str =
    "a single-file Markdown export takes one doc; the selection holds <n>";
pub const SINGLE_MD_DOCS_FIX: &str = "choose a profile that selects one doc";
/// A figure and no assets folder (D-ESHXM rule 10).
pub const SINGLE_MD_FIGURE_MESSAGE: &str = "figure <id> needs --assets <dir>";
pub const SINGLE_MD_FIGURE_FIX: &str = "pass --assets with a folder beside the file";
/// An assets path that is not a folder below the folder of `--out` (D-6PR3Y rule 2).
pub const SINGLE_MD_ASSETS_PATH_MESSAGE: &str =
    "--assets <path> must be a folder below the folder of --out";
pub const SINGLE_MD_ASSETS_PATH_FIX: &str =
    "pass a relative folder path with no .. part, such as assets";
/// An assets folder that holds an entry (D-6PR3Y rule 3).
pub const SINGLE_MD_ASSETS_FULL_MESSAGE: &str = "<path> is not empty";
pub const SINGLE_MD_ASSETS_FULL_FIX: &str = "choose an empty directory (or one that does not exist yet) so no stale files mix into the export";
/// A file where the assets folder goes (D-6PR3Y rule 3).
pub const SINGLE_MD_ASSETS_FILE_MESSAGE: &str = "<path> is a file: --assets names a folder";
pub const SINGLE_MD_ASSETS_FILE_FIX: &str = "pass a folder path such as assets";
/// A reference written as its label (D-6PR3Y rule 6): the reply's warning, with its prefix.
pub const SINGLE_MD_REF_MESSAGE: &str = "warning: <doc path>: <block id>: the ref to <target id> is not in this file; it is written as its label";

/// What the single file's build knows of each doc page beyond its print, by the page's url.
#[derive(Debug, Default)]
pub(crate) struct Facts {
    pub(crate) docs: BTreeMap<String, DocFacts>,
}

/// One doc page's facts: its figures in page order (each figure's block id and the blobs it
/// shows) and the warnings of its references that are written as their labels.
#[derive(Debug, Default)]
pub(crate) struct DocFacts {
    pub(crate) figures: Vec<(String, Vec<Hash>)>,
    pub(crate) warnings: Vec<String>,
}

// ---- links --------------------------------------------------------------------------------

/// GitHub's slug of a heading's text: lowercase; letters, digits, spaces, `-` and `_` kept and
/// every other character removed; each space a `-`.
pub fn slug(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// The slugs of a file's headings in page order: a repeat gets `-1`, `-2` and so on.
#[derive(Default)]
struct Slugs {
    seen: BTreeMap<String, usize>,
    taken: BTreeSet<String>,
}

impl Slugs {
    fn next(&mut self, text: &str) -> String {
        let base = slug(text);
        let n = self.seen.entry(base.clone()).or_insert(0);
        let mut s = if *n == 0 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        *n += 1;
        // a slug an earlier heading already has (a heading whose own text ends in `-1`) takes
        // the next number
        while self.taken.contains(&s) {
            let n = self.seen.entry(base.clone()).or_insert(1);
            s = format!("{base}-{n}");
            *n += 1;
        }
        self.taken.insert(s.clone());
        s
    }
}

/// The text of a line of runs, as a heading's slug reads it.
fn text_of(runs: &[Run]) -> String {
    runs.iter()
        .map(|r| r.text.replace('\n', " "))
        .collect::<String>()
}

/// Where the targets of a doc's file lead: the slug of the doc's title, and for each anchor a
/// printed block has, the slug of the nearest heading at or above it, or none when the block is
/// in or on a collapsed section.
pub(crate) struct Places {
    title: String,
    anchors: BTreeMap<String, Option<String>>,
}

impl Places {
    pub(crate) fn of(title: &str, blocks: &[PBlock]) -> Places {
        let mut slugs = Slugs::default();
        let title = slugs.next(title);
        let mut p = Places {
            title: title.clone(),
            anchors: BTreeMap::new(),
        };
        let mut current = title;
        p.walk(blocks, false, &mut slugs, &mut current);
        p
    }

    fn walk(&mut self, blocks: &[PBlock], folded: bool, slugs: &mut Slugs, current: &mut String) {
        for b in blocks {
            let mut put = |anchor: &Option<String>, at: Option<String>| {
                if let Some(a) = anchor {
                    self.anchors.entry(a.clone()).or_insert(at);
                }
            };
            match b {
                PBlock::Heading { anchor, runs, .. } => {
                    *current = slugs.next(&text_of(runs));
                    put(anchor, (!folded).then(|| current.clone()));
                }
                PBlock::Fold { anchor, blocks, .. } => {
                    // the summary is no heading; the blocks under it are folded
                    put(anchor, None);
                    self.walk(blocks, true, slugs, current);
                }
                PBlock::Para { anchor, .. }
                | PBlock::Table { anchor, .. }
                | PBlock::Code { anchor, .. }
                | PBlock::Figure { anchor, .. } => put(anchor, (!folded).then(|| current.clone())),
                PBlock::Callout { anchor, blocks, .. } => {
                    put(anchor, (!folded).then(|| current.clone()));
                    self.walk(blocks, folded, slugs, current);
                }
                PBlock::List { items, .. } => {
                    for item in items {
                        self.walk(item, folded, slugs, current);
                    }
                }
            }
        }
    }

    /// The slug a target on the doc's page leads to: the title's for the page itself, else its
    /// anchor's; none when the target is not printed or is folded.
    fn slug_of(&self, anchor: Option<&str>) -> Option<String> {
        match anchor {
            None => Some(self.title.clone()),
            Some(a) => self.anchors.get(a).cloned().flatten(),
        }
    }
}

/// A place on the site: a page, and its anchor (none for the page itself).
pub(crate) type Place = (String, Option<String>);

/// What the build tells [`resolve`] about a doc's targets.
pub(crate) struct Targets<'a> {
    /// The doc's path in the tree, as the warnings name it.
    pub(crate) doc_path: &'a str,
    /// The doc's page url.
    pub(crate) page: &'a str,
    /// Where a reference's token leads: a page and its anchor (none for the page itself), or
    /// none when it has no place on the site.
    pub(crate) place_of: &'a dyn Fn(&str) -> Option<Place>,
    /// The anchor a link's fragment on the doc's page names: a block id names its block's
    /// anchor.
    pub(crate) anchor_of: &'a dyn Fn(&str) -> String,
}

/// The warning of a reference written as its label (D-6PR3Y rule 6).
pub fn ref_warning(doc_path: &str, block: &str, target: &str) -> String {
    SINGLE_MD_REF_MESSAGE
        .replace("<doc path>", doc_path)
        .replace("<block id>", block)
        .replace("<target id>", target)
}

/// The links of a doc's `blocks` as its file writes them (D-ESHXM rule 8): a reference or an
/// own-page fragment whose target is in the doc and outside every collapsed section leads to
/// `#slug`; any other reference keeps its words with no link, and a warning names it; any other
/// link to a page of the site keeps its words; an external link stays.
pub(crate) fn resolve(blocks: &mut [PBlock], places: &Places, t: &Targets, out: &mut Vec<String>) {
    let fix = |runs: &mut Vec<Run>, out: &mut Vec<String>| {
        for r in runs.iter_mut() {
            if let Some(of) = &r.ref_of {
                let slug = (t.place_of)(&of.target)
                    .filter(|(page, _)| page == t.page)
                    .and_then(|(_, anchor)| places.slug_of(anchor.as_deref()));
                match slug {
                    Some(s) => r.link = Some(Href::External(format!("#{s}"))),
                    None => {
                        r.link = None;
                        out.push(ref_warning(t.doc_path, &of.block, &of.target));
                    }
                }
                continue;
            }
            r.link = match r.link.take() {
                Some(Href::Page {
                    page,
                    anchor: Some(a),
                }) if page == t.page => places
                    .slug_of(Some(&(t.anchor_of)(&a)))
                    .map(|s| Href::External(format!("#{s}"))),
                Some(Href::Page { .. }) | None => None,
                external => external,
            };
        }
        *runs = super::ir::tidy(std::mem::take(runs));
    };
    walk_runs(blocks, &mut |runs| fix(runs, out));
}

/// Calls `f` on every line of runs of `blocks`, in page order.
fn walk_runs(blocks: &mut [PBlock], f: &mut dyn FnMut(&mut Vec<Run>)) {
    for b in blocks {
        match b {
            PBlock::Heading { runs, .. } | PBlock::Para { runs, .. } => f(runs),
            PBlock::Fold { runs, blocks, .. } => {
                f(runs);
                walk_runs(blocks, f);
            }
            PBlock::List { items, .. } => {
                for item in items {
                    walk_runs(item, f);
                }
            }
            PBlock::Table { head, rows, .. } => {
                for cell in head.iter_mut().chain(rows.iter_mut().flatten()) {
                    f(cell);
                }
            }
            PBlock::Figure { caption, .. } => f(caption),
            PBlock::Callout { blocks, .. } => walk_runs(blocks, f),
            PBlock::Code { .. } => {}
        }
    }
}

/// Names the block of every reference in `blocks`: the block whose text the reader read.
pub(crate) fn name_refs(blocks: &mut [PBlock], block: &str) {
    walk_runs(blocks, &mut |runs| {
        for r in runs.iter_mut() {
            if let Some(of) = r.ref_of.as_mut().filter(|o| o.block.is_empty()) {
                of.block = block.to_string();
            }
        }
    });
}

// ---- paths --------------------------------------------------------------------------------

/// The checked destinations of a single file: `out`, and the assets folder when one is named.
#[derive(Debug)]
pub(crate) struct Paths {
    out: PathBuf,
    assets: Option<Assets>,
}

/// A checked assets folder: its parts with `/`, and the folders from the folder of `out` down
/// to it.
#[derive(Debug)]
struct Assets {
    rel: String,
    /// Each folder from the first part down to the assets folder, with whether it was there.
    chain: Vec<(PathBuf, bool)>,
}

fn invalid_assets(given: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        SINGLE_MD_ASSETS_PATH_MESSAGE.replace("<path>", given),
        SINGLE_MD_ASSETS_PATH_FIX,
    )
}

fn io(path: &Path, e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("{}: {e}", path.display()),
        "check the output directory",
    )
}

/// The command the shared file placement names in its texts.
const EXPORT_CMD: &str = "rdy export";

/// The refusal of an entry that is there: no export replaces a file, with the words of the file
/// formats' refusal.
fn exists(out: &Path) -> RdyError {
    RdyError::new(
        E_ALREADY_EXISTS,
        format!(
            "{} exists; {EXPORT_CMD} never overwrites a file",
            out.display()
        ),
        "pick another path, or remove the file",
    )
}

/// Whether an entry is a symbolic link, a junction or another reparse point: anything that is
/// neither a plain directory nor a plain file is never followed.
fn link_like(md: &std::fs::Metadata) -> bool {
    let t = md.file_type();
    t.is_symlink() || !(t.is_dir() || t.is_file())
}

/// Checks both destinations before anything is written (D-6PR3Y rules 1 to 3). `out` keeps the
/// print rules of the file formats (`super::check_out` ran first) and must not be there. The
/// assets folder is relative to the folder of `out`, a folder below it: not absolute or empty,
/// not `.`, with no `..` part and no part that is a link, not `out` or a path through it, and
/// not inside the project's `.rdy` directory `rdy_dir`. It is absent or an empty folder.
pub(crate) fn check_paths(
    rdy_dir: &Path,
    out: &Path,
    assets: Option<&str>,
) -> Result<Paths, RdyError> {
    let (dir, name) = super::parent_and_name(out)?;
    if std::fs::symlink_metadata(out).is_ok() {
        return Err(exists(out));
    }
    let Some(given) = assets else {
        return Ok(Paths {
            out: out.to_path_buf(),
            assets: None,
        });
    };
    let path = Path::new(given);
    if given.is_empty() || path.is_absolute() || path.has_root() {
        return Err(invalid_assets(given));
    }
    let mut parts = vec![];
    for c in path.components() {
        match c {
            Component::Normal(p) => parts.push(p.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(invalid_assets(given));
            }
        }
    }
    // a path through `out` holds it
    if parts.is_empty() || parts[0] == name {
        return Err(invalid_assets(given));
    }
    let mut chain = vec![];
    let mut at = dir.to_path_buf();
    let mut absent = false;
    for (i, part) in parts.iter().enumerate() {
        at.push(part);
        if absent {
            chain.push((at.clone(), false));
            continue;
        }
        match std::fs::symlink_metadata(&at) {
            Ok(md) if link_like(&md) => return Err(invalid_assets(given)),
            Ok(md) if md.is_dir() => chain.push((at.clone(), true)),
            Ok(_) if i + 1 == parts.len() => {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    SINGLE_MD_ASSETS_FILE_MESSAGE.replace("<path>", given),
                    SINGLE_MD_ASSETS_FILE_FIX,
                ));
            }
            Ok(_) => return Err(invalid_assets(given)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                absent = true;
                chain.push((at.clone(), false));
            }
            Err(e) => return Err(io(&at, e)),
        }
    }
    // the folder of `out` resolved, and the parts below it: never inside the store
    if let Ok(rdy) = std::fs::canonicalize(rdy_dir) {
        let base = std::fs::canonicalize(dir).map_err(|e| io(dir, e))?;
        let mut full = base;
        for p in &parts {
            full.push(p);
        }
        if full.starts_with(&rdy) {
            return Err(invalid_assets(given));
        }
    }
    if let Some((last, true)) = chain.last()
        && std::fs::read_dir(last)
            .map_err(|e| io(last, e))?
            .next()
            .is_some()
    {
        return Err(RdyError::new(
            E_BAD_INPUT,
            SINGLE_MD_ASSETS_FULL_MESSAGE.replace("<path>", given),
            SINGLE_MD_ASSETS_FULL_FIX,
        ));
    }
    let rel = parts
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Ok(Paths {
        out: out.to_path_buf(),
        assets: Some(Assets { rel, chain }),
    })
}

// ---- the file -----------------------------------------------------------------------------

/// The blobs the figures of `blocks` show, light then dark, in page order.
fn figure_blobs(blocks: &[PBlock], out: &mut Vec<Hash>) {
    for b in blocks {
        match b {
            PBlock::Figure { blob, dark, .. } => out.extend(blob.iter().chain(dark)),
            PBlock::List { items, .. } => items.iter().for_each(|i| figure_blobs(i, out)),
            PBlock::Callout { blocks, .. } | PBlock::Fold { blocks, .. } => {
                figure_blobs(blocks, out)
            }
            _ => {}
        }
    }
}

/// The file name of an image: the SHA-256 of its bytes and the extension the Markdown tree uses
/// for its media type (`bin` for one with none).
pub fn asset_name(bytes: &[u8], media_type: &str) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{hex}.{}", crate::assets::extension(media_type))
}

/// The bytes of the single file of `doc` (D-ESHXM rules 5 and 6): `# <doc title>`, then its
/// blocks, UTF-8 with LF line ends and one final newline. `files` names each figure blob's file
/// relative to the folder of the file.
pub fn markdown(doc: &PrintDoc, files: &BTreeMap<Hash, Blob>) -> String {
    let mut out = super::md::single_heading(1, &doc.title);
    out.push('\n');
    let body = super::md::single_blocks(&doc.blocks, files);
    if !body.is_empty() {
        out.push('\n');
        out.push_str(&body);
        out.push('\n');
    }
    out
}

/// The one doc a single file writes: the print must select exactly one (D-ESHXM rule 2).
pub(crate) fn the_doc(p: &Print) -> Result<&PrintDoc, RdyError> {
    let docs: Vec<&PrintDoc> = p.docs.iter().filter(|d| d.kind == PageKind::Doc).collect();
    match docs.as_slice() {
        [d] => Ok(d),
        _ => Err(RdyError::new(
            E_BAD_INPUT,
            SINGLE_MD_DOCS_MESSAGE.replace("<n>", &docs.len().to_string()),
            SINGLE_MD_DOCS_FIX,
        )),
    }
}

/// What this call made, so a failure removes exactly that.
#[derive(Default)]
struct Made {
    files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
}

impl Made {
    fn undo(&self) {
        for f in self.files.iter().rev() {
            let _ = std::fs::remove_file(f);
        }
        for d in self.dirs.iter().rev() {
            let _ = std::fs::remove_dir(d);
        }
    }
}

/// Writes one new file, durably; it never replaces one. Once the file is made it is `made`'s,
/// so a failed write removes it.
fn create(path: &Path, bytes: &[u8], made: &mut Made) -> Result<(), RdyError> {
    let mut file = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => exists(path),
            _ => io(path, e),
        })?;
    made.files.push(path.to_path_buf());
    file.write_all(bytes).map_err(|e| io(path, e))?;
    file.sync_all().map_err(|e| io(path, e))
}

thread_local! {
    /// A test's failure at the image write with this index (from 0), or at the Markdown file
    /// with `usize::MAX`: it proves what a failed write leaves behind.
    static FAIL_AT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Runs `f` with the write at `at` failing (an image by its index from 0, the Markdown file at
/// `usize::MAX`). For tests of what a failure leaves behind.
#[doc(hidden)]
pub fn with_failed_write_for_tests<T>(at: usize, f: impl FnOnce() -> T) -> T {
    let before = FAIL_AT.with(|c| c.replace(Some(at)));
    let out = f();
    FAIL_AT.with(|c| c.set(before));
    out
}

fn injected(i: usize, path: &Path) -> Result<(), RdyError> {
    if FAIL_AT.with(std::cell::Cell::get) == Some(i) {
        return Err(io(path, std::io::Error::other("an injected write failure")));
    }
    Ok(())
}

/// Writes the single file of `p`'s one doc at `paths` (D-ESHXM, D-6PR3Y): the images under the
/// assets folder first, then the Markdown file, created new. `facts` names its figures and its
/// warnings. A failure removes what this call made and keeps every other entry.
pub(crate) fn write(p: &Print, facts: &Facts, paths: &Paths) -> Result<PrintSummary, RdyError> {
    let doc = the_doc(p)?;
    let doc_facts = facts.docs.get(&doc.page);
    let mut shown = vec![];
    figure_blobs(&doc.blocks, &mut shown);
    let shown: Vec<Hash> = shown
        .into_iter()
        .filter(|h| p.blobs.contains_key(h))
        .collect();
    // each image's file, by the SHA-256 of its bytes; equal bytes share one file
    let mut files: BTreeMap<Hash, Blob> = BTreeMap::new();
    let mut images: Vec<(String, &[u8])> = vec![];
    if !shown.is_empty() {
        let Some(assets) = &paths.assets else {
            let id = doc_facts
                .and_then(|f| {
                    f.figures
                        .iter()
                        .find(|(_, blobs)| blobs.iter().any(|h| shown.contains(h)))
                })
                .map_or("", |(id, _)| id.as_str());
            return Err(RdyError::new(
                E_BAD_INPUT,
                SINGLE_MD_FIGURE_MESSAGE.replace("<id>", id),
                SINGLE_MD_FIGURE_FIX,
            ));
        };
        let mut names = BTreeSet::new();
        for h in &shown {
            let blob = &p.blobs[h];
            let name = asset_name(&blob.bytes, &blob.media_type);
            files.insert(
                *h,
                Blob {
                    media_type: blob.media_type.clone(),
                    bytes: vec![],
                    file: format!("{}/{name}", assets.rel),
                },
            );
            if names.insert(name.clone()) {
                images.push((name, &blob.bytes));
            }
        }
    }
    let text = markdown(doc, &files);
    let mut made = Made::default();
    let placed = (|| -> Result<u64, RdyError> {
        if let (Some(assets), false) = (&paths.assets, images.is_empty()) {
            for (dir, existed) in &assets.chain {
                if !existed {
                    std::fs::create_dir(dir).map_err(|e| io(dir, e))?;
                    made.dirs.push(dir.clone());
                }
            }
            let folder = &assets.chain.last().expect("an assets folder has a part").0;
            for (i, (name, bytes)) in images.iter().enumerate() {
                let path = folder.join(name);
                injected(i, &path)?;
                create(&path, bytes, &mut made)?;
            }
        }
        injected(usize::MAX, &paths.out)?;
        // the file is placed whole or not at all, and never replaces one
        place_file(&paths.out, text.as_bytes(), EXPORT_CMD)
    })();
    let bytes = match placed {
        Ok(n) => n,
        Err(e) => {
            made.undo();
            return Err(e);
        }
    };
    let mut warnings = p.warnings.clone();
    if let Some(f) = doc_facts {
        warnings.extend(f.warnings.iter().cloned());
    }
    Ok(PrintSummary {
        out: paths.out.display().to_string(),
        format: "md",
        layout: Some(LAYOUT),
        docs: 1,
        folders: 0,
        assets: images.len(),
        pages: None,
        bytes,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfr3_slugs_follow_githubs_rule() {
        assert_eq!(
            slug("Try it: your first project"),
            "try-it-your-first-project"
        );
        assert_eq!(slug("Über Ärger_x 2"), "über-ärger_x-2");
        assert_eq!(slug("A  B"), "a--b");
        let mut s = Slugs::default();
        assert_eq!(s.next("Install"), "install");
        assert_eq!(s.next("Install"), "install-1");
        assert_eq!(s.next("Install!"), "install-2");
    }

    #[test]
    fn rfr3_asset_names_are_the_sha256_of_the_bytes() {
        assert_eq!(
            asset_name(b"abc", "image/png"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad.png"
        );
        assert!(asset_name(b"abc", "application/x-thing").ends_with(".bin"));
    }
}
