//! hpb10 (D-KDFZ3, D-0VCQ5, D-48NYM, D-3AVJH, D-WDKWQ; hub i-FGX7D, r-B0ZYE, tb-G28CM, D-YJ9TA):
//! one page of a publication, rendered without the rest of the site. It is the page the full
//! site writes at that path, built from one page context, with the assets that page needs.
use rdy_engine::publication::{IMPLICIT_ORG, ReadPublicationSource};
use rdy_engine::publication_manifest::NOTICE_SLOT;
use rdy_engine::render_inputs::{DISAGREEMENT_SENTENCES, DisagreementInput, RenderInputs};
use rdy_engine::*;
use rdy_model::*;
use rdy_render::render_input::capture_inputs;
use rdy_render::{PublicationArtifact, RenderedPage, build_publication, render_publication_page};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const TAG: &str = "v1.0.0";
const NOT_FOUND: (&str, &str, &str) = (
    "E_NOT_FOUND",
    "publication page was not found",
    "choose a page from this publication",
);

// --- fixtures -----------------------------------------------------------------------------------

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

/// F2 (D-DKJS5): the person confirms each exception beside version `p` before it lands.
fn confirm_exceptions(r: &mut Rdy, s: &Session, p: &ProposalInfo) {
    use rdy_engine::answer::{AnswerRequest, relay::RelayInput};
    use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
    let q = r
        .read()
        .pending(&PendingQuery {
            person: s.actor.clone(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending");
    let refs: Vec<String> = q
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(p.r#ref.as_str()))
        .map(|i| i.r#ref.clone())
        .collect();
    for item in refs {
        let req = AnswerRequest {
            r#ref: item.clone(),
            option: "confirm".into(),
            card_hash: None,
            note: None,
            relay: RelayInput::default(),
            brief: None,
        };
        r.answer(s, &s.actor, req)
            .unwrap_or_else(|e| panic!("confirm {item}: {e}"));
    }
}

fn put(r: &mut Rdy, ops: Value) -> BTreeMap<String, String> {
    let w = r
        .write(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(ops).unwrap(),
                justification: None,
                message: "hpb9".into(),
            },
        )
        .unwrap();
    w.created.into_iter().collect()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(&writer(), "work", "batch", Some("test summary"))
        .unwrap();
    confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
}

/// A PNG of `w` by `h` pixels of one colour.
fn png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn adler32(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for &d in data {
            a = (a + d as u32) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let mut line = vec![0u8];
    for _ in 0..w {
        line.extend_from_slice(&rgb);
    }
    let raw = line.repeat(h as usize);
    let mut z = vec![0x78, 0x01, 1];
    z.extend_from_slice(&(raw.len() as u16).to_le_bytes());
    z.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
    z.extend_from_slice(&raw);
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut ihdr = vec![];
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A tagged source project named `Holder project`: folder `guide` with doc `intro` (a section
/// with two prose blocks, the first holding a reference to doc `other`, a figure, a term and an
/// open question), doc `other`, doc `flow` (a diagram) and an admin doc with an org profile
/// `orgp` selecting `intro` alone. Under `implicit:org` every page kind exists.
struct Project {
    dir: tempfile::TempDir,
    r: Rdy,
    ids: BTreeMap<String, String>,
    landing: Hash,
    figure: Vec<u8>,
}

fn project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(
        dir.path().join(".rdy.yaml"),
        "pack: rdy-standard\nname: \"Holder project\"\n",
    )
    .unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let figure = png(3, 2, [9, 99, 199]);
    let blob = r.store_mut().put_blob(&figure).unwrap();
    let mut ids = put(
        &mut r,
        json!([
            {"op": "create", "type": "folder", "id": "$guide", "fields": {"name": "guide", "title": "Guide"}},
            {"op": "create", "type": "doc", "id": "$intro", "parent": "$guide", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$intro", "fields": {"title": "Start here"}},
            {"op": "create", "type": "doc", "id": "$other", "fields": {"name": "other", "title": "Other page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$b", "parent": "$other", "body": "Other words."},
            {"op": "create", "type": "prose", "id": "$a", "parent": "$s", "body": "The INTROCANARY words point at {{$other}}."},
            {"op": "create", "type": "prose", "id": "$a2", "parent": "$s", "body": "A second point."},
            {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "A figure"}},
            {"op": "create", "type": "term", "parent": "$s", "fields": {"name": "Ledger"}, "body": "The ledger term."},
            {"op": "create", "type": "question", "parent": "$s", "fields": {"prompt": "Is it open?"}},
            {"op": "create", "type": "doc", "id": "$flow", "fields": {"name": "flow", "title": "Flow page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$fs", "parent": "$flow", "fields": {"title": "Flow section"}},
            {"op": "create", "type": "diagram", "parent": "$fs", "fields": {"lang": "mermaid", "alt": "flow"}, "body": "graph TD\n  A --> B"},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    land(&mut r);
    let p = put(
        &mut r,
        json!([
            {"op": "create", "type": "publication_profile", "id": "$orgp", "parent": ids["$admin"],
             "fields": {"key": "orgp", "title": "Holder org site", "audience": "org", "select": [ids["$intro"]]}}
        ]),
    );
    ids.extend(p);
    land(&mut r);
    r.tag(&founder(), TAG, None, None).unwrap();
    let tag = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == TAG)
        .unwrap()
        .1;
    let landing = r.store().read_tag(&tag).unwrap().landing;
    Project {
        dir,
        r,
        ids,
        landing,
        figure,
    }
}

impl Project {
    fn id(&self, ph: &str) -> String {
        self.ids[ph].clone()
    }

    /// The captured inputs: the source's settings, and one disagreement over the intro's two
    /// prose blocks and one over the intro's first block and the other doc's.
    fn inputs(&self) -> RenderInputs {
        let mut inputs = capture_inputs(&self.r.read(), &Scope::At(self.landing)).unwrap();
        let sentence = DISAGREEMENT_SENTENCES[0].1.to_string();
        let block = |ph: &str| ph_id(&self.ids, ph);
        inputs.disagreements = vec![DisagreementInput {
            sentence,
            blocks: vec![block("$a"), block("$a2")],
            lead: None,
        }];
        inputs
    }

    fn render(
        &self,
        selector: &str,
        inputs: &RenderInputs,
        tag: &str,
        path: &str,
    ) -> Result<RenderedPage, RdyError> {
        let read = self.r.read();
        let source = ReadPublicationSource::with_settings(
            &read,
            &inputs.pack,
            &inputs.title,
            inputs.description.as_deref(),
        );
        let m = read.materialized(&Scope::At(self.landing)).unwrap();
        render_publication_page(&source, &m, tag, false, selector, inputs, path)
    }

    fn one(&self, selector: &str, path: &str) -> RenderedPage {
        self.render(selector, &self.inputs(), TAG, path)
            .unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn full(&self, selector: &str, inputs: &RenderInputs) -> PublicationArtifact {
        let read = self.r.read();
        let source = ReadPublicationSource::with_settings(
            &read,
            &inputs.pack,
            &inputs.title,
            inputs.description.as_deref(),
        );
        let m = read.materialized(&Scope::At(self.landing)).unwrap();
        build_publication(&source, &m, TAG, false, selector, inputs).unwrap()
    }
}

fn ph_id(ids: &BTreeMap<String, String>, ph: &str) -> BlockId {
    BlockId::parse(&ids[ph]).unwrap()
}

#[track_caller]
fn assert_not_found(e: &RdyError, what: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        NOT_FOUND,
        "{what}: {e}"
    );
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

// --- tests --------------------------------------------------------------------------------------

#[test]
fn hpb10_one_page_matches_the_same_full_site_page() {
    let p = project();
    let inputs = p.inputs();
    let full = p.full(IMPLICIT_ORG, &inputs);
    let kinds: Vec<(&str, String)> = vec![
        ("home", "index.html".into()),
        ("folder", "guide/index.html".into()),
        ("doc", "guide/intro.html".into()),
        ("glossary", "glossary.html".into()),
        ("questions", "questions.html".into()),
        ("disagreements", "disagreements.html".into()),
        ("redirect", format!("go/{}.html", p.id("$a"))),
    ];
    for (kind, path) in &kinds {
        assert!(
            full.files.contains_key(path),
            "{kind}: {path} is not in the full site"
        );
        let one = p.one(IMPLICIT_ORG, path);
        assert_eq!(&one.path, path, "{kind}");
        assert!(
            one.html == full.files[path],
            "{kind}: {path} differs from the full site's"
        );
        // the manifest lists the page and its assets, with the full site's entries for them
        let entry = |m: &rdy_engine::publication_manifest::Manifest, f: &str| {
            m.files.iter().find(|e| e.path == f).cloned()
        };
        let page_entry = entry(&one.manifest, path).expect("the page is listed");
        assert_eq!(
            Some(page_entry.clone()),
            entry(&full.manifest, path),
            "{kind}"
        );
        assert_eq!(page_entry.notice_offset, Some(one.notice_offset), "{kind}");
        assert_eq!(page_entry.size as usize, one.html.len(), "{kind}");
        assert_eq!(page_entry.media_type, "text/html", "{kind}");
        assert_eq!(one.manifest.files.len(), 1 + one.assets.len(), "{kind}");
        for (name, bytes) in &one.assets {
            let e = entry(&one.manifest, name).unwrap_or_else(|| panic!("{kind}: {name}"));
            assert_eq!(e.hash, Hash::of(bytes), "{kind}: {name}");
            assert_eq!(e.notice_offset, None, "{kind}: {name}");
            // only the script depends on the page
            if name != "assets/app.js" {
                assert!(bytes == &full.files[name], "{kind}: {name}");
                assert_eq!(Some(e), entry(&full.manifest, name), "{kind}: {name}");
            }
        }
        assert_eq!(one.warnings, full.warnings, "{kind}");
    }
    // the intro page's figure is one of its assets, byte for byte
    let intro = p.one(IMPLICIT_ORG, "guide/intro.html");
    assert!(intro.assets.values().any(|b| *b == p.figure));
    // a profile's page reads its own selection
    let org = p.one(&p.id("$orgp"), "guide/intro.html");
    assert!(org.html == p.full(&p.id("$orgp"), &inputs).files["guide/intro.html"]);
    assert!(text(&org.html).contains("INTROCANARY"));
}

#[test]
fn hpb10_missing_and_excluded_pages_share_one_refusal() {
    let p = project();
    let inputs = p.inputs();
    let org = p.id("$orgp");
    let (b, fs) = (p.id("$b"), p.id("$fs"));
    // the full site of the implicit profile has the pages the org profile leaves out
    let all = p.full(IMPLICIT_ORG, &inputs);
    let excluded = [
        "other.html".to_string(),
        "flow.html".to_string(),
        "admin.html".to_string(),
        format!("go/{b}.html"),
        format!("go/{fs}.html"),
        format!("go/{}.html", p.id("$other")),
    ];
    for path in &excluded {
        assert!(all.files.contains_key(path), "{path} is in the full site");
        let e = p.render(&org, &inputs, TAG, path).unwrap_err();
        assert_not_found(&e, path);
    }
    // pages that never existed
    for path in [
        "nope.html",
        "guide/nope.html",
        "guide/intro",
        "index.htm",
        "assets/site.css",
        "search-index.json",
        "llms.txt",
        "go/ZZZZZZZZZZZZ.html",
        "go/nope.html",
        "changes.html",
        "guide/index.html/",
    ] {
        match p.render(&org, &inputs, TAG, path) {
            Err(e) => assert!(
                e.code == "E_NOT_FOUND" || e.code == "E_BAD_INPUT",
                "{path}: {e}"
            ),
            Ok(_) => panic!("{path} was rendered"),
        }
    }
    for path in [
        "nope.html",
        "guide/nope.html",
        "go/ZZZZZZZZZZZZ.html",
        "assets/site.css",
    ] {
        let e = p.render(&org, &inputs, TAG, path).unwrap_err();
        assert_not_found(&e, path);
    }
    // the refusal names no path and no block: a missing and an excluded page read the same
    let missing = p.render(&org, &inputs, TAG, "gone.html").unwrap_err();
    let hidden = p.render(&org, &inputs, TAG, "other.html").unwrap_err();
    assert_eq!(
        (missing.code, &missing.message, &missing.fix),
        (hidden.code, &hidden.message, &hidden.fix)
    );
    for e in [&missing, &hidden] {
        let said = format!("{e} {} {}", e.message, e.fix);
        for word in ["other", "gone", &b, &fs, "flow"] {
            assert!(!said.contains(word), "{said}");
        }
    }
    // a disagreement over an excluded block leaves no disagreements page behind
    let mut wide = p.inputs();
    wide.disagreements[0].blocks = vec![ph_id(&p.ids, "$a"), ph_id(&p.ids, "$b")];
    let e = p
        .render(&org, &wide, TAG, "disagreements.html")
        .unwrap_err();
    assert_not_found(&e, "disagreements.html");
    let ok = p
        .render(IMPLICIT_ORG, &wide, TAG, "disagreements.html")
        .unwrap();
    assert!(text(&ok.html).contains(DISAGREEMENT_SENTENCES[0].1));
    // a path the manifest refuses is its own error, whatever it names
    for path in [
        "",
        "/index.html",
        "a//b.html",
        "../x.html",
        "a/./b",
        "a b.html",
        "é.html",
    ] {
        let e = p.render(&org, &inputs, TAG, path).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{path:?}: {e}");
    }
    let long = format!("{}.html", "a".repeat(4096));
    let e = p.render(&org, &inputs, TAG, &long).unwrap_err();
    assert_eq!(e.code, "E_LIMIT", "{e}");
    let segment = format!("{}.html", "a".repeat(256));
    assert_eq!(
        p.render(&org, &inputs, TAG, &segment).unwrap_err().code,
        "E_LIMIT"
    );
    // other errors pass through as the full build gives them
    let mut bad = p.inputs();
    bad.title = String::new();
    let e = p.render(&org, &bad, TAG, "index.html").unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
}

#[test]
fn hpb10_page_assets_follow_the_page_context() {
    let p = project();
    let inputs = p.inputs();
    let full = p.full(IMPLICIT_ORG, &inputs);
    let theme = [
        "assets/site.css",
        "assets/fonts/onest.woff2",
        "assets/fonts/geist-mono.woff2",
        "assets/app.js",
    ];
    // a page with a figure and no diagram: the closed theme set, the figure, no diagram library
    let intro = p.one(IMPLICIT_ORG, "guide/intro.html");
    let figures: Vec<&String> = intro
        .assets
        .keys()
        .filter(|k| !theme.contains(&k.as_str()))
        .collect();
    assert_eq!(figures.len(), 1, "{:?}", intro.assets.keys());
    assert!(figures[0].starts_with("assets/") && figures[0].ends_with(".png"));
    assert!(intro.assets.contains_key(theme[3]));
    for t in theme {
        assert!(intro.assets.contains_key(t), "{t}");
    }
    assert!(!intro.assets.contains_key("assets/js/mermaid.min.js"));
    // a page with no figure has none of its blobs, and the home page none of the intro's
    let home = p.one(IMPLICIT_ORG, "index.html");
    let mut names: Vec<&str> = home.assets.keys().map(String::as_str).collect();
    names.sort_unstable();
    let mut closed = theme.to_vec();
    closed.sort_unstable();
    assert_eq!(names, closed);
    // a page with a diagram: the library and the script that starts it
    let flow = p.one(IMPLICIT_ORG, "flow.html");
    assert!(flow.assets.contains_key("assets/js/mermaid.min.js"));
    assert!(flow.assets["assets/app.js"] != intro.assets["assets/app.js"]);
    assert!(flow.assets["assets/app.js"].len() > intro.assets["assets/app.js"].len());
    // the full site has a diagram, so its script is the diagram page's, not the other pages'
    assert!(flow.assets["assets/app.js"] == full.files["assets/app.js"]);
    assert!(intro.assets["assets/app.js"] != full.files["assets/app.js"]);
    // the manifest entry of the script is the page's own
    let entry = |r: &RenderedPage| {
        r.manifest
            .files
            .iter()
            .find(|e| e.path == "assets/app.js")
            .unwrap()
            .clone()
    };
    assert_eq!(entry(&intro).hash, Hash::of(&intro.assets["assets/app.js"]));
    assert!(entry(&intro).hash != entry(&flow).hash);
    // no other page, no search index and no agent file is returned: those are the full
    // publication's, which the page still links
    for r in [&intro, &flow, &home] {
        for k in r.assets.keys() {
            assert!(k.starts_with("assets/"), "{k}");
            assert!(!k.ends_with(".html"), "{k}");
        }
    }
    let html = text(&intro.html);
    assert!(html.contains("../search-index.json"), "{html}");
    assert!(
        html.contains("../other.html"),
        "the page links the whole publication"
    );
    assert!(html.contains("../assets/app.js"));
    assert!(text(&home.html).contains("search-index.json"));
    // the notice slot: the manifest's offset is its place in the returned bytes
    for r in [&intro, &flow, &home] {
        let at = r.notice_offset as usize;
        assert_eq!(&r.html[at..at + NOTICE_SLOT.len()], NOTICE_SLOT);
        assert_eq!(
            r.html
                .windows(NOTICE_SLOT.len())
                .filter(|w| *w == NOTICE_SLOT)
                .count(),
            1
        );
    }
    let go = p.one(IMPLICIT_ORG, &format!("go/{}.html", p.id("$a")));
    let at = go.notice_offset as usize;
    assert_eq!(&go.html[at..at + NOTICE_SLOT.len()], NOTICE_SLOT);
    assert_eq!(
        go.manifest
            .files
            .iter()
            .find(|e| e.path == go.path)
            .unwrap()
            .notice_offset,
        Some(go.notice_offset)
    );
    // the same request twice gives the same bytes
    let again = p.one(IMPLICIT_ORG, "guide/intro.html");
    assert!(again.html == intro.html && again.assets == intro.assets);
    assert_eq!(again.manifest, intro.manifest);
}

#[test]
fn hpb10_one_page_has_no_holder_path_or_live_history() {
    let mut p = project();
    let mut inputs = p.inputs();
    inputs.title = "Captured elsewhere".into();
    inputs.description = Some("A captured description.".into());
    let first = p.render(IMPLICIT_ORG, &inputs, TAG, "index.html").unwrap();
    let html = text(&first.html);
    // the title and description are the captured ones, not the holder's settings
    assert!(html.contains("Captured elsewhere"), "{html}");
    assert!(html.contains("A captured description."));
    assert!(!html.contains("Holder project"));
    // the tag label is the one named, whatever tags the holder has
    assert!(html.contains("v1.0.0"));
    let other = p
        .render(IMPLICIT_ORG, &inputs, "v7.7.7", "index.html")
        .unwrap();
    assert!(text(&other.html).contains("v7.7.7") && !text(&other.html).contains("v1.0.0"));
    let none = p.render(IMPLICIT_ORG, &inputs, "", "index.html").unwrap();
    assert!(!text(&none.html).contains("v1.0.0") && !text(&none.html).contains("v7.7.7"));
    // no holder path or name in any returned byte, no history link or in-flight work in a page
    // (the shipped script names the live site's routes in every site, so it is no page)
    let dir = p.dir.path().to_string_lossy().into_owned();
    for path in ["index.html", "guide/intro.html", "disagreements.html"] {
        let r = p.render(IMPLICIT_ORG, &inputs, TAG, path).unwrap();
        for bytes in std::iter::once(&r.html).chain(r.assets.values()) {
            let t = String::from_utf8_lossy(bytes);
            for word in [dir.as_str(), "Holder project"] {
                assert!(!t.contains(word), "{path}: {word}");
            }
        }
        let t = text(&r.html);
        for word in ["~versions", "~proposal", "changes-since", "changes.html"] {
            assert!(!t.contains(word), "{path}: {word}");
        }
    }
    // the captured disagreement projection reaches the page only through the selected view
    let d = p
        .render(IMPLICIT_ORG, &inputs, TAG, "disagreements.html")
        .unwrap();
    assert!(text(&d.html).contains(DISAGREEMENT_SENTENCES[0].1));
    let mut none = inputs.clone();
    none.disagreements.clear();
    let e = p
        .render(IMPLICIT_ORG, &none, TAG, "disagreements.html")
        .unwrap_err();
    assert_not_found(&e, "no captured disagreement");
    // canon moving on after the capture changes no byte of the page
    let later = put(
        &mut p.r,
        json!([{"op": "create", "type": "doc", "id": "$later", "fields": {"name": "later", "title": "Later page", "doc_type": "page"}}]),
    );
    assert!(later.contains_key("$later"));
    land(&mut p.r);
    let after = p.render(IMPLICIT_ORG, &inputs, TAG, "index.html").unwrap();
    assert!(after.html == first.html && after.assets == first.assets);
    assert_eq!(after.manifest, first.manifest);
    let e = p
        .render(IMPLICIT_ORG, &inputs, TAG, "later.html")
        .unwrap_err();
    assert_not_found(&e, "later.html");
}
