//! sg9: the disagreements page: "Where the docs disagree", canon only, in plain words
//! (FW-SW2, D-QWSTV).
use rdy_engine::tensions::*;
use rdy_engine::{Rdy, Scope};
use rdy_model::*;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::*;
use rdy_qa::golden::retired_reference;
use rdy_qa::tensions::{created, put, upd};
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
    // the people surface and the agent surface list the same reference pages
    let nav = read(out.path(), "index.html");
    assert!(
        nav.contains("<span class=\"nav-group-label\">Reference</span>")
            && nav.contains("href=\"disagreements.html\">Where the docs disagree</a></li>"),
        "the Reference group of the navigation lists the page: {nav}"
    );
    let llms = read(out.path(), "llms.txt");
    assert!(
        llms.contains("- [Where the docs disagree](disagreements.html)\n"),
        "llms.txt lists the page: {llms}"
    );
}

/// The link texts of the entries on the page, in order, one list per entry.
fn entries(page: &str) -> Vec<Vec<String>> {
    page.split("<div class=\"callout\">")
        .skip(1)
        .map(|e| {
            e.split("<li><a ")
                .skip(1)
                .map(|l| {
                    let text = l.split_once('>').unwrap().1;
                    text.split_once("</a>").unwrap().0.to_string()
                })
                .collect()
        })
        .collect()
}

/// Finding 2 of the round 1 review: the home card adds no text the brief does not give.
#[test]
fn sg9_the_home_card_is_a_title_and_nothing_else() {
    let (_d, r, _s) = retired_reference();
    let out = render(&r, &Scope::Canon);
    let home = read(out.path(), "index.html");
    assert!(
        home.contains(
            "<a class=\"card\" href=\"disagreements.html\"><strong>Where the docs disagree</strong></a>"
        ),
        "{home}"
    );
}

/// Finding 3 of the round 1 review: a sentence that says "this page" is followed by the link of
/// that page first, whatever the reading order of the docs.
#[test]
fn sg9_a_retired_reference_lists_the_pointing_page_first() {
    let (_d, r, _s) = retired_reference();
    let out = render(&r, &Scope::Canon);
    let page = read(out.path(), "disagreements.html");
    let list = entries(&page);
    assert_eq!(list.len(), 1, "{page}");
    assert!(list[0][0].starts_with("Notes"), "the pointing page: {page}");
    assert!(list[0][1].starts_with("Spec"), "the retired target: {page}");
}

#[test]
fn sg9_a_drifted_restatement_lists_the_restating_page_first() {
    let (_d, mut r) = project();
    const SAID: &str =
        "Every job retries for five seconds and then reports a failure to the owner.";
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "jobs", "title": "Jobs", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$x", "parent": "$d", "body": SAID},
            {"op": "create", "type": "doc", "id": "$e", "fields": {"name": "ops", "title": "Ops", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$y", "parent": "$e", "body": format!("In short: {SAID}")}
        ]),
    );
    land(&mut r, "seed").unwrap();
    put(
        &mut r,
        "edit",
        upd(
            &created(&w, "$x"),
            "Every job retries for ten seconds and then reports a failure to the owner.",
        ),
    );
    land(&mut r, "edit").unwrap();
    let out = render(&r, &Scope::Canon);
    let page = read(out.path(), "disagreements.html");
    assert!(
        page.contains("This page restates a point that has since changed."),
        "{page}"
    );
    let list = entries(&page);
    assert_eq!(list.len(), 1, "{page}");
    assert!(list[0][0].starts_with("Ops"), "the restating page: {page}");
    assert!(list[0][1].starts_with("Jobs"), "the edited page: {page}");
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

/// The canon render reads canon's tensions only: on a fresh handle, with a change in flight, a
/// render of canon (and of a tag at canon's head) builds no whole tension set, so the in-flight
/// part is never detected for the page (perf fix of the M2g final review: render_50k).
#[test]
fn sg9_a_canon_render_builds_no_inflight_tension_set() {
    let (d, mut r, s) = retired_reference();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let tag: Hash = r.read().versions().unwrap()[0].landing.parse().unwrap();
    // an in-flight change that also points to the retired section
    write(
        &mut r,
        "later",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "later", "title": "Later", "doc_type": "overview"}},
            {"op": "create", "type": "claim", "parent": "$d", "body": format!("Later work relies on {{{{{}}}}}.", s.c)}
        ]),
    )
    .unwrap();
    drop(r);
    let builds = rdy_engine::tensions::set::full_builds;
    for scope in [Scope::Canon, Scope::At(tag)] {
        let r = Rdy::open(d.path()).unwrap();
        let st = r.read();
        st.materialized(&Scope::Canon).unwrap();
        let before = builds();
        let out = tempfile::tempdir().unwrap();
        rdy_render::render_site(&st, &scope, out.path()).unwrap();
        assert_eq!(
            builds() - before,
            0,
            "{scope:?}: the render built a whole set"
        );
        assert!(
            !st.tensions_built(),
            "{scope:?}: the state's set is unbuilt"
        );
        let page = read(out.path(), "disagreements.html");
        assert_eq!(entries(&page).len(), 1, "{scope:?}: {page}");
        assert!(page.contains(RETIRED), "{scope:?}: {page}");
    }
}
