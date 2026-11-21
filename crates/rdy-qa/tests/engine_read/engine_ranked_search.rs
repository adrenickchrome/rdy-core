//! vxs6 (D-76Y9H, D-J5FWG, D-5VGAS): `sort: relevance` ranks the hits that matching finds by
//! their lexical score against the whole query, inside the phrase tier and the all-words tier,
//! then in reading order. `sort: reading` names the default order. Without `relevance` a search
//! gives the same bytes as before, and builds no vector weights.
use rdy_engine::search::{SearchFilters, SearchQuery};
use rdy_engine::vectors::WeightScope;
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Ids from a seeded stream on this thread until the guard drops, so a run replays its bytes.
struct SeededIds;

impl SeededIds {
    fn new() -> SeededIds {
        rdy_model::seed_ids_for_tests(0x7673_7836);
        SeededIds
    }
}

impl Drop for SeededIds {
    fn drop(&mut self) {
        rdy_model::unseed_ids_for_tests();
    }
}

/// Doc `rank`, landed. Phrase hits of "alpha beta" `$r1` (eight words), `$r2` (three) and `$r3`
/// (the two words alone), in reading order; all-words hits `$r4` (eight words) and `$r5` (three).
/// `$s1` holds "cat" only inside "concatenate"; `$s2` holds the word. `$k1` (long) and `$k2`
/// (short) have the same matching line for "omega psi", and `$k3` between them has its own.
/// Term Ledger and claim `$c` that names it twice.
fn world(r: &mut Rdy) -> BTreeMap<String, String> {
    let w = write(
        r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "rank", "title": "Rank", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Tiers"}},
            {"op": "create", "type": "claim", "id": "$r1", "parent": "$s", "body": "alpha beta gamma delta epsilon zeta eta theta."},
            {"op": "create", "type": "claim", "id": "$r2", "parent": "$s", "body": "alpha beta gamma."},
            {"op": "create", "type": "claim", "id": "$r3", "parent": "$s", "body": "alpha beta."},
            {"op": "create", "type": "claim", "id": "$r4", "parent": "$s", "body": "beta comes before alpha in this longer line."},
            {"op": "create", "type": "claim", "id": "$r5", "parent": "$s", "body": "beta then alpha."},
            {"op": "create", "type": "prose", "id": "$s1", "parent": "$s", "body": "concatenate the logs."},
            {"op": "create", "type": "prose", "id": "$s2", "parent": "$s", "body": "the cat sleeps."},
            {"op": "create", "type": "claim", "id": "$k1", "parent": "$s", "body": "omega psi\nmany more words live in this long second line."},
            {"op": "create", "type": "claim", "id": "$k3", "parent": "$s", "body": "omega psi chi phi rho."},
            {"op": "create", "type": "claim", "id": "$k2", "parent": "$s", "body": "omega psi\nshort."},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "the Ledger is append-only, and the Ledger never shrinks."}
        ]),
    )
    .unwrap();
    land(r, "work").unwrap();
    w.created
}

fn query(text: &str, filters: Value) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        filters: serde_json::from_value::<SearchFilters>(filters).unwrap(),
        ..Default::default()
    }
}

fn search_at(r: &Rdy, scope: &Scope, text: &str, filters: Value) -> Value {
    let res = r
        .read()
        .search(scope, &query(text, filters), None, Some(4000))
        .unwrap();
    serde_json::to_value(res).unwrap()
}

fn search(r: &Rdy, text: &str, filters: Value) -> Value {
    search_at(r, &Scope::Canon, text, filters)
}

fn page(r: &Rdy, text: &str, filters: Value, cursor: Option<&str>) -> Result<Value, RdyError> {
    r.read()
        .search(&Scope::Canon, &query(text, filters), cursor, Some(1))
        .map(|res| serde_json::to_value(res).unwrap())
}

fn ids(res: &Value) -> Vec<String> {
    res["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect()
}

fn score(m: &Value) -> f64 {
    m["score"]
        .as_f64()
        .unwrap_or_else(|| panic!("a score: {m}"))
}

/// Whether any object in `v` has a `score` member.
fn has_score(v: &Value) -> bool {
    match v {
        Value::Object(o) => o.contains_key("score") || o.values().any(has_score),
        Value::Array(a) => a.iter().any(has_score),
        _ => false,
    }
}

/// The default-mode searches whose bytes the base gave, one compact JSON line each: phrase and
/// all-words tiers, substrings, collapse, a fact sheet, a filter, the two coverage options and a
/// cut page with its cursor.
fn default_lines(r: &Rdy) -> String {
    let st = r.read();
    let mut out = String::new();
    for (text, filters, max) in [
        ("alpha beta", json!({}), 4000),
        ("cat", json!({}), 4000),
        ("omega psi", json!({}), 4000),
        ("Ledger", json!({}), 4000),
        ("alpha beta", json!({"types": ["claim"]}), 4000),
        ("alpha beta", json!({"sort": "coverage"}), 4000),
        ("alpha beta", json!({"coverage_below": 100}), 4000),
        ("alpha beta", json!({}), 1),
    ] {
        let res = st
            .search(&Scope::Canon, &query(text, filters), None, Some(max))
            .unwrap();
        out.push_str(&serde_json::to_string(&res).unwrap());
        out.push('\n');
        if let Some(next) = &res.next {
            let more = st
                .search(
                    &Scope::Canon,
                    &query(text, json!({})),
                    Some(next),
                    Some(max),
                )
                .unwrap();
            out.push_str(&serde_json::to_string(&more).unwrap());
            out.push('\n');
        }
    }
    out
}

/// The sha256 of [`default_lines`] at the base, before `relevance` and `reading` existed.
const BASE_DEFAULT_SHA: &str = "325a5d2ea41b93beb75b37a55940b5b18fc4dc662191f992cce590dbdd3cc5c7";

#[test]
fn vxs6_default_and_explicit_reading_keep_existing_bytes() {
    let _clock = rdy_qa::golden::fixture_clock();
    let _ids = SeededIds::new();
    let (_d, mut r) = project();
    let _c = world(&mut r);
    let lines = default_lines(&r);
    let sha = format!("{:x}", Sha256::digest(lines.as_bytes()));
    assert_eq!(sha, BASE_DEFAULT_SHA, "the default search bytes:\n{lines}");
    assert!(!lines.contains("\"score\""), "{lines}");
    // a default search reads no vector weight
    let w = r
        .read()
        .vector_weights(WeightScope::Read(&Scope::Canon))
        .unwrap();
    assert_eq!(
        w.stats(),
        Default::default(),
        "no weight for a default search"
    );
    // `sort: reading` names the default order: the same bytes, cursor included, and each
    // cursor pages the other
    for text in ["alpha beta", "cat", "omega psi", "Ledger"] {
        for max in [4000, 1] {
            let st = r.read();
            let default = st
                .search(&Scope::Canon, &query(text, json!({})), None, Some(max))
                .unwrap();
            let reading = st
                .search(
                    &Scope::Canon,
                    &query(text, json!({"sort": "reading"})),
                    None,
                    Some(max),
                )
                .unwrap();
            assert_eq!(
                serde_json::to_string(&reading).unwrap(),
                serde_json::to_string(&default).unwrap(),
                "{text} {max}"
            );
            if let Some(next) = &default.next {
                let a = st
                    .search(
                        &Scope::Canon,
                        &query(text, json!({"sort": "reading"})),
                        Some(next),
                        Some(max),
                    )
                    .unwrap();
                let b = st
                    .search(
                        &Scope::Canon,
                        &query(text, json!({})),
                        Some(next),
                        Some(max),
                    )
                    .unwrap();
                assert_eq!(a, b, "{text}");
            }
        }
    }
    let w = r
        .read()
        .vector_weights(WeightScope::Read(&Scope::Canon))
        .unwrap();
    assert_eq!(w.stats(), Default::default(), "nor for `sort: reading`");
}

#[test]
fn vxs6_relevance_preserves_sheets_phrase_tiers_and_substrings() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let id = |k: &str| c[k].clone();
    let default = search(&r, "alpha beta", json!({}));
    assert_eq!(
        ids(&default),
        ["$r1", "$r2", "$r3", "$r4", "$r5"].map(id),
        "the base order: phrase hits, then all-words hits, in reading order"
    );
    let ranked = search(&r, "alpha beta", json!({"sort": "relevance"}));
    assert_eq!(
        ids(&ranked),
        ["$r3", "$r2", "$r1", "$r5", "$r4"].map(id),
        "each tier by score: {ranked}"
    );
    let m = ranked["matches"].as_array().unwrap();
    assert_eq!(score(&m[0]), 1.0, "the query's words alone");
    assert!(score(&m[0]) > score(&m[1]) && score(&m[1]) > score(&m[2]));
    assert!(score(&m[3]) > score(&m[4]));
    assert!(
        score(&m[3]) > score(&m[2]),
        "an all-words hit stays after every phrase hit, whatever its score"
    );
    for h in m {
        let s = score(h);
        assert_eq!(s, (s * 1000.0).round() / 1000.0, "shown to three decimals");
        assert!(h["all_words"].is_boolean() || h.get("all_words").is_none());
    }
    for key in ["total", "totals", "query", "regex", "ignore_case"] {
        assert_eq!(ranked[key], default[key], "{key}");
    }
    // a substring hit that shares no whole word scores 0 and stays, last in its tier
    let default = search(&r, "cat", json!({}));
    assert_eq!(ids(&default), ["$s1", "$s2"].map(id));
    let ranked = search(&r, "cat", json!({"sort": "relevance"}));
    assert_eq!(ids(&ranked), ["$s2", "$s1"].map(id), "{ranked}");
    assert!(score(&ranked["matches"][0]) > 0.0);
    assert_eq!(score(&ranked["matches"][1]), 0.0);
    assert_eq!(ranked["total"], 2);
    // the fact sheets come first, as in the default order, and hold the same hits out
    let default = search(&r, "Ledger", json!({}));
    let ranked = search(&r, "Ledger", json!({"sort": "relevance"}));
    assert_eq!(ranked["entities"], default["entities"]);
    assert_eq!(ranked["entities"][0]["id"], c["$t"]);
    assert_eq!(ranked["total"], default["total"]);
    let mut a = ids(&ranked);
    let mut b = ids(&default);
    a.sort();
    b.sort();
    assert_eq!(a, b, "the same hits");
    assert!(
        !has_score(&ranked["entities"]),
        "a fact sheet carries no score"
    );
    // the default and coverage orders carry no score
    assert!(!has_score(&search(&r, "alpha beta", json!({}))));
    assert!(!has_score(&search(
        &r,
        "alpha beta",
        json!({"sort": "coverage"})
    )));
}

#[test]
fn vxs6_more_than_64_query_words_all_score_without_seeds() {
    let (_d, mut r) = project();
    // seventy distinct words, past the 64 heaviest a candidate search reads
    let words: Vec<String> = (0..70)
        .map(|i| {
            let a = (b'a' + (i / 26) as u8) as char;
            let b = (b'a' + (i % 26) as u8) as char;
            format!("q{a}{b}x")
        })
        .collect();
    let text = words.join(" ");
    assert!(text.len() <= 1024);
    let numeric = "2026-11-20 12.5 7";
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "many", "title": "Many", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$all", "parent": "$doc", "body": text},
            {"op": "create", "type": "prose", "id": "$half", "parent": "$doc", "body": format!("{} and more", words[..35].join(" "))},
            {"op": "create", "type": "prose", "id": "$num", "parent": "$doc", "body": numeric}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let ranked = search(&r, &text, json!({"sort": "relevance"}));
    assert_eq!(ids(&ranked), [w.created["$all"].clone()]);
    // every query word scores: the block that holds exactly the query scores 1, where 64 of
    // its 70 words would give about 0.956
    assert_eq!(score(&ranked["matches"][0]), 1.0, "{ranked}");
    // numeric words never seed (D-ZDM0G) but still score
    let ranked = search(&r, numeric, json!({"sort": "relevance"}));
    assert_eq!(ids(&ranked), [w.created["$num"].clone()]);
    assert_eq!(score(&ranked["matches"][0]), 1.0, "{ranked}");
}

#[test]
fn vxs6_regex_and_historical_relevance_are_refused() {
    let (_d, mut r) = project();
    let _c = world(&mut r);
    let relevance = json!({"sort": "relevance"});
    let refused = |r: &Rdy, scope: &Scope, q: &SearchQuery| {
        r.read().search(scope, q, None, Some(4000)).unwrap_err()
    };
    // a pattern has no words
    let mut q = query("alpha", relevance.clone());
    q.regex = true;
    let e = refused(&r, &Scope::Canon, &q);
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(e.message, "sort relevance needs a literal query");
    assert_eq!(e.fix, "drop regex, or drop sort");
    // plain regex is unchanged
    let mut plain = query("alph[a]", json!({}));
    plain.regex = true;
    let res = r
        .read()
        .search(&Scope::Canon, &plain, None, Some(4000))
        .unwrap();
    assert!(res.total > 0);
    // an unknown sort value names the three
    let e = refused(&r, &Scope::Canon, &query("alpha", json!({"sort": "score"})));
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        "sort takes coverage, relevance or reading, not score"
    );
    assert_eq!(e.fix, "pass coverage, relevance or reading, or omit sort");

    // a past landing, by its hash and by a tag; the head, by its hash and its tag, reads canon
    let first: rdy_model::Hash = r.read().canon_head().unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(
        &mut r,
        "more",
        json!([{"op": "create", "type": "doc", "fields": {"name": "later", "title": "Later alpha", "doc_type": "page"}}]),
    )
    .unwrap();
    land(&mut r, "more").unwrap();
    r.tag(&approver(), "v1.1.0", None, None).unwrap();
    let past = r.read().resolve_scope("tag:v1.0.0").unwrap();
    assert_eq!(past, Scope::At(first));
    for scope in [Scope::At(first), past] {
        let e = refused(&r, &scope, &query("alpha", relevance.clone()));
        assert_eq!(e.code, E_BAD_INPUT);
        assert_eq!(
            e.message,
            format!("sort relevance ranks the current state only, not landing:{first}")
        );
        assert_eq!(
            e.fix,
            "ask for canon or a view; ranking over time is not part of rdy core"
        );
        // refused before any vector, lexicon or text is read there
        assert!(!r.read().lexicon_built(&scope));
        // the regex refusal comes first
        let mut q = query("alpha", relevance.clone());
        q.regex = true;
        assert_eq!(
            refused(&r, &scope, &q).message,
            "sort relevance needs a literal query"
        );
    }
    let canon = search(&r, "alpha", relevance.clone());
    let head = r.read().canon_head().unwrap();
    let tagged = r.read().resolve_scope("tag:v1.1.0").unwrap();
    for scope in [Scope::At(head), tagged] {
        assert_eq!(search_at(&r, &scope, "alpha", relevance.clone()), canon);
    }

    // a proposal version
    write(
        &mut r,
        "p",
        json!([{"op": "create", "type": "doc", "fields": {"name": "prop", "title": "Proposed alpha", "doc_type": "page"}}]),
    )
    .unwrap();
    let p = r.propose(&writer(), "p", "p", Some("Add a doc")).unwrap();
    let pref = r.resolve_proposal(&p.id).unwrap();
    let scope = r.read().resolve_scope(&format!("proposal:{pref}")).unwrap();
    let Scope::Proposal(v) = &scope else {
        panic!("a proposal version: {scope:?}");
    };
    let e = refused(&r, &scope, &query("alpha", relevance.clone()));
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        format!(
            "sort relevance ranks canon and views only, not proposal:{}",
            v.short()
        )
    );
    assert_eq!(e.fix, "ask for the proposal's view, or canon");
    // a default search there still reads
    search_at(&r, &scope, "alpha", json!({}));
}

#[test]
fn vxs6_ranked_collapse_keeps_each_blocks_score() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let default = search(&r, "omega psi", json!({}));
    let m = default["matches"].as_array().unwrap();
    assert_eq!(ids(&default), [c["$k1"].clone(), c["$k3"].clone()]);
    assert_eq!(m[0]["also"][0]["id"], c["$k2"]);
    assert!(!has_score(&default));

    let ranked = search(&r, "omega psi", json!({"sort": "relevance"}));
    let m = ranked["matches"].as_array().unwrap();
    // the entry is in its best member's place, and that member represents it
    assert_eq!(
        ids(&ranked),
        [c["$k2"].clone(), c["$k3"].clone()],
        "{ranked}"
    );
    let also = m[0]["also"].as_array().unwrap();
    assert_eq!(also.len(), 1);
    assert_eq!(also[0]["id"], c["$k1"]);
    let (best, mid, worst) = (score(&m[0]), score(&m[1]), score(&also[0]));
    assert!(best > mid && mid > worst, "{best} {mid} {worst}");
    assert_eq!(m[0]["lines"], default["matches"][0]["lines"]);
    assert_eq!(ranked["total"], 3);
}

#[test]
fn vxs6_ranked_cursor_ignores_score_drift_but_tracks_order() {
    let (_d, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "drift", "title": "Drift", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$a", "parent": "$doc", "body": "alpha"},
            {"op": "create", "type": "prose", "id": "$b", "parent": "$doc", "body": "alpha gamma"},
            {"op": "create", "type": "prose", "id": "$c", "parent": "$doc", "body": "alpha delta"}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let id = |k: &str| w.created[k].clone();
    let relevance = json!({"sort": "relevance"});
    let all = search(&r, "alpha", relevance.clone());
    assert_eq!(
        ids(&all),
        ["$a", "$b", "$c"].map(id),
        "a tie keeps reading order"
    );
    let before = score(&all["matches"][1]);
    let first = page(&r, "alpha", relevance.clone(), None).unwrap();
    assert_eq!(ids(&first), ["$a"].map(id));
    let cursor = first["next"].as_str().unwrap().to_string();
    let plain = page(&r, "alpha", json!({}), None).unwrap();
    let plain_cursor = plain["next"].as_str().unwrap().to_string();
    // the mode is part of the cursor
    assert_eq!(
        page(&r, "alpha", json!({}), Some(&cursor))
            .unwrap_err()
            .code,
        E_PAGE_STALE
    );
    assert_eq!(
        page(&r, "alpha", relevance.clone(), Some(&plain_cursor))
            .unwrap_err()
            .code,
        E_PAGE_STALE
    );

    // a block that holds none of the hits' words moves every weight, not the order
    write(
        &mut r,
        "w2",
        json!([{"op": "create", "type": "prose", "parent": id("$doc"), "body": "zeta"}]),
    )
    .unwrap();
    land(&mut r, "w2").unwrap();
    let all = search(&r, "alpha", relevance.clone());
    assert_eq!(ids(&all), ["$a", "$b", "$c"].map(id));
    assert_ne!(score(&all["matches"][1]), before, "the score drifts");
    let second = page(&r, "alpha", relevance.clone(), Some(&cursor)).unwrap();
    assert_eq!(ids(&second), ["$b"].map(id), "a drift keeps the cursor");

    // a block that lowers delta's weight puts `$c` before `$b`: the hits and versions stay,
    // the order moves, and the ranked cursor goes stale
    write(
        &mut r,
        "w3",
        json!([{"op": "create", "type": "prose", "parent": id("$doc"), "body": "delta"}]),
    )
    .unwrap();
    land(&mut r, "w3").unwrap();
    let all = search(&r, "alpha", relevance.clone());
    assert_eq!(ids(&all), ["$a", "$c", "$b"].map(id), "{all}");
    let e = page(&r, "alpha", relevance.clone(), Some(&cursor)).unwrap_err();
    assert_eq!(e.code, E_PAGE_STALE);
    // the default order did not move, so its cursor holds
    let more = page(&r, "alpha", json!({}), Some(&plain_cursor)).unwrap();
    assert_eq!(ids(&more), ["$b"].map(id));
}

#[test]
fn vxs6_coverage_sort_keeps_its_values_order_and_cursor() {
    let (_d, mut r) = project();
    let _c = world(&mut r);
    let default = search(&r, "alpha beta", json!({}));
    let sorted = search(&r, "alpha beta", json!({"sort": "coverage"}));
    assert!(!has_score(&sorted));
    assert_eq!(sorted["total"], default["total"]);
    let coverage = |res: &Value| -> BTreeMap<String, Value> {
        res["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| (m["id"].as_str().unwrap().to_string(), m["coverage"].clone()))
            .collect()
    };
    assert_eq!(coverage(&sorted), coverage(&default), "the same values");
    let scores: Vec<u64> = sorted["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["coverage"].as_u64().unwrap())
        .collect();
    assert!(scores.windows(2).all(|w| w[0] <= w[1]), "{scores:?}");
    // one entry per page, through every page, gives the full order
    let mut walked = vec![];
    let mut cursor: Option<String> = None;
    loop {
        let p = page(
            &r,
            "alpha beta",
            json!({"sort": "coverage"}),
            cursor.as_deref(),
        )
        .unwrap();
        walked.extend(ids(&p));
        match p["next"].as_str() {
            Some(n) => cursor = Some(n.to_string()),
            None => break,
        }
    }
    assert_eq!(walked, ids(&sorted));
    // a coverage cursor is not a relevance cursor
    let first = page(&r, "alpha beta", json!({"sort": "coverage"}), None).unwrap();
    let e = page(
        &r,
        "alpha beta",
        json!({"sort": "relevance"}),
        first["next"].as_str(),
    )
    .unwrap_err();
    assert_eq!(e.code, E_PAGE_STALE);
    // relevance with coverage_below keeps the bound
    let ranked = search(
        &r,
        "alpha beta",
        json!({"sort": "relevance", "coverage_below": 100}),
    );
    let bounded = search(&r, "alpha beta", json!({"coverage_below": 100}));
    assert_eq!(ranked["total"], bounded["total"]);
}
