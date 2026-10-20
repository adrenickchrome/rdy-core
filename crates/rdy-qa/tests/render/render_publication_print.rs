//! pub8 (D-5CVWB, D-0VCQ5, D-K7ZFK, D-X61DT): a named profile's print, in every format (PDF, one
//! HTML file, plain Markdown and the MkDocs tree), reads the same filtered view as its site and
//! runs the same reference check. Its title and description are the profile's; its only label is
//! an explicitly chosen tag's name (RULING F3); it carries no source scope or hash. A reference
//! outside the selection reads as the site reads it, and a link to a page the print leaves out
//! reads as its words. A public profile's refusal writes nothing, and the existing output-path
//! rules hold for every format. Without a profile the print stays the full source (RULING F2).
// the shared fixture is also a module of `render_publication_site` (pub6), which keeps it private
#[allow(clippy::duplicate_mod)]
#[path = "../../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::{
    E_ALREADY_EXISTS, E_BAD_INPUT, E_PUBLICATION_REFERENCE, Rdy, RdyError, ReadState, Scope,
};
use rdy_model::redaction::{CONTENT_REMOVED, RedactTarget};
use rdy_model::{BlockId, Change, ChangeLinks, Hash, Op};
use rdy_qa::fixtures::{approver, land, land_with_a_tie, raw, write, writer};
use rdy_qa::golden::tiny;
use rdy_render::build_profile_site;
use rdy_render::print::fonts::Fonts;
use rdy_render::print::layout::LItem;
use rdy_render::print::{self, Format, Href, Layout, PBlock, Print, PrintSummary, Run};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The words of a reference outside the selection with no display text.
const UNAVAILABLE: &str = "unavailable reference";

/// Every print format: the format, its layout, the name it writes and a short name.
const FORMATS: [(Format, Option<Layout>, &str, &str); 4] = [
    (Format::Pdf, None, "out.pdf", "pdf"),
    (Format::Html, None, "out.html", "html"),
    (Format::Md, None, "out-md", "md"),
    (Format::Md, Some(Layout::Mkdocs), "out-mkdocs", "mkdocs"),
];

/// `FORMATS` and the TechDocs tree, which the blocked-export and path-guard checks cover beside
/// MkDocs.
fn with_techdocs() -> Vec<(Format, Option<Layout>, &'static str, &'static str)> {
    let mut all = FORMATS.to_vec();
    all.push((
        Format::Md,
        Some(Layout::Techdocs),
        "out-techdocs",
        "techdocs",
    ));
    all
}

// --- helpers ------------------------------------------------------------------------------------

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Writes `ops` to `view` as the writer; panics on a refusal.
fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, BlockId> {
    let w = write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e}"));
    w.created.iter().map(|(k, v)| (k.clone(), bid(v))).collect()
}

/// One profile's create op under `parent`.
fn profile_op(key: &str, audience: &str, parent: &BlockId, select: &[&BlockId]) -> Value {
    json!({"op": "create", "type": "publication_profile", "id": format!("${key}"),
        "parent": parent.to_string(),
        "fields": {"key": key, "title": format!("Title of {key}"),
            "description": format!("Description of {key}."), "audience": audience,
            "select": select.iter().map(|s| s.to_string()).collect::<Vec<_>>()}})
}

/// Creates and lands an `admin` doc holding one profile per (key, audience, selection).
fn profiles(r: &mut Rdy, list: &[(&str, &str, Vec<&BlockId>)]) -> BTreeMap<String, BlockId> {
    let admin = put(
        r,
        "admin",
        json!([{"op": "create", "type": "doc", "id": "$admin",
            "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}]),
    )["$admin"]
        .clone();
    let ops: Vec<Value> = list
        .iter()
        .map(|(k, a, s)| profile_op(k, a, &admin, s))
        .collect();
    let mut ids = put(r, "admin", Value::Array(ops));
    land(r, "admin").expect("land the profiles");
    ids.insert("$admin".into(), admin);
    ids
}

/// An 8-bit RGB PNG of `w` × `h`, its one IDAT stored (no compression).
fn png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for b in data {
            c ^= u32::from(*b);
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let line: Vec<u8> = std::iter::once(0)
        .chain(rgb.iter().copied().cycle().take(3 * w as usize))
        .collect();
    let raw = line.repeat(h as usize);
    let (mut a, mut b) = (1u32, 0u32);
    for x in &raw {
        a = (a + u32::from(*x)) % 65521;
        b = (b + a) % 65521;
    }
    let mut z = vec![0x78, 0x01, 1];
    let len = raw.len() as u16;
    z.extend_from_slice(&len.to_le_bytes());
    z.extend_from_slice(&(!len).to_le_bytes());
    z.extend_from_slice(&raw);
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = vec![];
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Standard base64 with padding: how the single HTML file embeds a figure.
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Every file under `dir`, by its path relative to `dir`, dotfiles included.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(at).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                out.insert(rel.replace('\\', "/"), std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    if dir.exists() {
        walk(dir, dir, &mut out);
    }
    out
}

/// Every entry of `dir` (files and directories) by path, sorted.
fn entries(dir: &Path) -> Vec<String> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p.clone());
            }
            out.push(p.strip_prefix(dir).unwrap().to_string_lossy().into_owned());
        }
    }
    out.sort();
    out
}

/// One print output: its summary and its files (a single file under its own name).
struct Output {
    summary: PrintSummary,
    files: BTreeMap<String, Vec<u8>>,
}

/// The print of `profile` at `scope` in every format, each written under `dir`.
fn export_all(st: &ReadState, scope: &str, profile: &str, dir: &Path) -> BTreeMap<String, Output> {
    let mut out = BTreeMap::new();
    for (format, layout, name, short) in FORMATS {
        let at = dir.join(name);
        let summary = print::export_profile(st, scope, profile, &at, format, layout)
            .unwrap_or_else(|e| panic!("{short} of {profile} at {scope}: {e}"));
        let files = if at.is_dir() {
            tree(&at)
        } else {
            BTreeMap::from([(name.to_string(), std::fs::read(&at).unwrap())])
        };
        out.insert(short.to_string(), Output { summary, files });
    }
    out
}

/// The print of `profile` at `scope`.
fn model(st: &ReadState, scope: &str, profile: &str) -> Print {
    print::build_profile(st, scope, profile).unwrap_or_else(|e| panic!("{profile} at {scope}: {e}"))
}

/// The text the PDF of `p` draws, one laid-out item per line.
fn pdf_lines(p: &Print) -> Vec<Vec<String>> {
    print::pdf::lay_out(p, &Fonts::load())
        .pages
        .iter()
        .map(|page| {
            page.items
                .iter()
                .filter_map(|it| match it {
                    LItem::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

/// The PDF's drawn text as running words: each item's text, whitespace collapsed.
fn pdf_words(p: &Print) -> String {
    pdf_lines(p)
        .concat()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn runs_of<'a>(blocks: &'a [PBlock], out: &mut Vec<&'a Run>) {
    for b in blocks {
        match b {
            PBlock::Heading { runs, .. } | PBlock::Para { runs, .. } => out.extend(runs),
            PBlock::List { items, .. } => items.iter().for_each(|i| runs_of(i, out)),
            PBlock::Table { head, rows, .. } => {
                for cell in head.iter().chain(rows.iter().flatten()) {
                    out.extend(cell);
                }
            }
            PBlock::Figure { caption, .. } => out.extend(caption),
            PBlock::Callout { blocks, .. } => runs_of(blocks, out),
            PBlock::Code { .. } => {}
        }
    }
}

/// Every run of the print's pages.
fn runs(p: &Print) -> Vec<&Run> {
    let mut out = vec![];
    for d in &p.docs {
        runs_of(&d.blocks, &mut out);
    }
    out
}

/// The text of each paragraph of the print, its runs joined.
fn paras(p: &Print) -> Vec<String> {
    fn walk(blocks: &[PBlock], out: &mut Vec<String>) {
        for b in blocks {
            match b {
                PBlock::Para { runs, .. } => {
                    out.push(runs.iter().map(|r| r.text.as_str()).collect());
                }
                PBlock::List { items, .. } => items.iter().for_each(|i| walk(i, out)),
                PBlock::Callout { blocks, .. } => walk(blocks, out),
                _ => {}
            }
        }
    }
    let mut out = vec![];
    for d in &p.docs {
        walk(&d.blocks, &mut out);
    }
    out
}

/// The paths of the outputs whose file names or bytes hold `word`, as `format/path`.
fn holding(outputs: &BTreeMap<String, Output>, word: &[u8]) -> Vec<String> {
    let mut out = vec![];
    for (short, o) in outputs {
        for (path, bytes) in &o.files {
            if path.as_bytes().windows(word.len()).any(|w| w == word)
                || bytes.windows(word.len()).any(|w| w == word)
            {
                out.push(format!("{short}/{path}"));
            }
        }
    }
    out
}

/// `word` is in no output byte, no file name, no field of the print model and no text its PDF
/// draws (the PDF's text streams are compressed, so its raw bytes alone prove nothing).
#[track_caller]
fn assert_absent(p: &Print, outputs: &BTreeMap<String, Output>, word: &str) {
    assert_eq!(
        holding(outputs, word.as_bytes()),
        Vec::<String>::new(),
        "{word}"
    );
    assert!(
        !format!("{p:?}").contains(word),
        "{word} in the print model"
    );
    assert!(!pdf_words(p).contains(word), "{word} in the PDF text");
}

/// The text of a written file.
fn text(o: &Output, path: &str) -> String {
    String::from_utf8(
        o.files
            .get(path)
            .unwrap_or_else(|| panic!("no {path}: {:?}", o.files.keys()))
            .clone(),
    )
    .unwrap()
}

/// The ids of the world's blocks, by placeholder, and its blobs.
struct World {
    dir: tempfile::TempDir,
    r: Rdy,
    ids: BTreeMap<String, BlockId>,
    profiles: BTreeMap<String, BlockId>,
    chosen_png: Vec<u8>,
    chosen_blob: Hash,
    sib_png: Vec<u8>,
    sib_blob: Hash,
    /// The landing tag v1.0.0 names.
    landing: String,
}

/// A selected doc under a folder, with prose, a term, a question, a figure, references outside
/// the selection and authored links to site-only pages; a sibling doc and a far folder with
/// canaries; an org profile and a public one of the same doc, then the tag v1.0.0.
fn world() -> World {
    let (dir, mut r) = tiny();
    let chosen_png = png(5, 3, [10, 120, 200]);
    let sib_png = png(4, 7, [201, 33, 44]);
    let chosen_blob = r.store_mut().put_blob(&chosen_png).unwrap();
    let sib_blob = r.store_mut().put_blob(&sib_png).unwrap();
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area", "title": "Area own title", "description": "Area own description."}},
            {"op": "create", "type": "doc", "id": "$sib", "parent": "$area", "fields": {"name": "sibcanaryname", "title": "SIBCANARY doc title", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$ss", "parent": "$sib", "fields": {"title": "SIBCANARY section"}},
            {"op": "create", "type": "prose", "id": "$sp", "parent": "$ss", "body": "SIBCANARY prose."},
            {"op": "create", "type": "term", "id": "$st", "parent": "$ss", "fields": {"name": "SIBCANARY term", "key": "SIBCANARY-KEY"}, "body": "SIBCANARY term body."},
            {"op": "create", "type": "figure", "id": "$sf", "parent": "$ss", "fields": {"blob": sib_blob.to_string(), "media_type": "image/png", "alt": "SIBCANARY figure alt"}},
            {"op": "create", "type": "doc", "id": "$chosen", "parent": "$area", "fields": {"name": "chosen", "title": "Chosen page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$cs", "parent": "$chosen", "fields": {"title": "Chosen section"}},
            {"op": "create", "type": "prose", "id": "$cp", "parent": "$cs", "body": "Chosen prose words."},
            {"op": "create", "type": "prose", "parent": "$cs", "body": "Explicit {{$sp|Shown display words}} and bare {{$sp}} and typed {{$st|@key}}."},
            {"op": "create", "type": "prose", "parent": "$cs", "body": "Go [QLINK](../questions.html) and [HOMELINK](../index.html#contents) and [SIBLINK](sibcanaryname.html) and [OWNLINK](#chosen-section) now."},
            {"op": "create", "type": "term", "id": "$ct", "parent": "$cs", "fields": {"name": "Chosen term"}, "body": "Chosen term body."},
            {"op": "create", "type": "question", "id": "$cq", "parent": "$cs", "fields": {"prompt": "Chosen question prompt?"}},
            {"op": "create", "type": "figure", "id": "$cf", "parent": "$cs", "fields": {"blob": chosen_blob.to_string(), "media_type": "image/png", "alt": "Chosen figure alt"}},
            {"op": "create", "type": "folder", "id": "$far", "fields": {"name": "farcanaryname", "title": "FARCANARY folder", "description": "FARCANARY description."}},
            {"op": "create", "type": "doc", "id": "$fard", "parent": "$far", "fields": {"name": "faraway", "title": "FARCANARY doc", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$fard", "body": "FARCANARY words."}
        ]),
    );
    land(&mut r, "w1").unwrap();
    // a reference to no block of the source at all, written raw (the write path refuses one)
    let missing = raw(
        "p-M1SS1",
        "prose",
        Some(ids["$cs"].as_str()),
        "zz",
        &[],
        "Missing {{p-ZZZZZ}} here.",
    );
    let change = Change {
        view: "raw".into(),
        ops: vec![Op::Create { block: missing }],
        deps: vec![],
        provenance: writer().provenance(),
        justification: None,
        message: "raw".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    r.store_mut().commit_to_view("raw", change).unwrap();
    r.invalidate();
    land_with_a_tie(&mut r, "raw");
    let chosen = ids["$chosen"].clone();
    let profiles = profiles(
        &mut r,
        &[
            ("orgp", "org", vec![&chosen]),
            ("pubp", "public", vec![&chosen]),
        ],
    );
    let tag = r
        .tag(&approver(), "v1.0.0", None, Some("TAGMSGCANARY".into()))
        .unwrap();
    World {
        dir,
        r,
        ids,
        profiles,
        chosen_png,
        chosen_blob,
        sib_png,
        sib_blob,
        landing: tag.landing,
    }
}

impl World {
    fn id(&self, k: &str) -> String {
        self.ids[k].to_string()
    }

    /// Every word no profiled output of `orgp` may hold.
    fn canaries(&self) -> Vec<String> {
        let mut out: Vec<String> = [
            "SIBCANARY",
            "FARCANARY",
            "TAGMSGCANARY",
            "sibcanaryname",
            "farcanaryname",
            "Admin page",
            "Tiny",
            "p-ZZZZZ",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        out.push(self.sib_blob.to_string());
        for k in ["$sib", "$ss", "$sp", "$st", "$sf", "$far", "$fard"] {
            out.push(self.id(k));
        }
        for p in self.profiles.values() {
            out.push(p.to_string());
        }
        out
    }
}

// --- 1. one model ------------------------------------------------------------------------------

#[test]
fn pub8_all_print_formats_share_the_selected_model() {
    let w = world();
    let st = w.r.read();
    let p = model(&st, "canon", "orgp");
    // the selected docs, in tree order: the ancestor folder's own page, the doc, the glossary
    let pages: Vec<&str> = p.docs.iter().map(|d| d.page.as_str()).collect();
    assert_eq!(
        pages,
        ["area/index.html", "area/chosen.html", "glossary.html"]
    );
    let titles: Vec<&str> = p.docs.iter().map(|d| d.title.as_str()).collect();
    assert_eq!(titles, ["Area own title", "Chosen page", "Glossary"]);
    assert_eq!(p.title, "Title of orgp");
    assert_eq!(p.description.as_deref(), Some("Description of orgp."));
    assert_eq!(
        p.blobs.keys().copied().collect::<Vec<_>>(),
        vec![w.chosen_blob]
    );
    assert_eq!(p.blobs[&w.chosen_blob].bytes, w.chosen_png);
    let asset = p.blobs[&w.chosen_blob].file.clone();

    // the org warning comes first, once, in the reply only
    let count = rdy_engine::publication::resolve(&st, &Scope::Canon, "orgp")
        .unwrap()
        .references
        .blocked_count;
    assert!(count >= 3, "{count}");
    let warning = format!(
        "Org profile has {count} references outside its selection. Review these references before you share the output."
    );
    assert_eq!(p.warnings.first(), Some(&warning));
    assert_eq!(p.warnings.iter().filter(|w| **w == warning).count(), 1);

    let out = tempfile::tempdir().unwrap();
    let all = export_all(&st, "canon", "orgp", out.path());
    assert_eq!(holding(&all, b"Org profile has"), Vec::<String>::new());
    assert_eq!(
        holding(&all, b"Review these references"),
        Vec::<String>::new()
    );
    // every format counts the same pages and the one figure
    for (short, o) in &all {
        assert_eq!((o.summary.docs, o.summary.folders), (1, 1), "{short}");
        assert_eq!(o.summary.assets, 1, "{short}");
        assert_eq!(o.summary.warnings, p.warnings, "{short}");
    }
    // plain Markdown: the title file, one file per print page and the figure, nothing else
    let md_files = |prefix: &str| -> BTreeSet<String> {
        let mut want: BTreeSet<String> = p
            .docs
            .iter()
            .map(|d| format!("{prefix}{}.md", d.page.strip_suffix(".html").unwrap()))
            .collect();
        want.insert(format!("{prefix}index.md"));
        want.insert(format!("{prefix}{asset}"));
        want
    };
    let md = &all["md"];
    assert_eq!(
        md.files.keys().cloned().collect::<BTreeSet<_>>(),
        md_files("")
    );
    assert_eq!(md.files[&asset], w.chosen_png);
    // MkDocs: the same tree under docs/, and a navigation of the print pages only
    let mk = &all["mkdocs"];
    let mut want = md_files("docs/");
    want.insert("mkdocs.yml".into());
    assert_eq!(mk.files.keys().cloned().collect::<BTreeSet<_>>(), want);
    for (path, bytes) in &md.files {
        assert_eq!(&mk.files[&format!("docs/{path}")], bytes, "{path}");
    }
    let nav = text(mk, "mkdocs.yml");
    assert_eq!(
        nav,
        "site_name: \"Title of orgp\"\nnav:\n  - \"Title of orgp\": index.md\n  - \"Area own title\":\n      - \"Area own title\": area/index.md\n      - \"Chosen page\": area/chosen.md\n  - Glossary: glossary.md\n"
    );
    // one HTML file: a section per print page, the one figure embedded, no other image
    let html = text(&all["html"], "out.html");
    for d in &p.docs {
        let id = d.page.strip_suffix(".html").unwrap();
        assert!(html.contains(&format!("id=\"{id}\"")), "{id}");
    }
    assert_eq!(html.matches("<img ").count(), 1);
    assert!(html.contains(&base64(&w.chosen_png)));
    assert!(!html.contains(&base64(&w.sib_png)));
    // the PDF: each page title, and one image
    let words = pdf_words(&p);
    for t in titles {
        assert!(words.contains(t), "{t}");
    }
    assert_eq!(all["pdf"].summary.pages, Some(pdf_lines(&p).len()));
    // the same selected words in every format
    for want in [
        "Chosen prose words.",
        "Chosen term body.",
        "Chosen question prompt?",
        "Area own description.",
        "Chosen figure alt",
    ] {
        assert!(paras(&p).concat().contains(want) || format!("{p:?}").contains(want));
        assert!(html.contains(want), "html {want}");
        assert!(words.contains(want), "pdf {want}");
        assert!(
            md.files
                .values()
                .any(|b| String::from_utf8_lossy(b).contains(want)),
            "md {want}"
        );
    }
}

// --- 2. no excluded byte -----------------------------------------------------------------------

#[test]
fn pub8_print_has_no_excluded_text_metadata_or_links() {
    let w = world();
    let st = w.r.read();
    // the canaries are real: the full-source print holds them (RULING F2)
    let full = print::build(&st, &Scope::Canon).unwrap();
    let full_text = format!("{full:?}");
    for word in ["SIBCANARY", "FARCANARY", "Tiny", "sibcanaryname"] {
        assert!(full_text.contains(word), "{word}");
    }
    assert_eq!(full.scope, "canon");

    let out = tempfile::tempdir().unwrap();
    let landing = format!("landing:{}", w.landing);
    for (scope, label) in [
        ("canon", ""),
        ("tag:v1.0.0", "v1.0.0"),
        (landing.as_str(), ""),
    ] {
        let p = model(&st, scope, "orgp");
        let dir = out.path().join(scope.replace(':', "-"));
        std::fs::create_dir(&dir).unwrap();
        let all = export_all(&st, scope, "orgp", &dir);
        for word in w.canaries().iter().chain([&w.landing]) {
            assert_absent(&p, &all, word);
        }
        for o in all.values() {
            for bytes in o.files.values() {
                let hay = bytes.as_slice();
                assert!(
                    !hay.windows(w.sib_png.len())
                        .any(|x| x == w.sib_png.as_slice()),
                    "{scope}: the excluded figure's bytes"
                );
            }
        }
        // RULING F3: the explicit tag's name only; no source scope or hash
        assert_eq!(p.label, label, "{scope}");
        assert_eq!(p.scope, "", "{scope}");
        // no redaction touched this tag: no redacted marker in any format
        assert!(!p.tag_redacted, "{scope}");
        assert_eq!(holding(&all, b"redacted"), Vec::<String>::new(), "{scope}");
        assert!(!pdf_words(&p).contains("redacted"), "{scope}");
        // Markdown front matter and title file: no empty Version or Scope line, no leftover blank
        let title_md = text(&all["md"], "index.md");
        let version = if label.is_empty() {
            String::new()
        } else {
            format!("\nVersion: {label}\n")
        };
        assert_eq!(
            title_md,
            format!(
                "---\ntitle: \"Title of orgp\"\n---\n\n# Title of orgp\n\nDescription of orgp.\n{version}"
            ),
            "{scope}"
        );
        assert_eq!(text(&all["mkdocs"], "docs/index.md"), title_md, "{scope}");
        // one HTML file: no colophon without a label, the label alone with one
        let html = text(&all["html"], "out.html");
        assert!(!html.contains("class=\"scope\""), "{scope}");
        if label.is_empty() {
            assert!(!html.contains("class=\"colophon\""), "{scope}");
            assert!(
                html.contains("<p class=\"description\">Description of orgp.</p>\n</header>\n"),
                "{scope}"
            );
        } else {
            assert!(
                html.contains(&format!("<p class=\"colophon\">{label}</p>\n</header>\n")),
                "{scope}"
            );
        }
        // the PDF's title page: the title, the description and the label line when there is one
        // (then the page's number at its foot)
        let first = &pdf_lines(&p)[0];
        let mut want = vec![
            "Title of orgp".to_string(),
            "Description of orgp.".to_string(),
        ];
        if !label.is_empty() {
            want.push(format!("Version: {label}"));
        }
        want.push("1".into());
        assert_eq!(first, &want, "{scope}");
        // no link leads outside the print: each internal one names a printed page
        let printed: BTreeSet<&str> = p.docs.iter().map(|d| d.page.as_str()).collect();
        for r in runs(&p) {
            match &r.link {
                Some(Href::Page { page, .. }) => {
                    assert!(printed.contains(page.as_str()), "{scope}: {page}")
                }
                Some(Href::External(u)) => panic!("{scope}: no external link here: {u}"),
                None => {}
            }
        }
        for word in ["questions", "index.html", "contents", "disagreements"] {
            assert_eq!(
                holding(&all, format!("\">{word}").as_bytes()),
                Vec::<String>::new()
            );
        }
        // no script, search, navigation control, review mark or call to action
        let html_lower = html.to_lowercase();
        for word in [
            "<script",
            "<form",
            "<button",
            "<input",
            "search-index",
            "data-rdy",
        ] {
            assert!(!html_lower.contains(word), "{scope}: {word}");
        }
    }
}

// --- 4. reference words ------------------------------------------------------------------------

#[test]
fn pub8_print_reference_words_match_the_site() {
    let mut w = world();
    // a selected block the print leaves out (its only content is a link with blank words), a
    // reference to it with display words, and one to the selected doc's own page
    let (cs, doc) = (w.id("$cs"), w.id("$chosen"));
    let blank = put(
        &mut w.r,
        "w2",
        json!([
            {"op": "create", "type": "prose", "id": "$blank", "parent": cs, "body": "[ ](https://example.com)"},
            {"op": "create", "type": "prose", "parent": cs, "body": format!("To {{{{$blank|BLANKREF words}}}} and {{{{{doc}|PAGEREF words}}}} end.")}
        ]),
    )["$blank"]
        .clone();
    land(&mut w.r, "w2").unwrap();
    let st = w.r.read();
    let explicit =
        format!("Explicit Shown display words and bare {UNAVAILABLE} and typed {UNAVAILABLE}.");
    let missing = format!("Missing {UNAVAILABLE} here.");
    // the site's words for the same references
    let site = build_profile_site(&st, "canon", "orgp").unwrap();
    let chosen = String::from_utf8(site.files["area/chosen.html"].clone()).unwrap();
    assert!(chosen.contains(&format!("<p>{explicit}</p>")), "{chosen}");
    assert!(chosen.contains(&format!("<p>{missing}</p>")), "{chosen}");
    // the site permits links to its own collected pages; the sibling's page is not written
    for (link, href) in [
        ("QLINK", "href=\"../questions.html\""),
        ("HOMELINK", "href=\"../index.html#contents\""),
    ] {
        let at = chosen.find(&format!(">{link}</a>")).expect(link);
        let tag = &chosen[chosen[..at].rfind("<a ").unwrap()..at];
        assert!(tag.contains(href), "{link}: {tag}");
    }
    assert!(!chosen.contains(">SIBLINK</a>"));
    assert!(chosen.contains(">OWNLINK</a>"));
    // the site links the reference to the left-out block's anchor, and the page reference
    let at = chosen.find(">BLANKREF words</a>").expect("BLANKREF");
    let tag = &chosen[chosen[..at].rfind("<a ").unwrap()..at];
    let anchor = tag
        .split("href=\"#")
        .nth(1)
        .and_then(|t| t.split('"').next())
        .expect("a link to an anchor of this page");
    assert!(
        chosen.contains(&format!("id=\"{anchor}\" data-rdy-block=\"{blank}\"")),
        "{chosen}"
    );
    assert!(chosen.contains(">PAGEREF words</a>"), "{chosen}");

    let p = model(&st, "canon", "orgp");
    let paras = paras(&p);
    assert!(paras.contains(&explicit), "{paras:?}");
    assert!(paras.contains(&missing), "{paras:?}");
    assert!(
        paras.contains(&"Go QLINK and HOMELINK and SIBLINK and OWNLINK now.".to_string()),
        "{paras:?}"
    );
    // no reference words nor the site-only pages' links carry a link in the print
    for r in runs(&p) {
        if r.text.contains(UNAVAILABLE) || r.text.contains("Shown display words") {
            assert_eq!(r.link, None, "{r:?}");
        }
        for word in ["QLINK", "HOMELINK", "SIBLINK"] {
            if r.text.contains(word) {
                assert_eq!(r.link, None, "{word}: {r:?}");
            }
        }
    }
    // the print leaves the blank block out: a link to its anchor reads as its words, and a link
    // to a printed page with no anchor stays
    assert!(
        paras.contains(&"To BLANKREF words and PAGEREF words end.".to_string()),
        "{paras:?}"
    );
    let blank_ref = runs(&p)
        .into_iter()
        .find(|r| r.text.contains("BLANKREF"))
        .expect("BLANKREF");
    assert_eq!(blank_ref.link, None, "{blank_ref:?}");
    let page_ref = runs(&p)
        .into_iter()
        .find(|r| r.text == "PAGEREF words")
        .expect("PAGEREF is its own run");
    assert_eq!(
        page_ref.link,
        Some(Href::Page {
            page: "area/chosen.html".into(),
            anchor: None
        })
    );
    // a link to a printed page and anchor stays
    let own = runs(&p)
        .into_iter()
        .find(|r| r.text == "OWNLINK")
        .expect("OWNLINK is its own run");
    assert_eq!(
        own.link,
        Some(Href::Page {
            page: "area/chosen.html".into(),
            anchor: Some("chosen-section".into())
        })
    );

    let out = tempfile::tempdir().unwrap();
    let all = export_all(&st, "canon", "orgp", out.path());
    let md = text(&all["md"], "area/chosen.md");
    assert!(md.contains(&explicit), "{md}");
    assert!(md.contains(&missing), "{md}");
    assert!(
        md.contains("Go QLINK and HOMELINK and SIBLINK and [OWNLINK]("),
        "{md}"
    );
    assert!(
        md.contains("To BLANKREF words and [PAGEREF words]("),
        "{md}"
    );
    let html = text(&all["html"], "out.html");
    assert!(html.contains(&explicit) && html.contains(&missing));
    assert!(html.contains("Go QLINK and HOMELINK and SIBLINK and <a href=\"#area/chosen:chosen-section\">OWNLINK</a> now."), "{html}");
    assert!(html.contains("To BLANKREF words and <a href="), "{html}");
    let words = pdf_words(&p);
    assert!(words.contains(&explicit), "{words}");
    assert!(words.contains(&missing), "{words}");
    // the author's display words and the fixed phrase, never the target's label, key or id
    for word in ["SIBCANARY", "unresolved", "could not be resolved"] {
        assert_absent(&p, &all, word);
    }
}

/// Final fix round 1: a figure's media type is text the page shows (a download's meta) and
/// passes on (the asset's type): a reference in it reads through the selection in the site and
/// every print format, the PDF's drawn text included, and never reaches a file name, an embedded
/// data URI or a warning. The source reads it as before.
#[test]
fn pub8_figure_media_type_reads_references_through_the_selection() {
    let mut w = world();
    let (cs, sib) = (w.id("$cs"), w.id("$sib"));
    let file_bytes = b"media probe file bytes".to_vec();
    let file_blob = w.r.store_mut().put_blob(&file_bytes).unwrap();
    let image_blob = w.r.store_mut().put_blob(&png(2, 2, [9, 9, 9])).unwrap();
    put(
        &mut w.r,
        "w2",
        json!([
            {"op": "create", "type": "figure", "parent": cs, "fields": {"blob": file_blob.to_string(), "media_type": format!("application/{{{{{sib}}}}}"), "alt": "Media file probe"}},
            {"op": "create", "type": "figure", "parent": cs, "fields": {"blob": image_blob.to_string(), "media_type": format!("image/{{{{{sib}}}}}"), "alt": "Media image probe"}},
            {"op": "create", "type": "figure", "parent": cs, "fields": {"blob": file_blob.to_string(), "media_type": format!("application/x-{{{{{sib}|Shown media words}}}}; v=1"), "alt": "Media words probe"}}
        ]),
    );
    land(&mut w.r, "w2").unwrap();
    let st = w.r.read();
    // the org check counts each media type's reference; the public profile refuses
    let res = rdy_engine::publication::resolve(&st, &Scope::Canon, "orgp").unwrap();
    let media_rows = res
        .references
        .rows
        .iter()
        .filter(|r| r.location == "/fields/media_type")
        .count();
    assert_eq!(media_rows, 3, "{:?}", res.references.rows);
    let Err(e) = build_profile_site(&st, "canon", "pubp") else {
        panic!("the public profile must refuse");
    };
    assert_eq!(e.code, E_PUBLICATION_REFERENCE);

    let size = format!("{} bytes", file_bytes.len());
    let file_meta = format!("({UNAVAILABLE}, {size})");
    let words_meta = format!("(X-Shown media words, {size})");
    // the site
    let site = build_profile_site(&st, "canon", "orgp").unwrap();
    let chosen = String::from_utf8(site.files["area/chosen.html"].clone()).unwrap();
    assert!(
        chosen.contains(&format!(
            "download>Media file probe</a> <span class=\"meta\">{file_meta}</span>"
        )),
        "{chosen}"
    );
    assert!(
        chosen.contains(&format!(
            "download>Media words probe</a> <span class=\"meta\">{words_meta}</span>"
        )),
        "{chosen}"
    );
    assert!(chosen.contains("alt=\"Media image probe\""), "{chosen}");
    assert!(!chosen.contains("{{"), "{chosen}");
    for word in [sib.as_str(), &sib.to_uppercase(), "SIBCANARY"] {
        let held: Vec<&String> = site
            .files
            .iter()
            .filter(|(p, b)| {
                let w = word.as_bytes();
                p.as_bytes().windows(w.len()).any(|x| x == w) || b.windows(w.len()).any(|x| x == w)
            })
            .map(|(p, _)| p)
            .collect();
        assert!(held.is_empty(), "{word} in {held:?}");
    }
    // every print format and the PDF's drawn text
    let p = model(&st, "canon", "orgp");
    let out = tempfile::tempdir().unwrap();
    let all = export_all(&st, "canon", "orgp", out.path());
    for word in [sib.as_str(), &sib.to_uppercase(), "SIBCANARY"] {
        assert_absent(&p, &all, word);
        for (short, o) in &all {
            let warned = format!("{:?}", o.summary);
            assert!(
                !warned.contains(word),
                "{word} in the {short} summary: {warned}"
            );
        }
    }
    let words = pdf_words(&p);
    let html = text(&all["html"], "out.html");
    let md = text(&all["md"], "area/chosen.md");
    let mkdocs = text(&all["mkdocs"], "docs/area/chosen.md");
    for want in [
        format!("Media file probe {file_meta}"),
        format!("Media words probe {words_meta}"),
    ] {
        assert!(words.contains(&want), "{want} in {words}");
        assert!(html.contains(&format!(">{want}</p>")), "{want} in {html}");
        assert!(md.lines().any(|l| l == want), "{want} in {md}");
        assert!(mkdocs.lines().any(|l| l == want), "{want} in {mkdocs}");
    }
    // the image's embedded type carries none of the media type's reference
    assert!(
        html.contains("<img src=\"data:application/octet-stream;base64,"),
        "{html}"
    );

    // the source reads the media type as before: the subtype upper-cased, token and all
    let full = rdy_render::build_site(&st, &Scope::Canon, rdy_render::Mode::Static).unwrap();
    let source = String::from_utf8(full.files["area/chosen.html"].clone()).unwrap();
    let token = format!("{{{{{}}}}}", sib.to_uppercase());
    assert!(
        source.contains(&format!("<span class=\"meta\">({token}, {size})</span>")),
        "{source}"
    );
}

// --- 5. refusal and path rules -----------------------------------------------------------------

/// `print::export_profile` of `profile` at `scope` to `out`, as `format`.
fn export(
    st: &ReadState,
    profile: &str,
    out: &Path,
    format: Format,
    layout: Option<Layout>,
) -> Result<PrintSummary, RdyError> {
    print::export_profile(st, "canon", profile, out, format, layout)
}

#[test]
fn pub8_blocked_export_preserves_target_and_path_guards() {
    let w = world();
    let st = w.r.read();
    let count = rdy_engine::publication::resolve(&st, &Scope::Canon, "pubp")
        .unwrap()
        .references
        .blocked_count;
    assert!(count >= 3, "{count}");
    let parent = tempfile::tempdir().unwrap();
    let base = parent.path();
    // an existing file and an existing empty directory, which the public refusal leaves alone
    std::fs::write(base.join("held.pdf"), b"mine pdf").unwrap();
    std::fs::write(base.join("held.html"), b"mine html").unwrap();
    std::fs::create_dir(base.join("held-md")).unwrap();
    let before = tree(base);
    let entries_before = entries(base);
    for (format, layout, name, short) in with_techdocs() {
        for target in [base.join(name), base.join(format!("held{}", &name[3..]))] {
            let e = export(&st, "pubp", &target, format, layout).unwrap_err();
            assert_eq!(e.code, E_PUBLICATION_REFERENCE, "{short}: {e}");
            assert_eq!(
                e.message,
                format!("Public profile has {count} references outside its selection."),
                "{short}"
            );
            assert_eq!(entries(base), entries_before, "{short}: nothing staged");
            assert_eq!(tree(base), before, "{short}: nothing changed");
        }
    }
    // a scope that is not canon, a tag or a landing refuses before anything is read
    let e = print::export_profile(
        &st,
        "view:w1",
        "orgp",
        &base.join("v.pdf"),
        Format::Pdf,
        None,
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_BAD_INPUT,
            "A publication profile requires canon, a tag, or a landing."
        )
    );
    assert_eq!(entries(base), entries_before);

    // the existing path rules hold for every format, with the existing words
    let rdy = w.dir.path().join(".rdy");
    let store_before = entries(&rdy);
    let link = base.join("store-link");
    rdy_qa::platform::symlink(&rdy, &link).unwrap();
    let physical = std::fs::canonicalize(&rdy).unwrap();
    let other = w.dir.path().join("other");
    std::fs::create_dir(&other).unwrap();
    for (format, layout, name, short) in with_techdocs() {
        for out in [
            rdy.join(name),
            physical.join(name),
            link.join(name),
            link.clone(),
            other.join("..").join(".rdy").join(name),
        ] {
            let e = export(&st, "orgp", &out, format, layout).unwrap_err();
            assert_eq!(
                e.code,
                E_BAD_INPUT,
                "{short} {}: {}",
                out.display(),
                e.message
            );
            assert_eq!(
                e.message,
                format!("{} is inside the project .rdy directory", out.display()),
                "{short}"
            );
            assert_eq!(entries(&rdy), store_before, "{short}: no file in the store");
        }
        // a parent that does not exist, an out with no file name
        let e = export(&st, "orgp", &base.join("none").join(name), format, layout).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{short}");
        assert!(
            e.message.ends_with("does not exist"),
            "{short}: {}",
            e.message
        );
        let e = export(&st, "orgp", Path::new("/"), format, layout).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (E_BAD_INPUT, "/ does not name a directory"),
            "{short}"
        );
    }
    // no overwrite: an existing file for the file formats, a non-empty directory for Markdown
    for (format, name) in [(Format::Pdf, "held.pdf"), (Format::Html, "held.html")] {
        let e = export(&st, "orgp", &base.join(name), format, None).unwrap_err();
        assert_eq!(e.code, E_ALREADY_EXISTS, "{name}");
    }
    std::fs::write(base.join("held-md").join("x"), b"x").unwrap();
    for layout in [None, Some(Layout::Mkdocs), Some(Layout::Techdocs)] {
        let e = export(&st, "orgp", &base.join("held-md"), Format::Md, layout).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT);
        assert!(e.message.ends_with("is not empty"), "{}", e.message);
    }
    assert_eq!(std::fs::read(base.join("held.pdf")).unwrap(), b"mine pdf");
    assert_eq!(std::fs::read(base.join("held.html")).unwrap(), b"mine html");
    // a layout applies to Markdown only
    for format in [Format::Pdf, Format::Html] {
        let e = export(
            &st,
            "orgp",
            &base.join("l.out"),
            format,
            Some(Layout::Mkdocs),
        )
        .unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (E_BAD_INPUT, "--layout mkdocs applies to the md format only")
        );
    }
    for format in [Format::Pdf, Format::Html] {
        let e = export(
            &st,
            "orgp",
            &base.join("l.out"),
            format,
            Some(Layout::Techdocs),
        )
        .unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (
                E_BAD_INPUT,
                "--layout techdocs applies to the md format only"
            )
        );
    }
    assert!(!base.join("l.out").exists());
    drop(st);
    // the store still opens and checks clean
    let World { dir, r, .. } = w;
    drop(r);
    Rdy::open(dir.path())
        .expect("the store opens again")
        .fsck()
        .expect("fsck is clean");
}

// --- 6. determinism ----------------------------------------------------------------------------

#[test]
fn pub8_profile_print_is_deterministic() {
    let w = world();
    let out = tempfile::tempdir().unwrap();
    for scope in ["canon", "tag:v1.0.0"] {
        let (one, two) = (
            model(&w.r.read(), scope, "orgp"),
            model(&w.r.read(), scope, "orgp"),
        );
        assert_eq!(one, two, "{scope}");
        let fonts = Fonts::load();
        assert_eq!(
            print::pdf::pdf_bytes(&one, &fonts).0,
            print::pdf::pdf_bytes(&two, &fonts).0,
            "{scope}"
        );
        assert_eq!(
            print::single::single_html(&one),
            print::single::single_html(&two)
        );
        let tag = scope.replace(':', "-");
        let (a, b) = (
            out.path().join(format!("{tag}-a")),
            out.path().join(format!("{tag}-b")),
        );
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let first = export_all(&w.r.read(), scope, "orgp", &a);
        let second = export_all(&w.r.read(), scope, "orgp", &b);
        for (short, o) in &first {
            assert_eq!(o.files, second[short].files, "{scope} {short}");
            assert_eq!(
                o.summary.bytes, second[short].summary.bytes,
                "{scope} {short}"
            );
        }
    }
}

// --- 7. redaction ------------------------------------------------------------------------------

/// Applies one approved redaction of `targets` to the closed project at `dir`, then opens it.
fn redact(dir: &Path, targets: Vec<RedactTarget>) -> (Rdy, Hash) {
    let f = authorized_batch_fixture(
        &dir.join(".rdy/store"),
        &[RequestSpec::new("redact", targets)],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    (Rdy::open(dir).unwrap(), f.requests[0].request)
}

#[test]
fn pub8_redacted_print_uses_the_existing_notice() {
    let (dir, mut r) = tiny();
    let fig_png = png(7, 5, [200, 30, 30]);
    let fig_blob = r.store_mut().put_blob(&fig_png).unwrap();
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$vault", "fields": {"name": "vault", "title": "VAULTCANARY title", "description": "VAULTCANARY description."}},
            {"op": "create", "type": "doc", "id": "$kept", "parent": "$vault", "fields": {"name": "kept", "title": "Kept page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$kept", "body": "Kept under the folder."},
            {"op": "create", "type": "doc", "id": "$plan", "fields": {"name": "plan", "title": "Plan page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$ps", "parent": "$plan", "fields": {"title": "Plan section"}},
            {"op": "create", "type": "prose", "id": "$secretp", "parent": "$ps", "body": "PROSECANARY body."},
            {"op": "create", "type": "prose", "parent": "$ps", "body": "Readable sibling."},
            {"op": "create", "type": "figure", "id": "$fig", "parent": "$ps", "fields": {"blob": fig_blob.to_string(), "media_type": "image/png", "alt": "FIGCANARY alt"}},
            {"op": "create", "type": "doc", "id": "$secret", "fields": {"name": "secret", "title": "DOCCANARY title", "doc_type": "page"}},
            {"op": "create", "type": "section", "parent": "$secret", "fields": {"title": "Kept child section"}},
            {"op": "create", "type": "prose", "parent": "$secret", "body": "Child text stays."}
        ]),
    );
    land(&mut r, "w1").unwrap();
    let i = |k: &str| ids[k].clone();
    profiles(
        &mut r,
        &[(
            "vaultp",
            "org",
            vec![&i("$vault"), &i("$plan"), &i("$secret")],
        )],
    );
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    {
        let p = model(&r.read(), "canon", "vaultp");
        let t = format!("{p:?}");
        assert!(t.contains("PROSECANARY") && t.contains("FIGCANARY"));
        assert!(p.blobs.contains_key(&fig_blob));
    }
    r.close().unwrap();
    let (r, request) = redact(
        dir.path(),
        vec![
            RedactTarget::AllVersions(i("$vault")),
            RedactTarget::AllVersions(i("$secretp")),
            RedactTarget::AllVersions(i("$secret")),
            RedactTarget::Blob(fig_blob),
        ],
    );
    let st = r.read();
    let out = tempfile::tempdir().unwrap();
    for scope in ["canon", "tag:v1.0.0"] {
        let p = model(&st, scope, "vaultp");
        let dir = out.path().join(scope.replace(':', "-"));
        std::fs::create_dir(&dir).unwrap();
        let all = export_all(&st, scope, "vaultp", &dir);
        for word in [
            "VAULTCANARY",
            "PROSECANARY",
            "DOCCANARY",
            "FIGCANARY",
            "Admin page",
            &fig_blob.to_string(),
            &request.to_string(),
        ] {
            assert_absent(&p, &all, word);
        }
        assert!(p.blobs.is_empty(), "{scope}: no removed blob is printed");
        for o in all.values() {
            for bytes in o.files.values() {
                assert!(
                    !bytes
                        .windows(fig_png.len())
                        .any(|x| x == fig_png.as_slice()),
                    "{scope}"
                );
            }
        }
        // the plan page: the removed prose and the removed figure each read as the one notice,
        // with no old caption; the readable sibling stays
        let plan = p
            .docs
            .iter()
            .find(|d| d.page == "plan.html")
            .expect("the plan page");
        let mut plan_paras = vec![];
        for b in &plan.blocks {
            if let PBlock::Para { runs, .. } = b {
                plan_paras.push(runs.iter().map(|r| r.text.as_str()).collect::<String>());
            }
        }
        assert_eq!(
            plan_paras.iter().filter(|t| *t == CONTENT_REMOVED).count(),
            2,
            "{scope}: {plan_paras:?}"
        );
        assert!(plan_paras.contains(&"Readable sibling.".to_string()));
        assert!(
            !plan
                .blocks
                .iter()
                .any(|b| matches!(b, PBlock::Figure { .. })),
            "{scope}: the removed figure prints as the notice"
        );
        let md = text(&all["md"], "plan.md");
        assert_eq!(md.matches(CONTENT_REMOVED).count(), 2, "{scope}: {md}");
        let html = text(&all["html"], "out.html");
        // the plan page's own section (the removed folder and doc print their notice too)
        let plan_html = &html[html.find("<section id=\"plan\"").expect("the plan section")..];
        let plan_html = &plan_html[..plan_html.find("</section>").unwrap()];
        assert_eq!(plan_html.matches(CONTENT_REMOVED).count(), 2, "{scope}");
        assert!(plan_html.contains("Readable sibling."), "{scope}");
        // every notice the print model holds is in each format, and no other: a removed page's
        // title is the notice, which the HTML contents list and the Markdown front matter repeat
        let notices = format!("{p:?}").matches(CONTENT_REMOVED).count();
        let titled = p.docs.iter().filter(|d| d.title == CONTENT_REMOVED).count();
        assert_eq!(
            titled, 2,
            "{scope}: the removed folder and doc keep their pages"
        );
        assert_eq!(
            html.matches(CONTENT_REMOVED).count(),
            notices + titled,
            "{scope}"
        );
        let md_all: usize = all["md"]
            .files
            .values()
            .map(|b| String::from_utf8_lossy(b).matches(CONTENT_REMOVED).count())
            .sum();
        assert_eq!(md_all, notices + titled, "{scope}");
        assert!(html.matches("<img ").count() == 0, "{scope}");
        assert!(pdf_words(&p).contains(CONTENT_REMOVED), "{scope}");
        assert_eq!(all["pdf"].summary.assets, 0, "{scope}");
        // the removed folder and doc keep their children's pages
        let words = format!("{p:?}");
        assert!(words.contains("Kept under the folder."), "{scope}");
        assert!(words.contains("Child text stays."), "{scope}");
        // only the chosen tag's name, as a label
        let label = if scope == "canon" { "" } else { "v1.0.0" };
        assert_eq!((p.label.as_str(), p.scope.as_str()), (label, ""));
        // the chosen tag keeps its redacted marker in every format, as its site shows it; canon
        // has no label and no marker
        let site = build_profile_site(&st, scope, "vaultp").unwrap();
        let site_home = String::from_utf8(site.files["index.html"].clone()).unwrap();
        let badge = "<span class=\"badge badge-redacted\">redacted</span>";
        let title_md = text(&all["md"], "index.md");
        assert_eq!(text(&all["mkdocs"], "docs/index.md"), title_md, "{scope}");
        let first = &pdf_lines(&p)[0];
        if scope == "canon" {
            assert!(!p.tag_redacted, "{scope}");
            assert!(!site_home.contains(badge), "{scope}");
            assert!(!html.contains("class=\"colophon\""), "{scope}");
            assert!(!html.contains("badge-redacted"), "{scope}");
            assert!(!title_md.contains("Version"), "{scope}: {title_md}");
            assert!(!title_md.contains("redacted"), "{scope}: {title_md}");
            assert!(
                !first
                    .iter()
                    .any(|l| l.contains("Version") || l.contains("redacted")),
                "{scope}: {first:?}"
            );
        } else {
            assert!(p.tag_redacted, "{scope}");
            assert!(
                site_home.contains(&format!("v1.0.0</span></span> {badge}")),
                "{scope}: {site_home}"
            );
            assert!(
                html.contains(&format!(
                    "<p class=\"colophon\">v1.0.0 {badge}</p>\n</header>\n"
                )),
                "{scope}"
            );
            assert!(
                title_md.ends_with("\nVersion: v1.0.0 (redacted)\n"),
                "{scope}: {title_md}"
            );
            assert_eq!(
                title_md.matches("Version").count(),
                1,
                "{scope}: {title_md}"
            );
            assert!(
                first.contains(&"Version: v1.0.0 (redacted)".to_string()),
                "{scope}: {first:?}"
            );
            assert!(
                pdf_words(&p).contains("Version: v1.0.0 (redacted)"),
                "{scope}"
            );
        }
    }
}
