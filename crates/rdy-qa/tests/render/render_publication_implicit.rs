//! pub12 (D-48NYM, D-0VCQ5, D-5CVWB; rdyhub D-45W02 FW-PUB3; RULINGS F2, F3, F5; FW-PB9a): the
//! implicit profile `implicit:org` selects every block and then follows the publication rules of
//! a named profile on every output: every doc page, the chosen tag's name alone of the history,
//! no profile block, the trail of an org audience, and the project's own name as the title.
//! Without a profile, the site stays the full source (F2).
use rdy_engine::{E_BAD_INPUT, Rdy, ReadState, Scope};
use rdy_model::BlockId;
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::golden::tiny;
use rdy_render::{Mode, SiteFiles, build_profile_site, build_site, print};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const IMPLICIT: &str = "implicit:org";

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Writes `ops` to `view` as the writer; panics on a refusal.
fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, BlockId> {
    let w = write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e}"));
    w.created.iter().map(|(k, v)| (k.clone(), bid(v))).collect()
}

fn page(s: &SiteFiles, path: &str) -> String {
    String::from_utf8(
        s.files
            .get(path)
            .unwrap_or_else(|| panic!("no file {path}: {:?}", s.files.keys()))
            .clone(),
    )
    .unwrap()
}

/// Every path whose name or bytes hold `word`, but the theme's own assets (`assets/site.css`,
/// `assets/app.js`...), whose code may use a word a page must not.
fn holding_pages(s: &SiteFiles, word: &str) -> Vec<String> {
    let w = word.as_bytes();
    s.files
        .iter()
        .filter(|(p, _)| !p.starts_with("assets/"))
        .filter(|(p, b)| {
            p.as_bytes().windows(w.len()).any(|x| x == w) || b.windows(w.len()).any(|x| x == w)
        })
        .map(|(p, _)| p.clone())
        .collect()
}

#[track_caller]
fn assert_absent_pages(s: &SiteFiles, word: &str) {
    assert_eq!(holding_pages(s, word), Vec::<String>::new(), "{word}");
}

/// `Tiny` with a folder `area` holding the doc `governed` (a block a decision of the doc
/// `rulings` affects), an `admin` doc holding two profiles (titles and keys are canaries), a tag
/// `v1.0.0` with a message canary, and a later landing that adds the doc `later`.
fn world() -> (tempfile::TempDir, Rdy) {
    let (dir, mut r) = tiny();
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area", "title": "Area title"}},
            {"op": "create", "type": "doc", "id": "$gov", "parent": "$area", "fields": {"name": "governed", "title": "Governed page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$gs", "parent": "$gov", "fields": {"title": "Governed section"}},
            {"op": "create", "type": "prose", "id": "$gp", "parent": "$gs", "body": "Governed words."},
            {"op": "create", "type": "doc", "id": "$rul", "fields": {"name": "rulings", "title": "Rulings page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$rs", "parent": "$rul", "fields": {"title": "Rulings section"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    r.decide(
        &writer(),
        "w1",
        rdy_engine::DecisionInput {
            parent: ids["$rs"].to_string(),
            statement: "TRAILCANARY ruling.".into(),
            affects: vec![ids["$gp"].to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "w1").unwrap();
    put(
        &mut r,
        "w2",
        json!([
            {"op": "create", "type": "publication_profile", "parent": ids["$admin"].to_string(),
             "fields": {"key": "pfcanaryorg", "title": "PROFILECANARY org", "description": "PROFILECANARY org words.",
                "audience": "org", "select": [ids["$gov"].to_string()]}},
            {"op": "create", "type": "publication_profile", "parent": ids["$admin"].to_string(),
             "fields": {"key": "pfcanarypub", "title": "PROFILECANARY public", "audience": "public",
                "select": [ids["$rul"].to_string()]}}
        ]),
    );
    land(&mut r, "w2").unwrap();
    r.tag(&approver(), "v0.9.0", None, Some("TAGMSGCANARY old".into()))
        .unwrap();
    put(
        &mut r,
        "w3",
        json!([{"op": "update", "id": ids["$gp"].to_string(), "body": "Governed words, tagged."}]),
    );
    land(&mut r, "w3").unwrap();
    r.tag(&approver(), "v1.0.0", None, Some("TAGMSGCANARY one".into()))
        .unwrap();
    put(
        &mut r,
        "w4",
        json!([{"op": "create", "type": "doc", "fields": {"name": "later", "title": "Later page", "doc_type": "page"}}]),
    );
    land(&mut r, "w4").unwrap();
    (dir, r)
}

/// The doc pages of a source site: the page of every doc of `scope`, by the source site's paths.
fn doc_pages(st: &ReadState, scope: &Scope) -> BTreeSet<String> {
    let m = st.materialized(scope).unwrap();
    let site = build_site(st, scope, Mode::Static).unwrap();
    let mut out = BTreeSet::new();
    for b in m.iter().filter(|b| b.block_type == rdy_model::tree::DOC) {
        let title = b.field_str("title").unwrap().to_string();
        let found: Vec<&String> = site
            .files
            .iter()
            .filter(|(p, bytes)| {
                p.ends_with(".html")
                    && String::from_utf8_lossy(bytes).contains(&format!("<h1>{title}</h1>"))
            })
            .map(|(p, _)| p)
            .collect();
        assert_eq!(found.len(), 1, "{title}: {found:?}");
        out.insert(found[0].clone());
    }
    out
}

#[test]
fn pub12_implicit_outputs_follow_the_publication_rules() {
    let (_dir, r) = world();
    let st = r.read();
    let tag = st.resolve_scope("tag:v1.0.0").unwrap();
    let s = build_profile_site(&st, "tag:v1.0.0", IMPLICIT).unwrap();

    // every doc page of the tag, and the tag's docs alone
    let docs = doc_pages(&st, &tag);
    assert_eq!(docs.len(), 3, "{docs:?}");
    for p in &docs {
        assert!(s.files.contains_key(p), "{p}: {:?}", s.files.keys());
    }
    assert!(page(&s, "area/governed.html").contains("Governed words, tagged."));
    assert_absent_pages(&s, "Later page");

    // the chosen tag's name (F3), and nothing else of the history
    let home = page(&s, "index.html");
    assert!(
        home.contains("<span class=\"pill-text\">v1.0.0</span>"),
        "{home}"
    );
    assert!(
        home.contains("A snapshot of Tiny at <span data-rdy-label>v1.0.0</span>."),
        "{home}"
    );
    for word in [
        "TAGMSGCANARY",
        "v0.9.0",
        "#versions",
        "id=\"versions\"",
        "Versions",
        "No versions yet",
        "changes since",
        "changes-since",
        "Changes in this",
        "review-changes",
        "<a class=\"pill\"",
    ] {
        assert_absent_pages(&s, word);
    }
    // no profile block: neither profile's fields reach any output
    for word in ["PROFILECANARY", "pfcanaryorg", "pfcanarypub"] {
        assert_absent_pages(&s, word);
    }
    // the trail of an org audience: the decision's text on the governed block
    let gov = page(&s, "area/governed.html");
    let trail = gov.find("data-rdy=\"trail\"").expect("the trail");
    assert!(gov[trail..].contains("TRAILCANARY ruling."), "{gov}");
    // the site's title is the project's name
    assert!(home.contains("<title>Tiny</title>"), "{home}");
    assert!(
        home.contains("<span class=\"brand-name\">Tiny</span>"),
        "{home}"
    );
    assert!(
        page(&s, "llms.txt").starts_with("# Tiny\n"),
        "{}",
        page(&s, "llms.txt")
    );
    // a public implicit profile, which would hide the trail, cannot be requested (FW-PB9a)
    let e = build_profile_site(&st, "tag:v1.0.0", "implicit:public")
        .err()
        .expect("refused");
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_BAD_INPUT,
            "the implicit profile is always org (FW-PB9a)",
            "name an explicit publication profile whose audience is public"
        )
    );

    // the print: the same rules, the project's name, the tag's name as its only label
    let p = print::build_profile(&st, "tag:v1.0.0", IMPLICIT).unwrap();
    assert_eq!(p.title, "Tiny");
    assert_eq!(p.description, st.project_description());
    assert_eq!(p.label, "v1.0.0");
    assert_eq!(p.scope, "");
    let pages: BTreeSet<String> = p.docs.iter().map(|d| d.page.clone()).collect();
    for d in &docs {
        assert!(pages.contains(d), "{d}: {pages:?}");
    }
    let dump = format!("{p:?}");
    for word in [
        "TAGMSGCANARY",
        "v0.9.0",
        "PROFILECANARY",
        "pfcanaryorg",
        "Later page",
    ] {
        assert!(!dump.contains(word), "{word}");
    }
    assert!(dump.contains("Governed words, tagged."));
    let e = print::build_profile(&st, "tag:v1.0.0", "implicit:public").unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);

    // the same at canon: the later doc appears, and no label
    let c = build_profile_site(&st, "canon", IMPLICIT).unwrap();
    assert!(!holding_pages(&c, "Later page").is_empty());
    assert_absent_pages(&c, "data-rdy-label");
    let home = page(&c, "index.html");
    assert!(home.contains("<p class=\"colophon\">Tiny.</p>"), "{home}");
    // the same request gives the same bytes
    assert_eq!(
        build_profile_site(&st, "tag:v1.0.0", IMPLICIT)
            .unwrap()
            .files,
        s.files
    );
}

#[test]
fn pub12_unprofiled_outputs_stay_full_source() {
    let (_dir, r) = world();
    let st = r.read();
    let full = build_site(&st, &Scope::Canon, Mode::Static).unwrap();
    // every source version and its message are listed, as before (F2)
    let all: String = full
        .files
        .values()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .collect();
    for word in ["v0.9.0", "v1.0.0", "TAGMSGCANARY old", "TAGMSGCANARY one"] {
        assert!(all.contains(word), "{word}");
    }
    // the source build is unchanged by the implicit profile's existence: built again, same bytes
    assert_eq!(
        build_site(&st, &Scope::Canon, Mode::Static).unwrap().files,
        full.files
    );
    // its file map keeps the source pages: the history page the implicit one leaves out
    let implicit = build_profile_site(&st, "canon", IMPLICIT).unwrap();
    let full_keys: BTreeSet<&String> = full.files.keys().collect();
    let implicit_keys: BTreeSet<&String> = implicit.files.keys().collect();
    assert_ne!(full_keys, implicit_keys);
    assert!(implicit_keys.is_subset(&full_keys), "{implicit_keys:?}");
    assert_eq!(
        full_keys.difference(&implicit_keys).collect::<Vec<_>>(),
        vec![&&"changes-since.html".to_string()]
    );
    assert_ne!(full.files, implicit.files);
    assert!(holding_pages(&implicit, "TAGMSGCANARY").is_empty());
    // the source print stays the full source too
    let p = print::build(&st, &Scope::Canon).unwrap();
    assert_eq!(p.scope, "canon");
}

/// Fix round 1: canon's next landing holds a snapshot that gives the doc `kept` an absent parent. The
/// implicit profile refuses it with the named profile's tree error on the site and on print;
/// neither output is built without `kept`.
#[test]
fn pub12_implicit_outputs_refuse_a_stored_orphan_doc() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let ids = put(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "kept", "title": "ORPHANDOC", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (id, admin) = (ids["$doc"].clone(), ids["$admin"].clone());
    put(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": id.to_string(), "body": "ORPHANWORDS"},
            {"op": "create", "type": "publication_profile", "parent": admin.to_string(),
             "fields": {"key": "site", "title": "Site", "audience": "org", "select": [id.to_string()]}}
        ]),
    );
    land(&mut r, "work").unwrap();
    // the valid canon builds both outputs with the doc
    {
        let st = r.read();
        let s = build_profile_site(&st, "canon", IMPLICIT).unwrap();
        assert!(page(&s, "kept.html").contains("ORPHANWORDS"));
        print::build_profile(&st, "canon", IMPLICIT).unwrap();
    }
    let head = r.read().canon_head().unwrap();
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let mut snapshot = m.snapshot();
    let mut orphan = m.get(&id).unwrap().clone();
    orphan.parent = Some(bid("fd-G0NE1"));
    let hash = r.store_mut().put(rdy_store::kinds::BLOCK, &orphan).unwrap();
    snapshot.blocks.insert(id.clone(), hash);
    let state = r
        .store_mut()
        .put(rdy_store::kinds::SNAPSHOT, &snapshot)
        .unwrap();
    let mut landing = r.store().landing(&head).unwrap();
    landing.state = state;
    landing.prev = Some(head);
    let damaged = r
        .store_mut()
        .put(rdy_store::kinds::LANDING, &landing)
        .unwrap();
    // the damaged landing becomes the next landing of canon
    r.store_mut()
        .set_refs(vec![("canon".into(), damaged.0.to_vec())])
        .unwrap();
    r.invalidate();
    let st = r.read();
    assert_eq!(st.canon_head(), Some(damaged));
    let scope = format!("landing:{damaged}");
    let tree_error = |e: rdy_engine::RdyError| {
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_BAD_INPUT,
                "Publication selection has an invalid document tree.",
                "Run rdy fsck and fix the document tree."
            ),
            "{e}"
        );
    };
    for name in ["site", IMPLICIT] {
        tree_error(
            build_profile_site(&st, &scope, name)
                .err()
                .unwrap_or_else(|| panic!("{name}: the site omitted the orphan doc")),
        );
        tree_error(
            print::build_profile(&st, &scope, name)
                .err()
                .unwrap_or_else(|| panic!("{name}: the print omitted the orphan doc")),
        );
    }
}
