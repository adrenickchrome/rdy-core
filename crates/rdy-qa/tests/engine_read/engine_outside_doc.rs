//! ndh3 (plan Decisions need no doc, Task 3; D-P3YD5): the reads of a block outside a doc. A
//! decision or a question at the root has no doc and no path; one under a folder has no doc and
//! takes the folder's path. Context, search, the briefing and the batch card read that as `None`;
//! the `path` filters of search and coverage keep a block under the folder, and coverage counts
//! its issues toward the areas and the corpus, never a page.
use rdy_engine::lexicon::NameKind;
use rdy_engine::pending::ItemKind;
use rdy_engine::search::SearchFilters;
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_qa::golden::{fixture_clock, write_as};
use rdy_qa::intake::{TEST_SUMMARY, pending};
use serde_json::json;
use std::collections::BTreeMap;

/// A project with no home. In `work`: folder `area` holding doc `guide` (an overview with section
/// `Rules` and claim `$c`); decision `$dr` at the root (key DR-1); decision `$df` under `area`
/// (key DF-1); `$c` decided by `$dr`; question `$qr` at the root and `$qf` under `area`. Landed.
fn outside() -> (tempfile::TempDir, Rdy, BTreeMap<String, String>) {
    let _clock = fixture_clock();
    let (d, mut r) = project();
    let w = write_as(&approver(), &mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "area", "title": "Area"}},
        {"op": "create", "type": "doc", "id": "$g", "parent": "$f", "fields": {"name": "guide", "title": "Guide", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$g", "fields": {"title": "Rules"}},
        {"op": "create", "type": "decision", "id": "$dr",
         "fields": {"key": "DR-1", "statement": "Keys are lowercase.", "status": "proposed"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Keys come from paths.",
         "links": {"decided_by": [{"target": "$dr"}]}},
        {"op": "create", "type": "decision", "id": "$df", "parent": "$f",
         "fields": {"key": "DF-1", "statement": "Keys are short.", "status": "proposed"}},
        {"op": "create", "type": "question", "id": "$qr", "fields": {"prompt": "Root question?"}},
        {"op": "create", "type": "question", "id": "$qf", "parent": "$f", "fields": {"prompt": "Folder question?"}}
    ]))
    .unwrap();
    let ids = w.created;
    land(&mut r, "work").unwrap();
    (d, r, ids)
}

fn id(ids: &BTreeMap<String, String>, k: &str) -> BlockId {
    BlockId::parse(&ids[k]).unwrap()
}

fn search_of(r: &Rdy, types: &[&str], path: Option<&str>) -> SearchResult {
    let q = SearchQuery {
        text: ".".into(),
        regex: true,
        ignore_case: false,
        filters: SearchFilters {
            types: types.iter().map(|t| t.to_string()).collect(),
            path: path.map(str::to_string),
            ..Default::default()
        },
    };
    r.read().search(&Scope::Canon, &q, None, None).unwrap()
}

#[test]
fn ndh3_context_gives_no_doc_for_a_block_outside_a_doc() {
    let (_d, r, ids) = outside();
    let st = r.read();
    let dr = st.context(&Scope::Canon, &id(&ids, "$dr")).unwrap();
    assert_eq!((dr.doc, dr.parent), (None, None));
    let df = st.context(&Scope::Canon, &id(&ids, "$df")).unwrap();
    assert_eq!(df.doc, None);
    assert_eq!(df.parent, Some(ids["$f"].clone()));
    let c = st.context(&Scope::Canon, &id(&ids, "$c")).unwrap();
    assert_eq!(c.doc, Some(ids["$g"].clone()));
}

#[test]
fn ndh3_search_and_fact_sheets_give_the_folder_path_or_none() {
    let (_d, mut r, ids) = outside();
    let res = search_of(&r, &["decision"], None);
    let mut got: Vec<(String, Option<String>)> = res
        .matches
        .iter()
        .map(|m| (m.id.clone(), m.path.clone()))
        .collect();
    got.sort();
    let mut want = vec![
        (ids["$dr"].clone(), None),
        (ids["$df"].clone(), Some("area".to_string())),
    ];
    want.sort();
    assert_eq!(got, want);
    let mut totals: Vec<(Option<String>, usize)> = res
        .totals
        .docs
        .iter()
        .map(|d| (d.path.clone(), d.hits))
        .collect();
    totals.sort();
    assert_eq!(totals, vec![(None, 1), (Some("area".to_string()), 1)]);
    // a decision is no entity, so it has no sheet (D-P3YD5 keeps the four types outside a doc)
    let e = r
        .read()
        .fact_sheet(&Scope::Canon, &id(&ids, "$df"), NameKind::Key)
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    // the sheet reads the same `Option`: an entity that a store holds at the root or under the
    // folder (written past the rules, as an imported tree can hold it) gives none or the path
    let folder = Some(ids["$f"].as_str());
    let fields = [("name", "Rooted"), ("kind", "service")];
    raw_commit(
        &mut r,
        "raw",
        vec![
            raw(
                "e-AAAAA",
                "entity",
                folder,
                "V",
                &fields,
                "Under the folder.",
            ),
            raw("e-BBBBB", "entity", None, "W", &fields, "At the root."),
        ],
    );
    let sheet = |k: &str| {
        r.read()
            .fact_sheet(
                &Scope::View("raw".into()),
                &BlockId::parse(k).unwrap(),
                NameKind::Name,
            )
            .unwrap()
    };
    assert_eq!(sheet("e-AAAAA").path, Some("area".to_string()));
    assert_eq!(sheet("e-BBBBB").path, None);
}

#[test]
fn ndh3_path_filters_keep_a_block_under_the_folder() {
    let (_d, r, ids) = outside();
    let types = ["decision", "question"];
    let mut got: Vec<String> = search_of(&r, &types, Some("area"))
        .matches
        .iter()
        .map(|m| m.id.clone())
        .collect();
    got.sort();
    let mut want = vec![ids["$df"].clone(), ids["$qf"].clone()];
    want.sort();
    assert_eq!(got, want);
    assert!(search_of(&r, &types, Some("area/guide")).matches.is_empty());

    let st = r.read();
    let open_questions = |rep: &coverage::CoverageReport| -> Vec<String> {
        let mut v: Vec<String> = rep
            .issues
            .iter()
            .filter(|i| i.code == IssueCode::OpenQuestion)
            .map(|i| i.block.clone())
            .collect();
        v.sort();
        v
    };
    let under = st.coverage_report(&Scope::Canon, Some("area")).unwrap();
    assert_eq!(open_questions(&under), vec![ids["$qf"].clone()]);
    let whole = st.coverage_report(&Scope::Canon, None).unwrap();
    let mut both = vec![ids["$qf"].clone(), ids["$qr"].clone()];
    both.sort();
    assert_eq!(open_questions(&whole), both);
    for rep in [&under, &whole] {
        for p in &rep.pages {
            for k in ["$dr", "$df", "$qr", "$qf"] {
                assert_ne!(p.doc, ids[k], "{k} is a page: {:?}", rep.pages);
            }
        }
    }
}

#[test]
fn ndh3_briefings_count_no_doc_for_them() {
    let (_d, r, ids) = outside();
    let cov = |ks: &[&str]| Coverage {
        ids: ks.iter().map(|k| ids[*k].clone()).collect(),
        path: None,
    };
    let b = r
        .read()
        .brief(&Scope::Canon, &cov(&["$dr", "$qr"]), None)
        .unwrap();
    assert!(b.docs.is_empty(), "{:?}", b.docs);
    let pages = b.coverage.expect("coverage").pages;
    assert!(pages.is_empty(), "{pages:?}");
    let b = r.read().brief(&Scope::Canon, &cov(&["$c"]), None).unwrap();
    let pages = b.coverage.expect("coverage").pages;
    assert_eq!(pages.len(), 1, "{pages:?}");
    assert_eq!(pages[0].doc, ids["$g"]);
}

#[test]
fn ndh3_the_changes_fact_counts_no_doc() {
    let (_d, mut r, _ids) = outside();
    write_as(
        &approver(),
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "question", "fields": {"prompt": "Another root question?"}}
        ]),
    )
    .unwrap();
    r.propose(&writer(), "w", "w", Some(TEST_SUMMARY)).unwrap();
    let q = pending(&r);
    let card: Vec<_> = q
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Approve)
        .collect();
    assert_eq!(card.len(), 1, "{:#?}", q.items);
    let changes = card[0].facts.iter().find(|f| f.name == "Changes").unwrap();
    assert_eq!(changes.value, "0 docs, 1 block");
}
