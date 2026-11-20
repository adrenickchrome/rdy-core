//! D-CST6K through the adapter: a render request of a landing the holder reaches (D-Y32MN), alone
//! or as a review pair against a second landing. Each side travels as its own complete canon
//! history stream; the render process builds the state at `scope`, with core's review marks
//! against `against` when the request names one. A request with no scope is today's tag request.

use crate::hub_world::{AT, World, check, doc, land, moved, project, seed, tag, world};
use rdy_adapter::rendering::{
    PublicationArtifact, RenderFormat, RenderInputs, ScopeRender, read_render_artifact,
    run_render_process,
};
use rdy_adapter::stream::ObjectKey;
use rdy_adapter::sync::CanonMove;
use rdy_adapter::{Hash, RdyError, Snapshot, StoreHandle};
use rdy_engine::publication::{IMPLICIT_ORG, ReadPublicationSource};
use rdy_engine::{Rdy, Scope};
use rdy_render::build_review_publication;
use rdy_render::render_input::capture_inputs;
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
