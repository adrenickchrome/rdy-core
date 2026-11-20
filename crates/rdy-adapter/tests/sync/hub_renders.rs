//! D-CST6K through the adapter: a render request of a landing the holder reaches (D-Y32MN), alone
//! or as a review pair against a second landing. Each side travels as its own complete canon
//! history stream; the render process builds the state at `scope`, with core's review marks
//! against `against` when the request names one. A request with no scope is today's tag request.
//! In a holder, a scope may name a pushed view at an op (D-V9RF5): its side carries the view's
//! history and the state list of the holder's merge, and the process renders the base with each
//! row applied.

use crate::hub_world::{
    AT, World, check, doc, land, moved, project, pushed_draft, seed, tag, world,
};
use rdy_adapter::rendering::{
    PublicationArtifact, RenderFormat, RenderInputs, ScopeRender, read_render_artifact,
    run_render_process,
};
use rdy_adapter::stream::ObjectKey;
use rdy_adapter::sync::CanonMove;
use rdy_adapter::{Hash, RdyError, SearchRequest, Snapshot, StoreHandle};
use rdy_engine::publication::{IMPLICIT_ORG, ReadPublicationSource};
use rdy_engine::{Rdy, Scope};
use rdy_model::BlockId;
use rdy_render::process::write_state_list;
use rdy_render::render_input::capture_inputs;
use rdy_render::{build_publication, build_review_publication};
use std::collections::{BTreeMap, BTreeSet};

fn landing(h: &Hash) -> String {
    format!("landing:{h}")
}

/// The render inputs of project P, captured from its canon.
fn inputs(w: &World) -> RenderInputs {
    let r = Rdy::open(&w.p()).unwrap();
    let i = capture_inputs(&r.read(), &Scope::Canon).unwrap();
    r.close().unwrap();
    i
}

/// The scope render request of `scope` (against `against`) on `snap`, in `format`.
fn request(
    snap: &Snapshot,
    scope: &str,
    against: Option<&str>,
    inputs: &RenderInputs,
    format: RenderFormat,
) -> Result<Vec<u8>, RdyError> {
    let target = ScopeRender {
        scope: scope.into(),
        against: against.map(Into::into),
    };
    let mut out = Vec::new();
    snap.write_scope_render_request(&target, IMPLICIT_ORG, inputs, format, &mut out)
        .map(|_| out)
}

/// The site the render process makes of the scope request.
fn render(
    snap: &Snapshot,
    scope: &str,
    against: Option<&str>,
    inputs: &RenderInputs,
) -> PublicationArtifact {
    let req = request(snap, scope, against, inputs, RenderFormat::Site)
        .unwrap_or_else(|e| panic!("{e:#?}"));
    let mut packet = Vec::new();
    run_render_process(req.as_slice(), &mut packet).unwrap_or_else(|e| panic!("{e:#?}"));
    read_render_artifact(packet.as_slice()).unwrap()
}

/// The HTML pages of an artifact, by path.
fn pages(a: &PublicationArtifact) -> BTreeMap<String, String> {
    a.manifest
        .files
        .iter()
        .filter(|e| e.media_type == "text/html")
        .map(|e| {
            (
                e.path.clone(),
                String::from_utf8(a.files[&e.path].clone()).unwrap(),
            )
        })
        .collect()
}

/// Each (block id, mark word) a page writes: every element with a change mark that names its
/// block (`has-chg chg-<mark>` and `data-rdy-block` in one opening tag).
fn marks(html: &str) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        rest = &rest[at + 1..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..end];
        let word = if tag.contains(" has-chg chg-new") {
            Some("New")
        } else if tag.contains(" has-chg chg-edited") {
            Some("Edited")
        } else {
            None
        };
        if let Some(word) = word
            && let Some(i) = tag.find("data-rdy-block=\"")
        {
            let id = &tag[i + 16..];
            let id = &id[..id.find('"').unwrap()];
            out.insert((id.to_string(), word.to_string()));
        }
    }
    out
}

/// The page at `path`, which must hold `needle`.
fn page<'a>(pages: &'a BTreeMap<String, String>, path: &str, needle: &str) -> &'a str {
    let html = pages
        .get(path)
        .unwrap_or_else(|| panic!("{path}: {:?}", pages.keys()));
    assert!(html.contains(needle), "{path}: {needle}");
    html
}

/// The holder with canon at `p1` and `p2`.
fn extended(w: &World) -> StoreHandle {
    let h = w.holder("hub");
    seed(&h, w);
    let s = h.snapshot().unwrap();
    let c = check(&s, &[&w.e2]).unwrap();
    assert_eq!(
        h.move_canon(&c, CanonMove::Extend, Some(w.p1)).unwrap(),
        moved(w.p2)
    );
    h
}

/// Holds `q2` beside canon's tip `tip`.
fn hold_q2(h: &StoreHandle, w: &World, tip: Hash) {
    let s = h.snapshot().unwrap();
    let c = check(&s, &[&w.eq]).unwrap();
    assert_eq!(
        h.move_canon(&c, CanonMove::Hold, Some(tip)).unwrap(),
        moved(w.q2)
    );
}

#[test]
fn cra5_held_landing_renders_its_own_state() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    hold_q2(&h, &w, w.p1);
    let snap = h.snapshot().unwrap();
    assert_eq!(snap.canon(), Some(w.p1));
    let a = render(&snap, &landing(&w.q2), None, &inputs(&w));
    let pages = pages(&a);
    page(&pages, "held.html", "A chain the hub holds beside canon.");
    assert!(!pages.contains_key("extend.html"));
    for (path, html) in &pages {
        assert!(marks(html).is_empty(), "{path}");
        assert!(!html.contains("chg-removed"), "{path}");
    }
}

#[test]
fn cra5_review_pair_marks_against_from() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = extended(&w);
    let snap = h.snapshot().unwrap();
    let a = render(&snap, &landing(&w.p2), Some(&landing(&w.p1)), &inputs(&w));
    let pages = pages(&a);
    let extend = page(&pages, "extend.html", "The canon moves forward.");
    let marked = marks(extend);
    assert!(!marked.is_empty());
    assert!(marked.iter().all(|(_, word)| word == "New"), "{marked:?}");
    let canon = page(&pages, "canon.html", "The hub keeps one canon.");
    assert!(marks(canon).is_empty());
    for html in pages.values() {
        assert!(!html.contains("chg-removed"));
        assert!(!html.contains("nav-removed"));
    }
}

#[test]
fn cra5_process_output_equals_the_in_store_build() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let inputs = inputs(&w);
    let h = extended(&w);
    let snap = h.snapshot().unwrap();
    let a = render(&snap, &landing(&w.p2), Some(&landing(&w.p1)), &inputs);
    let r = Rdy::open(&w.p()).unwrap();
    let read = r.read();
    let source = ReadPublicationSource::with_settings(
        &read,
        &inputs.pack,
        &inputs.title,
        inputs.description.as_deref(),
    );
    let to = read.materialized(&Scope::At(w.p2)).unwrap();
    let from = read.materialized(&Scope::At(w.p1)).unwrap();
    let direct =
        build_review_publication(&source, &to, &source, &from, false, IMPLICIT_ORG, &inputs)
            .unwrap();
    assert_eq!(a.manifest_hash, direct.manifest_hash);
    assert_eq!(a.files, direct.files);
}

#[test]
fn cra5_against_outside_the_scope_closure_renders() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = extended(&w);
    hold_q2(&h, &w, w.p2);
    let snap = h.snapshot().unwrap();
    assert_eq!(snap.canon(), Some(w.p2));
    let a = render(&snap, &landing(&w.p2), Some(&landing(&w.q2)), &inputs(&w));
    let pages = pages(&a);
    // q2's own doc is not in p2's state: every page lists it as removed, as a review page does,
    // and no page holds its text
    assert!(!pages.contains_key("held.html"));
    for (path, html) in &pages {
        assert!(
            !html.contains("A chain the hub holds beside canon."),
            "{path}"
        );
        // a short link's page only sends the reader on
        if !path.starts_with("go/") {
            assert!(
                html.contains("<li class=\"nav-item nav-removed\">Held</li>"),
                "{path}"
            );
        }
    }
    // p2's own doc is new against q2, and canon's doc, in both, has no mark
    let extend = page(&pages, "extend.html", "The canon moves forward.");
    assert!(!marks(extend).is_empty());
    assert!(marks(page(&pages, "canon.html", "The hub keeps one canon.")).is_empty());
}

#[test]
fn cra5_adapter_refuses_a_scope_it_cannot_render() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let inputs = inputs(&w);
    let h = extended(&w);
    let snap = h.snapshot().unwrap();
    let (p1, p2) = (landing(&w.p1), landing(&w.p2));
    let short = format!("landing:{}", &w.p2.to_string()[..12]);
    for text in ["canon", "tag:1.0.0", short.as_str(), "view:draft"] {
        let want = format!("scope {text:?} cannot be rendered: use landing:<64 hex digits>");
        let fix = "name the landing by its full hash, or render a tag with write_render_request";
        for (scope, against) in [(text, None), (p2.as_str(), Some(text))] {
            let mut out = Vec::new();
            let target = ScopeRender {
                scope: scope.into(),
                against: against.map(Into::into),
            };
            let e = snap
                .write_scope_render_request(
                    &target,
                    IMPLICIT_ORG,
                    &inputs,
                    RenderFormat::Site,
                    &mut out,
                )
                .unwrap_err();
            assert_eq!(
                (e.code, e.message.as_str(), e.fix.as_str()),
                ("E_BAD_INPUT", want.as_str(), fix)
            );
            assert!(out.is_empty());
        }
    }
    // a full hash the holder does not reach is the read's own refusal
    let none = Hash::of(b"none");
    let mut out = Vec::new();
    let e = snap
        .write_scope_render_request(
            &ScopeRender {
                scope: p2.clone(),
                against: Some(landing(&none)),
            },
            IMPLICIT_ORG,
            &inputs,
            RenderFormat::Site,
            &mut out,
        )
        .unwrap_err();
    assert_eq!(
        (e.code, e.message),
        (
            "E_NOT_FOUND",
            format!(
                "landing {none} is not reached from canon, a tag, a held chain or a replaced mark"
            )
        )
    );
    assert!(out.is_empty());
    // what the process refuses is never written: a review request builds a site only
    for format in [RenderFormat::Markdown, RenderFormat::Techdocs] {
        let e = request(&snap, &p2, Some(&p1), &inputs, format).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                "E_BAD_INPUT",
                "a review render request builds a site only",
                "use format site, or omit against"
            )
        );
    }
    // a scope alone renders in every format
    for format in [
        RenderFormat::Site,
        RenderFormat::Markdown,
        RenderFormat::Techdocs,
    ] {
        let req = request(&snap, &p2, None, &inputs, format).unwrap();
        let mut packet = Vec::new();
        run_render_process(req.as_slice(), &mut packet).unwrap();
    }
}

#[test]
fn cra5_tag_request_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = project(dir.path());
    doc(&mut p, "w0", "Canon", "The project keeps one canon.");
    let l1 = land(&mut p, "w0");
    tag(&mut p, "v1.0.0");
    let inputs = capture_inputs(&p.read(), &Scope::At(l1)).unwrap();
    p.close().unwrap();
    let h = StoreHandle::open(dir.path()).unwrap();
    let snap = h.snapshot().unwrap();
    let mut req = Vec::new();
    snap.write_render_request("1.0.0", IMPLICIT_ORG, &inputs, RenderFormat::Site, &mut req)
        .unwrap();
    // the prelude of today: its three keys in this order, compact, with no scope or against;
    // then the tag's whole stream, to the end
    let compact = format!(
        "{{\"profile\":{},\"format\":\"site\",\"render_inputs\":{}}}",
        serde_json::to_string(IMPLICIT_ORG).unwrap(),
        serde_json::to_string(&inputs).unwrap()
    );
    let mut want = (compact.len() as u32).to_le_bytes().to_vec();
    want.extend_from_slice(compact.as_bytes());
    snap.write_stream("1.0.0", &BTreeSet::<ObjectKey>::new(), &mut want)
        .unwrap();
    assert!(req == want, "the tag request changed");
}

// ---------------------------------------------------------------------------------------------
// a pushed view at an op (D-V9RF5)

fn view_at(name: &str, op: &Hash) -> String {
    format!("view:{name}@{op}")
}

/// The base of view `draft` at its captured head `d2`.
fn base_of_d2(snap: &Snapshot, w: &World) -> Hash {
    let head = snap.view_head("draft").unwrap().unwrap();
    assert_eq!(head.head, w.d2);
    head.base.expect("the draft view has a base")
}

#[test]
fn cra6_view_render_at_the_first_head_shows_the_first_head() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = pushed_draft(&w);
    let snap = h.snapshot().unwrap();
    let a = render(&snap, &view_at("draft", &w.d1), None, &inputs(&w));
    let pages = pages(&a);
    page(&pages, "draft.html", "Work in flight.");
    page(&pages, "canon.html", "The hub keeps one canon.");
    for (path, html) in &pages {
        assert!(!html.contains("More work in flight."), "{path}");
        assert!(marks(html).is_empty(), "{path}");
    }
}

#[test]
fn cra6_view_render_marks_against_its_base() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = pushed_draft(&w);
    let snap = h.snapshot().unwrap();
    let base = base_of_d2(&snap, &w);
    let a = render(
        &snap,
        &view_at("draft", &w.d2),
        Some(&landing(&base)),
        &inputs(&w),
    );
    let pages = pages(&a);
    for (path, needle) in [
        ("draft.html", "Work in flight."),
        ("draft-two.html", "More work in flight."),
    ] {
        let marked = marks(page(&pages, path, needle));
        assert!(!marked.is_empty(), "{path}");
        assert!(marked.iter().all(|(_, word)| word == "New"), "{marked:?}");
    }
    assert!(marks(page(&pages, "canon.html", "The hub keeps one canon.")).is_empty());
    for html in pages.values() {
        assert!(!html.contains("chg-removed"));
    }
}

/// D-V9RF5 "renders what a read gives": the process's site of the view at `d2` is the site that
/// a build over a read of the same view at the same op gives, file for file. The source project
/// still holds the view at `d2`, with the same base and included changes as the holder's chain.
#[test]
fn cra6_view_render_equals_the_read_of_the_scope() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let inputs = inputs(&w);
    let h = pushed_draft(&w);
    let snap = h.snapshot().unwrap();
    let scope = view_at("draft", &w.d2);
    let a = render(&snap, &scope, None, &inputs);
    // the holder's read of the scope holds what the site shows
    let hit = snap
        .search(SearchRequest {
            query: "More work in flight".into(),
            scope: Some(scope.clone()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(hit["matches"][0]["path"], "draft-two", "{hit:#}");
    let r = Rdy::open(&w.p()).unwrap();
    let read = r.read();
    assert_eq!(
        read.reader().view_head("draft").unwrap().unwrap().head,
        w.d2
    );
    let source = ReadPublicationSource::with_settings(
        &read,
        &inputs.pack,
        &inputs.title,
        inputs.description.as_deref(),
    );
    let m = read.materialized(&Scope::View("draft".into())).unwrap();
    let direct = build_publication(&source, &m, "", false, IMPLICIT_ORG, &inputs).unwrap();
    assert_eq!(a.manifest_hash, direct.manifest_hash);
    assert_eq!(a.files, direct.files);
}

/// A scope render request split at its parts: the prelude (with its length), the view stream
/// part (with its length), and the state list's rows.
struct ViewRequest {
    prelude: Vec<u8>,
    stream: Vec<u8>,
    rows: Vec<(BlockId, Option<Hash>)>,
}

impl ViewRequest {
    fn split(req: &[u8]) -> ViewRequest {
        let take = |at: &mut usize, n: usize| {
            let part = &req[*at..*at + n];
            *at += n;
            part
        };
        let mut at = 0;
        let len = u32::from_le_bytes(take(&mut at, 4).try_into().unwrap()) as usize;
        let prelude = req[..4 + len].to_vec();
        at += len;
        let len = u64::from_le_bytes(take(&mut at, 8).try_into().unwrap()) as usize;
        let stream = req[at - 8..at + len].to_vec();
        at += len;
        let len = u64::from_le_bytes(take(&mut at, 8).try_into().unwrap()) as usize;
        let end = at + len;
        assert_eq!(end, req.len());
        let mut rows = vec![];
        while at < end {
            let n = take(&mut at, 1)[0] as usize;
            let id: BlockId = std::str::from_utf8(take(&mut at, n))
                .unwrap()
                .parse()
                .unwrap();
            let version = match take(&mut at, 1)[0] {
                0 => None,
                _ => Some(Hash(take(&mut at, 32).try_into().unwrap())),
            };
            rows.push((id, version));
        }
        ViewRequest {
            prelude,
            stream,
            rows,
        }
    }

    /// The request with the state list `rows` in place of its own.
    fn with_rows(&self, rows: &[(BlockId, Option<Hash>)]) -> Vec<u8> {
        let mut out = self.prelude.clone();
        out.extend_from_slice(&self.stream);
        write_state_list(1, rows, &mut out).unwrap();
        out
    }
}

/// The refusal of the render process for `req`, with no output byte written.
fn process_refusal(req: &[u8]) -> RdyError {
    let mut out = Vec::new();
    let e = run_render_process(req, &mut out).unwrap_err();
    assert!(out.is_empty(), "{e}: {} output bytes", out.len());
    e
}

#[test]
fn cra6_view_state_list_refusals_write_no_byte() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let inputs = inputs(&w);
    let h = pushed_draft(&w);
    let snap = h.snapshot().unwrap();
    let scope = view_at("draft", &w.d2);
    let req = request(&snap, &scope, None, &inputs, RenderFormat::Site).unwrap();
    let split = ViewRequest::split(&req);
    // both drafts' doc, section and prose; the request as written renders
    let rows = split.rows.clone();
    assert_eq!(rows.len(), 6, "{rows:?}");
    assert!(rows.iter().all(|(_, v)| v.is_some()), "{rows:?}");
    assert!(rows.windows(2).all(|p| p[0].0 < p[1].0));
    assert!(split.with_rows(&rows) == req);
    let mut packet = Vec::new();
    run_render_process(req.as_slice(), &mut packet).unwrap();
    let fix = "write the request with the adapter's scope render request";
    let refused = |rows: &[(BlockId, Option<Hash>)], want: String| {
        let e = process_refusal(&split.with_rows(rows));
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            ("E_BAD_INPUT", want.as_str(), fix)
        );
    };
    let (first, second) = (rows[0].0.clone(), rows[1].0.clone());
    // a row for a block that no included change touches: canon's block, in its place in order
    let canon: BlockId = {
        let hit = snap
            .search(SearchRequest {
                query: "The hub keeps one canon".into(),
                scope: Some(scope.clone()),
                ..Default::default()
            })
            .unwrap();
        hit["matches"][0]["id"].as_str().unwrap().parse().unwrap()
    };
    let mut extra = rows.clone();
    extra.push((canon.clone(), None));
    extra.sort_by(|a, b| a.0.cmp(&b.0));
    refused(
        &extra,
        format!(
            "render request state list 1 names block {canon}, which no included change touches"
        ),
    );
    // a touched block with no row
    refused(
        &rows[1..],
        format!(
            "render request state list 1 has no row for block {first}, which an included change touches"
        ),
    );
    // two rows for one block
    let mut twice = rows.clone();
    twice.insert(1, rows[0].clone());
    refused(
        &twice,
        format!("render request state list 1 names block {first} twice"),
    );
    // rows out of order
    let mut swapped = rows.clone();
    swapped.swap(0, 1);
    refused(
        &swapped,
        format!("render request state list 1 names block {first} after block {second}"),
    );
    // a version the stream does not hold
    let none = Hash::of(b"none");
    let mut unheld = rows.clone();
    unheld[0].1 = Some(none);
    refused(
        &unheld,
        format!(
            "render request state list 1 gives block {first} version {none}, which stream 1 does not hold"
        ),
    );
    // a state list over the bound, before any byte of it is read
    let mut over = split.prelude.clone();
    over.extend_from_slice(&split.stream);
    over.extend_from_slice(&2_147_483_649u64.to_le_bytes());
    let e = process_refusal(&over);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_LIMIT",
            "render request state list 1 declares 2147483649 bytes, over 2147483648",
            "render a smaller scope"
        )
    );
    // a stream rooted at an op of another view than the prelude's
    let other = String::from_utf8(split.prelude[4..].to_vec())
        .unwrap()
        .replace("view:draft@", "view:other@");
    let mut renamed = (other.len() as u32).to_le_bytes().to_vec();
    renamed.extend_from_slice(other.as_bytes());
    renamed.extend_from_slice(&req[split.prelude.len()..]);
    let e = process_refusal(&renamed);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_BAD_INPUT",
            format!(
                "render request stream 1 has root {} of view \"draft\", not of view \"other\"",
                w.d2
            )
            .as_str(),
            fix
        )
    );
    // the adapter: an op outside the view's chain is the read's own refusal, and a project
    // keeps today's refusal of every view scope
    let none_op = view_at("draft", &none);
    let e = request(&snap, &none_op, None, &inputs, RenderFormat::Site).unwrap_err();
    assert_eq!(
        (e.code, e.message),
        (
            "E_NOT_FOUND",
            format!("view \"draft\" has no op {none} in its chain")
        )
    );
    drop(snap);
    h.close().unwrap();
    let p = StoreHandle::open(&w.p()).unwrap();
    let ps = p.snapshot().unwrap();
    let e = request(&ps, &scope, None, &inputs, RenderFormat::Site).unwrap_err();
    assert_eq!(
        (e.code, e.message),
        (
            "E_BAD_INPUT",
            format!("scope {scope:?} cannot be rendered: use landing:<64 hex digits>")
        )
    );
}
