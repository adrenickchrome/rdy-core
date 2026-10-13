//! sg9: the disagreements page: "Where the docs disagree", canon only, in plain words
//! (FW-SW2, D-QWSTV).
use rdy_engine::tensions::*;
use rdy_engine::{Rdy, Scope};
use rdy_model::*;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::*;
use rdy_qa::golden::retired_reference;
use serde_json::json;
use std::path::Path;

const TITLE: &str = "Where the docs disagree";
const RETIRED: &str = "This page still points to something that was retired.";
const CONTRADICT: &str = "A reviewer found that these statements contradict each other.";

/// Two landed claims of `Spec` with a judged contradiction (a `tension` verdict on the pair).
pub fn judged_contradiction() -> (tempfile::TempDir, Rdy, Spec) {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    let content = |r: &Rdy, id: &BlockId| {
        let m = r.read().materialized(&Scope::Canon).unwrap();
        m.get(id).unwrap().content_hash_with(r.store().tracking())
    };
    let mut items = vec![
        PairItem {
            block: s.a.clone(),
            content: content(&r, &s.a),
        },
        PairItem {
            block: s.c.clone(),
            content: content(&r, &s.c),
        },
    ];
    items.sort();
    let provenance = Provenance {
        actor: "agent:r".into(),
        run: Some("r".into()),
        role: Role::Reviewer,
        at_ms: 1,
    };
    let review = Review {
        subject: ReviewSubject::Pair {
            key: pair_key(&items),
            items: items.clone(),
        },
        case_hash: Some(Hash([9; 32])),
        verdict: Verdict::Tension,
        reason: String::new(),
        findings: vec![],
        provenance: provenance.clone(),
    };
    let tension = SemanticTension {
        key: pair_key(&items),
        items,
        review: Hash::of(&postcard::to_allocvec(&review).unwrap()),
        advice: Some("A and C cannot both hold".into()),
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance,
    };
    r.record_verdict(&review, Some(&tension)).unwrap();
    (d, r, s)
}

fn render(r: &Rdy, scope: &Scope) -> tempfile::TempDir {
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), scope, out.path()).unwrap();
    out
}

fn read(site: &Path, page: &str) -> String {
    std::fs::read_to_string(site.join(page)).unwrap_or_else(|e| panic!("{page}: {e}"))
}

#[test]
fn sg9_a_canon_with_a_retired_reference_renders_the_page() {
    let (_d, r, _s) = retired_reference();
    let out = render(&r, &Scope::Canon);
    let page = read(out.path(), "disagreements.html");
    assert!(page.contains(TITLE), "{page}");
    assert!(page.contains(RETIRED), "{page}");
    assert!(
        page.contains("href=\"spec.html#") && page.contains("href=\"notes.html#"),
        "the page links to both pages: {page}"
    );
    assert!(
        read(out.path(), "index.html").contains("href=\"disagreements.html\""),
        "home links to the page"
    );
}

#[test]
fn sg9_no_disagreement_no_page() {
    let (_d, mut r) = project();
    spec_doc(&mut r);
    let out = render(&r, &Scope::Canon);
    assert!(!out.path().join("disagreements.html").exists());
    assert!(!read(out.path(), "index.html").contains("disagreements.html"));
}

#[test]
fn sg9_a_view_render_has_no_page() {
    let (_d, r, _s) = retired_reference();
    let out = render(&r, &Scope::View("notes".into()));
    assert!(!out.path().join("disagreements.html").exists());
    assert!(!read(out.path(), "index.html").contains("disagreements.html"));
}

/// RULING (founder, 2026-10-12): a tag at canon's head carries the page; a tag at a past
/// landing carries none.
#[test]
fn sg9_a_past_tag_render_has_no_page() {
    let (_d, mut r, s) = retired_reference();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let tag: Hash = r.read().versions().unwrap()[0].landing.parse().unwrap();
    let at_head = render(&r, &Scope::At(tag));
    assert!(
        at_head.path().join("disagreements.html").exists(),
        "a tag at canon's head has the page"
    );
    write(
        &mut r,
        "more",
        json!([{"op": "update", "id": s.c.to_string(), "body": "C stands alone, still."}]),
    )
    .unwrap();
    land(&mut r, "more").unwrap();
    assert_ne!(r.read().canon_head(), Some(tag), "guard: canon moved on");
    let past = render(&r, &Scope::At(tag));
    assert!(!past.path().join("disagreements.html").exists());
    assert!(!read(past.path(), "index.html").contains("disagreements.html"));
}

#[test]
fn sg9_a_semantic_tension_in_canon_is_listed() {
    let (_d, r, _s) = judged_contradiction();
    let out = render(&r, &Scope::Canon);
    let page = read(out.path(), "disagreements.html");
    assert!(page.contains(CONTRADICT), "{page}");
    assert!(
        !page.contains("A and C cannot both hold"),
        "the reviewer's advice is not shown: {page}"
    );
}

#[test]
fn sg9_a_dismissed_tension_is_not_listed() {
    let (_d, mut r, _s) = retired_reference();
    let t = {
        let set = r.read().tensions().unwrap();
        set.iter()
            .find(|t| t.kind == TensionKind::DeprecatedReference)
            .cloned()
            .expect("guard: the canon holds the tension")
    };
    r.dismiss_tension(&writer(), &tension_label(&t.id), "fine as it is")
        .unwrap();
    let out = render(&r, &Scope::Canon);
    assert!(!out.path().join("disagreements.html").exists());
    assert!(!read(out.path(), "index.html").contains("disagreements.html"));
}

#[test]
fn sg9_the_page_passes_the_leak_scan() {
    for (name, (_d, r, _s)) in [
        ("retired", retired_reference()),
        ("judged", judged_contradiction()),
    ] {
        let out = render(&r, &Scope::Canon);
        assert!(out.path().join("disagreements.html").exists(), "{name}");
        let leaks = rdy_qa::site::leaks(out.path());
        assert!(leaks.is_empty(), "{name}: {leaks:#?}");
        let page = read(out.path(), "disagreements.html").to_lowercase();
        for word in [
            "tension",
            "deprecated_reference",
            "semantic",
            "advisory",
            "blocking",
        ] {
            assert!(!page.contains(word), "{name}: {word}");
        }
    }
}

#[test]
fn sg9_the_closed_table_has_six_sentences_and_no_other() {
    for kind in TensionKind::ALL {
        let said = rdy_render::disagree::sentence(kind.name());
        let listed = matches!(
            kind,
            TensionKind::Fork
                | TensionKind::DuplicateDefinition
                | TensionKind::DeprecatedReference
                | TensionKind::ValueDisagreement
                | TensionKind::RestatementDrift
                | TensionKind::Semantic
        );
        assert_eq!(said.is_some(), listed, "{kind}");
    }
    assert_eq!(rdy_render::disagree::sentence("nonsense"), None);
}
