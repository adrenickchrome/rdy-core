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

/// The digest of a whole built site: every file by path and bytes, the label, the header, where
/// the label and header went, and the summary.
fn site_digest(s: &SiteFiles) -> String {
    let mut d = Digest::default();
    for (path, bytes) in &s.files {
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
/// Re-taken in PRS Task 5 (plan 2026-10-20-rdy-site-profiles, Task 5: the menu `LABEL` becomes
/// "Versions, proposals, views and profiles" and the script fetches the pill's href in place of
/// the hard-coded PAGE; fix rounds 2827fddd and 2f7023bd add its same-origin http/https check):
/// only `assets/app.js`, which bundles `js/menu.js`, changed; every other file, the label, the
/// header, their ranges and the summary kept their bytes.
const SITE_DIGESTS: [(&str, &str, &str); 4] = [
    (
        "canon",
        "4f64de348cef587c86874945afaae939fa4218f6fbd60f735dc9650b423c8653",
        "851cfcc46b876fc1a306a6c06b38ebb7c985327fe1ac5e983b65543c3fe3d01f",
    ),
    (
        "tag",
        "990c5e75fd658a17158dc323539972cf040f31411b022944b1ac6e87a1491f74",
        "3f93c55625ab0d470ded2dcd20cc90d4a55f338f9ce09829c062d8e391867fde",
    ),
    (
        "view",
        "df67151694ff6f4f26ad94dd1f38deeb6ee783b0e13757f329ba4956ae68aae0",
        "42d4fe73a0ec70ef07c7cec2f307316921c82903a0b80abfc8d4fe3316913f60",
    ),
    (
        "proposal",
        "c368d4dec5a3f6a76ea226abe65f741be7a44daa051d222c05b02aa1151261e5",
        "440ce521b45445fb2dd389a718a6b3d092f73c302a8a983f50f58d1ff29d1e53",
    ),
];

/// The source print digests, captured from the unmodified base (22af149) before the refactor:
/// (scope, [model without rdy_dir, pdf, html, md plain, md mkdocs]). The PDF digests carry the
/// crate version (the PDF producer field) and were re-taken at the 0.7.0 merge of main.
const PRINT_DIGESTS: [(&str, [&str; 5]); 4] = [
    (
        "canon",
        [
            "84fbbf923af3bea784c101f69f0c8bde48b5de7f587d02ce62132eba628728b9",
            "1f11b48f2af3af7ab8d9e6a8ad98596e3181b8dd63b4bc85e4e0e1c756a809ba",
            "1254c1001737892e1f0ea20ec3ab4993f9ce684c97b193f5ada6bb09d8445142",
            "a943098d24ccf984658403df96a3f311b142c90e7b764eef247562699e2cd8d7",
            "6984953d91796ef67f268dc7bdc3bfbf076253aa1356c77eb46c9d4bacb3c24d",
        ],
    ),
    (
        "tag",
        [
            "855eab380b64b94361c87f775b60a406e78a7d0e8fea00b5da8ffec1f0f384c8",
            "27e70ec170c51d8c9689312ccc6fd5c39605838d4a7ad8e595d84836c8052148",
            "53ce7675b9e938c31ce92b8160230b6af99d10d6ca48af11e72d16c92667a015",
            "9cffc2a308540e6ad7610e2af7c3973fc8655d15f34668682f5219cc490ce4be",
            "b561e223280d7fabc9c6d01dc1f629e59181ea4bea1360aa9f443ef09f3205be",
        ],
    ),
    (
        "view",
        [
            "fcbe79bc01b641aee08d7ad7794cff02a96208e7b503ce21c7b9128c18bae49b",
            "c197b6e9dc4e2f617f2bc7151364040f878b781e0342d1594dc4760bea1eacd7",
            "21001f066775b54d02924d382c65a73ea776a315612b3e0e930cb9bacb5e2d77",
            "6dbce21b6ae082807d92313182695e098d5737ca9de9e63af530d24f10f02689",
            "6d41c3661756568ca0957040786ecab66dd94cdb9f1b664804eedd5460376704",
        ],
    ),
    (
        "proposal",
        [
            "4de53923b9900182e01c67ccd1643c993e6c326baf4f5c5778270ca5a8ed680a",
            "ee6e483ca4215c2853dec0a863cf43cf561ecd6a6a320dd9bf9ba608d4b2c87a",
            "4b10693e6304e921edaf44bac4315b3331b0be5fa9f17ec517ea5cd3c0e5139e",
            "eed98db235160352f5118b7c057ea1d69914af9a4acb0f15b798db983187d8d9",
            "f22bf84190f83a8e13dcbe3f893e71b79143d545ab734e4990af9003a7974957",
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
