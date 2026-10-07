//! sflt1: search filters (FW-PR1a, FW-SR4), AND-combined: types, traits, path, lifecycle,
//! changed_since, governed_by, describes, linked_to, suspect; history on matches inside a
//! superseded, rejected or revoked decision, and exclude_history.
use rdy_engine::search::SearchFilters;
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::golden::write_as;
use serde_json::json;
use std::collections::BTreeMap;

/// Folder guides / doc alpha / section A (stable): claim ac, code ak. Doc beta / section B
/// (draft): claim bg, decision d (affects bg), rejected decision r, term t, claim bc (decided_by
/// d), claim bd (describes t), claim be (references t), claim bf (implements d). Each says
/// "needle". Landed. Returns the ids by placeholder, and the landing id under "landing".
fn corpus(r: &mut Rdy) -> BTreeMap<String, String> {
    let w = write_as(&approver(), r, "work", json!([
        {"op": "create", "type": "folder", "id": "$g", "fields": {"name": "guides", "title": "Guides"}},
        {"op": "create", "type": "doc", "id": "$a", "parent": "$g", "fields": {"name": "alpha", "title": "Alpha", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$as", "parent": "$a", "fields": {"title": "A", "lifecycle": "stable"}},
        {"op": "create", "type": "claim", "id": "$ac", "parent": "$as", "body": "Alpha has a needle."},
        {"op": "create", "type": "code", "id": "$ak", "parent": "$as", "body": "needle = 1", "fields": {"lang": "sh"}},
        {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "beta", "title": "Beta", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$bs", "parent": "$b", "fields": {"title": "B"}},
        {"op": "create", "type": "claim", "id": "$bg", "parent": "$bs", "body": "An affected needle."},
        {"op": "create", "type": "decision", "id": "$d", "parent": "$bs",
         "fields": {"statement": "Every needle is steel", "status": "proposed"}, "links": {"affects": [{"target": "$bg"}]}},
        {"op": "create", "type": "decision", "id": "$r", "parent": "$bs",
         "fields": {"statement": "A needle may be wood", "status": "rejected",
                    "decided_at": "2021-01-10T09:00:00Z", "recorded_at": "2021-01-10T09:00:00Z", "options": ["yes", "no"]}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$bs", "fields": {"name": "Needle"}, "body": "A thin needle."},
        {"op": "create", "type": "claim", "id": "$bc", "parent": "$bs", "body": "A decided needle.", "links": {"decided_by": [{"target": "$d"}]}},
        {"op": "create", "type": "claim", "id": "$bd", "parent": "$bs", "body": "A described needle.", "links": {"describes": [{"target": "$t"}]}},
        {"op": "create", "type": "claim", "id": "$be", "parent": "$bs", "body": "Per {{$t}}, a needle."},
        {"op": "create", "type": "claim", "id": "$bf", "parent": "$bs", "body": "An implementing needle.", "links": {"implements": [{"target": "$d"}]}}
    ]))
    .unwrap();
    let mut ids = w.created;
    ids.insert("landing".into(), land(r, "work").unwrap());
    ids
}

fn query(f: SearchFilters) -> SearchQuery {
    SearchQuery {
        text: "needle".into(),
        regex: false,
        ignore_case: true,
        filters: f,
    }
}

fn hits_in(r: &Rdy, scope: &Scope, f: SearchFilters) -> Vec<String> {
    r.read()
        .search(scope, &query(f), None, Some(100_000))
        .unwrap()
        .matches
        .iter()
        .map(|m| m.id.clone())
        .collect()
}

fn hits(r: &Rdy, f: SearchFilters) -> Vec<String> {
    hits_in(r, &Scope::Canon, f)
}

fn refused(r: &Rdy, f: SearchFilters) -> RdyError {
    r.read()
        .search(&Scope::Canon, &query(f), None, None)
        .unwrap_err()
}

fn pick(ids: &BTreeMap<String, String>, keys: &[&str]) -> Vec<String> {
    keys.iter().map(|k| ids[*k].clone()).collect()
}

const ALL: [&str; 10] = [
    "$ac", "$ak", "$bg", "$d", "$r", "$t", "$bc", "$bd", "$be", "$bf",
];

#[test]
fn sflt1_no_filter_finds_every_block_in_reading_order() {
    let (_d, mut r) = project();
    let ids = corpus(&mut r);
    assert_eq!(hits(&r, SearchFilters::default()), pick(&ids, &ALL));
}

#[test]
fn sflt1_types_and_traits_narrow_by_block_type_and_trait() {
    let (_d, mut r) = project();
    let ids = corpus(&mut r);
    let types = |t: &[&str]| SearchFilters {
        types: t.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    let traits = |t: &[&str]| SearchFilters {
        traits: t.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    assert_eq!(
        hits(&r, types(&["code", "term"])),
        pick(&ids, &["$ak", "$t"])
    );
    assert_eq!(hits(&r, traits(&["defines"])), pick(&ids, &["$t"]));
    assert_eq!(
        hits(&r, traits(&["defines", "informative"])),
        pick(&ids, &["$ak", "$t"])
    );
    let both = SearchFilters {
        types: vec!["claim".into()],
        traits: vec!["defines".into()],
        ..Default::default()
    };
    assert_eq!(
        hits(&r, both),
        Vec::<String>::new(),
        "filters combine with AND"
    );
    let e = refused(&r, types(&["nope"]));
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(
        e.message
            .starts_with("types: no block type \"nope\" in pack "),
        "{e}"
    );
    let e = refused(&r, traits(&["shiny"]));
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_BAD_INPUT, "traits: \"shiny\" is not a trait")
    );
}

#[test]
fn sflt1_path_takes_whole_segments_and_lifecycle_is_inherited() {
    let (_d, mut r) = project();
    let ids = corpus(&mut r);
    let path = |p: &str| SearchFilters {
        path: Some(p.into()),
        ..Default::default()
    };
    let life = |l: &str| SearchFilters {
        lifecycle: Some(l.into()),
        ..Default::default()
    };
    let beta = ["$bg", "$d", "$r", "$t", "$bc", "$bd", "$be", "$bf"];
    assert_eq!(hits(&r, path("guides")), pick(&ids, &["$ac", "$ak"]));
    assert_eq!(hits(&r, path("guides/")), pick(&ids, &["$ac", "$ak"]));
    assert_eq!(hits(&r, path("guides/alpha")), pick(&ids, &["$ac", "$ak"]));
    // final review 4: a leading slash reads as the project root, as a trailing one is dropped
    assert_eq!(hits(&r, path("/guides")), pick(&ids, &["$ac", "$ak"]));
    assert_eq!(
        hits(&r, path("/guides/alpha/")),
        pick(&ids, &["$ac", "$ak"])
    );
    assert_eq!(
        hits(&r, path("gui")),
        Vec::<String>::new(),
        "a prefix is whole segments"
    );
    assert_eq!(
        hits(&r, path("Guides")),
        Vec::<String>::new(),
        "paths are identifiers: case counts"
    );
    assert_eq!(hits(&r, path("beta")), pick(&ids, &beta));
    assert_eq!(hits(&r, life("stable")), pick(&ids, &["$ac", "$ak"]));
    assert_eq!(
        hits(&r, life("draft")),
        pick(&ids, &beta),
        "no lifecycle anywhere is draft"
    );
    assert_eq!(hits(&r, life("deprecated")), Vec::<String>::new());
    let e = refused(&r, life("final"));
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_BAD_INPUT,
            "lifecycle: \"final\" is not draft, stable or deprecated"
        )
    );
    assert_eq!(refused(&r, path("/")).code, E_BAD_INPUT);
}

#[test]
fn sflt1_changed_since_keeps_blocks_whose_version_moved() {
    let (_d, mut r) = project();
    let ids = corpus(&mut r);
    let first = ids["landing"].clone();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let w = write(&mut r, "work", json!([
        {"op": "update", "id": ids["$bc"], "body": "A decided needle, again."},
        {"op": "create", "type": "claim", "id": "$n", "parent": ids["$bs"], "body": "A new needle."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let want = vec![ids["$bc"].clone(), w.created["$n"].clone()];
    let since = |t: &str| SearchFilters {
        changed_since: Some(t.into()),
        ..Default::default()
    };
    assert_eq!(hits(&r, since("tag:v1.0.0")), want);
    assert_eq!(hits(&r, since("v1.0.0")), want, "a bare version is a tag");
    assert_eq!(hits(&r, since(&format!("landing:{first}"))), want);
    assert_eq!(hits(&r, since(&first[..12])), want, "a bare landing id");
    let e = refused(&r, since("work"));
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_BAD_INPUT,
            "changed_since \"work\" names no version, tag or landing"
        )
    );
}

#[test]
fn sflt1_graph_filters_follow_links_and_references() {
    let (_d, mut r) = project();
    let ids = corpus(&mut r);
    let (d, t) = (ids["$d"].clone(), ids["$t"].clone());
    let gov = SearchFilters {
        governed_by: Some(d.clone()),
        ..Default::default()
    };
    assert_eq!(
        hits(&r, gov),
        pick(&ids, &["$bg", "$bc", "$bf"]),
        "affects, decided_by, implements"
    );
    let desc = SearchFilters {
        describes: Some(t.clone()),
        ..Default::default()
    };
    assert_eq!(hits(&r, desc), pick(&ids, &["$bd"]));
    let to_t = SearchFilters {
        linked_to: Some(t.clone()),
        ..Default::default()
    };
    assert_eq!(
        hits(&r, to_t),
        pick(&ids, &["$bd", "$be"]),
        "a link and a reference"
    );
    let to_d = SearchFilters {
        linked_to: Some(d.clone()),
        ..Default::default()
    };
    assert_eq!(
        hits(&r, to_d),
        pick(&ids, &["$bg", "$bc", "$bf"]),
        "either direction; never the block itself"
    );
    let and = SearchFilters {
        types: vec!["claim".into()],
        lifecycle: Some("draft".into()),
        linked_to: Some(t.clone()),
        ..Default::default()
    };
    assert_eq!(hits(&r, and), pick(&ids, &["$bd", "$be"]));
    let e = refused(
        &r,
        SearchFilters {
            governed_by: Some(t.clone()),
            ..Default::default()
        },
    );
    assert_eq!(
        (e.code, e.message.clone()),
        (
            E_BAD_INPUT,
            format!("governed_by \"{t}\" is a term, not a decision")
        )
    );
}

#[test]
fn sflt1_suspect_keeps_blocks_with_an_open_suspect() {
    let (_d, mut r) = project();
    let ids = corpus(&mut r);
    let work = Scope::View("work".into());
    let sus = || SearchFilters {
        suspect: true,
        ..Default::default()
    };
    assert_eq!(hits_in(&r, &work, sus()), Vec::<String>::new());
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": ids["$t"], "body": "A thin steel needle."}]),
    )
    .unwrap();
    assert_eq!(
        hits_in(&r, &work, sus()),
        pick(&ids, &["$be"]),
        "be references t; describes is no premise"
    );
    assert_eq!(
        hits_in(&r, &work, SearchFilters::default()).len(),
        ALL.len(),
        "suspect: false filters nothing"
    );
}

#[test]
fn sflt1_matches_inside_a_superseded_or_rejected_decision_carry_history() {
    let (_d, mut r) = project();
    let ids = corpus(&mut r);
    let flagged = |r: &Rdy| -> Vec<String> {
        r.read()
            .search(
                &Scope::Canon,
                &query(SearchFilters::default()),
                None,
                Some(100_000),
            )
            .unwrap()
            .matches
            .iter()
            .filter(|m| m.history)
            .map(|m| m.id.clone())
            .collect()
    };
    assert_eq!(flagged(&r), pick(&ids, &["$r"]));
    let ex = || SearchFilters {
        exclude_history: true,
        ..Default::default()
    };
    let rest: Vec<&str> = ALL.iter().copied().filter(|k| *k != "$r").collect();
    assert_eq!(hits(&r, ex()), pick(&ids, &rest));
    let w = r
        .supersede(
            &writer(),
            "work",
            &ids["$d"],
            None,
            DecisionInput {
                parent: ids["$bs"].clone(),
                statement: "Every needle is iron".into(),
                ..Default::default()
            },
        )
        .unwrap();
    land(&mut r, "work").unwrap();
    let new = w.created["$decision"].clone();
    assert_eq!(flagged(&r), pick(&ids, &["$d", "$r"]));
    let kept = hits(&r, ex());
    assert!(
        !kept.contains(&ids["$d"]) && !kept.contains(&ids["$r"]) && kept.contains(&new),
        "{kept:?}"
    );
    let m = r
        .read()
        .search(
            &Scope::Canon,
            &query(SearchFilters::default()),
            None,
            Some(100_000),
        )
        .unwrap();
    let json = serde_json::to_value(&m.matches).unwrap();
    let r_entry = json
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == ids["$r"].as_str())
        .unwrap();
    assert_eq!(r_entry["history"], true);
    let c_entry = json
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == ids["$ac"].as_str())
        .unwrap();
    assert!(
        c_entry.get("history").is_none(),
        "history is left out when false: {c_entry}"
    );
}

#[test]
fn sflt1_a_cursor_belongs_to_its_filters() {
    let (_d, mut r) = project();
    corpus(&mut r);
    let claims = query(SearchFilters {
        types: vec!["claim".into()],
        ..Default::default()
    });
    let first = r
        .read()
        .search(&Scope::Canon, &claims, None, Some(1))
        .unwrap();
    let cursor = first.next.clone().expect("one claim per page");
    assert!(
        r.read()
            .search(&Scope::Canon, &claims, Some(&cursor), Some(1))
            .is_ok()
    );
    let e = r
        .read()
        .search(
            &Scope::Canon,
            &query(SearchFilters::default()),
            Some(&cursor),
            Some(1),
        )
        .unwrap_err();
    assert_eq!(e.code, E_PAGE_STALE, "{e}");
}
