//! ab5 (D-WJ0SJ, D-69YCN): every output a profile makes uses the profile's as-built status. A
//! profile that hides planned content leaves the same blocks and assets out of the static site,
//! the single HTML file, the PDF, the Markdown trees, TechDocs, the llms files, the search
//! index, the glossary, the questions, the navigation, the trail, the copy payloads and the
//! figures. A profile that marks shows exactly `Planned`, and a reference doc shows `since
//! <version>` unless the profile turns it off. A refused output keeps the old files.
use rdy_engine::Rdy;
use rdy_model::BlockId;
use rdy_qa::fixtures::{land, write};
use rdy_render::print::fonts::Fonts;
use rdy_render::print::layout::LItem;
use rdy_render::print::{self, Format, Layout, Print};
use rdy_render::{SiteFiles, build_profile_site};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

const HID: &str = "HIDDENPLANNEDSENTINEL";
const SHOWN: &str = "SHOWNSHIPPEDSENTINEL";
const DEC: &str = "DECISIONPLANNEDSENTINEL";

/// The standard pack with a `reference` doc type and a custom content type, `memo`.
const TEAM_PACK: &str = "\
name: team
version: \"1\"
extends: rdy-standard
doc_types:
  reference: { skeleton: [] }
block_types:
  memo: { prefix: mm, traits: [informative], body: required }
  section:
    prefix: s
    traits: [structural]
    children: [section, prose, claim, requirement, constraint, term, component, actor, interface, value, entity, flow, example, note, rationale, table, diagram, figure, code, decision, question, memo]
";

/// Every print format: the format, its layout, the name it writes and a short name.
const FORMATS: [(Format, Option<Layout>, &str, &str); 5] = [
    (Format::Pdf, None, "out.pdf", "pdf"),
    (Format::Html, None, "out.html", "html"),
    (Format::Md, None, "out-md", "md"),
    (Format::Md, Some(Layout::Mkdocs), "out-mkdocs", "mkdocs"),
    (
        Format::Md,
        Some(Layout::Techdocs),
        "out-techdocs",
        "techdocs",
    ),
];

// --- helpers ------------------------------------------------------------------------------------

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Writes `ops` to `view` as the writer; panics on a refusal.
fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, BlockId> {
    let w = write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e}"));
    w.created.iter().map(|(k, v)| (k.clone(), bid(v))).collect()
}

/// A project using the `team` pack.
fn team_project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    rdy_qa::packs::use_pack(dir.path(), "team", TEAM_PACK, "name: \"As built\"\n");
    let r = Rdy::open(dir.path()).unwrap();
    (dir, r)
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

/// The words (name and bytes) any file holds of `words`, as `path: word`.
fn leaks(files: &BTreeMap<String, Vec<u8>>, words: &[String]) -> Vec<String> {
    let mut out = vec![];
    for (path, bytes) in files {
        for w in words {
            let w = w.as_bytes();
            if path.as_bytes().windows(w.len()).any(|x| x == w)
                || bytes.windows(w.len()).any(|x| x == w)
            {
                out.push(format!("{path}: {}", String::from_utf8_lossy(w)));
            }
        }
    }
    out
}

/// A file of a map, as text.
fn text(files: &BTreeMap<String, Vec<u8>>, path: &str) -> String {
    String::from_utf8(
        files
            .get(path)
            .unwrap_or_else(|| panic!("no file {path}: {:?}", files.keys()))
            .clone(),
    )
    .unwrap()
}

/// All text files of a map joined, figure files left out.
fn joined(files: &BTreeMap<String, Vec<u8>>) -> String {
    files
        .iter()
        .filter(|(p, _)| !p.ends_with(".png"))
        .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

// --- the fixture --------------------------------------------------------------------------------

struct Fix {
    _dir: tempfile::TempDir,
    r: Rdy,
    ids: BTreeMap<String, BlockId>,
    hidden_png: Vec<u8>,
    hidden_blob: String,
    shown_blob: String,
}

impl Fix {
    fn id(&self, k: &str) -> String {
        self.ids[k].to_string()
    }

    /// Every id of a planned block, and of the decision that is planned.
    fn hidden_ids(&self) -> Vec<String> {
        [
            "$np", "$op", "$hp", "$ts", "$tc", "$tt", "$ff", "$tb", "$cd", "$cl", "$dg", "$mm",
            "$dp",
        ]
        .iter()
        .map(|k| self.id(k))
        .collect()
    }

    /// Words no output of the hiding profile holds.
    fn hidden_words(&self) -> Vec<String> {
        let mut w = vec![HID.to_string(), DEC.to_string()];
        w.extend(self.hidden_ids());
        w.push(self.hidden_blob.clone());
        w.push(base64(&self.hidden_png));
        w
    }
}

/// A reference doc `guide` with shipped content and planned content of every shape, the planned
/// decision `$dp` and shipped decision `$dv` in a doc outside every selection, and org profiles
/// `hide`, `mark` and `off` selecting the guide.
fn fixture() -> Fix {
    let (dir, mut r) = team_project();
    let hidden_png = png(4, 4, [200, 10, 10]);
    let shown_png = png(3, 3, [10, 200, 10]);
    let hidden_blob = r.store_mut().put_blob(&hidden_png).unwrap();
    let shown_blob = r.store_mut().put_blob(&shown_png).unwrap();
    let mut ids = put(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$rules", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dp", "parent": "$rules", "fields": {"statement": format!("{DEC} later."), "status": "proposed", "built": "planned"}},
            {"op": "create", "type": "decision", "id": "$dv", "parent": "$rules", "fields": {"statement": "Ship the built part.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "reference"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$guide", "fields": {"title": format!("{SHOWN} section")}},
            {"op": "create", "type": "term", "id": "$tv", "parent": "$s", "fields": {"name": format!("{SHOWN}Term")}, "body": "A shipped term."},
            {"op": "create", "type": "question", "id": "$qv", "parent": "$s", "fields": {"prompt": format!("{SHOWN} question?")}},
            {"op": "create", "type": "figure", "id": "$fv", "parent": "$s", "fields": {"blob": shown_blob.to_string(), "media_type": "image/png", "alt": format!("{SHOWN} figure")}},
            {"op": "create", "type": "table", "id": "$tv2", "parent": "$s", "fields": {"columns": ["Col"], "rows": [[format!("{SHOWN} cell")]]}},
            {"op": "create", "type": "code", "id": "$cv", "parent": "$s", "fields": {"lang": "python"}, "body": format!("print('{SHOWN} code')")},
            {"op": "create", "type": "note", "id": "$np", "parent": "$s", "fields": {"planned": true}, "body": format!("{HID} old planned note.")},
            {"op": "create", "type": "note", "id": "$op", "parent": "$s", "body": format!("{HID} note of the planned decision."), "links": {"decided_by": ["$dp"]}},
            {"op": "create", "type": "prose", "id": "$hp", "parent": "$s", "fields": {"planned": true}, "body": format!("{HID} prose.")},
            {"op": "create", "type": "section", "id": "$ts", "parent": "$s", "fields": {"title": format!("{HID} section"), "planned": true}},
            {"op": "create", "type": "prose", "id": "$tc", "parent": "$ts", "body": format!("{HID} child.")},
            {"op": "create", "type": "term", "id": "$tt", "parent": "$s", "fields": {"name": format!("{HID}Term"), "planned": true}, "body": "A planned term."},
            {"op": "create", "type": "figure", "id": "$ff", "parent": "$s", "fields": {"blob": hidden_blob.to_string(), "media_type": "image/png", "alt": format!("{HID} figure"), "planned": true}},
            {"op": "create", "type": "table", "id": "$tb", "parent": "$s", "fields": {"columns": ["Col"], "rows": [[format!("{HID} cell")]], "planned": true}},
            {"op": "create", "type": "code", "id": "$cd", "parent": "$s", "fields": {"lang": "python", "planned": true}, "body": format!("print('{HID} code')")},
            {"op": "create", "type": "claim", "id": "$cl", "parent": "$s", "fields": {"planned": true}, "body": format!("{HID} claim.")},
            {"op": "create", "type": "diagram", "id": "$dg", "parent": "$s", "fields": {"lang": "mermaid", "alt": format!("{HID} diagram"), "planned": true}, "body": format!("graph TD\n  A[{HID}] --> B[x]")},
            {"op": "create", "type": "memo", "id": "$mm", "parent": "$s", "fields": {"planned": true}, "body": format!("{HID} memo.")},
            {"op": "create", "type": "prose", "id": "$pv", "parent": "$s", "body": format!("{SHOWN} prose. See {{{{$hp}}}} and {{{{$dp}}}}."), "links": {"decided_by": ["$dv"]}},
            {"op": "create", "type": "prose", "id": "$ps", "parent": "$s", "fields": {"since": "v2.0.0-rc.1"}, "body": "Prerelease prose."},
            {"op": "create", "type": "prose", "id": "$p3", "parent": "$s", "fields": {"since": "1.2.3"}, "body": "Patch prose."},
            {"op": "create", "type": "doc", "id": "$notes", "fields": {"name": "notes", "title": "Notes", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$pn", "parent": "$notes", "fields": {"since": "3.0.0"}, "body": "Notes prose."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    land(&mut r, "work").expect("land the guide");
    let profiles = put(
        &mut r,
        "work2",
        json!([
            {"op": "create", "type": "publication_profile", "id": "$hide", "parent": ids["$admin"].to_string(),
             "fields": {"key": "hide", "title": "Hide site", "audience": "org", "planned": "hide", "select": [ids["$guide"].to_string()]}},
            {"op": "create", "type": "publication_profile", "id": "$mark", "parent": ids["$admin"].to_string(),
             "fields": {"key": "mark", "title": "Mark site", "audience": "org", "planned": "mark", "select": [ids["$guide"].to_string()]}},
            {"op": "create", "type": "publication_profile", "id": "$show", "parent": ids["$admin"].to_string(),
             "fields": {"key": "show", "title": "Show site", "audience": "org", "planned": "mark", "since": "show", "select": [ids["$guide"].to_string(), ids["$notes"].to_string()]}},
            {"op": "create", "type": "publication_profile", "id": "$omit", "parent": ids["$admin"].to_string(),
             "fields": {"key": "omit", "title": "Omit site", "audience": "org", "planned": "mark", "select": [ids["$guide"].to_string(), ids["$notes"].to_string()]}},
            {"op": "create", "type": "publication_profile", "id": "$off", "parent": ids["$admin"].to_string(),
             "fields": {"key": "off", "title": "Off site", "audience": "org", "planned": "mark", "since": "off", "select": [ids["$guide"].to_string()]}}
        ]),
    );
    land(&mut r, "work2").expect("land the profiles");
    ids.extend(profiles);
    Fix {
        _dir: dir,
        r,
        ids,
        hidden_png,
        hidden_blob: hidden_blob.to_string(),
        shown_blob: shown_blob.to_string(),
    }
}

/// The print of `profile` at canon in every format, each written under `dir`.
fn prints(f: &Fix, profile: &str, dir: &Path) -> BTreeMap<String, BTreeMap<String, Vec<u8>>> {
    let mut out = BTreeMap::new();
    for (format, layout, name, short) in FORMATS {
        let at = dir.join(name);
        print::export_profile(&f.r.read(), "canon", profile, &at, format, layout)
            .unwrap_or_else(|e| panic!("{short} of {profile}: {e}"));
        let files = if at.is_dir() {
            tree(&at)
        } else {
            BTreeMap::from([(name.to_string(), std::fs::read(&at).unwrap())])
        };
        out.insert(short.to_string(), files);
    }
    out
}

fn site(f: &Fix, profile: &str) -> SiteFiles {
    build_profile_site(&f.r.read(), "canon", profile).unwrap_or_else(|e| panic!("{profile}: {e}"))
}

fn model(f: &Fix, profile: &str) -> Print {
    print::build_profile(&f.r.read(), "canon", profile).unwrap_or_else(|e| panic!("{profile}: {e}"))
}

// --- tests --------------------------------------------------------------------------------------

#[test]
fn ab5_all_formats_hide_the_same_blocks_and_assets() {
    let f = fixture();
    let hidden = f.hidden_words();
    let s = site(&f, "hide");
    // the static site: no file name or byte holds a hidden word, in any page, index, payload,
    // navigation, trail, figure or agent file
    assert_eq!(leaks(&s.files, &hidden), Vec::<String>::new());
    let all = joined(&s.files);
    for w in [
        format!("{SHOWN} prose"),
        format!("{SHOWN}Term"),
        format!("{SHOWN} question?"),
        format!("{SHOWN} cell"),
        format!("{SHOWN} section"),
    ] {
        assert!(all.contains(&w), "{w} is on the site");
    }
    // a reference to a hidden block reads as neutral words, with no link to its anchor
    assert!(all.contains("See unavailable reference and"), "{all}");
    for id in f.hidden_ids() {
        assert!(!all.contains(&format!("#{id}")), "{id}");
        assert!(!all.contains(&format!("go/{id}.html")), "{id}");
    }
    // the shown figure stays, the hidden figure's file does not
    assert!(
        s.files
            .contains_key(&format!("assets/{}.png", f.shown_blob))
    );
    assert!(
        !s.files
            .contains_key(&format!("assets/{}.png", f.hidden_blob))
    );
    // the agent files and the pages keep the shown words only
    for file in [
        "llms.txt",
        "llms-full.txt",
        "glossary.html",
        "questions.html",
    ] {
        let t = text(&s.files, file);
        assert!(t.contains(SHOWN), "{file}");
        assert!(!t.contains(HID), "{file}");
    }
    assert!(text(&s.files, "llms-full.txt").contains(&format!("{SHOWN} prose")));
    // the planned decision sits outside the selection: its words reach no page, and the copy
    // payload of the shipped block carries its status line
    let guide = text(&s.files, "guide.html");
    assert!(guide.contains("**since 1.0.0**"), "the copy payload");
    assert!(!guide.contains("later."));

    // every print format reads the same view
    let dir = tempfile::tempdir().unwrap();
    let outputs = prints(&f, "hide", dir.path());
    let p = model(&f, "hide");
    let words = pdf_words(&p);
    for (short, files) in &outputs {
        assert_eq!(leaks(files, &hidden), Vec::<String>::new(), "{short}");
        let t = joined(files);
        for w in [
            format!("{SHOWN} prose"),
            format!("{SHOWN} section"),
            format!("{SHOWN} cell"),
            format!("{SHOWN} code"),
        ] {
            if short != "pdf" {
                assert!(t.contains(&w), "{short}: {w}");
            }
        }
        assert!(
            !t.contains("Planned"),
            "{short}: a hidden profile marks nothing"
        );
    }
    for w in &hidden {
        assert!(!words.contains(w), "{w} in the PDF text");
        assert!(!format!("{p:?}").contains(w), "{w} in the print model");
    }
    for w in [
        format!("{SHOWN} prose"),
        format!("{SHOWN} section"),
        format!("{SHOWN} cell"),
        format!("{SHOWN} code"),
    ] {
        assert!(words.contains(&w), "{w} in the PDF text");
    }
    // the embedded figure of the single file is the shown one
    let single = text(&outputs["html"], "out.html");
    assert!(single.contains(&base64(&png(3, 3, [10, 200, 10]))));
}

#[test]
fn ab5_mark_and_since_are_consistent_and_quiet() {
    let f = fixture();
    // 12 planned blocks show `Planned`: two notes carry it as their eyebrow, ten others as a
    // status line (the section, its child by inheritance, and the rest by their own flag)
    let marker = "<span class=\"marker\">Planned</span>";
    let pill = "<p class=\"block-status\"><span class=\"status status-planned\">Planned</span></p>";
    for profile in ["mark", "off", "show", "omit"] {
        let s = site(&f, profile);
        let guide = text(&s.files, "guide.html");
        assert_eq!(guide.matches(marker).count(), 2, "{profile}");
        assert_eq!(guide.matches(pill).count(), 10, "{profile}");
        assert_eq!(
            guide.matches(">Planned</span>").count(),
            12,
            "{profile}: no duplicate"
        );
        // every note here is planned: none keeps its type's eyebrow
        assert!(
            !guide.contains("<span class=\"marker\">Note</span>"),
            "{profile}"
        );
        // a marked block keeps its words
        assert!(guide.contains(&format!("{HID} prose.")), "{profile}");
        // the copy payload and the agent files carry the same word
        assert!(guide.contains("**Planned**"), "{profile}: copy payload");
        assert!(
            text(&s.files, "llms-full.txt").contains("Planned"),
            "{profile}"
        );
        // a since label never links, and no page lists source versions
        assert!(!guide.contains("<a href=\"~versions"), "{profile}");
        assert!(!guide.contains("/~versions/"), "{profile}");
    }
    // since: a reference doc shows the label unless the profile says off; other docs only on show
    let since = |profile: &str, page: &str, label: &str| {
        let s = site(&f, profile);
        text(&s.files, page).contains(&format!("status-since\">{label}</span>"))
    };
    for (profile, page, label, want) in [
        ("omit", "guide.html", "since 1.0.0", true),
        ("omit", "guide.html", "since 2.0.0-rc.1", true),
        ("omit", "guide.html", "since 1.2.3", true),
        ("omit", "notes.html", "since 3.0.0", false),
        ("show", "notes.html", "since 3.0.0", true),
        ("show", "guide.html", "since 1.0.0", true),
        ("off", "guide.html", "since 1.0.0", false),
        ("off", "guide.html", "since 1.2.3", false),
        ("mark", "guide.html", "since 1.2.3", true),
    ] {
        assert_eq!(
            since(profile, page, label),
            want,
            "{profile} {page} {label}"
        );
    }
    // the label keeps the whole version: 1.2.3 is never cut to 1.2, a leading v goes, a
    // prerelease stays
    let shown = text(&site(&f, "omit").files, "guide.html");
    assert!(!shown.contains("since 1.2<"));
    assert!(!shown.contains("since v2"));
    // an unversioned block carries no label, and off removes the pill only
    assert_eq!(shown.matches("status-since").count(), 3);
    let off = text(&site(&f, "off").files, "guide.html");
    assert_eq!(off.matches("status-since").count(), 0);
    assert_eq!(off.matches(">Planned</span>").count(), 12);

    // the source site (no profile) marks planned blocks and shows no since label
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&f.r.read(), &rdy_engine::Scope::Canon, out.path()).unwrap();
    let source = tree(out.path());
    let src_guide = text(&source, "guide.html");
    assert_eq!(src_guide.matches(">Planned</span>").count(), 12);
    assert!(!src_guide.contains("status-since"));

    // the PDF text, the single file and the Markdown trees say the same
    let dir = tempfile::tempdir().unwrap();
    let outputs = prints(&f, "omit", dir.path());
    let words = pdf_words(&model(&f, "omit"));
    assert!(words.contains("since 1.0.0") && words.contains("since 2.0.0-rc.1"));
    assert!(words.contains("since 1.2.3") && !words.contains("since 3.0.0"));
    assert_eq!(words.matches("Planned").count(), 12, "{words}");
    for (short, files) in &outputs {
        if short == "pdf" {
            continue;
        }
        let t = joined(files);
        assert!(
            t.contains("since 1.0.0") && t.contains("since 2.0.0-rc.1"),
            "{short}"
        );
        assert!(
            t.contains("since 1.2.3") && !t.contains("since 3.0.0"),
            "{short}"
        );
        assert!(
            !t.contains("since 1.2 ") && !t.contains("since 1.2\n"),
            "{short}"
        );
        assert_eq!(t.matches("Planned").count(), 12, "{short}");
    }
    // the status line is a paragraph of its own in the Markdown, not part of a heading or a
    // code block
    let md = joined(&outputs["md"]);
    assert!(md.contains("**Planned**\n"), "{md}");
    assert!(!md.contains("# Planned"));
    assert!(!md.contains("`Planned`"));
}

/// The refusal a public profile gives for `n` references outside its selection.
fn blocked(n: usize) -> (String, &'static str) {
    (
        format!("Public profile has {n} references outside its selection."),
        "Include the target docs or remove these references. Run the publication check again.",
    )
}

#[test]
fn ab5_failed_profile_output_preserves_existing_files() {
    let (dir, mut r) = team_project();
    let hidden_png = png(4, 4, [200, 10, 10]);
    let hidden_blob = r.store_mut().put_blob(&hidden_png).unwrap().to_string();
    let ids = put(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$guide", "fields": {"title": "Usage"}},
            {"op": "create", "type": "prose", "id": "$hp", "parent": "$s", "fields": {"planned": true}, "body": format!("{HID} prose.")},
            {"op": "create", "type": "figure", "id": "$ff", "parent": "$s", "fields": {"blob": hidden_blob, "media_type": "image/png", "alt": format!("{HID} figure"), "planned": true}},
            {"op": "create", "type": "prose", "id": "$pv", "parent": "$s", "body": format!("{SHOWN} prose, see {{{{$hp}}}}.")},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    land(&mut r, "work").unwrap();
    let prof = put(
        &mut r,
        "work2",
        json!([{"op": "create", "type": "publication_profile", "id": "$pubp", "parent": ids["$admin"].to_string(),
            "fields": {"key": "pubp", "title": "Public site", "audience": "public", "planned": "mark", "select": [ids["$guide"].to_string()]}}]),
    );
    land(&mut r, "work2").unwrap();
    let hidden_file = format!("assets/{hidden_blob}.png");

    // a marking public profile ships the planned block and its figure
    let site_dir = dir.path().join("pub-site");
    render_profile_site_at(&r, "pubp", &site_dir).expect("marked output");
    let shown = tree(&site_dir);
    assert!(shown.contains_key(&hidden_file));
    assert!(text(&shown, "guide.html").contains(&format!("{HID} prose.")));

    // the profile now hides planned content, which turns the reference into one outside the
    // selection: the output refuses with the exact words, reads no asset and keeps every file
    write(
        &mut r,
        "work3",
        json!([{"op": "update", "id": prof["$pubp"].to_string(), "fields": {"planned": "hide"}}]),
    )
    .unwrap();
    land(&mut r, "work3").unwrap();
    let reads = rdy_engine::publication::blob_reads_for_tests();
    let (message, fix) = blocked(1);
    let e = render_profile_site_at(&r, "pubp", &site_dir).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (rdy_engine::E_PUBLICATION_REFERENCE, message.as_str(), fix),
        "{e}"
    );
    for (format, layout, name, short) in FORMATS {
        let at = dir.path().join(format!("refused-{name}"));
        let e = print::export_profile(&r.read(), "canon", "pubp", &at, format, layout).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (rdy_engine::E_PUBLICATION_REFERENCE, message.as_str(), fix),
            "{short}: {e}"
        );
        assert!(!at.exists(), "{short}");
    }
    assert_eq!(
        rdy_engine::publication::blob_reads_for_tests(),
        reads,
        "a refusal reads no asset"
    );
    assert!(tree(&site_dir) == shown, "the old output is intact");
    // the in-memory build refuses the same way
    let e = build_profile_site(&r.read(), "canon", "pubp")
        .err()
        .expect("refused");
    assert_eq!(e.code, rdy_engine::E_PUBLICATION_REFERENCE);

    // the author removes the reference: the output regenerates, and the formerly shown files go
    write(
        &mut r,
        "work4",
        json!([{"op": "update", "id": ids["$pv"].to_string(), "body": format!("{SHOWN} prose.")}]),
    )
    .unwrap();
    land(&mut r, "work4").unwrap();
    render_profile_site_at(&r, "pubp", &site_dir).expect("regenerated output");
    let after = tree(&site_dir);
    assert!(
        !after.contains_key(&hidden_file),
        "the hidden figure's file is gone"
    );
    let hide = vec![HID.to_string(), hidden_blob.clone(), ids["$hp"].to_string()];
    assert_eq!(leaks(&after, &hide), Vec::<String>::new());
    assert!(text(&after, "guide.html").contains(&format!("{SHOWN} prose.")));
    for (format, layout, name, short) in FORMATS {
        let at = dir.path().join(format!("ok-{name}"));
        print::export_profile(&r.read(), "canon", "pubp", &at, format, layout)
            .unwrap_or_else(|e| panic!("{short}: {e}"));
        let files = if at.is_dir() {
            tree(&at)
        } else {
            BTreeMap::from([(name.to_string(), std::fs::read(&at).unwrap())])
        };
        assert_eq!(leaks(&files, &hide), Vec::<String>::new(), "{short}");
    }
}

fn render_profile_site_at(r: &Rdy, profile: &str, out: &Path) -> Result<(), rdy_engine::RdyError> {
    rdy_render::render_profile_site(&r.read(), "canon", profile, out).map(|_| ())
}
