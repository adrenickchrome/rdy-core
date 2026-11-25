//! vxs9 (D-ZX5S6, D-AT49D, D-5BM47, D-ZDM0G, D-76Y9H): the same committed blocks give the same
//! vector bytes and the same advice and search replies, whatever the run, the cache, the open,
//! the index or the order the blocks went in. One fixed project with fixed ids holds two docs of
//! claims that share words and a link target. The stable payload of a read is every block's
//! lexical and link vector and norm as binary64 bits, the briefing's vector rows and skipped
//! count, and the `like`, `like_text` and `sort: relevance` replies; session tokens, cursors and
//! leases are compared on their own, never as part of it. The platform golden builds a store from
//! the blocks of `fixtures/vectors/m5a-v1.json` and pins its weights, scores, order and reasons
//! exactly: no value is rounded or normalized, and a difference on a system is a failure.
use rdy_engine::brief::Coverage;
use rdy_engine::consequence::write_and_report;
use rdy_engine::engine::Local;
use rdy_engine::search::{SearchFilters, SearchInput};
use rdy_engine::vectors::{Doc, VectorBudget, WeightScope, WeightStats, round3, score_pair};
use rdy_engine::*;
use rdy_model::{Block, BlockId, FieldValue, Link};
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// The view that holds the project's blocks.
const VIEW: &str = "det";

fn view() -> Scope {
    Scope::View(VIEW.into())
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn hex(v: f64) -> String {
    format!("{:016x}", v.to_bits())
}

/// One block of the fixed project: id, type, parent, body, fields and describes targets.
type Spec = (
    &'static str,
    &'static str,
    Option<&'static str>,
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static [&'static str],
);

/// Doc `harbor` (two sections) and doc `reef` (one): eleven content blocks that share the kelp,
/// otter, urchin, reef and gauge words, and a term that five of them describe. Parents come
/// before their children.
const SPECS: [Spec; 17] = [
    (
        "d-0000A",
        "doc",
        None,
        "",
        &[
            ("name", "harbor"),
            ("title", "Harbor"),
            ("doc_type", "overview"),
        ],
        &[],
    ),
    (
        "s-0000A",
        "section",
        Some("d-0000A"),
        "",
        &[("title", "Shore")],
        &[],
    ),
    (
        "t-00001",
        "term",
        Some("s-0000A"),
        "Measures the water level.",
        &[("name", "Tide gauge")],
        &[],
    ),
    (
        "c-00001",
        "claim",
        Some("s-0000A"),
        "Kelp forests shelter otters near the harbor.",
        &[],
        &["t-00001"],
    ),
    (
        "c-00002",
        "claim",
        Some("s-0000A"),
        "Kelp forests feed urchins in the harbor.",
        &[],
        &[],
    ),
    (
        "c-00003",
        "claim",
        Some("s-0000A"),
        "Otters crack urchins on the harbor rocks.",
        &[],
        &["t-00001"],
    ),
    (
        "c-00004",
        "claim",
        Some("s-0000A"),
        "The tide gauge reads the harbor water level.",
        &[],
        &["t-00001"],
    ),
    (
        "s-0000B",
        "section",
        Some("d-0000A"),
        "",
        &[("title", "Piers")],
        &[],
    ),
    (
        "c-00005",
        "claim",
        Some("s-0000B"),
        "Gulls rest on the pier pilings at dawn.",
        &[],
        &[],
    ),
    (
        "d-0000B",
        "doc",
        None,
        "",
        &[
            ("name", "reef"),
            ("title", "Reef"),
            ("doc_type", "overview"),
        ],
        &[],
    ),
    (
        "s-0000C",
        "section",
        Some("d-0000B"),
        "",
        &[("title", "Life")],
        &[],
    ),
    (
        "c-00011",
        "claim",
        Some("s-0000C"),
        "Urchins graze kelp on the reef.",
        &[],
        &[],
    ),
    (
        "c-00012",
        "claim",
        Some("s-0000C"),
        "Otters rest on the reef at low tide.",
        &[],
        &["t-00001"],
    ),
    (
        "c-00013",
        "claim",
        Some("s-0000C"),
        "Coral polyps build the reef slowly.",
        &[],
        &[],
    ),
    (
        "c-00014",
        "claim",
        Some("s-0000C"),
        "The reef gauge logs the water level hourly.",
        &[],
        &["t-00001"],
    ),
    (
        "c-00015",
        "claim",
        Some("s-0000C"),
        "Kelp forests and otters keep urchins in check.",
        &[],
        &[],
    ),
    (
        "c-00016",
        "claim",
        Some("s-0000C"),
        "Kelp forests and otters keep urchins in check.",
        &[],
        &[],
    ),
];

fn block_of(spec: &Spec, order: &str) -> Block {
    let (i, ty, parent, body, fields, describes) = *spec;
    let mut b = raw(i, ty, parent, order, fields, body);
    if !describes.is_empty() {
        b.links.insert(
            "describes".into(),
            describes
                .iter()
                .map(|t| Link {
                    target: id(t),
                    part: None,
                })
                .collect(),
        );
    }
    b
}

/// The fixed blocks, each with its order among its siblings.
fn world() -> Vec<Block> {
    let mut seen: BTreeMap<Option<&str>, usize> = BTreeMap::new();
    SPECS
        .iter()
        .map(|s| {
            let n = seen.entry(s.2).or_default();
            *n += 1;
            block_of(s, &format!("a{n:02}"))
        })
        .collect()
}

/// A project at `dir` with the fixed blocks in view [`VIEW`].
fn build(dir: &Path) -> Rdy {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    raw_commit(&mut r, VIEW, world());
    r
}

fn filters(f: Value) -> SearchFilters {
    serde_json::from_value(f).unwrap()
}

fn search(r: &Rdy, scope: &Scope, input: SearchInput) -> Value {
    let res = r
        .read()
        .search_input(scope, &input, None, Some(1_000_000))
        .unwrap_or_else(|e| panic!("{e}"));
    serde_json::to_value(res).unwrap()
}

fn like(source: &str) -> SearchInput {
    SearchInput::new(
        None,
        Some(source.into()),
        None,
        false,
        false,
        filters(json!({})),
    )
    .unwrap()
}

fn like_text(text: &str) -> SearchInput {
    SearchInput::new(
        None,
        None,
        Some(text.into()),
        false,
        false,
        filters(json!({})),
    )
    .unwrap()
}

fn relevance(text: &str) -> SearchInput {
    SearchInput::new(
        Some(text.into()),
        None,
        None,
        false,
        false,
        filters(json!({"sort": "relevance"})),
    )
    .unwrap()
}

/// Every block's counts, lexical and link vectors and norms in `scope`, as exact bits, with the
/// corpus counts.
fn vector_bytes(r: &Rdy, scope: &Scope) -> Value {
    let st = r.read();
    let w = st.vector_weights(WeightScope::Read(scope)).unwrap();
    let m = st.materialized(scope).unwrap();
    let mut ids: Vec<BlockId> = m.iter().map(|b| b.id.clone()).collect();
    ids.sort();
    let mut blocks = serde_json::Map::new();
    for b in &ids {
        let d = Doc::Block(b);
        let norm = |n: Option<f64>| n.map_or(Value::Null, |n| json!(hex(n)));
        let lexical: Vec<Value> = w
            .lexical(d)
            .unwrap()
            .into_iter()
            .map(|(k, v)| json!([k, hex(v)]))
            .collect();
        let link: Vec<Value> = w
            .link(d)
            .unwrap()
            .into_iter()
            .map(|(k, v)| json!([k.as_str(), hex(v)]))
            .collect();
        let counts: Vec<Value> = w
            .counts(d)
            .map(|c| c.iter().map(|(k, n)| json!([k, n])).collect())
            .unwrap_or_default();
        blocks.insert(
            b.to_string(),
            json!({
                "counts": counts,
                "lexical": lexical,
                "link": link,
                "lexical_norm": norm(w.lexical_norm(d).unwrap()),
                "link_norm": norm(w.link_norm(d).unwrap()),
            }),
        );
    }
    json!({
        "corpus_words": w.corpus_words(),
        "corpus_links": w.corpus_links(),
        "blocks": blocks,
    })
}

/// The briefing of `cov` in `scope`: its stable vector payload, and its token on its own.
fn brief_payload(r: &Rdy, scope: &Scope, cov: &[&str]) -> (Value, String) {
    let b = r
        .read()
        .brief(
            scope,
            &Coverage {
                ids: cov.iter().map(|s| s.to_string()).collect(),
                path: None,
            },
            Some(1_000_000),
        )
        .unwrap();
    let v = serde_json::to_value(&b).unwrap();
    (
        json!({
            "vector_neighbors": v.get("vector_neighbors").cloned().unwrap_or(Value::Null),
            "vector_neighbors_skipped": v.get("vector_neighbors_skipped").cloned().unwrap_or(Value::Null),
        }),
        b.base.token,
    )
}

/// The blocks of section Shore that a briefing covers.
const SHORE: [&str; 5] = ["t-00001", "c-00001", "c-00002", "c-00003", "c-00004"];

/// The stable payload of the advice and search replies in `scope`: the briefing rows of section
/// Shore's blocks, two `like` sources, one `like_text` and one `sort: relevance` query. Each reply is
/// whole but for its cursor; none of them holds a token. `at` gives the id a fixed id has in
/// this store.
fn reply_bytes(r: &Rdy, scope: &Scope, at: &dyn Fn(&str) -> String) -> Value {
    let cov: Vec<String> = SHORE.iter().map(|i| at(i)).collect();
    let cov: Vec<&str> = cov.iter().map(String::as_str).collect();
    let (brief, _) = brief_payload(r, scope, &cov);
    let mut out = json!({ "brief": brief });
    for (name, input) in [
        ("like c-00001", like(&at("c-00001"))),
        ("like c-00012", like(&at("c-00012"))),
        (
            "like_text",
            like_text("Otters and kelp forests by the tide gauge."),
        ),
        ("relevance", relevance("forests")),
    ] {
        let mut v = search(r, scope, input);
        assert!(v.get("next").is_none(), "one page: {v}");
        assert!(
            v["matches"].as_array().is_some_and(|m| !m.is_empty()),
            "{name}: populated: {v}"
        );
        v.as_object_mut().unwrap().remove("next");
        out[name] = v;
    }
    assert!(
        out["brief"]["vector_neighbors"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "{out}"
    );
    out
}

/// The full stable payload of a read: vector bytes and reply bytes.
fn payload(r: &Rdy) -> Value {
    json!({
        "vectors": vector_bytes(r, &view()),
        "replies": reply_bytes(r, &view(), &|i| i.to_string()),
    })
}

/// The write receipt of two new claims through `e`, with each created id named by its
/// placeholder: its vector rows and skipped count, then the rest of the receipt's keys.
fn receipt(e: &Local) -> Value {
    let ops = parse_test_ops(json!([
        {"op": "create", "type": "claim", "id": "$n1", "parent": "s-0000B", "body": "Kelp forests shelter otters by the pier.", "links": {"describes": [{"target": "t-00001"}]}},
        {"op": "create", "type": "claim", "id": "$n2", "parent": "s-0000B", "body": "Urchins graze the reef kelp at the pier."}
    ]))
    .unwrap();
    let v = write_and_report(e, VIEW, None, move |r| {
        r.write(
            &writer(),
            WriteRequest {
                view: VIEW.into(),
                ops,
                justification: None,
                message: String::new(),
            },
        )
    })
    .unwrap();
    let names: BTreeMap<String, String> = v["created"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, x)| (x.as_str().unwrap().to_string(), k.clone()))
        .collect();
    let rows: Vec<Value> = v["vector_neighbors"]
        .as_array()
        .unwrap_or_else(|| panic!("write rows: {v}"))
        .iter()
        .cloned()
        .map(|mut row| {
            for k in ["for", "block"] {
                if let Some(n) = row[k].as_str().and_then(|s| names.get(s)) {
                    row[k] = json!(n);
                }
            }
            row
        })
        .collect();
    assert!(!rows.is_empty(), "{v}");
    json!({
        "vector_neighbors": rows,
        "vector_neighbors_skipped": v.get("vector_neighbors_skipped").cloned().unwrap_or(Value::Null),
    })
}

/// vxs9: two stores made from the same blocks, each read twice, give the same vector bits and the
/// same advice and search replies, and the same write receipt for the same write. Their briefing
/// tokens are compared on their own: each is a token of its own store.
#[test]
fn vxs9_same_store_has_same_vector_and_reply_bytes_across_runs() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (ra, rb) = (build(a.path()), build(b.path()));
    let first = payload(&ra);
    assert_eq!(payload(&ra), first, "a second read of one store");
    assert_eq!(payload(&rb), first, "a second store of the same blocks");
    // the briefing tokens are session values, outside the payload; each is a live token
    let (pa, ta) = brief_payload(&ra, &view(), &SHORE);
    let (pb, tb) = brief_payload(&rb, &view(), &SHORE);
    assert_eq!(pa, pb);
    assert!(!ta.is_empty() && !tb.is_empty());
    // the same write in each store gives the same receipt rows
    let (ea, eb) = (Local::new(ra), Local::new(rb));
    let wa = receipt(&ea);
    assert_eq!(
        receipt(&eb),
        wa,
        "the same write's receipt in the second store"
    );
}

/// vxs9: one store keeps its vector bytes and replies through a cold weight cache, a close and
/// reopen, and a reindex. The cold read builds its weights lazily again: before any read asks, a
/// new snapshot's weights hold nothing.
#[test]
fn vxs9_reopen_reindex_and_cold_cache_keep_vector_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = build(dir.path());
    let warm = payload(&r);
    // a cold cache: the caches are dropped, and the new weights start empty
    r.invalidate();
    let w = r.read().vector_weights(WeightScope::Read(&view())).unwrap();
    assert_eq!(w.stats(), WeightStats::default(), "nothing computed yet");
    drop(w);
    assert_eq!(payload(&r), warm, "a cold cache");
    // a close and a reopen
    r.close().unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    assert_eq!(payload(&r), warm, "a reopen");
    // a reindex, then a reopen of the reindexed store
    r.reindex().unwrap();
    assert_eq!(payload(&r), warm, "a reindex");
    r.close().unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    assert_eq!(payload(&r), warm, "a reopen after the reindex");
}

/// The create ops of [`SPECS`] from `from` to `to`, without their links: each block by the
/// placeholder of its own id, and a parent of an earlier batch by its created id in `made`.
fn ops_of(from: usize, to: usize, made: &BTreeMap<String, String>) -> Value {
    let batch = &SPECS[from..to];
    Value::Array(
        batch
            .iter()
            .map(|&(i, ty, parent, body, fields, _)| {
                let mut op = json!({"op": "create", "type": ty, "id": format!("${i}")});
                if let Some(p) = parent {
                    op["parent"] = if batch.iter().any(|s| s.0 == p) {
                        json!(format!("${p}"))
                    } else {
                        json!(made[p])
                    };
                }
                if !body.is_empty() {
                    op["body"] = json!(body);
                }
                if !fields.is_empty() {
                    let f: serde_json::Map<String, Value> = fields
                        .iter()
                        .map(|(k, v)| (k.to_string(), json!(v)))
                        .collect();
                    op["fields"] = Value::Object(f);
                }
                op
            })
            .collect(),
    )
}

/// vxs9: weights carried across a store's writes equal the weights rebuilt from the same blocks
/// committed at once, and the order the blocks go into a store changes no byte. Store A gets the
/// project in three engine writes, each read before the next, so each snapshot carries the last
/// one's counts; stores B and C get A's final blocks, as they are, in one commit each: B in
/// reverse order within each depth, C in A's order.
#[test]
fn vxs9_carry_rebuild_and_reordered_map_insertion_match() {
    let a = tempfile::tempdir().unwrap();
    let mut ra = Rdy::init(a.path(), "rdy-standard").unwrap();
    // three batches: harbor, its piers, the reef; each read in between, so the next carries
    let mut made: BTreeMap<String, String> = BTreeMap::new();
    for (from, to) in [(0, 7), (7, 9), (9, SPECS.len())] {
        let w = write(&mut ra, VIEW, ops_of(from, to, &made)).unwrap();
        for (k, v) in &w.created {
            made.insert(k.trim_start_matches('$').to_string(), v.clone());
        }
        let _ = vector_bytes(&ra, &view());
    }
    // the describes links, written last: a link changes link counts on a carried snapshot
    let links: Vec<Value> = SPECS
        .iter()
        .filter(|s| !s.5.is_empty())
        .map(|s| {
            json!({"op": "update", "id": made[s.0], "links": {"describes": s.5.iter().map(|t| json!({"target": made[*t]})).collect::<Vec<_>>()}})
        })
        .collect();
    write(&mut ra, VIEW, Value::Array(links)).unwrap();
    let carried = vector_bytes(&ra, &view());
    let st = ra.read();
    let m = st.materialized(&view()).unwrap();
    let blocks: Vec<Block> = m.iter().cloned().collect();
    drop(st);
    let depth = |b: &Block| {
        let mut d = 0;
        let mut at = b.parent.clone();
        while let Some(p) = at {
            d += 1;
            at = blocks
                .iter()
                .find(|x| x.id == p)
                .and_then(|x| x.parent.clone());
        }
        d
    };
    let mut reversed = blocks.clone();
    reversed.reverse();
    reversed.sort_by_key(depth);
    assert_ne!(
        reversed.iter().map(|b| &b.id).collect::<Vec<_>>(),
        blocks.iter().map(|b| &b.id).collect::<Vec<_>>(),
        "another insertion order"
    );
    let mut forward = blocks.clone();
    forward.sort_by_key(depth);
    for (name, order) in [("reversed", reversed), ("forward", forward)] {
        let d = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(d.path(), "rdy-standard").unwrap();
        raw_commit(&mut r, VIEW, order);
        assert_eq!(
            vector_bytes(&r, &view()),
            carried,
            "{name}: rebuilt weights"
        );
        let at = |i: &str| made[i].clone();
        assert_eq!(
            reply_bytes(&r, &view(), &at),
            reply_bytes(&ra, &view(), &at),
            "{name}: the replies"
        );
    }
    // the carried snapshot reads as the rebuilt one after its caches are dropped
    ra.invalidate();
    assert_eq!(vector_bytes(&ra, &view()), carried, "rebuilt in place");
}

/// The vector golden: its blocks, constants and expected bits.
fn golden() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/vectors/m5a-v1.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

/// The golden's blocks, as a store holds them: each with its order among its siblings.
fn golden_blocks(g: &Value) -> Vec<Block> {
    let mut seen: BTreeMap<Option<String>, usize> = BTreeMap::new();
    g["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            let parent = v["parent"].as_str().map(str::to_string);
            let n = seen.entry(parent.clone()).or_default();
            *n += 1;
            let mut b = raw(
                v["id"].as_str().unwrap(),
                v["type"].as_str().unwrap(),
                parent.as_deref(),
                &format!("a{n:02}"),
                &[],
                v["body"].as_str().unwrap_or(""),
            );
            for (k, x) in v["fields"].as_object().into_iter().flatten() {
                let f = match x {
                    Value::String(t) => FieldValue::Str(t.clone()),
                    Value::Array(items) => FieldValue::List(
                        items
                            .iter()
                            .map(|i| FieldValue::Str(i.as_str().unwrap().to_string()))
                            .collect(),
                    ),
                    other => panic!("a fixture field is a string or a list: {other}"),
                };
                b.fields.insert(k.clone(), f);
            }
            for (k, ts) in v["links"].as_object().into_iter().flatten() {
                b.links.insert(
                    k.clone(),
                    ts.as_array()
                        .unwrap()
                        .iter()
                        .map(|t| Link {
                            target: id(t.as_str().unwrap()),
                            part: None,
                        })
                        .collect(),
                );
            }
            b
        })
        .collect()
}

/// One hit or row with its exact score in `w`'s weights, as bits beside the shown score.
fn exact(w: &rdy_engine::vectors::VectorWeights, from: &str, to: &str) -> String {
    hex(score_pair(
        w,
        Doc::Block(&id(from)),
        Doc::Block(&id(to)),
        &mut VectorBudget::search(),
    )
    .unwrap())
}

/// What the golden store gives, by exact value: the `like` hits of every content source, by id
/// (id, shown and exact score, words, links, in the reply's order), and the order and scores of
/// one `sort: relevance` query.
fn golden_replies(r: &Rdy, scope: &Scope) -> Value {
    let st = r.read();
    let w = st.vector_weights(WeightScope::Read(scope)).unwrap();
    let m = st.materialized(scope).unwrap();
    let mut likes = serde_json::Map::new();
    let pack = st.pack();
    let mut sources: Vec<&Block> = m
        .iter()
        .filter(|b| rdy_engine::vector_neighbors::eligible_source(&m, pack, b))
        .collect();
    sources.sort_by(|a, b| a.id.cmp(&b.id));
    for b in sources {
        let v = search(r, scope, like(b.id.as_str()));
        let hits: Vec<Value> = v["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                let to = h["id"].as_str().unwrap();
                assert_eq!(
                    h["score"].as_f64().unwrap(),
                    round3(
                        score_pair(
                            &w,
                            Doc::Block(&b.id),
                            Doc::Block(&id(to)),
                            &mut VectorBudget::search()
                        )
                        .unwrap()
                    ),
                    "{to}"
                );
                json!({
                    "id": to,
                    "score": h["score"],
                    "exact": exact(&w, b.id.as_str(), to),
                    "words": h["words"],
                    "links": h["links"],
                })
            })
            .collect();
        likes.insert(b.id.to_string(), Value::Array(hits));
    }
    let ranked = search(r, scope, relevance("gateway request ledger"));
    let order: Vec<Value> = ranked["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            json!({
                "id": h["id"],
                "score": h["score"],
                "also": h.get("also").cloned().unwrap_or(json!([])),
            })
        })
        .collect();
    json!({
        "like": likes,
        "relevance": {"query": "gateway request ledger", "matches": order},
    })
}

/// The advice of the fixed project of this file, by exact value: the briefing rows of section
/// Shore's blocks, each with its exact score bits, and the write receipt of [`receipt`]. The
/// golden's own blocks share one section, so a briefing of them has no vector row: every block
/// there is a section neighbor.
fn advice_replies() -> Value {
    let dir = tempfile::tempdir().unwrap();
    let r = build(dir.path());
    let (brief, _) = brief_payload(&r, &view(), &SHORE);
    let st = r.read();
    let w = st.vector_weights(WeightScope::Read(&view())).unwrap();
    let rows: Vec<Value> = brief["vector_neighbors"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut row| {
            let x = exact(
                &w,
                row["for"].as_str().unwrap(),
                row["block"].as_str().unwrap(),
            );
            row["exact"] = json!(x);
            row
        })
        .collect();
    drop((w, st));
    json!({
        "covers": SHORE,
        "vector_neighbors": rows,
        "vector_neighbors_skipped": brief["vector_neighbors_skipped"],
        "receipt": receipt(&Local::new(r)),
    })
}

/// vxs9: the golden's blocks in a store give, through the store's own read, exactly the golden's
/// corpus counts, idf and target weights, every block's counts, vectors and norms, and exactly
/// its `store` replies: every source's `like` hits with their exact score bits, words and links
/// in their order, and a relevance order. The fixed project of this file gives exactly the
/// golden's `store.advice`: its briefing rows with their exact bits, and a write receipt. The bits are the
/// same on every system; none is computed from another platform's run.
#[test]
fn vxs9_platform_golden_pins_weights_scores_order_and_reasons() {
    let g = golden();
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    raw_commit(&mut r, VIEW, golden_blocks(&g));
    let st = r.read();
    let w = st.vector_weights(WeightScope::Read(&view())).unwrap();
    let x = &g["expected"];
    assert_eq!(x["corpus_words"], json!(w.corpus_words()));
    assert_eq!(x["corpus_links"], json!(w.corpus_links()));
    for (word, want) in x["idf"].as_object().unwrap() {
        assert_eq!(want["df"], json!(w.df(word)), "df {word}");
        assert_eq!(want["seed"], json!(w.word_is_seed(word)), "seed {word}");
        assert_eq!(
            json!(hex(w.idf(word).unwrap().unwrap())),
            want["bits"],
            "idf {word}"
        );
    }
    for (t, want) in x["targets"].as_object().unwrap() {
        let t = id(t);
        assert_eq!(want["indeg"], json!(w.indeg(&t)), "indeg {t}");
        assert_eq!(
            json!(hex(w.target_weight(&t).unwrap().unwrap())),
            want["bits"],
            "weight {t}"
        );
    }
    drop((w, st));
    let bytes = vector_bytes(&r, &view());
    let per_block = x["blocks"].as_object().unwrap();
    assert_eq!(
        bytes["blocks"].as_object().unwrap().len(),
        per_block.len(),
        "the store holds the golden's blocks and no other"
    );
    for (b, want) in per_block {
        let got = &bytes["blocks"][b];
        for k in ["lexical", "link", "lexical_norm", "link_norm", "counts"] {
            assert_eq!(got[k], want[k], "{b} {k}");
        }
    }
    // the replies: scores, order and reasons, exactly
    let replies = golden_replies(&r, &view());
    let like_rows: usize = replies["like"]
        .as_object()
        .unwrap()
        .values()
        .map(|h| h.as_array().unwrap().len())
        .sum();
    assert!(
        like_rows >= 10,
        "the golden scores real neighbors: {replies}"
    );
    let advice = advice_replies();
    assert!(
        advice["vector_neighbors"]
            .as_array()
            .is_some_and(|rows| rows.len() >= 2),
        "{advice}"
    );
    assert_eq!(replies["like"], g["store"]["like"], "the like hits");
    assert_eq!(
        replies["relevance"], g["store"]["relevance"],
        "the relevance order"
    );
    assert_eq!(
        advice, g["store"]["advice"],
        "the briefing rows and the write receipt"
    );
}
