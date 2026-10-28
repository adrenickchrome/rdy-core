//! pub3 (D-0VCQ5, D-5CVWB): every output reads source data that preparation read before the
//! output boundary. The site and print builders receive the prepared data and no source reader;
//! the source outputs keep their bytes. A `publication_profile` block is administrative: it is off
//! every site and print output, like a rationale, and stays in the source state.
use rdy_engine::{Rdy, Scope};
use rdy_model::{Block, BlockId, Change, ChangeLinks, FieldValue, Hash, Link, Op};
use rdy_qa::golden::*;
use rdy_render::print::{self, Layout, Print};
use rdy_render::{Mode, SiteFiles, build_site};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A digest of bytes: each piece after its length, so no two inputs share bytes.
#[derive(Default)]
struct Digest(Vec<u8>);

impl Digest {
    fn piece(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(&(b.len() as u64).to_le_bytes());
        self.0.extend_from_slice(b);
        self
    }
    fn hex(&self) -> String {
        Hash::of(&self.0).to_string()
    }
}

/// Whether `bytes` are the site's generated not-found page: its marker, never its file name.
fn generated_404(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes).contains("data-rdy-kind=\"not-found\"")
}

/// The digest of a whole built site: every file by path and bytes, the label, the header, where
/// the label and header went, and the summary.
fn site_digest(s: &SiteFiles) -> String {
    let mut d = Digest::default();
    // the generated not-found page is left out, so the digest pins every other file's bytes
    for (path, bytes) in s.files.iter().filter(|(_, b)| !generated_404(b)) {
        d.piece(path.as_bytes()).piece(bytes);
    }
    d.piece(s.label.as_bytes()).piece(s.header.as_bytes());
    d.piece(format!("{:?}", s.label_at).as_bytes());
    d.piece(
        format!(
            "{} {} {} {:?}",
            s.summary.pages, s.summary.assets, s.summary.redirects, s.summary.warnings
        )
        .as_bytes(),
    );
    d.hex()
}

/// Every file under `dir`, by its path relative to `dir`, in order.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(at).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn tree_digest(files: &BTreeMap<String, Vec<u8>>) -> String {
    let mut d = Digest::default();
    for (path, bytes) in files {
        d.piece(path.as_bytes()).piece(bytes);
    }
    d.hex()
}

/// Every output of a print: the model (without `rdy_dir`, and without `tag_redacted`, which a
/// source print always leaves false), the PDF, the single HTML file and both Markdown layouts.
struct PrintOut {
    model: String,
    pdf: Vec<u8>,
    html: Vec<u8>,
    md: BTreeMap<String, Vec<u8>>,
    mkdocs: BTreeMap<String, Vec<u8>>,
}

fn print_out(p: &Print) -> PrintOut {
    let mut model = p.clone();
    model.rdy_dir = PathBuf::new();
    let out = tempfile::tempdir().unwrap();
    let (pdf, html, md, mkdocs) = (
        out.path().join("p.pdf"),
        out.path().join("p.html"),
        out.path().join("md"),
        out.path().join("mkdocs"),
    );
    print::pdf::write_pdf(p, &pdf).expect("the pdf");
    print::single::write_single_html(p, &html).expect("the html file");
    print::md::write_markdown(p, &md, Layout::Plain).expect("the markdown tree");
    print::md::write_markdown(p, &mkdocs, Layout::Mkdocs).expect("the mkdocs tree");
    PrintOut {
        // pub8 added `tag_redacted` for a named profile's chosen tag; a source print has none
        model: {
            assert!(!p.tag_redacted, "a source print marks no tag");
            format!("{model:?}").replacen(", tag_redacted: false", "", 1)
        },
        pdf: std::fs::read(&pdf).unwrap(),
        html: std::fs::read(&html).unwrap(),
        md: tree(&md),
        mkdocs: tree(&mkdocs),
    }
}

fn print_digests(p: &Print) -> [String; 5] {
    let o = print_out(p);
    let one = |b: &[u8]| Digest::default().piece(b).hex();
    [
        one(o.model.as_bytes()),
        one(&versionless(&o.pdf)),
        one(&o.html),
        tree_digest(&o.md),
        tree_digest(&o.mkdocs),
    ]
}

/// The PDF with its `/Producer (rdy <version>)` text masked to the same length, so a version
/// bump keeps the digest while the version keeps its length. A bump that changes the version's
/// length (0.9.0 to 0.10.0) moves the PDF's later byte offsets: re-take the PDF digests then.
fn versionless(pdf: &[u8]) -> Vec<u8> {
    let producer = format!("rdy {}", env!("CARGO_PKG_VERSION")).into_bytes();
    let mask = [b"rdy ".as_slice(), &vec![b'#'; producer.len() - 4]].concat();
    let mut out = pdf.to_vec();
    let at = out
        .windows(producer.len())
        .position(|w| w == producer.as_slice())
        .expect("the producer text in the PDF");
    out[at..at + producer.len()].copy_from_slice(&mask);
    out
}

/// The golden project's `v1.0.0` landing.
fn tag_scope(r: &Rdy) -> Scope {
    Scope::At(
        r.read().versions().expect("versions")[0]
            .landing
            .parse()
            .expect("a landing hash"),
    )
}

/// The review project's proposal of the view `rewrite`, made at the fixture's time.
fn proposal_scope(r: &mut Rdy) -> Scope {
    let _clock = fixture_clock();
    let p = r
        .propose(&writer(), REVIEW_VIEW, "Rewrite the guide", None)
        .expect("propose");
    r.read()
        .resolve_scope(&format!("proposal:{}", p.id))
        .expect("the proposal scope")
}

/// The seed of this file's ids: the fixtures' generated ids (and the repo id) replay exactly, so
/// their outputs have the same bytes on every run.
const ID_SEED: u64 = 0x7075_6233;

/// Each source scope the outputs support, with the project that holds it: canon, the `v1.0.0`
/// tag and the `editing` view of the golden project, and a proposal of the review project. Every
/// project is written with this thread's ids seeded.
fn scopes() -> Vec<(&'static str, tempfile::TempDir, Rdy, Scope)> {
    let mut out = vec![];
    for name in ["canon", "tag", "view"] {
        rdy_model::seed_ids_for_tests(ID_SEED);
        let (dir, r, _ids) = project();
        let scope = match name {
            "tag" => tag_scope(&r),
            "view" => Scope::View("editing".into()),
            _ => Scope::Canon,
        };
        out.push((name, dir, r, scope));
    }
    rdy_model::seed_ids_for_tests(ID_SEED);
    let (dir, mut r, _ids) = review_project();
    let scope = proposal_scope(&mut r);
    rdy_model::unseed_ids_for_tests();
    out.push(("proposal", dir, r, scope));
    out
}

/// The source site digests, captured from the unmodified base (22af149) before the refactor:
/// (scope, static, live at `/`). Re-taken in hpb6 for the notice styles added to
/// `assets/site.css`; every other file, the label, the header, their ranges and the summary
/// kept their bytes.
/// Re-taken for the live profiles change (D-CWC91: the menu `LABEL` becomes
/// "Versions, proposals, views and profiles" and the script fetches the pill's href in place of
/// the hard-coded PAGE; fix rounds 2827fddd and 2f7023bd add its same-origin http/https check):
/// only `assets/app.js`, which bundles `js/menu.js`, changed; every other file, the label, the
/// header, their ranges and the summary kept their bytes.
/// Re-taken after the reference period rule (ruling task-7): an automatic label drops its final period before an authored period; only that text moved.
/// Re-taken for the renderer plan's Task 8 (sidebar folders): the navigation markup of the pages,
/// the sidebar rules of `assets/site.css` and `assets/app.js` (which now bundles `js/sidebar.js`)
/// moved; their content rows keep their text.
/// Re-taken again in Task 8 fix round 1: only `js/sidebar.js` changed (byte-size bounds and absolute-URL
/// validation of the stored state), so only `assets/app.js` moved.
/// Re-taken for the renderer plan's Task 6 (native diagrams, ruling task-8): the diagram figures
/// of the pages that have one, their removed library script tag, the new `assets/diagrams/` files,
/// `assets/js/mermaid.min.js` (gone), the diagram rules of `assets/site.css`, `assets/app.js`
/// (without `js/mermaid-init.js`) and an `open full size` line per drawing in `llms-full.txt` moved.
/// Re-taken for Task 6 fix round 1 (ruling task-6): the open link is a reader control, so each
/// drawing's `open full size` line left `llms-full.txt`; every other file and the warnings kept
/// their bytes.
/// Re-taken for the renderer plan's Task 9 (Copy, ruling task-8 form): each doc and folder page's
/// title toolbar (Link, Copy ref, Copy), the Copy button on each block and section toolbar, the
/// page's hidden Copy payload template and `assets/app.js` (which bundles `js/copy.js`) moved;
/// their content rows keep their text (the gold1_ goldens).
/// Re-taken for the final review's fix round 1 (M2): Cite skips a selection in the eyebrow, so
/// only `js/copy.js` changed and only `assets/app.js` moved; with the old script the digests
/// were the ones before.
/// Re-taken for the review markers plan's Task 1 (D-NYCMF): the sidebar marker rules of
/// `assets/site.css` moved, so every scope's digest moved; in a scope with review marks, the
/// sidebar's changed rows moved too. Canon and tag pages keep their bytes.
/// Re-taken for the review markers plan's Task 2 (D-NYCMF): the page index's marker rules of
/// `assets/site.css` moved, so every scope's digest moved; in a view and a proposal, the changed
/// doc pages' index moved too. With the old stylesheet the canon and tag digests are the ones
/// before.
/// Re-taken for Task 2's fix round (ruling task-2): only the index marker rule of
/// `assets/site.css` moved (`.toc-link:not(.toc-chg) .chg-mark`); with the old stylesheet every
/// digest is the one before.
const SITE_DIGESTS: [(&str, &str, &str); 4] = [
    (
        "canon",
        "65226f6a02b833a5f5e3decb742a18c4c26c8b37b5c1ca50e5af3fa7863db677",
        "4190f0e4220ebe5f63ec45114c7f35945ce09cef26951a69233968534aa69806",
    ),
    (
        "tag",
        "74551ea6342d8eaabd066a7dcc0c96ba37d3c36328c58a754244912034ef7aeb",
        "4d3edea62cf5d688a37d164b29ff277415950dca5a69fdd6500a098df860e7d6",
    ),
    (
        "view",
        "26f60d0d3cb864465833c6e6f62db789a114143b04205fec09c99ee2706bdc03",
        "58197184a6b10a2545c2206ea68eea14452275b5fd0568a2cf1a54bf8432b346",
    ),
    (
        "proposal",
        "c08f703f960dbc76ab6a4ac9fc5c370567d882576381cd6230a462c0b90b150f",
        "a532c6f415a1870b3d4ee3bf2f684858f24cc566871b41a910a0b0cfd7f879e8",
    ),
];

/// The source print digests, captured from the unmodified base (22af149) before the refactor:
/// (scope, [model without rdy_dir, pdf, html, md plain, md mkdocs]). The PDF digests carry the
/// crate version (the PDF producer field) and were re-taken at the 0.7.0 merge of main.
/// Re-taken after the reference period rule (ruling task-7): an automatic label drops its final period before an authored period; only that text moved.
const PRINT_DIGESTS: [(&str, [&str; 5]); 4] = [
    (
        "canon",
        [
            "449e831d3d9a8ce4c815d67c82ceb9882cb4ef3a0d7942814d9e9cc19b23cf59",
            "998a127285a0c7a623a78212f23eb98cceb92d1bdfaf7bdfa5f6f43d26853adc",
            "938c9836e463189769cb4d487178c6425a9fbe988027970549c20768293d1b40",
            "9c07f3c2a6b76e09131538f66b58ef5eb6826791faa2351d8ac4e0102744a980",
            "5be1453c6b29b76de993e5a562b3e972926d57b1073fd4dccf2a21a4b4058832",
        ],
    ),
    (
        "tag",
        [
            "fa8390847bb9fb3ca37037994919553542c8e3d35034d23b63d24c7c3d5b71f0",
            "b343a5b5d478cfdeca78cd8fc3c4040afcacfdf16f76306be4122c254a97a104",
            "c8a8b44f679d1d5be3116346f252b6dd14e1109418d8a070c73a0767096bdcc5",
            "db1857ceb8bf4b1082e70cfd9998c0fc162f16aee6dc0d19b244a7fcf0905bef",
            "64ee2f7e057704eabf633568380874172061d9c64de3b0ea5209611ffa945fb0",
        ],
    ),
    (
        "view",
        [
            "28270a66f2a099986b7123828f7797dfba9a748da1a7bfc42d71527355879eca",
            "9d0ee3035447042b8ef884e13947f952fdf3269a2f159cdc34c5a2123b8576f8",
            "40640e6325548294e74f05ccb7a4910a65d9242fab0df3b5c2c232ecbad93fb1",
            "6fd72f8df9966c7d1429a0c30d2081800699fb43a7288cad204b42c7e68f548f",
            "513f948e73af5d16b2352b42e9611a59bd64f1b80e772a929fed3f829411def6",
        ],
    ),
    (
        "proposal",
        [
            "631048370e9da5fdcf57e88f1be37d4610444152fc9edce1835a4568c4c76a62",
            "6f6f4bdf061c34fedacbc12cd0dd168451b99b305b211db9c65c9e88a4608524",
            "2c77b990417a0b86e3d613b340af6fe30562c58142c0fa4c0e05bd3c1f589b25",
            "e7a334c53da6cc5ad8fd0e46af8cd9fbbc5c1c3ca45f2db2e7874079d49f0ad6",
            "60236789e2b544c65d11d5e3c825fb879b4b4f0e7230d4557570a8f4ecb495e9",
        ],
    ),
];

#[test]
fn pub3_source_site_bytes_survive_preparation() {
    let mut got = vec![];
    for (name, _dir, r, scope) in scopes() {
        let st = r.read();
        let s = build_site(&st, &scope, Mode::Static).expect("the static site");
        let l = build_site(&st, &scope, Mode::Live { root: "/" }).expect("the live site");
        got.push((name, site_digest(&s), site_digest(&l)));
    }
    let got: Vec<(&str, &str, &str)> = got
        .iter()
        .map(|(n, s, l)| (*n, s.as_str(), l.as_str()))
        .collect();
    assert_eq!(got, SITE_DIGESTS.to_vec(), "{got:#?}");
}

#[test]
fn pub3_source_print_bytes_survive_preparation() {
    let mut got = vec![];
    for (name, dir, r, scope) in scopes() {
        let p = print::build(&r.read(), &scope).expect("the print");
        assert_eq!(p.rdy_dir, dir.path().join(".rdy"), "{name}");
        got.push((name, print_digests(&p)));
    }
    let got: Vec<(&str, [&str; 5])> = got
        .iter()
        .map(|(n, d)| (*n, [0, 1, 2, 3, 4].map(|i| d[i].as_str())))
        .collect();
    assert_eq!(got, PRINT_DIGESTS.to_vec(), "{got:#?}");
}

/// The signature and body of the function `name` in `src`, through its closing brace.
fn function<'a>(src: &'a str, name: &str) -> &'a str {
    let start = src
        .find(&format!("fn {name}("))
        .unwrap_or_else(|| panic!("no fn {name}"));
    let open = start + src[start..].find('{').expect("a body");
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[start..=open + i];
                }
            }
            _ => {}
        }
    }
    panic!("fn {name} does not close");
}

#[test]
fn pub3_output_consumers_have_no_source_reader() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rdy-render/src");
    let lib = std::fs::read_to_string(src.join("lib.rs")).unwrap();
    let print = std::fs::read_to_string(src.join("print/mod.rs")).unwrap();
    for (file, f) in [
        ("lib.rs", function(&lib, "build_site_from")),
        ("print/mod.rs", function(&print, "build_from")),
    ] {
        let sig = &f[..f.find('{').unwrap()];
        for bad in ["ReadState", "Scope"] {
            assert!(
                !sig.contains(bad),
                "{file}: the signature takes {bad}: {sig}"
            );
        }
        for bad in [
            "ReadState",
            "reader()",
            "materialized(",
            ".versions()",
            "canon_tensions(",
            ".diff(",
        ] {
            assert!(!f.contains(bad), "{file}: the consumer reads source: {bad}");
        }
    }
}

const PROFILE: &str = "pf-7QXZ3";
const PROFILE_TITLE: &str = "Zebracorn profile title";
const PROFILE_DESCRIPTION: &str = "Quokkalyx profile description words";
const PROFILE_SELECT: &str = "Wombatrix select words";
const PROFILE_KEY: &str = "zebrakeyprofile";
const DISPLAY: &str = "the authored display words";

/// Commits `blocks` to `view` as one raw change (no engine rule runs), then drops caches.
fn raw_create(r: &mut Rdy, view: &str, blocks: Vec<Block>) {
    let change = Change {
        view: view.into(),
        ops: blocks
            .into_iter()
            .map(|block| Op::Create { block })
            .collect(),
        deps: vec![],
        provenance: writer().provenance(),
        justification: None,
        message: "raw".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    r.store_mut().commit_to_view(view, change).unwrap();
    r.invalidate();
}

/// A raw `publication_profile` block under the tutorial doc, and two references to it from the
/// overview: one with authored display text, one bare.
fn profile_blocks(ids: &Ids) -> Vec<Block> {
    let mut profile = raw(
        PROFILE,
        "publication_profile",
        Some(ids.tutorial_doc.as_str()),
        "a0",
        &[
            ("title", PROFILE_TITLE),
            ("description", PROFILE_DESCRIPTION),
            ("audience", "org"),
            ("key", PROFILE_KEY),
        ],
        "",
    );
    profile.fields.insert(
        "select".into(),
        FieldValue::List(vec![
            FieldValue::Str(ids.tutorial_doc.to_string()),
            FieldValue::Str(PROFILE_SELECT.into()),
        ]),
    );
    let authored = raw(
        "p-7QXZ4",
        "prose",
        Some(ids.overview_context_section.as_str()),
        "z8",
        &[],
        &format!("Read {{{{{PROFILE}|{DISPLAY}}}}} for the audience."),
    );
    let bare = raw(
        "p-7QXZ5",
        "prose",
        Some(ids.overview_context_section.as_str()),
        "z9",
        &[],
        &format!("Bare reference {{{{{PROFILE}}}}} here."),
    );
    // an accepted decision on the authored prose whose statement is a bare reference to the
    // profile: the prose's decision trail quotes the statement
    let mut ruling = raw(
        "d-7QXZ6",
        "decision",
        Some(ids.overview_context_section.as_str()),
        "z7",
        &[
            ("statement", &format!("Follow {{{{{PROFILE}}}}}.")),
            ("status", "accepted"),
            ("decided_at", "2021-02-08T10:00:00Z"),
            ("recorded_at", "2021-02-08T10:00:00Z"),
        ],
        "",
    );
    ruling.links.insert(
        "affects".into(),
        vec![Link {
            target: BlockId::parse("p-7QXZ4").unwrap(),
            part: None,
        }],
    );
    vec![profile, authored, bare, ruling]
}

/// What no output may hold: the profile's id and every text it owns.
fn profile_owned() -> [&'static str; 5] {
    [
        PROFILE,
        PROFILE_TITLE,
        PROFILE_DESCRIPTION,
        PROFILE_SELECT,
        PROFILE_KEY,
    ]
}

fn assert_no_profile(what: &str, bytes: &[u8]) {
    let text = String::from_utf8_lossy(bytes);
    for s in profile_owned() {
        assert!(!text.contains(s), "{what} holds {s:?}");
    }
}

/// The `href` of the first `<a` whose text ends with `words` in `html`.
fn href_of(html: &str, words: &str) -> String {
    let at = html
        .find(&format!("{words}</a>"))
        .unwrap_or_else(|| panic!("no link reads {words:?}"));
    let a = html[..at].rfind("<a ").expect("a link");
    let tag = &html[a..at];
    let h = tag.find("href=\"").expect("an href") + 6;
    tag[h..h + tag[h..].find('"').unwrap()].to_string()
}

/// The site path of the page of `id`, as its permalink page leads to it.
fn page_of(s: &SiteFiles, id: &BlockId) -> String {
    let go = String::from_utf8(s.files[&format!("go/{id}.html")].clone()).unwrap();
    let h = go.find("url=").expect("the permalink's target") + 4;
    go[h..h + go[h..].find('"').unwrap()]
        .trim_start_matches("../")
        .to_string()
}

#[test]
fn pub3_profile_blocks_are_off_every_source_output() {
    let (dir, mut r, ids) = project();
    let view = "editing";
    raw_create(&mut r, view, profile_blocks(&ids));
    let scope = Scope::View(view.into());
    let profile = BlockId::parse(PROFILE).unwrap();
    // the profile stays in the source state
    assert!(
        r.read()
            .materialized(&scope)
            .unwrap()
            .get(&profile)
            .is_some()
    );

    let st = r.read();
    for mode in [
        Mode::Static,
        Mode::Live {
            root: "/~view/editing/",
        },
    ] {
        let s = build_site(&st, &scope, mode).expect("the site");
        assert!(
            !s.files.contains_key(&format!("go/{PROFILE}.html")),
            "{mode:?}"
        );
        for (path, bytes) in &s.files {
            assert_no_profile(&format!("{mode:?} {path}"), bytes);
        }
        for f in ["search-index.json", "llms.txt", "llms-full.txt"] {
            assert!(s.files.contains_key(f), "{f}");
        }
        // the authored reference keeps its words and leads to the tutorial doc's page, with no
        // fragment
        let doc_page = page_of(&s, &ids.tutorial_doc);
        let overview = String::from_utf8(s.files[&page_of(&s, &ids.overview_doc)].clone()).unwrap();
        let href = href_of(&overview, DISPLAY);
        assert!(!href.contains('#'), "{href}");
        assert!(href.ends_with(&doc_page), "{href} leads to {doc_page}");
        // the prose's decision trail quotes the ruling: the bare reference in its statement reads
        // as the doc, not the profile's title, and leads to the doc's page
        let ruled = overview
            .find("Ruled: Follow <a ")
            .unwrap_or_else(|| panic!("{mode:?}: no trail quotes the ruling"));
        let tag = &overview[ruled..ruled + overview[ruled..].find('>').unwrap()];
        let h = tag.find("href=\"").expect("an href") + 6;
        let trail_href = &tag[h..h + tag[h..].find('"').unwrap()];
        assert!(!trail_href.contains('#'), "{trail_href}");
        assert!(
            trail_href.ends_with(&doc_page),
            "{trail_href} leads to {doc_page}"
        );
        // the tutorial doc's page shows nothing of the profile, and the page still renders
        let page = String::from_utf8(s.files[&doc_page].clone()).unwrap();
        assert!(!page.contains("publication_profile"), "{doc_page}");
    }

    // the rendered site on disk: the same
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&st, &scope, out.path()).expect("render");
    for (path, bytes) in tree(out.path()) {
        assert_no_profile(&format!("render {path}"), &bytes);
    }

    // every print format
    let p = print::build(&st, &scope).expect("the print");
    assert_no_profile("the print model", format!("{p:?}").as_bytes());
    let o = print_out(&p);
    assert_no_profile("the pdf", &o.pdf);
    assert_no_profile("the html file", &o.html);
    for (path, bytes) in o.md.iter().chain(&o.mkdocs) {
        assert_no_profile(&format!("md {path}"), bytes);
    }
    assert!(
        format!("{:?}", p).contains(DISPLAY),
        "the authored display text stays"
    );
    drop(dir);
}
