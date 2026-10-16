//! ls8: a proposal version as a read scope: its base plus its changes, in review mode.
use rdy_engine::search::{SearchFilters, SearchQuery};
use rdy_engine::{Rdy, Scope};
use rdy_qa::fixtures::writer;
use serde_json::{Value, json};

fn proposed() -> (tempfile::TempDir, Rdy, String) {
    let _clock = rdy_qa::golden::fixture_clock();
    let (d, mut r) = rdy_qa::golden::tiny_with_doc();
    rdy_qa::golden::write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "fields": {"title": "Proposed doc", "doc_type": "overview"}}
        ]),
    )
    .unwrap();
    let p = r.propose(&writer(), "w", "Add a doc", None).unwrap();
    let pref = r.resolve_proposal(&p.id).unwrap().to_string();
    (d, r, pref)
}

fn titles(r: &Rdy, scope: &Scope) -> Vec<String> {
    let m = r.read().materialized(scope).unwrap();
    let mut t: Vec<String> = m
        .iter()
        .filter_map(|b| b.field_str("title").map(str::to_string))
        .collect();
    t.sort();
    t
}

#[test]
fn ls8_a_version_reads_its_base_plus_its_changes() {
    let (_d, mut r, pref) = proposed();
    rdy_qa::golden::write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "fields": {"title": "Later doc", "doc_type": "overview"}}
        ]),
    )
    .unwrap();
    // fix round 1: canon moves after the propose; the version keeps reading its own base
    rdy_qa::golden::write(
        &mut r,
        "x",
        json!([
            {"op": "create", "type": "doc", "fields": {"title": "Landed doc", "doc_type": "overview"}}
        ]),
    )
    .unwrap();
    rdy_qa::golden::land(&mut r, "x").unwrap();
    let scope = r.read().resolve_scope(&format!("proposal:{pref}")).unwrap();
    assert!(matches!(scope, Scope::Proposal(_)));
    let t = titles(&r, &scope);
    assert!(t.contains(&"Proposed doc".to_string()), "{t:?}");
    assert!(
        !t.contains(&"Later doc".to_string()),
        "a version never moves: {t:?}"
    );
    assert!(
        !t.contains(&"Landed doc".to_string()),
        "a version reads canon at its base, not canon's head: {t:?}"
    );
    assert!(titles(&r, &Scope::Canon).contains(&"Landed doc".to_string()));
    assert!(titles(&r, &Scope::View("w".into())).contains(&"Later doc".to_string()));
}

#[test]
fn ls8_the_latest_ref_follows_new_versions() {
    let (_d, mut r, pref) = proposed();
    let stable = pref.split('@').next().unwrap().to_string();
    let v1 = r
        .read()
        .resolve_scope(&format!("proposal:{stable}"))
        .unwrap();
    rdy_qa::golden::write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "fields": {"title": "Second version doc", "doc_type": "overview"}}
        ]),
    )
    .unwrap();
    r.propose(&writer(), "w", "Add two docs", None).unwrap();
    let v2 = r
        .read()
        .resolve_scope(&format!("proposal:{stable}"))
        .unwrap();
    assert_ne!(v1, v2);
    assert_eq!(
        r.read().resolve_scope(&format!("proposal:{pref}")).unwrap(),
        v1,
        "@1 stays"
    );
    // a ref is P- and 4 to 64 hex digits (gate.rs:410-418); a non-hex ref is E_BAD_INPUT
    let e = r
        .read()
        .resolve_scope("proposal:P-ffffffffffff@9")
        .unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
}

#[test]
fn ls8_the_position_names_the_proposal() {
    let (_d, r, pref) = proposed();
    let scope = r.read().resolve_scope(&format!("proposal:{pref}")).unwrap();
    let p = r.read().version_position(&scope).unwrap();
    assert!(!p.frozen);
    let v = p.view.expect("a proposal reads as a draft");
    assert_eq!(v.name, "Add a doc");
    assert_eq!(v.unlanded_changes, 1);
}

#[test]
fn ls8_coverage_refuses_a_proposal_version() {
    let (_d, r, pref) = proposed();
    let scope = r.read().resolve_scope(&format!("proposal:{pref}")).unwrap();
    let e = r.read().coverage_scope(&scope).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(
        e.message
            .starts_with("coverage is computed for canon and views, not for proposal:"),
        "{e}"
    );
}

#[test]
fn ls8_a_cursor_reads_back() {
    let (_d, r, pref) = proposed();
    let scope = r.read().resolve_scope(&format!("proposal:{pref}")).unwrap();
    let Scope::Proposal(v) = &scope else {
        panic!("{scope:?}")
    };
    assert_eq!(
        r.read().resolve_scope(&format!("proposal:{v}")).unwrap(),
        scope,
        "a hash resolves too"
    );
}

/// Fix round 1: doc `proposed` (one prose block) written to view `w` of an empty project and
/// proposed, so the version's base is the empty canon. Returns the version's scope.
fn first_proposal() -> (tempfile::TempDir, Rdy, Scope) {
    let _clock = rdy_qa::golden::fixture_clock();
    let (d, mut r) = rdy_qa::golden::tiny();
    rdy_qa::golden::write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "proposed", "title": "Proposed doc", "doc_type": "overview"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "Before anything lands."}
        ]),
    )
    .unwrap();
    let p = r.propose(&writer(), "w", "Add a doc", None).unwrap();
    let scope = r
        .read()
        .resolve_scope(&format!("proposal:{}", p.id))
        .unwrap();
    (d, r, scope)
}

fn render(r: &Rdy, scope: &Scope) -> tempfile::TempDir {
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), scope, out.path()).unwrap();
    out
}

const NEW: &str = "<span class=\"chg chg-label chg-new\">New</span>";

/// The task-8 ruling (D-N5NSN, "in review mode"): a version made before the first landing is
/// based on the empty canon, which is canon while nothing has landed. So it renders in review
/// mode, as a view of that project does.
#[test]
fn ls8_a_first_proposal_before_any_landing_is_reviewed() {
    let (_d, r, scope) = first_proposal();
    let Scope::Proposal(v) = &scope else {
        panic!("{scope:?}")
    };
    assert_eq!(r.read().proposal_version(v).unwrap().canon_at, None);
    assert_eq!(r.read().canon_head(), None);
    assert_eq!(
        rdy_render::review::base_of(&r.read(), &scope),
        Some(Scope::Canon)
    );
    let out = render(&r, &scope);
    let doc = rdy_qa::golden::html(out.path(), "proposed.html");
    assert!(doc.contains("id=\"show-changes\""), "{doc}");
    assert!(doc.contains(NEW), "the proposed doc is new: {doc}");
    let index = rdy_qa::golden::html(out.path(), "index.html");
    assert!(index.contains("Changes in this proposal"), "{index}");
}

/// The task-8 ruling, rule 2 (guard): once canon has landed, a version made before the first
/// landing has a base that no scope names (the empty project). It renders without review mode,
/// and its label still names the draft.
#[test]
fn ls8_a_pre_landing_version_after_a_landing_renders_without_review() {
    let (_d, mut r, scope) = first_proposal();
    rdy_qa::golden::write(
        &mut r,
        "x",
        json!([
            {"op": "create", "type": "doc", "fields": {"name": "landed", "title": "Landed doc", "doc_type": "overview"}}
        ]),
    )
    .unwrap();
    rdy_qa::golden::land(&mut r, "x").unwrap();
    assert!(r.read().canon_head().is_some());
    assert_eq!(rdy_render::review::base_of(&r.read(), &scope), None);
    let out = render(&r, &scope);
    let doc = rdy_qa::golden::html(out.path(), "proposed.html");
    assert!(!doc.contains("show-changes"), "{doc}");
    assert!(!doc.contains("chg-label"), "{doc}");
    assert!(
        !out.path().join("landed.html").exists(),
        "the version still reads its own base"
    );
    let p = r.read().version_position(&scope).unwrap();
    assert_eq!(p.view.expect("the label names the draft").name, "Add a doc");
}

/// Doc `ops` with term Ledger and a claim that names it, proposed from view `w` on a landed
/// project. Returns the version's scope and the claim's id.
fn ledger_proposal() -> (tempfile::TempDir, Rdy, Scope, String) {
    let (d, mut r, _pref) = proposed();
    let w = rdy_qa::golden::write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary"}},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "The Ledger is append-only."}
        ]),
    )
    .unwrap();
    let p = r.propose(&writer(), "w", "Add the ledger", None).unwrap();
    let scope = r
        .read()
        .resolve_scope(&format!("proposal:{}", p.id))
        .unwrap();
    (d, r, scope, w.created["$c"].clone())
}

fn query(text: &str, filters: Value) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        filters: serde_json::from_value::<SearchFilters>(filters).unwrap(),
        ..Default::default()
    }
}

/// Fix round 1 (cc6, FW-H7): a proposal version has no coverage, as a past landing has none.
/// Search hits, fact sheets and briefings leave it out silently: no coverage and no
/// `coverage_error`.
#[test]
fn ls8_a_proposal_version_leaves_coverage_out() {
    let (_d, r, scope, claim) = ledger_proposal();
    let res = r
        .read()
        .search(&scope, &query("Ledger", json!({})), None, Some(4000))
        .unwrap();
    let res = serde_json::to_value(res).unwrap();
    assert!(res.get("coverage_error").is_none(), "{res}");
    let hits = res["matches"].as_array().unwrap();
    assert!(!hits.is_empty(), "{res}");
    for h in hits {
        assert!(h.get("coverage").is_none(), "{h}");
    }
    let sheet = &res["entities"][0];
    assert!(sheet.is_object(), "{res}");
    assert!(sheet.get("confidence").is_none(), "{sheet}");
    assert!(sheet.get("coverage_error").is_none(), "{sheet}");
    let cov = rdy_engine::brief::Coverage {
        ids: vec![claim],
        path: None,
    };
    let b = serde_json::to_value(r.read().brief(&scope, &cov, None).unwrap()).unwrap();
    assert!(b.get("coverage").is_none(), "{b}");
    assert!(b.get("coverage_error").is_none(), "{b}");
    assert!(
        b["base"].is_object(),
        "the briefing itself still works: {b}"
    );
}

/// Fix round 1 (cc6): a coverage filter or sort in a proposal version is refused, as at a past
/// landing.
#[test]
fn ls8_a_coverage_filter_in_a_proposal_version_is_refused() {
    let (_d, r, scope, _claim) = ledger_proposal();
    for filters in [json!({"coverage_below": 60}), json!({"sort": "coverage"})] {
        let e = r
            .read()
            .search(&scope, &query("Ledger", filters.clone()), None, Some(4000))
            .unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{filters}: {e}");
        assert!(
            e.message
                .starts_with("coverage is computed for canon and views, not for proposal:"),
            "{e}"
        );
    }
}

/// Fix round 1: `rdy_overview` takes the proposal scope its help lists. It returns the version
/// position and leaves coverage out silently.
#[test]
fn ls8_the_overview_reads_a_proposal_version() {
    use rdy_mcp::tools::{McpState, rdy_overview, rdy_session};
    let (_d, r, pref) = proposed();
    let mut st = McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:a", "run": "a1", "role": "reader"})).unwrap(),
    )
    .unwrap();
    let v = rdy_overview(
        &mut st,
        serde_json::from_value(json!({"scope": format!("proposal:{pref}")})).unwrap(),
    )
    .unwrap();
    assert_eq!(v["version_position"]["view"]["name"], "Add a doc", "{v}");
    assert_eq!(v["version_position"]["view"]["unlanded_changes"], 1, "{v}");
    assert!(v.get("coverage").is_none(), "{v}");
    assert!(v.get("coverage_error").is_none(), "{v}");
}
