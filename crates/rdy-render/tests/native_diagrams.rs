//! dgm6 (D-FY4VF, D-KAGB2, D-E8P8J, D-3BM8P, D-1QFTE, D-0VCQ5): Mermaid diagrams drawn as SVG
//! when the site is built. Every page draws its diagrams with no script, follows the site's
//! tokens, links one generated file per drawing and shows the source of a diagram it cannot
//! draw. The full site, one page, a profile and the print read the same selected source.
use rdy_engine::publication::{IMPLICIT_ORG, ReadPublicationSource};
use rdy_engine::render_inputs::RenderInputs;
use rdy_engine::*;
use rdy_model::*;
use rdy_render::render_input::capture_inputs;
use rdy_render::{
    Mode, PublicationArtifact, RenderedPage, SiteFiles, build_profile_site, build_publication,
    build_site, render_publication_page,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const TAG: &str = "v1.0.0";
/// A flowchart with a subgraph.
const FLOW: &str =
    "flowchart LR\n  subgraph S[first]\n    A[start] --> B[end]\n  end\n  B --> C[after]";
/// A source the pinned parser refuses.
const INVALID: &str = "flowchart LR\n A[";
/// A family rdy does not draw.
const PIE: &str = "pie\n  \"a\" : 1";
/// A drawing whose class definition carries an event attribute: rdy's checks refuse it.
const ACTIVE: &str =
    "flowchart LR\n    A --> B\n    classDef e fill:#f00\" onload=\"alert(1)\n    class A e";

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
                message: "dgm6".into(),
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

/// A project named `Diagram project` holding `ops`, landed.
fn project(ops: Value) -> (tempfile::TempDir, Rdy, BTreeMap<String, String>) {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(
        dir.path().join(".rdy.yaml"),
        "pack: rdy-standard\nname: \"Diagram project\"\n",
    )
    .unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let ids = put(&mut r, ops);
    land(&mut r);
    (dir, r, ids)
}

/// The landing of tag `v1.0.0`, made now.
fn tag(r: &mut Rdy) -> Hash {
    r.tag(&founder(), TAG, None, None).unwrap();
    let tag = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == TAG)
        .unwrap()
        .1;
    r.store().read_tag(&tag).unwrap().landing
}

fn inputs(r: &Rdy, landing: Hash) -> RenderInputs {
    capture_inputs(&r.read(), &Scope::At(landing)).unwrap()
}

fn full(r: &Rdy, landing: Hash, selector: &str) -> PublicationArtifact {
    let read = r.read();
    let inputs = inputs(r, landing);
    let source = ReadPublicationSource::with_settings(
        &read,
        &inputs.pack,
        &inputs.title,
        inputs.description.as_deref(),
    );
    let m = read.materialized(&Scope::At(landing)).unwrap();
    build_publication(&source, &m, TAG, false, selector, &inputs).unwrap()
}

fn one(r: &Rdy, landing: Hash, selector: &str, path: &str) -> RenderedPage {
    let read = r.read();
    let inputs = inputs(r, landing);
    let source = ReadPublicationSource::with_settings(
        &read,
        &inputs.pack,
        &inputs.title,
        inputs.description.as_deref(),
    );
    let m = read.materialized(&Scope::At(landing)).unwrap();
    render_publication_page(&source, &m, TAG, false, selector, &inputs, path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn site(r: &Rdy) -> SiteFiles {
    build_site(&r.read(), &Scope::Canon, Mode::Static).unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn page(files: &BTreeMap<String, Vec<u8>>, path: &str) -> String {
    text(
        files
            .get(path)
            .unwrap_or_else(|| panic!("no {path}: {:?}", files.keys())),
    )
}

/// The diagram files of a build, by path.
fn diagram_files(files: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    files
        .iter()
        .filter(|(p, _)| p.starts_with("assets/diagrams/"))
        .map(|(p, b)| (p.clone(), b.clone()))
        .collect()
}

/// Every diagram figure of a page, in order.
fn figures(html: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut rest = html;
    while let Some(i) = rest.find("<figure class=\"diagram\"") {
        let end = rest[i..].find("</figure>").expect("a closed figure") + i + "</figure>".len();
        out.push(&rest[i..end]);
        rest = &rest[end..];
    }
    out
}

/// The value of every `id` attribute in `html`.
fn ids_in(html: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut rest = html;
    while let Some(i) = rest.find(" id=\"") {
        let start = i + " id=\"".len();
        let end = rest[start..].find('"').unwrap() + start;
        out.push(&rest[start..end]);
        rest = &rest[end..];
    }
    out
}

/// The path of the file a drawn figure's `open full size` link leads to, from a page at the
/// site's root.
fn full_size(fig: &str) -> String {
    let at = fig
        .find(">open full size</a>")
        .unwrap_or_else(|| panic!("no full-size link: {fig}"));
    let href = fig[..at].rfind("href=\"").unwrap() + "href=\"".len();
    let end = fig[href..].find('"').unwrap() + href;
    fig[href..end].to_string()
}

fn diagram_doc(name: &str, title: &str, bodies: &[(&str, &str, &str)]) -> Vec<Value> {
    let doc = format!("${name}");
    let sec = format!("${name}s");
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": doc, "fields": {"name": name, "title": title, "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": sec, "parent": doc, "fields": {"title": "Flow"}}),
    ];
    for (i, (lang, alt, body)) in bodies.iter().enumerate() {
        ops.push(
            json!({"op": "create", "type": "diagram", "id": format!("${name}{i}"), "parent": sec,
            "fields": {"lang": lang, "alt": alt}, "body": body}),
        );
    }
    ops
}

// --- tests --------------------------------------------------------------------------------------

#[test]
fn dgm6_svg_renders_without_script_and_follows_tokens() {
    let (_d, r, _ids) = project(Value::Array(diagram_doc(
        "flow",
        "Flow page",
        &[("mermaid", "The flow", FLOW)],
    )));
    let s = site(&r);
    let html = page(&s.files, "flow.html");
    let figs = figures(&html);
    assert_eq!(figs.len(), 1, "{html}");
    let fig = figs[0];
    // the drawing: native disclosure, its summary the one inline SVG, hidden from the text
    assert!(
        fig.starts_with("<figure class=\"diagram\" data-lang=\"mermaid\">"),
        "{fig}"
    );
    assert!(
        fig.contains(
            "<details class=\"diagram-canvas\"><summary aria-label=\"Expand diagram\"><span class=\"diagram-scroll\"><span class=\"diagram-svg\" aria-hidden=\"true\"><svg "
        ),
        "{fig}"
    );
    assert_eq!(fig.matches("<svg").count(), 1, "one inline SVG: {fig}");
    assert!(fig.contains("<p class=\"visually-hidden\">The flow</p>"));
    assert!(fig.contains("<figcaption>The flow</figcaption>"));
    assert!(fig.contains(
        "<details class=\"diagram-source\"><summary>Diagram source</summary><pre class=\"code\"><code>flowchart LR"
    ));
    // the labels show as drawn text, and the subgraph is drawn
    for label in [">start<", ">end<", ">after<", ">first<"] {
        assert!(fig.contains(label), "{label}: {fig}");
    }
    // the inline copy reads the site's tokens: the page's Auto, Light and Dark setting wins
    let svg = &fig[fig.find("<svg").unwrap()..fig.find("</svg>").unwrap()];
    for token in [
        "var(--bg)",
        "var(--ink)",
        "var(--line-strong)",
        "var(--surface)",
    ] {
        assert!(svg.contains(token), "{token}: {svg}");
    }
    assert!(!svg.contains("prefers-color-scheme"), "{svg}");
    assert!(
        !svg.contains("#fbfbfc") && !svg.contains("#121419"),
        "{svg}"
    );
    // the full-size file: generated, named by the hash of its own bytes, themed for the OS
    let href = full_size(fig);
    assert!(href.starts_with("assets/diagrams/") && href.ends_with(".svg"));
    let bytes = &s.files[&href];
    let name = href
        .trim_start_matches("assets/diagrams/")
        .trim_end_matches(".svg");
    assert_eq!(name, Hash::of(bytes).to_string());
    assert_ne!(name, Hash::of(FLOW.as_bytes()).to_string());
    let asset = text(bytes);
    assert!(asset.contains("prefers-color-scheme: dark"), "{asset}");
    assert!(asset.contains("#fbfbfc") && asset.contains("#121419"));
    assert!(asset.contains(">start<") && asset.contains(">first<"));
    // no script draws, expands or opens anything; no diagram library ships
    assert!(!fig.contains("<script") && !asset.contains("<script"));
    assert_eq!(html.matches("<script").count(), 2, "init and app.js only");
    for (path, bytes) in &s.files {
        assert!(!path.contains("mermaid"), "{path}");
        let t = String::from_utf8_lossy(bytes);
        for word in ["mermaid.min.js", "mermaid.initialize", "mermaid-init"] {
            assert!(!t.contains(word), "{path}: {word}");
        }
    }
    assert_eq!(diagram_files(&s.files).len(), 1);
    assert!(s.summary.warnings.is_empty(), "{:?}", s.summary.warnings);
}

#[test]
fn dgm6_identical_diagrams_have_unique_inline_ids() {
    let mut ops = diagram_doc(
        "twice",
        "Twice page",
        &[("mermaid", "First", FLOW), ("mermaid", "Second", FLOW)],
    );
    ops.extend(diagram_doc(
        "again",
        "Again page",
        &[("mermaid", "Third", FLOW)],
    ));
    let (_d, r, _ids) = project(Value::Array(ops));
    let s = site(&r);
    // one drawing: one file, linked from every place
    let files = diagram_files(&s.files);
    assert_eq!(files.len(), 1, "{:?}", files.keys());
    let path = files.keys().next().unwrap().clone();
    let hash = path
        .trim_start_matches("assets/diagrams/")
        .trim_end_matches(".svg")
        .to_string();
    for p in ["twice.html", "again.html"] {
        let html = page(&s.files, p);
        let ids = ids_in(&html);
        let unique: BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "{p}: repeated ids");
        for fig in figures(&html) {
            assert_eq!(full_size(fig), path);
        }
    }
    let twice = page(&s.files, "twice.html");
    let figs = figures(&twice);
    assert_eq!(figs.len(), 2);
    // each place prefixes its ids and its references with its own place
    for (place, fig) in figs.iter().enumerate() {
        let own = format!("dgm-{hash}-{place}-");
        let svg = &fig[fig.find("<svg").unwrap()..fig.find("</svg>").unwrap()];
        let svg_ids = ids_in(svg);
        assert!(!svg_ids.is_empty());
        assert!(svg_ids.iter().all(|i| i.starts_with(&own)), "{svg_ids:?}");
        assert!(svg.contains(&format!("url(#{own}")), "{svg}");
        assert!(svg.contains(&format!("#{own}")), "{svg}");
    }
    let again = page(&s.files, "again.html");
    assert!(again.contains(&format!("id=\"dgm-{hash}-0-")));
}

#[test]
fn dgm6_fallbacks_keep_source_alt_and_caption() {
    let (_d, r, _ids) = project(Value::Array(diagram_doc(
        "fall",
        "Fall page",
        &[
            ("mermaid", "Broken", INVALID),
            ("mermaid", "Slices", PIE),
            ("mermaid", "Active", ACTIVE),
            ("d2", "Boxes", "a -> b: `go`"),
            ("mermaid", "", INVALID),
        ],
    )));
    let s = site(&r);
    let html = page(&s.files, "fall.html");
    let figs = figures(&html);
    assert_eq!(figs.len(), 5, "{html}");
    let shape = |lang: &str, human: &str, alt: Option<&str>, source: &str| {
        let hidden = alt.unwrap_or("Diagram (source below)");
        let caption = alt
            .map(|a| format!("<figcaption>{a}</figcaption>"))
            .unwrap_or_default();
        format!(
            "<figure class=\"diagram\" data-lang=\"{lang}\"><p class=\"visually-hidden\">{hidden}</p><div class=\"code-block\"><div class=\"code-head\"><span class=\"lang\">{human}</span><button type=\"button\" class=\"tool copy-code\" aria-label=\"Copy code\">Copy</button></div><pre class=\"code\"><code class=\"language-{lang}\">{source}</code></pre></div>{caption}</figure>"
        )
    };
    let want = [
        shape("mermaid", "Mermaid", Some("Broken"), "flowchart LR\n A["),
        shape(
            "mermaid",
            "Mermaid",
            Some("Slices"),
            "pie\n  &quot;a&quot; : 1",
        ),
        shape(
            "mermaid",
            "Mermaid",
            Some("Active"),
            "flowchart LR\n    A --&gt; B\n    classDef e fill:#f00&quot; onload=&quot;alert(1)\n    class A e",
        ),
        shape("d2", "D2", Some("Boxes"), "a -&gt; b: `go`"),
        shape("mermaid", "Mermaid", None, "flowchart LR\n A["),
    ];
    for (got, want) in figs.iter().zip(&want) {
        assert_eq!(got, want);
    }
    // no drawing, no file, no highlighting; the code-copy control is the code block's own
    assert!(!html.contains("diagram-canvas") && !html.contains("open full size"));
    assert!(diagram_files(&s.files).is_empty());
    assert!(!html.contains("onload=\"alert"));
}

#[test]
fn dgm6_warnings_have_exact_order_and_text() {
    // a top-level doc named glossary gives the build's own warning first
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$g", "fields": {"name": "glossary", "title": "Glossary", "doc_type": "page"}}),
        json!({"op": "create", "type": "prose", "parent": "$g", "body": "Words."}),
    ];
    ops.extend(diagram_doc(
        "alpha",
        "Alpha page",
        &[
            ("mermaid", "Drawn", FLOW),
            ("mermaid", "Broken", INVALID),
            ("d2", "Boxes", "a -> b"),
            ("mermaid", "Slices", PIE),
            ("mermaid", "Broken again", INVALID),
        ],
    ));
    ops.extend(diagram_doc(
        "beta",
        "Beta page",
        &[("mermaid", "Active", ACTIVE)],
    ));
    let (_d, mut r, ids) = project(Value::Array(ops));
    let id = |ph: &str| ids[ph].clone();
    let want = vec![
        "the top-level doc named glossary has no page: the site keeps the name glossary.html for its generated glossary. Rename the doc to publish it.".to_string(),
        format!(
            "Alpha page ({}): the Mermaid diagram does not parse: Parse error at line 2, column 4: expected text_rect",
            id("$alpha1")
        ),
        format!(
            "Alpha page ({}): rdy does not draw pie diagrams, so it shows the source",
            id("$alpha3")
        ),
        format!(
            "Alpha page ({}): the Mermaid diagram does not parse: Parse error at line 2, column 4: expected text_rect",
            id("$alpha4")
        ),
        format!(
            "Beta page ({}): rdy could not draw the diagram: the drawn diagram holds active content, so it shows the source",
            id("$beta0")
        ),
    ];
    // the site, built twice: the same warnings once per block, in page and block order
    assert_eq!(site(&r).summary.warnings, want);
    assert_eq!(site(&r).summary.warnings, want);
    let out = tempfile::tempdir().unwrap();
    let summary =
        rdy_render::render_site(&r.read(), &Scope::Canon, &out.path().join("site")).unwrap();
    assert_eq!(summary.warnings, want);
    // the publication and its one page give the same warnings; a page gives its own blocks'
    let landing = tag(&mut r);
    assert_eq!(full(&r, landing, IMPLICIT_ORG).warnings, want);
    let beta = one(&r, landing, IMPLICIT_ORG, "beta.html");
    assert_eq!(beta.warnings, vec![want[0].clone(), want[4].clone()]);
    let home = one(&r, landing, IMPLICIT_ORG, "index.html");
    assert_eq!(home.warnings, vec![want[0].clone()]);
    // the print shows each diagram as its source and warns of none
    let print = rdy_render::print::build(&r.read(), &Scope::Canon).unwrap();
    assert_eq!(print.warnings, vec![want[0].clone()]);
}

#[test]
fn dgm6_full_and_one_page_assets_match() {
    let mut ops = diagram_doc(
        "flow",
        "Flow page",
        &[("mermaid", "The flow", FLOW), ("mermaid", "Again", FLOW)],
    );
    ops.extend(diagram_doc(
        "other",
        "Other page",
        &[("mermaid", "Other", "flowchart TD\n  X[one] --> Y[two]")],
    ));
    ops.push(json!({"op": "create", "type": "doc", "id": "$plain", "fields": {"name": "plain", "title": "Plain page", "doc_type": "page"}}));
    ops.push(json!({"op": "create", "type": "prose", "parent": "$plain", "body": "No diagram."}));
    let (_d, mut r, _ids) = project(Value::Array(ops));
    let landing = tag(&mut r);
    let full = full(&r, landing, IMPLICIT_ORG);
    let all = diagram_files(&full.files);
    assert_eq!(all.len(), 2, "{:?}", all.keys());
    let entry = |m: &rdy_engine::publication_manifest::Manifest, path: &str| {
        m.files.iter().find(|e| e.path == path).cloned()
    };
    for (path, want_count) in [("flow.html", 1), ("other.html", 1), ("plain.html", 0)] {
        let p = one(&r, landing, IMPLICIT_ORG, path);
        // the page is the full site's page, and its assets are the full site's files
        assert_eq!(p.html, full.files[path], "{path}");
        let mine = diagram_files(&p.assets);
        assert_eq!(mine.len(), want_count, "{path}: {:?}", mine.keys());
        for (asset, bytes) in &p.assets {
            assert_eq!(Some(bytes), full.files.get(asset), "{path}: {asset}");
            assert_eq!(
                entry(&p.manifest, asset),
                entry(&full.manifest, asset),
                "{path}: {asset}"
            );
        }
        let html = text(&p.html);
        for fig in figures(&html) {
            assert!(mine.contains_key(&full_size(fig)), "{path}");
        }
        // the theme carries no diagram library anywhere
        assert!(!p.assets.keys().any(|k| k.starts_with("assets/js/")));
    }
    assert!(!full.files.keys().any(|k| k.starts_with("assets/js/")));
}

#[test]
fn dgm6_manifest_svg_is_generated_only() {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let mut r = Rdy::open(dir.path()).unwrap();
    // an author's own SVG figure keeps its own media type and path
    let authored = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec();
    let blob = r.store_mut().put_blob(&authored).unwrap();
    let mut ops = diagram_doc("flow", "Flow page", &[("mermaid", "The flow", FLOW)]);
    ops.push(json!({"op": "create", "type": "figure", "parent": "$flows",
        "fields": {"blob": blob.to_string(), "media_type": "image/svg+xml", "alt": "Authored"}}));
    put(&mut r, Value::Array(ops));
    land(&mut r);
    let landing = tag(&mut r);
    let a = full(&r, landing, IMPLICIT_ORG);
    let drawn: Vec<_> = a
        .manifest
        .files
        .iter()
        .filter(|e| e.path.ends_with(".svg"))
        .collect();
    assert_eq!(drawn.len(), 2, "{drawn:?}");
    for e in drawn {
        let bytes = &a.files[&e.path];
        assert_eq!(e.media_type, "image/svg+xml", "{}", e.path);
        assert_eq!(e.hash, Hash::of(bytes));
        assert_eq!(e.size, bytes.len() as u64);
        assert_eq!(e.notice_offset, None);
        if e.path == format!("assets/{blob}.svg") {
            assert_eq!(bytes, &authored);
        } else {
            let name = e
                .path
                .strip_prefix("assets/diagrams/")
                .and_then(|n| n.strip_suffix(".svg"))
                .unwrap_or_else(|| panic!("{}", e.path));
            assert_eq!(name, Hash::of(bytes).to_string());
            assert!(text(bytes).contains(">start<"));
        }
    }
}

#[test]
fn dgm6_profiles_never_emit_excluded_text() {
    let ops = vec![
        json!({"op": "create", "type": "doc", "id": "$hid", "fields": {"name": "hidden", "title": "HIDDENCANARY doc", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$hs", "parent": "$hid", "fields": {"title": "HIDDENCANARY section"}}),
        json!({"op": "create", "type": "prose", "id": "$hp", "parent": "$hs", "body": "HIDDENCANARY prose."}),
        json!({"op": "create", "type": "diagram", "parent": "$hs", "fields": {"lang": "mermaid", "alt": "HIDDENCANARY alt"}, "body": "flowchart LR\n  H[HIDDENCANARY drawn] --> I[HIDDENCANARY too]"}),
        json!({"op": "create", "type": "diagram", "parent": "$hs", "fields": {"lang": "mermaid", "alt": "Hidden broken"}, "body": "flowchart LR\n H[HIDDENCANARY"}),
        json!({"op": "create", "type": "doc", "id": "$shown", "fields": {"name": "shown", "title": "Shown page", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$ss", "parent": "$shown", "fields": {"title": "Flow"}}),
        json!({"op": "create", "type": "diagram", "id": "$drawn", "parent": "$ss", "fields": {"lang": "mermaid", "alt": "Alt {{$hp}}"}, "body": "flowchart LR\n  A[{{$hp}}] --> B[{{$hs|Shown words}}]"}),
        json!({"op": "create", "type": "diagram", "id": "$broken", "parent": "$ss", "fields": {"lang": "mermaid", "alt": "Broken"}, "body": "flowchart LR\n  A[{{$hp}}"}),
        json!({"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}),
    ];
    let (_d, mut r, ids) = project(Value::Array(ops));
    let profile = put(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "id": "$p", "parent": ids["$admin"],
            "fields": {"key": "shownorg", "title": "Shown site", "audience": "org", "select": [ids["$shown"]]}}]),
    );
    land(&mut r);
    let selector = profile["$p"].clone();
    let u = rdy_render::inline::UNAVAILABLE;
    let canary = |files: &BTreeMap<String, Vec<u8>>, warnings: &[String]| {
        for (path, bytes) in files {
            let t = String::from_utf8_lossy(bytes);
            assert!(!t.contains("HIDDENCANARY"), "{path}");
            for ph in ["$hid", "$hs", "$hp"] {
                assert!(!t.contains(&ids[ph]), "{path}: {ph}");
            }
        }
        for w in warnings {
            assert!(!w.contains("HIDDENCANARY"), "{w}");
        }
    };
    let s = build_profile_site(&r.read(), "canon", "shownorg").unwrap();
    canary(&s.files, &s.summary.warnings);
    let html = page(&s.files, "shown.html");
    let figs = figures(&html);
    assert_eq!(figs.len(), 2);
    // the drawing and its source read the selected words, never the hidden ones
    let drawn = figs[0];
    assert!(drawn.contains(&format!(">{u}<")), "{drawn}");
    assert!(drawn.contains(">Shown words<"), "{drawn}");
    assert!(drawn.contains(&format!("A[{u}] --&gt; B[Shown words]")));
    let files = diagram_files(&s.files);
    assert_eq!(files.len(), 1);
    let asset = text(files.values().next().unwrap());
    assert!(asset.contains(&format!(">{u}<")) && asset.contains(">Shown words<"));
    // the shown page's failing block warns; the hidden doc's blocks are never drawn
    let broken = ids["$broken"].clone();
    let diagram_warnings: Vec<&String> = s
        .summary
        .warnings
        .iter()
        .filter(|w| w.contains("Mermaid"))
        .collect();
    assert_eq!(diagram_warnings.len(), 1, "{:?}", s.summary.warnings);
    assert!(
        diagram_warnings[0].starts_with(&format!(
            "Shown page ({broken}): the Mermaid diagram does not parse: "
        )),
        "{diagram_warnings:?}"
    );
    // the publication and one page of it read the same selection
    let landing = tag(&mut r);
    let a = full(&r, landing, &selector);
    canary(&a.files, &a.warnings);
    assert_eq!(diagram_files(&a.files), files);
    let p = one(&r, landing, &selector, "shown.html");
    canary(&p.assets, &p.warnings);
    assert_eq!(text(&p.html), page(&a.files, "shown.html"));
    // the print draws nothing and ships no drawing
    let print = rdy_render::print::build_profile(&r.read(), "canon", "shownorg").unwrap();
    for w in &print.warnings {
        assert!(!w.contains("Mermaid") && !w.contains("HIDDENCANARY"), "{w}");
    }
    assert!(print.blobs.is_empty());
}

/// The page's Copy payload: the JSON in its one Copy template, as a browser reads its text.
fn copy_payload(html: &str) -> Value {
    let open = "<template data-copy-page hidden>";
    let start = html.find(open).expect("a Copy template") + open.len();
    let end = html[start..].find("</template>").unwrap() + start;
    let text = html[start..end]
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// Every string in a JSON value, joined.
fn strings(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(strings).collect(),
        Value::Object(o) => o.values().map(strings).collect(),
        _ => String::new(),
    }
}

/// HTML text as the page escapes it.
fn escaped(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// rdb1 (D-R7C3W): rdy draws from a rewritten copy of the source (CRLF as LF, no blank line
/// after a subgraph header), and every fallback shows the author's own bytes. The refusal is the
/// length reason, which the rewrite reaches: at the base this source was not closed.
#[test]
fn rdb1_normalized_fallback_keeps_original_source() {
    let long = "y".repeat(300);
    let refused =
        format!("flowchart TD\r\nsubgraph S\r\n\r\nA --> B\r\nC -- {long} --> D\r\nend\r\n");
    let valid = "flowchart TD\nsubgraph S\n\n  \nA[start] --> B[stop]\nend\n";
    let (_d, r, ids) = project(Value::Array(diagram_doc(
        "norm",
        "Norm page",
        &[("mermaid", "Long", &refused), ("mermaid", "Blank", valid)],
    )));
    let s = site(&r);
    let html = page(&s.files, "norm.html");
    let figs = figures(&html);
    assert_eq!(figs.len(), 2, "{html}");
    // the refused source: its code block holds the original bytes, CR and blank line kept
    assert!(
        figs[0].contains(&format!(
            "<pre class=\"code\"><code class=\"language-mermaid\">{}</code></pre>",
            escaped(&refused)
        )),
        "{:?}",
        figs[0]
    );
    // and so does the page's Copy data
    let copy = strings(&copy_payload(&html));
    assert!(copy.contains(&refused), "{copy:?}");
    // the valid blank-line diagram draws, with no warning
    assert!(figs[1].contains("diagram-canvas"), "{}", figs[1]);
    assert!(figs[1].contains(">start<") && figs[1].contains(">stop<"));
    assert!(copy.contains(valid), "{copy:?}");
    assert_eq!(
        s.summary.warnings,
        vec![format!(
            "Norm page ({}): the Mermaid diagram does not parse: the source has an arrow over 256 bytes",
            ids["$norm0"]
        )]
    );
}

/// rdb1 (D-438Y2): the hidden arrows that stack separate subgraphs are in the drawing input
/// only. A four-panel source that is refused shows its own bytes, with no `~~~` line.
#[test]
fn rdb1_stacked_fallback_keeps_original_source() {
    let four = include_str!("../../rdy-diagram/tests/fixtures/diagram/four-panels.mmd");
    let refused = four.replacen(
        "        direction LR\n",
        &format!(
            "        direction LR\n        z1 -- {} --> z2\n",
            "y".repeat(300)
        ),
        1,
    );
    assert_ne!(refused, four);
    let (_d, r, ids) = project(Value::Array(diagram_doc(
        "stack",
        "Stack page",
        &[("mermaid", "Refused", &refused), ("mermaid", "Drawn", four)],
    )));
    let s = site(&r);
    let html = page(&s.files, "stack.html");
    let figs = figures(&html);
    assert_eq!(figs.len(), 2, "{html}");
    assert!(
        figs[0].contains(&format!(
            "<pre class=\"code\"><code class=\"language-mermaid\">{}</code></pre>",
            escaped(&refused)
        )),
        "{:?}",
        figs[0]
    );
    let copy = strings(&copy_payload(&html));
    assert!(copy.contains(&refused), "{copy:?}");
    // the drawn one's source under "Diagram source" is the author's too
    assert!(figs[1].contains("diagram-canvas"), "{}", figs[1]);
    assert!(figs[1].contains(&format!(
        "<pre class=\"code\"><code>{}</code></pre>",
        escaped(four)
    )));
    assert!(copy.contains(four), "{copy:?}");
    for fig in &figs {
        let source = &fig[fig.find("<pre class=\"code\">").unwrap()..];
        assert!(!source.contains("~~~"), "{source}");
    }
    assert!(!copy.contains("~~~"), "{copy:?}");
    assert_eq!(
        s.summary.warnings,
        vec![format!(
            "Stack page ({}): the Mermaid diagram does not parse: the source has an arrow over 256 bytes",
            ids["$stack0"]
        )]
    );
}
