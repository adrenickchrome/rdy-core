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
        // pub8 added `tag_redacted` for a named profile's chosen tag; a source print has none.
        // README Task 3 added a run's `ref_of`, which only the single Markdown file's reading sets;
        // the site's reading leaves it unset, so it is masked the same way
        model: {
            assert!(!p.tag_redacted, "a source print marks no tag");
            let text = format!("{model:?}");
            assert!(!text.contains("ref_of: Some"), "the print sets no ref_of");
            text.replacen(", tag_redacted: false", "", 1)
                .replace(", ref_of: None", "")
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
/// Re-taken for the as-built plan's Task 5 (FW-AB1): only `assets/site.css` moved (the status
/// line's `.block-status` and `.status` rules); with the old stylesheet every digest is the one
/// before. The fixture holds no planned or since content, so no page text moved.
/// Re-taken for the README plan's Task 1 (D-3VVNC): `assets/site.css` gained the fold rules and
/// `js/theme.js` (bundled in `assets/app.js`) the fold opener, so every scope's digest moved; the
/// fixture holds no collapsed section, so no page text moved. With the old assets every digest is
/// the one before.
/// Re-taken for the README plan's Task 2 (D-3DAX4): `assets/site.css` gained the dark-figure
/// rules, so every scope's digest moved; the fixture holds no figure with a dark image. With the
/// old stylesheet every digest is the one before.
const SITE_DIGESTS: [(&str, &str, &str); 4] = [
    (
        "canon",
        "a014b1e828d012ed1cb52f9deaa8e9d5f16d053e9684b9b71a3e7ed2e984c03c",
        "ae6a9387695491845a2132384ed853608721740fd0ef7392abba40d4fb7f0955",
    ),
    (
        "tag",
        "2eee6d6a963f8b38413ab0c113cd5cb6792c108a8ff114ce35b14d14152f5eba",
        "63a7fc3e201ffd4291d36a40477f8f476e4d9531b7697f325fc492928654b629",
    ),
    (
        "view",
        "1e8cf5ac2e31a1322df5ca7ad2bac43f7719765eca2094a3cb85255c97a1c8ba",
        "e2f80b3870d166c1064ab250046f781dc821479434a75679cb971b814b1c52a7",
    ),
    (
        "proposal",
        "fb9d31b0ba578866e877a2af35281c511b4dd524e5155ffa45521e0a7eef8185",
        "aae90c062fcce4fe49b27f077dc7458afde8661d3385f9dc12bb53653073856e",
    ),
];

/// The source print digests, captured from the unmodified base (22af149) before the refactor:
/// (scope, [model without rdy_dir, pdf, html, md plain, md mkdocs]). The PDF digests carry the
/// crate version (the PDF producer field) and were re-taken at the 0.7.0 merge of main. The
/// proposal's were re-taken for object encoding 1: its scope text is `proposal:<hash>`, and the
/// version's hash follows its changes' encoding-1 ids; the other scopes kept their bytes.
/// Re-taken after the reference period rule (ruling task-7): an automatic label drops its final period before an authored period; only that text moved.
/// The proposal's re-taken for the propose's assessment record (ruling task-10 Amendment 1, item
/// 5): the earlier landing names its assessment, so canon's head and the version's hash move,
/// and only the `proposal:<hash>` scope text changed; the other scopes kept their bytes.
/// The proposal's re-taken again when a landing names its assessment (ruling task-12 R2.3): the
/// landing record and canon's head move, so the version's hash does; only that scope's text
/// changed.
/// The proposal's re-taken again when the engine's decision type gained `confirmation` (ck1): the
/// resolved pack's digest moves the assessment, so the version's hash does; only that scope's
/// text changed.
/// It moved again when every defining type gained the optional `everyday` field (nb3), for the
/// same reason.
/// Re-taken for M3e's neighbor items (nb8): only the proposal scope moved. Its print names the
/// version (`proposal:<id>`), and the id hashes `canon_at`, canon's head, whose landing records
/// now seal the review project's neighbor items; the site bytes of the same scope, which carry
/// no id, kept their digests.
/// Re-taken for the as-built plan's Task 1 (FW-AB1): a new decision records `built: planned`, so
/// the review project's decision record, canon's head and the version's hash move; only the
/// proposal scope's `proposal:<id>` text changed. Its site bytes, and every other scope, kept
/// their digests.
/// Re-taken for the instruction block type (ain1): it moves the pack's digest, so the version's
/// hash moves again. Only `proposal:<id>` changed in the model, the HTML, both Markdown layouts
/// and the PDF text; the other three scopes kept their bytes.
/// Re-taken when main's instruction type (0.40.0) met the as-built fields: the combined pack's
/// digest moves the version's hash once more. Only the proposal scope moved; the site digests
/// and the other three scopes kept their bytes.
/// Re-taken for the repo_link block type (rpl4): it joins every resolved pack, so the pack's
/// digest and the version's hash move. Only the proposal scope moved; the site digests and the
/// other three scopes kept their bytes.
/// Re-taken for the section field `collapsed` (README plan Task 1): it joins the standard pack, so
/// the pack's digest and the version's hash move. Only the proposal scope moved; the other three
/// scopes kept their bytes.
/// Re-taken for the figure fields `dark_blob` and `dark_media_type` (README plan Task 2): the pack's
/// digest moves the proposal scope's five digests. The print model gained a run's `image` and a
/// figure's `dark`, so each scope's model text (the first digest) moved too; the PDF, the HTML file
/// and both Markdown trees of the other three scopes kept their bytes.
const PRINT_DIGESTS: [(&str, [&str; 5]); 4] = [
    (
        "canon",
        [
            "cbf11b977e365c9c5c4453ee5fe90f358f09fc14d79b37b490b69b5b31ecf07f",
            "b3826ab0c67fd3712ab13832626f1fdd1d830a05d6e904387ee20c6612678bd8",
            "840b9ce1dc431cc9550fc985144ebedab4c4127b55212bb159938a733ec159d7",
            "b8ced92384c39f43280164e3daedc7b303d8aa7cd3ddcd4601ab85dac0f73b88",
            "7123dfbf1473c1ecd951631f0dd656eb5e42663ed712749e8d14d87b3b4e2d3d",
        ],
    ),
    (
        "tag",
        [
            "d57d4821aada24a53c3f228da8647e895109abc9a84131b86ce6eb31c7f4c41e",
            "1c6f05d0f8c4d0bf68b2840751f487348db56b51191c8fd8aaa4eea25cdff01d",
            "b6637f99fd02c5c1d3fd452e14b12a29dc80dfdda0a02067160d242becad5ed4",
            "b3ada5447380be2263827a850d76ee5e4272d6b0a8dd08ab5be2f169ed073df3",
            "f46fdbf2dd1c477b9274cfd1a47fb9548212eaebc9a7992eca5344a32b195e3d",
        ],
    ),
    (
        "view",
        [
            "25231ad92b9d95496de39db09bd46188b28fc9ae238225cd18e7a3870d7c5f25",
            "630fa826e8fb59d9cb68883fb583453b492a50075fff2e92211b9bb7663acbb9",
            "9dc60ab3b6704cb058f23f01514f718b169f3bb781e622bf10cfb924d35826aa",
            "c0ceb6424823c01fb23b5528ba5e2c4ac673eb4033d2cf4989115719fb269349",
            "c59ee2168cce38620d411e7b6799a12e07c7028a2cd4d75ae142adfd551804ff",
        ],
    ),
    (
        "proposal",
        [
            "5b82854324480f2a3d3d840aeea8d5a39fac7ba3376e0ed0d5cd611e39fac3a0",
            "d9f0a218460cc0e2d5de5532e0191aefb5230c70775233e81c81b11b96209df2",
            "636136e174f383d50fb45644f5ffc87d8dd9d75eaf8eb5e255b284712a56afc2",
            "40cc9607bfa4eecf59ee4c1109c11c34a505aeb57cb7ee3347ccd26762efd5dc",
            "3240e85850ea13a10715520e43ba5686e0f53a730fb9d63eebfb814271b9bb0f",
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
