//! vxs7 (D-76Y9H, D-5VGAS, D-RY7B5, D-ZDM0G): `like` ranks the blocks of the scope by their
//! score against one block, with both of its vectors; `like_text` takes its lexical vector from
//! its words and its link vector from the entities it names. Every block type the filters allow
//! takes part, a hit needs an exact score of at least 0.10, the `like` block is never a hit, and
//! a hit's lines are the lines that hold its shown words. A source with no seed has no hits and
//! no refusal.
use rdy_engine::search::{LIKE_FLOOR, SearchFilters, SearchInput};
use rdy_engine::vectors::{Doc, VectorBudget, WeightScope, round3, score_pair};
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn filters(f: Value) -> SearchFilters {
    serde_json::from_value(f).unwrap()
}

fn input(
    query: Option<&str>,
    like: Option<&str>,
    like_text: Option<&str>,
    f: Value,
) -> SearchInput {
    SearchInput::new(
        query.map(str::to_string),
        like.map(str::to_string),
        like_text.map(str::to_string),
        false,
        false,
        filters(f),
    )
    .unwrap()
}

fn run(r: &Rdy, scope: &Scope, i: &SearchInput) -> Value {
    let res = r
        .read()
        .search_input(scope, i, None, Some(100_000))
        .unwrap();
    serde_json::to_value(res).unwrap()
}

fn like(r: &Rdy, id: &str, f: Value) -> Value {
    run(r, &Scope::Canon, &input(None, Some(id), None, f))
}

fn like_text(r: &Rdy, scope: &Scope, text: &str) -> Value {
    run(r, scope, &input(None, None, Some(text), json!({})))
}

fn ids(res: &Value) -> Vec<String> {
    res["matches"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| {
            std::iter::once(m["id"].as_str().unwrap().to_string()).chain(
                m["also"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| a["id"].as_str().unwrap().to_string()),
            )
        })
        .collect()
}

/// The hit of `id` (an entry or an `also` member).
fn hit<'a>(res: &'a Value, id: &str) -> &'a Value {
    for m in res["matches"].as_array().unwrap() {
        if m["id"] == id {
            return m;
        }
        for a in m["also"].as_array().into_iter().flatten() {
            if a["id"] == id {
                return a;
            }
        }
    }
    panic!("no hit {id}: {res}")
}

fn score(m: &Value) -> f64 {
    m["score"]
        .as_f64()
        .unwrap_or_else(|| panic!("a score: {m}"))
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("a list: {v}"))
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

/// Term Ledger, and four claims in doc `sea`: the source `$src` (kelp words, a describes link
/// to the term), `$both` (some of its words and the link), `$words` (some of its words, no
/// link), `$link` (the link, no shared word) and `$none` (neither).
fn sea(r: &mut Rdy) -> BTreeMap<String, String> {
    let w = write(
        r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "sea", "title": "Sea", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Shore"}},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
            {"op": "create", "type": "claim", "id": "$src", "parent": "$s", "body": "Kelp forests shelter otters.", "links": {"describes": [{"target": "$t"}]}},
            {"op": "create", "type": "claim", "id": "$both", "parent": "$s", "body": "Kelp forests feed urchins.", "links": {"describes": [{"target": "$t"}]}},
            {"op": "create", "type": "claim", "id": "$words", "parent": "$s", "body": "Kelp forests grow fast."},
            {"op": "create", "type": "claim", "id": "$link", "parent": "$s", "body": "Zebra stripes.", "links": {"describes": [{"target": "$t"}]}},
            {"op": "create", "type": "claim", "id": "$none", "parent": "$s", "body": "Desert sand."}
        ]),
    )
    .unwrap();
    land(r, "work").unwrap();
    w.created
}

#[test]
fn vxs7_like_uses_both_vectors_and_excludes_its_source() {
    let (_d, mut r) = project();
    let c = sea(&mut r);
    let id = |k: &str| c[k].clone();
    let res = like(&r, &id("$src"), json!({}));
    let found = ids(&res);
    assert!(
        !found.contains(&id("$src")),
        "the like block is never a hit: {res}"
    );
    assert!(!found.contains(&id("$none")), "{res}");
    for k in ["$both", "$words", "$link"] {
        assert!(found.contains(&id(k)), "{k}: {res}");
    }
    // each hit's score is the exact pair score with both vectors, shown to three decimals
    let w = r
        .read()
        .vector_weights(WeightScope::Read(&Scope::Canon))
        .unwrap();
    let src = rdy_model::BlockId::parse(&id("$src")).unwrap();
    let exact = |k: &str| {
        let b = rdy_model::BlockId::parse(&id(k)).unwrap();
        score_pair(
            &w,
            Doc::Block(&src),
            Doc::Block(&b),
            &mut VectorBudget::search(),
        )
        .unwrap()
    };
    for k in ["$both", "$words", "$link"] {
        assert_eq!(score(hit(&res, &id(k))), round3(exact(k)), "{k}");
    }
    // the link counts: a block with the source's words and its link scores above the same words
    // without it, and the link alone scores 0.4 (D-ZX5S6)
    assert_eq!(score(hit(&res, &id("$link"))), 0.4, "{res}");
    assert!(exact("$both") > exact("$words"));
    assert_eq!(strings(&hit(&res, &id("$both"))["links"]), [id("$t")]);
    assert_eq!(
        strings(&hit(&res, &id("$words"))["links"]),
        Vec::<String>::new()
    );
    let shared = strings(&hit(&res, &id("$both"))["words"]);
    assert!(shared.contains(&"kelp".to_string()) && shared.contains(&"forests".to_string()));
    // a link-only hit has no shared word and no line
    let l = hit(&res, &id("$link"));
    assert_eq!(strings(&l["words"]), Vec::<String>::new());
    assert_eq!(l["lines"], json!([]));
    // hits come by score
    let scores: Vec<f64> = res["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(score)
        .collect();
    assert!(scores.windows(2).all(|p| p[0] >= p[1]), "{scores:?}");
    assert!(scores.iter().all(|s| *s >= LIKE_FLOOR), "{scores:?}");
    // the source is echoed, and the query is empty
    assert_eq!(res["like"], id("$src"));
    assert_eq!(res["query"], "");
    assert!(res.get("like_text").is_none(), "{res}");
    // reading every hit built only what scoring needed: the source is never a candidate
    assert!(res["total"].as_u64().unwrap() as usize == found.len());
}

#[test]
fn vxs7_like_text_names_entities_by_alias_and_uses_scope_weights() {
    let (_d, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "books", "title": "Books", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Close"}},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "General ledger", "aliases": ["Kardex"]}, "body": "Holds balances."},
            {"op": "create", "type": "term", "id": "$wg", "parent": "$s", "fields": {"name": "Widget"}, "body": "A part."},
            {"op": "create", "type": "claim", "id": "$d", "parent": "$s", "body": "Posts entries nightly.", "links": {"describes": [{"target": "$t"}]}},
            {"op": "create", "type": "claim", "id": "$f", "parent": "$s", "body": "Zebra stripes.", "links": {"describes": [{"target": "$t"}]}},
            {"op": "create", "type": "claim", "id": "$e", "parent": "$s", "body": "Posts entries weekly."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let c = w.created;
    let id = |k: &str| c[k].clone();
    // the alias names the entity, so the text has a link vector
    let res = like_text(&r, &Scope::Canon, "the Kardex posts entries");
    assert_eq!(res["like_text"], "the Kardex posts entries");
    assert_eq!(res["query"], "");
    assert!(res.get("like").is_none(), "{res}");
    let d = hit(&res, &id("$d"));
    assert_eq!(strings(&d["links"]), [id("$t")], "{res}");
    let e = hit(&res, &id("$e"));
    assert_eq!(strings(&e["links"]), Vec::<String>::new());
    assert!(score(d) > score(e), "the link adds: {res}");
    // a block that only describes the entity scores on its link alone
    let f = hit(&res, &id("$f"));
    assert_eq!(score(f), 0.4, "{res}");
    assert_eq!(f["lines"], json!([]));
    // without the alias, no link vector: the same words score alone
    let plain = like_text(&r, &Scope::Canon, "posts entries");
    assert!(!ids(&plain).contains(&id("$f")), "{plain}");
    assert_eq!(
        strings(&hit(&plain, &id("$d"))["links"]),
        Vec::<String>::new()
    );
    // an entity that the text names, with no word a block holds, is still refused
    let e = r
        .read()
        .search_input(
            &Scope::Canon,
            &input(None, None, Some("widgets"), json!({})),
            None,
            None,
        )
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        "like_text has no word that a block in this scope holds"
    );
    // the weights are the searched scope's: more blocks that hold "posts" in a view lower its
    // weight there and leave canon's alone
    write(
        &mut r,
        "v",
        json!([
            {"op": "create", "type": "claim", "parent": id("$s"), "body": "Posts again."},
            {"op": "create", "type": "claim", "parent": id("$s"), "body": "Posts more."},
            {"op": "create", "type": "claim", "parent": id("$s"), "body": "Posts often."}
        ]),
    )
    .unwrap();
    let view = Scope::View("v".into());
    let in_view = like_text(&r, &view, "the Kardex posts entries");
    let again = like_text(&r, &Scope::Canon, "the Kardex posts entries");
    assert_eq!(
        score(hit(&again, &id("$d"))),
        score(d),
        "canon is unchanged"
    );
    assert_ne!(score(hit(&in_view, &id("$d"))), score(d), "{in_view}");
}

#[test]
fn vxs7_like_filters_all_types_and_keeps_point_one_floor() {
    assert_eq!(LIKE_FLOOR, 0.10);
    let (_d, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "reef", "title": "Reef corals", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Corals bleach"}},
            {"op": "create", "type": "section", "id": "$old", "parent": "$doc", "fields": {"title": "Old", "lifecycle": "deprecated"}},
            {"op": "create", "type": "claim", "id": "$src", "parent": "$s", "body": "Corals bleach in warm water."},
            {"op": "create", "type": "claim", "id": "$near", "parent": "$s", "body": "Corals bleach when water warms."},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Bleaching"}, "body": "Corals bleach in warm water."},
            {"op": "create", "type": "claim", "id": "$dep", "parent": "$old", "body": "Corals bleach in warm water slowly."},
            {"op": "create", "type": "claim", "id": "$far", "parent": "$s", "body": "Water flows past many rocks, sand, shells, fish, eels, crabs, weeds, stones and corals."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let c = w.created;
    let id = |k: &str| c[k].clone();
    let res = like(&r, &id("$src"), json!({}));
    let found: BTreeSet<String> = ids(&res).into_iter().collect();
    // every type takes part, a section neighbor and a deprecated block too
    for k in ["$near", "$t", "$s", "$dep"] {
        assert!(found.contains(&id(k)), "{k}: {res}");
    }
    // the hits are exactly the blocks whose exact score reaches the floor
    let w = r
        .read()
        .vector_weights(WeightScope::Read(&Scope::Canon))
        .unwrap();
    let src = rdy_model::BlockId::parse(&id("$src")).unwrap();
    let mut want = BTreeSet::new();
    let mut below = 0;
    for b in r.read().materialized(&Scope::Canon).unwrap().iter() {
        if b.id == src {
            continue;
        }
        let s = score_pair(
            &w,
            Doc::Block(&src),
            Doc::Block(&b.id),
            &mut VectorBudget::search(),
        )
        .unwrap();
        if s >= LIKE_FLOOR {
            want.insert(b.id.to_string());
        } else if s > 0.0 {
            below += 1;
        }
    }
    assert_eq!(found, want, "{res}");
    assert!(below > 0, "a block scores under the floor");
    assert!(!found.contains(&id("$far")), "{res}");
    // filters keep their meaning
    let terms = like(&r, &id("$src"), json!({"types": ["term"]}));
    assert_eq!(ids(&terms), [id("$t")]);
    let live = like(&r, &id("$src"), json!({"lifecycle": "deprecated"}));
    assert_eq!(ids(&live), [id("$dep")]);
    let claims = like(&r, &id("$src"), json!({"types": ["claim"]}));
    assert_eq!(claims["totals"]["types"], json!({"claim": 2}), "{claims}");
}

#[test]
fn vxs7_like_lines_keep_existing_unicode_windows() {
    let (_d, mut r) = project();
    let long = format!("{} narwhal tusks {}", "é".repeat(150), "ü".repeat(150));
    let many: Vec<String> = (0..12).map(|_| "narwhal".to_string()).collect();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "arctic", "title": "Arctic", "doc_type": "page"}},
            {"op": "create", "type": "claim", "id": "$src", "parent": "$doc", "body": "Narwhal tusks spiral."},
            {"op": "create", "type": "claim", "id": "$long", "parent": "$doc", "body": format!("first line\n{long}")},
            {"op": "create", "type": "claim", "id": "$many", "parent": "$doc", "body": many.join("\n")}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let c = w.created;
    let id = |k: &str| c[k].clone();
    let res = like(&r, &id("$src"), json!({}));
    // a long line is windowed around its first shown word, as a query's line is
    let l = hit(&res, &id("$long"));
    let q = r
        .read()
        .search(
            &Scope::Canon,
            &rdy_engine::SearchQuery {
                text: "narwhal".into(),
                ..Default::default()
            },
            None,
            Some(100_000),
        )
        .unwrap();
    let q = serde_json::to_value(q).unwrap();
    let ql = hit(&q, &id("$long"));
    assert_eq!(l["lines"], ql["lines"], "{res}");
    let text = l["lines"][0]["text"].as_str().unwrap();
    assert!(text.starts_with('…') && text.ends_with('…'), "{text}");
    assert_eq!(text.chars().count(), 242, "{text}");
    assert_eq!(l["lines"][0]["line"], 2);
    // ten lines at most, the rest counted
    let m = hit(&res, &id("$many"));
    assert_eq!(m["lines"].as_array().unwrap().len(), 10);
    assert_eq!(m["more_lines"], 2);
    assert_eq!(m["lines"], hit(&q, &id("$many"))["lines"]);
}

/// 120 claims that each hold `kelp` and the numbers `2026` and `12.5`, and one that also holds
/// `otters`: `kelp` is common (df 121 > 100, 10 × 121 > N) and never seeds.
fn common(r: &mut Rdy) -> BTreeMap<String, String> {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "bed", "title": "Bed", "doc_type": "page"}}),
        json!({"op": "create", "type": "claim", "id": "$o", "parent": "$doc", "body": "kelp otters 2026 12.5"}),
    ];
    for i in 0..120 {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$k{i}"), "parent": "$doc", "body": "kelp 2026 12.5"}));
    }
    let w = write(r, "work", Value::Array(ops)).unwrap();
    land(r, "work").unwrap();
    w.created
}

#[test]
fn vxs7_common_only_like_text_is_empty_not_refused() {
    let (_d, mut r) = project();
    let c = common(&mut r);
    // a text of common words only finds no candidate: no hits, no refusal (D-RY7B5)
    for text in ["kelp", "KELP kelp", "2026 12.5", "kelp 2026"] {
        let res = like_text(&r, &Scope::Canon, text);
        assert_eq!(res["total"], 0, "{text}: {res}");
        assert_eq!(res["matches"], json!([]), "{text}");
        assert!(res.get("next").is_none());
    }
    // numeric words never seed (D-ZDM0G), and a common-only block finds nothing either
    let res = like(&r, &c["$k0"], json!({}));
    assert_eq!(res["total"], 0, "{res}");
    // one rare word seeds, and the common words still score
    let res = like_text(&r, &Scope::Canon, "kelp otters 2026");
    assert_eq!(ids(&res), [c["$o"].clone()], "{res}");
    assert!(score(&res["matches"][0]) > 0.9, "{res}");
    // ranking query hits has no seeds: every common hit scores
    let ranked = run(
        &r,
        &Scope::Canon,
        &input(Some("kelp"), None, None, json!({"sort": "relevance"})),
    );
    assert_eq!(ranked["total"], 121, "{ranked}");
    for m in ranked["matches"].as_array().unwrap() {
        assert!(score(m) > 0.0, "{m}");
    }
}

/// The first page of `like` over `source` that holds exactly one entry and a next cursor.
fn one_entry_page(r: &Rdy, source: &str) -> (Value, String) {
    let i = input(None, Some(source), None, json!({}));
    for max in 1..2000 {
        let Ok(res) = r.read().search_input(&Scope::Canon, &i, None, Some(max)) else {
            continue;
        };
        let res = serde_json::to_value(res).unwrap();
        if res["matches"].as_array().unwrap().len() == 1
            && let Some(next) = res["next"].as_str().map(str::to_string)
        {
            return (res, next);
        }
    }
    panic!("no one-entry page")
}

#[test]
fn vxs7_like_cursor_goes_stale_when_the_source_regroups_hits() {
    let (_d, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "fen", "title": "Fen", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Reeds"}},
            {"op": "create", "type": "claim", "id": "$src", "parent": "$s", "body": "alpha beta"},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "alpha\nbeta"},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "alpha\ngamma"},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "alpha!\ndelta"},
            {"op": "create", "type": "claim", "id": "$f1", "parent": "$s", "body": "Herons wade."},
            {"op": "create", "type": "claim", "id": "$f2", "parent": "$s", "body": "Otters swim."},
            {"op": "create", "type": "claim", "id": "$f3", "parent": "$s", "body": "Moss grows."},
            {"op": "create", "type": "claim", "id": "$f4", "parent": "$s", "body": "Frogs croak."},
            {"op": "create", "type": "claim", "id": "$f5", "parent": "$s", "body": "Eels hide."},
            {"op": "create", "type": "claim", "id": "$f6", "parent": "$s", "body": "Newts rest."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let id = |k: &str| w.created[k].clone();
    let order = like(&r, &id("$src"), json!({}));
    assert_eq!(ids(&order), [id("$a"), id("$b"), id("$c")], "{order}");
    assert_eq!(order["matches"].as_array().unwrap().len(), 3, "{order}");

    // a score drift alone (a filler edit moves the weights) keeps the cursor
    let (first, next) = one_entry_page(&r, &id("$src"));
    assert_eq!(first["matches"][0]["id"], id("$a"), "{first}");
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": id("$f1"), "body": "Herons wade slowly."}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let i = input(None, Some(&id("$src")), None, json!({}));
    let more = r
        .read()
        .search_input(&Scope::Canon, &i, Some(&next), Some(100_000))
        .unwrap();
    let more = serde_json::to_value(more).unwrap();
    assert_eq!(ids(&more), [id("$b"), id("$c")], "{more}");

    // the source now shares only alpha: a and b show the same line and collapse, so the old
    // cursor's offset names a different entry and must go stale
    let (_, next) = one_entry_page(&r, &id("$src"));
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": id("$src"), "body": "alpha"}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let regrouped = like(&r, &id("$src"), json!({}));
    assert_eq!(
        ids(&regrouped),
        [id("$a"), id("$b"), id("$c")],
        "{regrouped}"
    );
    assert_eq!(
        regrouped["matches"].as_array().unwrap().len(),
        2,
        "{regrouped}"
    );
    let err = r
        .read()
        .search_input(&Scope::Canon, &i, Some(&next), Some(100_000))
        .unwrap_err();
    assert_eq!(err.code, "E_PAGE_STALE", "{err:?}");
}
