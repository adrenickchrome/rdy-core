//! rdb7 (D-XM8MP, D-6H0ZX, D-ZJVRT, D-0VCQ5): drawn diagrams are kept in the store's derived
//! diagram cache. A hit skips drawing on every surface that reads a store; a refusal is never a
//! key; the cache never reaches the log, a dump or fsck, and never changes a build's bytes.
use rdy_engine::publication::{IMPLICIT_ORG, PublicationSource, ReadPublicationSource};
use rdy_engine::render_inputs::RenderInputs;
use rdy_engine::site_scope::ScopeRef;
use rdy_engine::site_target::SiteTarget;
use rdy_engine::*;
use rdy_model::*;
use rdy_render::render_input::capture_inputs;
use rdy_render::{
    Mode, build_profile_site, build_publication, build_site, diagram, diagram_cache_key,
};
use rdy_render::{print::PBlock, render_publication_page};
use rdy_schema::Pack;
use rdy_store::kinds::BlobManifest;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const TAG: &str = "v1.0.0";
/// A flowchart with a subgraph (native_diagrams.rs).
const FLOW: &str =
    "flowchart LR\n  subgraph S[first]\n    A[start] --> B[end]\n  end\n  B --> C[after]";
/// Another drawn source, whose drawing is planted under FLOW's key.
const OTHER: &str = "flowchart TB\n  X[other one] --> Y[other two]";
/// A source the pinned parser refuses after admission.
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
                message: "rdb7".into(),
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

/// The flow doc (FLOW) and the other doc (OTHER), landed.
fn two_docs() -> (tempfile::TempDir, Rdy, BTreeMap<String, String>) {
    let mut ops = diagram_doc("flow", "Flow page", &[("mermaid", "The flow", FLOW)]);
    ops.extend(diagram_doc(
        "other",
        "Other page",
        &[("mermaid", "The other", OTHER)],
    ));
    project(Value::Array(ops))
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

fn site(r: &Rdy) -> BTreeMap<String, Vec<u8>> {
    build_site(&r.read(), &Scope::Canon, Mode::Static)
        .unwrap()
        .files
}

fn cache(r: &Rdy) -> DiagramCache {
    r.read().reader().diagram_cache()
}

fn key(source: &str) -> Vec<u8> {
    diagram_cache_key(source).unwrap_or_else(|| panic!("refused: {source}"))
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

/// The path of the file a drawn figure's `open full size` link leads to, as the page writes it.
fn full_size(fig: &str) -> String {
    let at = fig
        .find(">open full size</a>")
        .unwrap_or_else(|| panic!("no full-size link: {fig}"));
    let href = fig[..at].rfind("href=\"").unwrap() + "href=\"".len();
    let end = fig[href..].find('"').unwrap() + href;
    fig[href..end].trim_start_matches("../").to_string()
}

/// The asset path of the one drawn figure of `html`.
fn asset_of(html: &str) -> String {
    let figs = figures(html);
    assert_eq!(figs.len(), 1, "{html}");
    full_size(figs[0])
}

/// The asset path of a standalone file.
fn path_of(file: &str) -> String {
    format!("assets/diagrams/{}.svg", Hash::of(file.as_bytes()))
}

/// OTHER's standalone file, from a build of `r`.
fn other_file(r: &Rdy) -> String {
    let files = site(r);
    let path = asset_of(&page(&files, "other.html"));
    text(&files[&path])
}

/// The page's Copy payload text: the JSON in its one Copy template, as a browser reads it.
fn page_copy(html: &str) -> String {
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
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
    fn strings(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Array(a) => a.iter().map(strings).collect(),
            Value::Object(o) => o.values().map(strings).collect(),
            _ => String::new(),
        }
    }
    strings(&v)
}

// --- tests --------------------------------------------------------------------------------------

#[test]
fn rdb7_a_second_build_reads_the_cache() {
    let (_d, mut r, _ids) = two_docs();
    let first = site(&r);
    let second = site(&r);
    assert!(first == second, "a second build gives equal bytes");
    let own = asset_of(&page(&first, "flow.html"));
    let other = other_file(&r);
    assert_ne!(own, path_of(&other));
    // FLOW's row now holds OTHER's drawing: the build reads it and does not draw
    cache(&r).put(&key(FLOW), &CachedDrawing::File(other.clone()));
    let planted = site(&r);
    let shown = asset_of(&page(&planted, "flow.html"));
    assert_eq!(shown, path_of(&other));
    assert_eq!(text(&planted[&shown]), other);
    // a reindex drops the cache: FLOW draws again
    r.reindex().unwrap();
    assert_eq!(cache(&r).get(&key(FLOW)), None);
    let again = site(&r);
    assert_eq!(asset_of(&page(&again, "flow.html")), own);
    assert!(again == first);
}

/// A publication source that keeps the trait's default diagram cache: none.
struct NoCache<'a>(ReadPublicationSource<'a>);

impl PublicationSource for NoCache<'_> {
    fn pack(&self) -> &Pack {
        self.0.pack()
    }
    fn title(&self) -> &str {
        self.0.title()
    }
    fn description(&self) -> Option<&str> {
        self.0.description()
    }
    fn manifest(&self, blob: &Hash) -> Result<Option<BlobManifest>, RdyError> {
        self.0.manifest(blob)
    }
    fn blob_available(&self, blob: &Hash) -> Result<bool, RdyError> {
        self.0.blob_available(blob)
    }
    fn blob(&self, blob: &Hash) -> Result<Vec<u8>, RdyError> {
        self.0.blob(blob)
    }
    fn removed_blob(&self, blob: &Hash) -> Result<Option<Hash>, RdyError> {
        self.0.removed_blob(blob)
    }
    fn visit_blob_redactions(
        &self,
        blob: &Hash,
        visit: &mut dyn FnMut(Hash) -> Result<(), RdyError>,
    ) -> Result<(), RdyError> {
        self.0.visit_blob_redactions(blob, visit)
    }
}

/// Where the FLOW code block and its caption stand among a print doc's blocks.
fn print_order(blocks: &[PBlock]) -> (usize, usize) {
    let code = blocks
        .iter()
        .position(|b| matches!(b, PBlock::Code { text, .. } if text.contains("A[start]")))
        .unwrap_or_else(|| panic!("no FLOW code: {blocks:?}"));
    let caption = blocks
        .iter()
        .position(|b| match b {
            PBlock::Para { runs, .. } => {
                runs.iter().map(|r| r.text.as_str()).collect::<String>() == "The flow"
            }
            _ => false,
        })
        .unwrap_or_else(|| panic!("no caption: {blocks:?}"));
    (code, caption)
}

fn flow_print(r: &Rdy) -> (usize, usize) {
    let p = rdy_render::print::build(&r.read(), &Scope::Canon).unwrap();
    let d = p
        .docs
        .iter()
        .find(|d| d.title == "Flow page")
        .expect("the flow doc");
    print_order(&d.blocks)
}

#[test]
fn rdb7_every_surface_reads_the_cache() {
    let (_d, mut r, ids) = two_docs();
    let profile = put(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "id": "$p", "parent": ids["$flow"],
            "fields": {"key": "floworg", "title": "Flow site", "audience": "org", "select": [ids["$flow"]]}}]),
    );
    land(&mut r);
    let selector = profile["$p"].clone();
    let landing = tag(&mut r);
    let other = other_file(&r);
    let own = asset_of(&page(&site(&r), "flow.html"));
    cache(&r).put(&key(FLOW), &CachedDrawing::File(other.clone()));
    let want = path_of(&other);
    // the static site
    assert_eq!(asset_of(&page(&site(&r), "flow.html")), want, "build_site");
    // the live build
    let live = rdy_render::live::build_live(
        &r.read(),
        &Scope::Canon,
        &SiteTarget::Source(ScopeRef::Canon),
    )
    .unwrap();
    assert_eq!(
        asset_of(&page(&live.files, "flow.html")),
        want,
        "build_live"
    );
    // the profile's static site
    let prof = build_profile_site(&r.read(), "canon", "floworg").unwrap();
    assert_eq!(
        asset_of(&page(&prof.files, "flow.html")),
        want,
        "build_profile_site"
    );
    // the publication and one page of it, through the store
    let read = r.read();
    let inputs = inputs(&r, landing);
    let source = ReadPublicationSource::with_settings(
        &read,
        &inputs.pack,
        &inputs.title,
        inputs.description.as_deref(),
    );
    let m = read.materialized(&Scope::At(landing)).unwrap();
    for sel in [selector.as_str(), IMPLICIT_ORG] {
        let a = build_publication(&source, &m, TAG, false, sel, &inputs).unwrap();
        assert_eq!(asset_of(&page(&a.files, "flow.html")), want, "{sel}");
        let one =
            render_publication_page(&source, &m, TAG, false, sel, &inputs, "flow.html").unwrap();
        assert_eq!(asset_of(&text(&one.html)), want, "{sel}: one page");
    }
    // a publication source with no cache draws FLOW itself
    let bare = NoCache(ReadPublicationSource::with_settings(
        &read,
        &inputs.pack,
        &inputs.title,
        inputs.description.as_deref(),
    ));
    let a = build_publication(&bare, &m, TAG, false, IMPLICIT_ORG, &inputs).unwrap();
    assert_eq!(asset_of(&page(&a.files, "flow.html")), own, "no cache");
    drop(read);
    // the print: a drawn diagram prints as its source (caption, then code); a failed row
    // prints as the site's fallback code block (code, then caption)
    let (code, caption) = flow_print(&r);
    assert!(caption < code, "drawn: the source after its caption");
    cache(&r).put(
        &key(FLOW),
        &CachedDrawing::Failed("a planted reason".into()),
    );
    let (code, caption) = flow_print(&r);
    assert!(code < caption, "failed: the code block, then its caption");
}

#[test]
fn rdb7_refusals_are_never_cached() {
    let arrow = format!("flowchart LR\nA -. x{}- B\n", ".".repeat(16000));
    let (_d, r, _ids) = project(Value::Array(diagram_doc(
        "refused",
        "Refused page",
        &[
            ("mermaid", "Long", arrow.as_str()),
            ("mermaid", "Slices", PIE),
            ("mermaid", "The flow", FLOW),
        ],
    )));
    let files = site(&r);
    assert_eq!(diagram_files(&files).len(), 1);
    assert_eq!(diagram_cache_key(&arrow), None);
    assert_eq!(diagram_cache_key(PIE), None);
    assert_eq!(cache(&r).rows_for_tests(), 1, "only the drawn source");
    assert!(cache(&r).get(&key(FLOW)).is_some());
}

#[test]
fn rdb7_failed_and_invalid_draws_are_cached() {
    let (_d, r, _ids) = project(Value::Array(diagram_doc(
        "fall",
        "Fall page",
        &[
            ("mermaid", "Active", ACTIVE),
            ("mermaid", "Broken", INVALID),
        ],
    )));
    let first = build_site(&r.read(), &Scope::Canon, Mode::Static).unwrap();
    let warnings = first.summary.warnings.clone();
    let failed = match cache(&r).get(&key(ACTIVE)) {
        Some(CachedDrawing::Failed(reason)) => reason,
        other => panic!("ACTIVE: {other:?}"),
    };
    let invalid = match cache(&r).get(&key(INVALID)) {
        Some(CachedDrawing::Invalid(reason)) => reason,
        other => panic!("INVALID: {other:?}"),
    };
    let shown = warnings.join("\n");
    assert!(
        shown.contains(&format!(
            "rdy could not draw the diagram: {failed}, so it shows the source"
        )),
        "{shown}"
    );
    assert!(
        shown.contains(&format!("the Mermaid diagram does not parse: {invalid}")),
        "{shown}"
    );
    assert_eq!(cache(&r).rows_for_tests(), 2);
    let second = build_site(&r.read(), &Scope::Canon, Mode::Static).unwrap();
    assert_eq!(second.summary.warnings, warnings);
    assert!(second.files == first.files);
}

#[test]
fn rdb7_line_endings_share_one_row() {
    let crlf = FLOW.replace('\n', "\r\n");
    let (_d, r, _ids) = project(Value::Array(diagram_doc(
        "ends",
        "Ends page",
        &[("mermaid", "LF", FLOW), ("mermaid", "CRLF", crlf.as_str())],
    )));
    let files = site(&r);
    assert_eq!(key(FLOW), key(&crlf));
    assert_eq!(cache(&r).rows_for_tests(), 1);
    assert_eq!(diagram_files(&files).len(), 1);
    let html = page(&files, "ends.html");
    let figs = figures(&html);
    assert_eq!(figs.len(), 2);
    assert_eq!(full_size(figs[0]), full_size(figs[1]));
    // each source shows its own bytes
    assert!(!figs[0].contains('\r'), "{}", figs[0]);
    assert!(figs[1].contains("flowchart LR\r\n"), "{}", figs[1]);
    let copy = page_copy(&html);
    assert!(copy.contains("flowchart LR\r\n"), "{copy}");
    assert!(copy.contains("flowchart LR\n  subgraph"), "{copy}");
}

#[test]
fn rdb7_a_stacked_source_caches_the_kept_drawing() {
    let four = include_str!("../../rdy-diagram/tests/fixtures/diagram/four-panels.mmd");
    let (_d, r, _ids) = project(Value::Array(diagram_doc(
        "four",
        "Four page",
        &[("mermaid", "Four", four)],
    )));
    let files = site(&r);
    let asset = asset_of(&page(&files, "four.html"));
    assert_eq!(cache(&r).rows_for_tests(), 1);
    match cache(&r).get(&key(four)) {
        Some(CachedDrawing::File(file)) => {
            assert_eq!(path_of(&file), asset);
            assert_eq!(text(&files[&asset]), file);
        }
        other => panic!("{other:?}"),
    }
    // the key reads the drawing input before its hidden arrows
    let admitted = diagram::admit(four).unwrap_or_else(|d| panic!("{d:?}"));
    let mut want = Hash::of(admitted.normalized().as_bytes()).0.to_vec();
    want.extend_from_slice(diagram::DRAWING_CRATE.as_bytes());
    want.push(0);
    want.extend_from_slice(env!("CARGO_PKG_VERSION").as_bytes());
    assert_eq!(key(four), want);
}

#[test]
fn rdb7_cache_never_reaches_the_log_a_dump_or_fsck() {
    let (d, r, _ids) = two_docs();
    let dump = |name: &str| {
        let out = d.path().join(name);
        r.read().dump(&out).unwrap();
        std::fs::read(&out).unwrap()
    };
    let before = (dump("before.rdydump"), format!("{:?}", r.fsck().unwrap()));
    let index = r.store().index_dump_for_tests().unwrap();
    assert_eq!(cache(&r).rows_for_tests(), 0);
    site(&r);
    assert_eq!(cache(&r).rows_for_tests(), 2);
    let after = (dump("after.rdydump"), format!("{:?}", r.fsck().unwrap()));
    assert!(
        before == after,
        "the dump and the fsck report do not change"
    );
    assert!(r.store().index_dump_for_tests().unwrap() == index);
}

#[test]
fn rdb7_concurrent_builds_agree() {
    let (_d, r, _ids) = two_docs();
    let barrier = std::sync::Barrier::new(2);
    let (one, two) = (r.read(), r.read());
    let (a, b) = std::thread::scope(|s| {
        let barrier = &barrier;
        let run = move |st: std::sync::Arc<ReadState>| {
            barrier.wait();
            build_site(&st, &Scope::Canon, Mode::Static).unwrap().files
        };
        let a = s.spawn(move || run(one));
        let b = s.spawn(move || run(two));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert!(a == b, "both builds give equal bytes");
    assert_eq!(cache(&r).rows_for_tests(), 2, "one row per drawn source");
}
