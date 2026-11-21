//! D-Y1WEY, D-0ZPAP, D-5BM47: a briefing lists vector neighbors after its neighbors. The rows of
//! every source sort together; they are advice and change no token, coverage, consequence,
//! suspect or plan item. A briefing of at most 20 covered content blocks scores each of them, with
//! at most two rows each and each neighbor once, under the covered block where it scores highest
//! (D-5Y64F); a wider one scores none and counts them all in `vector_neighbors_skipped`, as it
//! counts the ones a work guard stops. The rows page like neighbors, their order vouches for a
//! page and their scores do not, and a past landing or a proposal version has none.
use rdy_engine::brief::{Briefing, Coverage, PAGE_SECTIONS, VectorNeighbor};
use rdy_engine::plan::api::PlanQuery;
use rdy_engine::vectors::{WeightScope, WeightStats};
use rdy_engine::*;
use rdy_model::{Block, BlockId, CROCKFORD};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn work() -> Scope {
    Scope::View("work".into())
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn blk(i: &str, ty: &str, parent: &str, order: &str, body: &str) -> Block {
    raw(i, ty, Some(parent), order, &[], body)
}

fn doc(i: &str, name: &str, order: &str) -> Block {
    raw(
        i,
        "doc",
        None,
        order,
        &[("name", name), ("title", name), ("doc_type", "overview")],
        "",
    )
}

fn cov(ids: &[&str]) -> Coverage {
    Coverage {
        ids: ids.iter().map(|s| s.to_string()).collect(),
        path: None,
    }
}

/// The `n`th id with `prefix`, in id order for a fixed prefix.
fn nid(prefix: &str, n: usize) -> String {
    let mut body = [b'0'; 5];
    let mut v = n;
    for d in body.iter_mut().rev() {
        *d = CROCKFORD[v % 32];
        v /= 32;
    }
    format!("{prefix}-{}", std::str::from_utf8(&body).unwrap())
}

/// Doc `alpha` (d-0000A): section s-0000A with source S1 c-00001 and its section neighbor
/// c-00011, section s-0000B with source S2 c-00002 and its section neighbor c-00012, and section
/// s-0000D with c-00031 and c-00041. Doc `beta` (d-0000B): section s-0000C with c-00042, c-00032,
/// c-00043, c-00044 and c-00045. Every word of S2's side mirrors one of S1's (otter for zebra,
/// newt for yak, moose for walrus) in as many blocks, so mirrored rows score the same exact value.
/// Written raw to view `work`.
fn pair(r: &mut Rdy, view: &str) {
    raw_commit(r, view, pair_blocks());
}

fn pair_blocks() -> Vec<Block> {
    vec![
        doc("d-0000A", "alpha", "a1"),
        blk("s-0000A", "section", "d-0000A", "a1", ""),
        blk("c-00001", "claim", "s-0000A", "a1", "zebra walrus yak"),
        blk("c-00011", "claim", "s-0000A", "a2", "zebra walrus sibling"),
        blk("s-0000B", "section", "d-0000A", "a2", ""),
        blk("c-00002", "claim", "s-0000B", "a1", "otter moose newt"),
        blk("c-00012", "claim", "s-0000B", "a2", "otter moose sibling"),
        blk("s-0000D", "section", "d-0000A", "a3", ""),
        blk("c-00031", "claim", "s-0000D", "a1", "yak yak yak walrus"),
        blk("c-00041", "claim", "s-0000D", "a2", "zebra yak"),
        doc("d-0000B", "beta", "a2"),
        blk("s-0000C", "section", "d-0000B", "a1", ""),
        blk("c-00042", "claim", "s-0000C", "a1", "zebra yak"),
        blk("c-00032", "claim", "s-0000C", "a2", "newt moose"),
        blk("c-00043", "claim", "s-0000C", "a3", "otter newt"),
        blk("c-00044", "claim", "s-0000C", "a4", "otter newt newt"),
        blk("c-00045", "claim", "s-0000C", "a5", "zebra otter"),
    ]
}

fn vrow(
    r#for: &str,
    block: &str,
    score: f64,
    words: &[&str],
    doc: &str,
    summary: &str,
) -> VectorNeighbor {
    VectorNeighbor {
        r#for: r#for.into(),
        block: block.into(),
        score,
        words: words.iter().map(|s| s.to_string()).collect(),
        links: vec![],
        doc: doc.into(),
        summary: summary.into(),
    }
}

/// The rows of [`pair`] for covered c-00001 and c-00002, computed by hand from the weights (N =
/// 13 blocks with words; df 5 for zebra, otter, yak and newt, 3 for walrus and moose, 2 for
/// sibling). Each covered block keeps its two best rows among the neighbors it owns, and all
/// sources' rows sort together by exact score. S1 owns c-00031, c-00041, c-00042 and c-00045 (the
/// two sources tie on c-00045, and S1 comes first in reading order); S2 owns c-00032, c-00043 and
/// c-00044. The two rows at 0.725 have the same exact score: c-00041 shares its source's doc and
/// comes first. S1's c-00042 and c-00045 and S2's c-00044 are past the cap of two and show
/// nowhere.
fn pair_rows() -> Vec<VectorNeighbor> {
    let (a, b) = ("d-0000A", "d-0000B");
    vec![
        vrow(
            "c-00002",
            "c-00032",
            0.888,
            &["moose", "newt"],
            b,
            "newt moose",
        ),
        vrow(
            "c-00001",
            "c-00031",
            0.802,
            &["yak", "walrus"],
            a,
            "yak yak yak walrus",
        ),
        vrow(
            "c-00001",
            "c-00041",
            0.725,
            &["yak", "zebra"],
            a,
            "zebra yak",
        ),
        vrow(
            "c-00002",
            "c-00043",
            0.725,
            &["newt", "otter"],
            b,
            "otter newt",
        ),
    ]
}

fn json_of(b: &Briefing) -> Value {
    serde_json::to_value(b).unwrap()
}

#[test]
fn vxs3_brief_rows_follow_neighbors_and_keep_advice_separate() {
    let (_d, mut r) = project();
    pair(&mut r, "work");
    let b = r
        .read()
        .brief(&work(), &cov(&["c-00001", "c-00002"]), Some(1_000_000))
        .unwrap();
    assert!(!b.truncated && b.next.is_none(), "{:?}", b.omitted);
    assert_eq!(b.vector_neighbors, pair_rows());
    assert_eq!(b.vector_neighbors_skipped, 0);
    // the section neighbors stay in `neighbors` only: c-00011 would score 0.571 with S1
    let nb: Vec<&str> = b.neighbors.iter().map(|n| n.block.as_str()).collect();
    assert_eq!(nb, ["c-00011", "c-00012"]);
    assert_eq!(
        serde_json::to_value(&b.neighbors_by_doc).unwrap(),
        json!([{"doc": "d-0000A", "count": 2}])
    );
    let advised: BTreeSet<&str> = b
        .vector_neighbors
        .iter()
        .map(|v| v.block.as_str())
        .collect();
    assert!(advised.iter().all(|v| !nb.contains(v)), "{advised:?}");
    // a row serializes {for, block, score, words, links, doc, summary}, in that order
    let text = serde_json::to_string(&b).unwrap();
    assert!(
        text.contains(r#""vector_neighbors":[{"for":"c-00002","block":"c-00032","score":0.888,"words":["moose","newt"],"links":[],"doc":"d-0000B","summary":"newt moose"},"#),
        "{text}"
    );
    // after neighbors and their counts, before base; a zero skip count is left out
    let at = |k: &str| {
        text.find(&format!("\"{k}\":"))
            .unwrap_or_else(|| panic!("{k} in {text}"))
    };
    assert!(at("neighbors") < at("neighbors_by_doc"));
    assert!(at("neighbors_by_doc") < at("vector_neighbors"));
    assert!(at("vector_neighbors") < at("base"), "{text}");
    assert!(!text.contains("vector_neighbors_skipped"), "{text}");
    let n = PAGE_SECTIONS
        .iter()
        .position(|s| *s == "neighbors")
        .unwrap();
    assert_eq!(PAGE_SECTIONS[n + 1], "vector_neighbors");
    assert_eq!(PAGE_SECTIONS[n + 2], "base.blocks");
    assert_eq!(PAGE_SECTIONS.len(), n + 3);
    // advice only: the record, the token's coverage, the coverage section and governs hold no row
    assert_eq!(
        b.record.recorded.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([id("c-00001"), id("c-00002")])
    );
    assert_eq!(
        b.record.covers,
        BTreeSet::from([id("c-00001"), id("c-00002")])
    );
    assert_eq!(b.covers, 2);
    let coverage = serde_json::to_string(&b.coverage).unwrap();
    for v in &advised {
        assert!(!coverage.contains(v), "{v} is in coverage: {coverage}");
    }
    assert!(b.governs.is_empty() && b.dependents.is_empty() && b.will_go_stale.is_empty());
    assert!(b.docs.is_empty());
    // one source alone keeps its own rows, in the same order
    let one = r
        .read()
        .brief(&work(), &cov(&["c-00001"]), Some(1_000_000))
        .unwrap();
    let mine: Vec<VectorNeighbor> = pair_rows()
        .into_iter()
        .filter(|v| v.r#for == "c-00001")
        .collect();
    assert_eq!(one.vector_neighbors, mine);
    assert_eq!(one.base.token, {
        // the same token as a briefing in a store with no vector neighbor at all
        let (_d2, mut bare) = project();
        raw_commit(
            &mut bare,
            "work",
            pair_blocks()
                .into_iter()
                .filter(|b| ["d-0000A", "s-0000A", "c-00001"].contains(&b.id.as_str()))
                .collect(),
        );
        let alone = bare
            .read()
            .brief(&work(), &cov(&["c-00001"]), None)
            .unwrap();
        assert!(alone.vector_neighbors.is_empty());
        alone.base.token
    });
}

/// Doc `many` (d-0000M) with sections s-0000P (first) and s-0000Q, and `n` claims: claim `k` is
/// in s-0000P when `k` is even and s-0000Q when odd, at order `a<k>`, with id `c-<1000 - k>` (so
/// id order is the reverse of the claims' order) and body `pair<k / 2>`: claims `2j` and `2j + 1`
/// share one word and are each other's only vector neighbor. Written raw to view `work`. The
/// claims, by `k`.
fn pairs(r: &mut Rdy, n: usize) -> Vec<String> {
    let mut blocks = vec![
        doc("d-0000M", "many", "a1"),
        blk("s-0000P", "section", "d-0000M", "a1", ""),
        blk("s-0000Q", "section", "d-0000M", "a2", ""),
    ];
    let mut claims = vec![];
    for k in 0..n {
        let c = nid("c", 1000 - k);
        let s = if k % 2 == 0 { "s-0000P" } else { "s-0000Q" };
        blocks.push(blk(
            &c,
            "claim",
            s,
            &format!("a{k:05}"),
            &format!("pair{}", k / 2),
        ));
        claims.push(c);
    }
    raw_commit(r, "work", blocks);
    claims
}

/// Doc `wide` (d-0000G) with 100 sections of six claims each: claim `i` is in section `i / 6` and
/// holds the 64 words `w<64g>` to `w<64g + 63>` of its group `g = i % 6`, so each word is in 100
/// blocks (a seed) and each claim's 99 group members are its vector neighbors. Reading order is
/// claim order. Written raw to view `work`. The claims, in order.
fn wide(r: &mut Rdy) -> Vec<String> {
    let mut blocks = vec![doc("d-0000G", "wide", "a1")];
    let mut claims = vec![];
    for j in 0..100 {
        let s = nid("s", j + 1);
        blocks.push(blk(&s, "section", "d-0000G", &format!("a{j:05}"), ""));
        for k in 0..6 {
            let i = 6 * j + k;
            let g = i % 6;
            let words: Vec<String> = (0..64).map(|w| format!("w{}", 64 * g + w)).collect();
            let c = nid("c", i + 1);
            blocks.push(blk(&c, "claim", &s, &format!("a{i:05}"), &words.join(" ")));
            claims.push(c);
        }
    }
    raw_commit(r, "work", blocks);
    claims
}

#[test]
fn vxs3_twenty_sources_score_wider_briefings_skip_and_guard_counts_are_explicit() {
    // 20 covered claims: every one is scored, and each lists its partner, its only neighbor
    let path = Coverage {
        ids: vec![],
        path: Some("many".into()),
    };
    let (_d, mut r) = project();
    let claims = pairs(&mut r, 20);
    let b = r.read().brief(&work(), &path, Some(usize::MAX)).unwrap();
    assert_eq!(b.covers, 23);
    assert_eq!(b.vector_neighbors_skipped, 0);
    assert_eq!(b.vector_neighbors.len(), 20, "one row for each source");
    for v in &b.vector_neighbors {
        let k = claims.iter().position(|c| *c == v.r#for).unwrap();
        assert_eq!(v.block, claims[k ^ 1], "{v:?}");
        assert_eq!(
            (v.score, v.words.clone()),
            (1.0, vec![format!("pair{}", k / 2)])
        );
    }
    // 26 covered claims: none is scored, whatever the reading order, and all 26 count
    let (_d, mut r) = project();
    pairs(&mut r, 26);
    let b = r.read().brief(&work(), &path, Some(usize::MAX)).unwrap();
    assert_eq!(b.covers, 29);
    assert!(b.vector_neighbors.is_empty());
    assert_eq!(b.vector_neighbors_skipped, 26);
    assert_eq!(json_of(&b)["vector_neighbors_skipped"], json!(26));

    // the section work guard: each source charges 64 seeds of 100 postings each (6,400), so 20
    // sources (128,000) fit the 2,000,000 posting incidences of a briefing and none is stopped.
    // The 20 are claims 0 to 19 of six groups: in each group the first source owns every member
    // at the same score and lists two; the second owns only the first, the others own nothing.
    let (_d, mut r) = project();
    let claims = wide(&mut r);
    let first = Coverage {
        ids: claims[..20].to_vec(),
        path: None,
    };
    let b = r.read().brief(&work(), &first, Some(usize::MAX)).unwrap();
    assert_eq!(b.vector_neighbors_skipped, 0);
    for (k, c) in claims[..20].iter().enumerate() {
        let n = b.vector_neighbors.iter().filter(|v| v.r#for == *c).count();
        let want = match k {
            0..6 => 2,
            6..12 => 1,
            _ => 0,
        };
        assert_eq!(n, want, "claim {k}");
        if (6..12).contains(&k) {
            let row = b.vector_neighbors.iter().find(|v| v.r#for == *c).unwrap();
            assert_eq!(
                row.block,
                claims[k - 6],
                "claim {k} owns the first of its group"
            );
        }
    }
    let blocks: BTreeSet<&str> = b
        .vector_neighbors
        .iter()
        .map(|v| v.block.as_str())
        .collect();
    assert_eq!(blocks.len(), b.vector_neighbors.len(), "each neighbor once");
    assert!(b.omitted.is_empty(), "{:?}", b.omitted);
    // the whole doc: 600 sources, none scored, all counted
    let path = Coverage {
        ids: vec![],
        path: Some("wide".into()),
    };
    let b = r.read().brief(&work(), &path, Some(usize::MAX)).unwrap();
    assert!(b.vector_neighbors.is_empty());
    assert_eq!(b.vector_neighbors_skipped, 600);
}

#[test]
fn vxs3_empty_fields_row_caps_and_no_seed_sources_have_exact_counts() {
    // no row and nothing skipped: both members are left out of the JSON
    let (_d, mut r) = project();
    pair(&mut r, "work");
    for c in [cov(&["s-0000A"]), cov(&["d-0000B"])] {
        let b = r.read().brief(&work(), &c, Some(1_000_000)).unwrap();
        assert!(b.vector_neighbors.is_empty(), "{:?}", c.ids);
        let v = json_of(&b);
        assert!(v.get("vector_neighbors").is_none(), "{v}");
        assert!(v.get("vector_neighbors_skipped").is_none(), "{v}");
        assert!(v.get("omitted").is_none(), "{v}");
    }

    // a source with twelve neighbors of one score keeps two, the first two in reading order; the
    // other ten count nowhere
    let (_d, mut r) = project();
    let mut blocks = vec![
        doc("d-0000A", "alpha", "a1"),
        blk("s-0000A", "section", "d-0000A", "a1", ""),
        blk("c-00001", "claim", "s-0000A", "a1", "capword"),
        doc("d-0000B", "beta", "a2"),
        blk("s-0000B", "section", "d-0000B", "a1", ""),
    ];
    // ids against reading order: the neighbor at order a<k> is c-<20 - k>
    let neighbors: Vec<String> = (0..12).map(|k| nid("c", 20 - k)).collect();
    for (k, n) in neighbors.iter().enumerate() {
        blocks.push(blk(n, "claim", "s-0000B", &format!("a{k:02}"), "capword"));
    }
    raw_commit(&mut r, "work", blocks);
    let b = r
        .read()
        .brief(&work(), &cov(&["c-00001"]), Some(1_000_000))
        .unwrap();
    let shown: Vec<&str> = b
        .vector_neighbors
        .iter()
        .map(|v| v.block.as_str())
        .collect();
    let want: Vec<&str> = neighbors[..2].iter().map(String::as_str).collect();
    assert_eq!(shown, want);
    assert!(b.vector_neighbors.iter().all(|v| v.score == 1.0));
    assert_eq!(b.vector_neighbors_skipped, 0);
    let v = json_of(&b);
    assert!(v.get("vector_neighbors_skipped").is_none(), "{v}");
    assert!(v.get("omitted").is_none(), "{v}");

    // a source whose only word is common (df 102 > 100 and 10 x 102 > N = 104) has no seed: no
    // row and no skip. So does a source with no word at all.
    let (_d, mut r) = project();
    let mut blocks = vec![
        doc("d-0000A", "alpha", "a1"),
        blk("s-0000A", "section", "d-0000A", "a1", ""),
        blk("c-00001", "claim", "s-0000A", "a1", "common common"),
        blk("c-00002", "claim", "s-0000A", "a2", ""),
        doc("d-0000B", "beta", "a2"),
    ];
    for k in 0..101 {
        let s = nid("s", 100 + k);
        blocks.push(blk(&s, "section", "d-0000B", &format!("a{k:03}"), ""));
        blocks.push(blk(&nid("c", 200 + k), "claim", &s, "a1", "common"));
    }
    raw_commit(&mut r, "work", blocks);
    let b = r
        .read()
        .brief(&work(), &cov(&["c-00001", "c-00002"]), Some(1_000_000))
        .unwrap();
    assert!(b.vector_neighbors.is_empty());
    assert_eq!(b.vector_neighbors_skipped, 0);
    let v = json_of(&b);
    assert!(v.get("vector_neighbors").is_none(), "{v}");
    assert!(v.get("vector_neighbors_skipped").is_none(), "{v}");
}

/// Every page of the briefing of `c` at `max` tokens, first to last, each registered first.
fn pages(r: &Rdy, c: &Coverage, max: usize) -> Vec<Briefing> {
    let first = r.read().brief(&work(), c, Some(max)).unwrap();
    first.register(&r.briefs());
    let mut out = vec![first];
    while let Some(cursor) = out.last().unwrap().next.clone() {
        let p = r
            .read()
            .brief_page(&work(), &r.briefs(), &cursor, Some(max))
            .unwrap_or_else(|e| panic!("page {} at {max}: {e}", out.len()));
        out.push(p);
        assert!(out.len() < 1_000, "paging at {max} does not end");
    }
    out
}

/// How many detail entries a briefing shows, over every paged section.
fn shown(b: &Briefing) -> usize {
    b.governs.len()
        + b.dependents.len()
        + b.lineage.len()
        + b.will_go_stale.len()
        + b.suspects.len()
        + b.sources.len()
        + b.inflight.views.len()
        + b.inflight.proposals.len()
        + b.inflight.intents.len()
        + b.inflight.tensions.len()
        + b.neighbors.len()
        + b.vector_neighbors.len()
        + b.base.blocks.len()
}

#[test]
fn vxs3_pages_fit_and_show_each_vector_row_once() {
    let (_d, mut r) = project();
    pair(&mut r, "work");
    let c = cov(&["c-00001", "c-00002"]);
    let full = r.read().brief(&work(), &c, Some(1_000_000)).unwrap();
    assert_eq!(full.vector_neighbors, pair_rows());
    let at = PAGE_SECTIONS
        .iter()
        .position(|s| *s == "vector_neighbors")
        .unwrap();
    let mut cut_rows = false;
    for max in 1..=1_500 {
        let all = pages(&r, &c, max);
        let mut seen: Vec<VectorNeighbor> = vec![];
        for (i, p) in all.iter().enumerate() {
            let tokens = serde_json::to_string(p).unwrap().len() / 4;
            assert!(
                tokens <= max || shown(p) <= 1,
                "page {i} at {max}: {tokens} tokens with {} entries",
                shown(p)
            );
            assert_eq!(p.base.token, full.base.token);
            seen.extend(p.vector_neighbors.iter().cloned());
            // the first page counts the rows the budget cut from it
            if i == 0 && p.truncated && !p.vector_neighbors.is_empty() {
                let omitted = p.omitted.get("vector_neighbors").copied().unwrap_or(0);
                assert_eq!(
                    p.vector_neighbors.len() + omitted,
                    full.vector_neighbors.len(),
                    "at {max}"
                );
                cut_rows |= omitted > 0;
            }
        }
        assert_eq!(
            seen, full.vector_neighbors,
            "every row once, in order, at {max}"
        );
        let targets: BTreeSet<&str> = seen.iter().map(|v| v.block.as_str()).collect();
        assert_eq!(
            targets.len(),
            seen.len(),
            "each neighbor once over the pages, at {max}"
        );
        assert_eq!(all.last().unwrap().offsets[at], full.vector_neighbors.len());
    }
    assert!(cut_rows, "some budget cuts vector rows from a first page");
    // the cut measures the rows exactly: the whole JSON's size fits, one token less cuts
    let whole = serde_json::to_string(&full).unwrap();
    let tokens = whole.len() / 4;
    let fits = r.read().brief(&work(), &c, Some(tokens)).unwrap();
    assert_eq!(serde_json::to_string(&fits).unwrap(), whole);
    let cut = r.read().brief(&work(), &c, Some(tokens - 1)).unwrap();
    assert!(cut.truncated && cut.next.is_some());
}

/// The first page of the briefing of S1 and S2 in [`pair`] at a budget that cuts it, registered,
/// and its cursor.
fn cut_first(r: &Rdy) -> (Briefing, String) {
    let b = r
        .read()
        .brief(&work(), &cov(&["c-00001", "c-00002"]), Some(400))
        .unwrap();
    b.register(&r.briefs());
    let cursor = b
        .next
        .clone()
        .expect("the briefing does not fit 400 tokens");
    (b, cursor)
}

#[test]
fn vxs3_order_changes_stale_pages_but_score_drift_does_not() {
    type Change = fn(&mut Rdy);
    let changes: [(&str, Change); 3] = [
        ("a row goes", |r| {
            write(
                r,
                "work",
                json!([{"op": "update", "id": "c-00041", "body": "kiwi lemon"}]),
            )
            .unwrap();
        }),
        ("a row comes", |r| {
            raw_commit(
                r,
                "work",
                vec![blk("c-00046", "claim", "s-0000C", "a6", "zebra walrus yak")],
            );
        }),
        ("two rows swap", |r| {
            // c-00043 now holds two newts more: c-00044's row with S2 passes its row
            write(
                r,
                "work",
                json!([{"op": "update", "id": "c-00043", "body": "otter newt newt newt"}]),
            )
            .unwrap();
        }),
    ];
    for (what, change) in changes {
        let (_d, mut r) = project();
        pair(&mut r, "work");
        let (first, cursor) = cut_first(&r);
        change(&mut r);
        let again = r
            .read()
            .brief(&work(), &cov(&["c-00001", "c-00002"]), Some(1_000_000))
            .unwrap();
        assert_ne!(again.vector_neighbors, pair_rows(), "{what}");
        assert_eq!(
            again.base.token, first.base.token,
            "{what}: the token holds"
        );
        let e = r
            .read()
            .brief_page(&work(), &r.briefs(), &cursor, Some(400))
            .unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (
                E_PAGE_STALE,
                "the briefing moved since its first page: brief again"
            ),
            "{what}"
        );
    }
    // a doc and a claim with new words change N and every weight: every score drifts (0.888 to
    // 0.882, 0.802 to 0.797, 0.725 to 0.734) and the order stays, so the page holds
    let (_d, mut r) = project();
    pair(&mut r, "work");
    let (_first, cursor) = cut_first(&r);
    raw_commit(
        &mut r,
        "work",
        vec![
            doc("d-0000Z", "zeta", "a3"),
            blk("s-0000Z", "section", "d-0000Z", "a1", ""),
            blk("c-00091", "claim", "s-0000Z", "a1", "kiwi lemon"),
        ],
    );
    let drifted = r
        .read()
        .brief(&work(), &cov(&["c-00001", "c-00002"]), Some(1_000_000))
        .unwrap();
    let scores: Vec<f64> = drifted.vector_neighbors.iter().map(|v| v.score).collect();
    assert_eq!(scores, [0.882, 0.797, 0.734, 0.734]);
    let pairs_of = |rows: &[VectorNeighbor]| -> Vec<(String, String)> {
        rows.iter()
            .map(|v| (v.r#for.clone(), v.block.clone()))
            .collect()
    };
    assert_eq!(pairs_of(&drifted.vector_neighbors), pairs_of(&pair_rows()));
    let page = r
        .read()
        .brief_page(&work(), &r.briefs(), &cursor, Some(400))
        .unwrap();
    assert!(page.offsets.iter().sum::<usize>() > 0);
}

#[test]
fn vxs3_historical_briefs_omit_vectors_without_building_weights() {
    let (_d, mut r) = project();
    pair(&mut r, "work");
    land_with_a_tie(&mut r, "work");
    let first = r.read().canon_head().expect("a landing");
    raw_commit(
        &mut r,
        "more",
        vec![
            doc("d-0000Z", "zeta", "a3"),
            blk("s-0000Z", "section", "d-0000Z", "a1", ""),
            blk("c-00091", "claim", "s-0000Z", "a1", "kiwi lemon"),
        ],
    );
    land_with_a_tie(&mut r, "more");
    let head = r.read().canon_head().expect("a second landing");
    assert_ne!(first, head);
    let c = cov(&["c-00001", "c-00002"]);
    let unbuilt = |st: &ReadState, scope: &Scope| -> WeightStats {
        st.vector_weights(WeightScope::Read(scope)).unwrap().stats()
    };
    // a past landing: no rows, no count, and no weight computed for it
    let st = r.read();
    let past = Scope::At(first);
    let b = st.brief(&past, &c, Some(1_000_000)).unwrap();
    let v = json_of(&b);
    assert!(v.get("vector_neighbors").is_none(), "{v}");
    assert!(v.get("vector_neighbors_skipped").is_none(), "{v}");
    assert_eq!(b.neighbors.len(), 2, "the deterministic neighbors stay");
    assert_eq!(unbuilt(&st, &past), WeightStats::default());
    // a landing at canon's head reads canon: its rows are there
    let now = st.brief(&Scope::At(head), &c, Some(1_000_000)).unwrap();
    assert_eq!(now.vector_neighbors.len(), pair_rows().len());
    let canon = st.brief(&Scope::Canon, &c, Some(1_000_000)).unwrap();
    assert_eq!(
        canon
            .vector_neighbors
            .iter()
            .map(|v| (&v.r#for, &v.block))
            .collect::<Vec<_>>(),
        pair_rows()
            .iter()
            .map(|v| (&v.r#for, &v.block))
            .collect::<Vec<_>>()
    );
    assert_ne!(unbuilt(&st, &Scope::Canon), WeightStats::default());
    drop(st);
    // a proposal version: no rows and no weights either
    raw_commit(
        &mut r,
        "p",
        vec![blk("c-00092", "claim", "s-0000Z", "a2", "zebra walrus yak")],
    );
    let proposal = r
        .propose(&writer(), "p", "Add a claim", Some("Adds a claim."))
        .unwrap();
    let version = r.resolve_proposal(&proposal.id).unwrap().to_string();
    let st = r.read();
    let scope = st.resolve_scope(&format!("proposal:{version}")).unwrap();
    assert!(matches!(scope, Scope::Proposal(_)));
    let b = st.brief(&scope, &c, Some(1_000_000)).unwrap();
    let v = json_of(&b);
    assert!(v.get("vector_neighbors").is_none(), "{v}");
    assert!(v.get("vector_neighbors_skipped").is_none(), "{v}");
    assert_eq!(unbuilt(&st, &scope), WeightStats::default());
}

#[test]
fn vxs3_vector_advice_changes_no_token_plan_or_suspect() {
    let (_d, mut r) = project();
    pair(&mut r, "work");
    let c = cov(&["c-00001"]);
    let before = r.read().brief(&work(), &c, Some(1_000_000)).unwrap();
    before.register(&r.briefs());
    let advised: Vec<String> = before
        .vector_neighbors
        .iter()
        .map(|v| v.block.clone())
        .collect();
    assert_eq!(advised, ["c-00031", "c-00041"]);
    // a vector neighbor that changes moves its row, never the token
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": "c-00031", "body": "kiwi lemon"}]),
    )
    .unwrap();
    let after = r.read().brief(&work(), &c, Some(1_000_000)).unwrap();
    assert!(after.vector_neighbors.iter().all(|v| v.block != "c-00031"));
    assert_eq!(after.base.token, before.base.token);
    assert_eq!(after.record.recorded, before.record.recorded);
    // a write that cites the briefing: not stale, no consequence, suspect or plan item on advice
    let w = r
        .write_briefed(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: parse_test_ops(
                    json!([{"op": "update", "id": "c-00001", "body": "zebra walrus yak again"}]),
                )
                .unwrap(),
                justification: None,
                message: String::new(),
            },
            Some(before.base.token.clone()),
        )
        .unwrap();
    assert!(
        !w.warnings
            .iter()
            .any(|x| x.code == rdy_engine::brief::W_BRIEF_STALE),
        "{:?}",
        w.warnings
    );
    let reply = serde_json::to_string(&w).unwrap();
    let suspects = serde_json::to_string(&r.read().suspects(&work()).unwrap()).unwrap();
    let plan = r
        .read()
        .change_plan(
            &PlanQuery {
                view: Some("work".into()),
                ..Default::default()
            },
            "work",
        )
        .unwrap();
    let plan = serde_json::to_string(&plan).unwrap();
    for v in &advised {
        assert!(
            !reply.contains(v.as_str()),
            "{v} in the write reply: {reply}"
        );
        assert!(
            !suspects.contains(v.as_str()),
            "{v} in suspects: {suspects}"
        );
        assert!(!plan.contains(v.as_str()), "{v} in the plan: {plan}");
    }
}

#[test]
fn vxs3_cli_and_mcp_show_the_same_vector_advice() {
    let s = Served::new();
    // both sources in section One: the section's address briefs them together
    let (s1, s2, address) = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let w = write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "alpha", "title": "Alpha", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$sa", "parent": "$a", "fields": {"title": "One"}},
            {"op": "create", "type": "claim", "id": "$c1", "parent": "$sa", "body": "Zebras graze near the walrus colony."},
            {"op": "create", "type": "claim", "id": "$c2", "parent": "$sa", "body": "Otters swim past the moose herd."},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "beta", "title": "Beta", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$sc", "parent": "$b", "fields": {"title": "Three"}},
            {"op": "create", "type": "claim", "id": "$t1", "parent": "$sc", "body": "Zebras graze."},
            {"op": "create", "type": "claim", "id": "$t2", "parent": "$sc", "body": "The walrus colony and the moose herd."},
            {"op": "create", "type": "claim", "id": "$t3", "parent": "$sc", "body": "Otters swim."}
        ]))
        .unwrap();
        land(&mut r, "work").unwrap();
        r.close().unwrap();
        (
            w.created["$c1"].clone(),
            w.created["$c2"].clone(),
            format!("{}#one", w.created["$a"]),
        )
    };
    let mut c = McpClient::start_proxy(&s);
    let reply = c.call(
        "rdy_session",
        json!({"actor": "agent:vxs3", "run": "v1", "role": "writer"}),
    );
    assert!(reply.code().is_none());
    let m = c
        .call("rdy_brief", json!({"ids": [address], "scope": "canon"}))
        .ok();
    drop(c);
    let (ok, cli) = s.json(&["--json", "brief", &address]);
    assert!(ok, "{cli}");
    let rows = m["vector_neighbors"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(rows.len() >= 3, "{m}");
    assert_eq!(m["vector_neighbors"], cli["vector_neighbors"]);
    assert_eq!(
        m["vector_neighbors_skipped"],
        cli["vector_neighbors_skipped"]
    );
    assert_eq!(m["base"]["token"], cli["base"]["token"]);
    // the rows of both sources, sorted together by score
    let fors: BTreeSet<&str> = rows.iter().filter_map(|v| v["for"].as_str()).collect();
    assert_eq!(fors, BTreeSet::from([s1.as_str(), s2.as_str()]));
    let scores: Vec<f64> = rows.iter().map(|v| v["score"].as_f64().unwrap()).collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "{scores:?}");
    // the text: after the neighbors, one line per row
    let o = s.rdy(&["brief", &address]);
    assert!(o.status.success());
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    let list = |r: &Value, k: &str| {
        let items: Vec<&str> = r[k]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        if items.is_empty() {
            String::new()
        } else {
            format!("; {k} {}", items.join(", "))
        }
    };
    let lines: Vec<String> = rows
        .iter()
        .map(|r| {
            format!(
                "  {} (vector neighbor of {}, score {}{}{}; doc {}) {}",
                r["block"].as_str().unwrap(),
                r["for"].as_str().unwrap(),
                r["score"],
                list(r, "words"),
                list(r, "links"),
                r["doc"].as_str().unwrap(),
                r["summary"].as_str().unwrap()
            )
        })
        .collect();
    let want = format!("vector neighbors:\n{}\n", lines.join("\n"));
    assert!(text.contains(&want), "{want}\nin\n{text}");
    assert!(
        text.find("\nneighbors:").unwrap() < text.find("\nvector neighbors:").unwrap(),
        "{text}"
    );
    s.stop();
}

/// A coverage made from the covered claims.
type CoverageOf = fn(&[String]) -> Coverage;

/// The weight stats of `scope` in `st`: all zero while no weight was computed.
fn weight_stats(st: &ReadState, scope: &Scope) -> WeightStats {
    st.vector_weights(WeightScope::Read(scope)).unwrap().stats()
}

#[test]
fn vxs11_twenty_scores_twenty_one_skips_before_weights() {
    let many = Coverage {
        ids: vec![],
        path: Some("many".into()),
    };
    // 0 and 1 covered content blocks: no row and no count; the doc and its sections count
    // nowhere
    for n in [0, 1] {
        let (_d, mut r) = project();
        pairs(&mut r, n);
        let b = r.read().brief(&work(), &many, Some(usize::MAX)).unwrap();
        assert_eq!(b.covers, 3 + n, "{n}");
        assert!(b.vector_neighbors.is_empty(), "{n}");
        assert_eq!(b.vector_neighbors_skipped, 0, "{n}");
    }
    // 20 claims beside the doc, two sections and a supporting finding: every claim is scored,
    // and each lists its partner once
    let (_d, mut r) = project();
    let claims = pairs(&mut r, 20);
    raw_commit(
        &mut r,
        "work",
        vec![blk("fi-00001", "finding", "s-0000P", "b1", "pair0 pair1")],
    );
    let st = r.read();
    let b = st.brief(&work(), &many, Some(usize::MAX)).unwrap();
    assert_eq!(b.covers, 24);
    assert_eq!(b.vector_neighbors_skipped, 0);
    let fors: BTreeSet<&str> = b
        .vector_neighbors
        .iter()
        .map(|v| v.r#for.as_str())
        .collect();
    assert_eq!(fors, claims.iter().map(String::as_str).collect());
    assert_eq!(b.vector_neighbors.len(), 20);
    assert_ne!(weight_stats(&st, &work()), WeightStats::default());
    drop(st);

    // 21: the 20 claims and a deprecated one, which is no target but is covered content. No
    // block is scored, every covered content block counts, and no weight is computed
    let wide_cases: [(&str, CoverageOf); 3] = [
        ("the doc's path", |_| Coverage {
            ids: vec![],
            path: Some("many".into()),
        }),
        ("the ids", |c| Coverage {
            ids: c.to_vec(),
            path: None,
        }),
        ("the ids reversed", |c| Coverage {
            ids: c.iter().rev().cloned().collect(),
            path: None,
        }),
    ];
    for (what, coverage) in wide_cases {
        let (_d, mut r) = project();
        let mut claims = pairs(&mut r, 20);
        raw_commit(
            &mut r,
            "work",
            vec![raw(
                "c-00500",
                "claim",
                Some("s-0000P"),
                "b1",
                &[("lifecycle", "deprecated")],
                "pair0 pair1",
            )],
        );
        claims.push("c-00500".into());
        let c = coverage(&claims);
        let st = r.read();
        let b = st.brief(&work(), &c, Some(usize::MAX)).unwrap();
        assert!(b.vector_neighbors.is_empty(), "{what}");
        assert_eq!(b.vector_neighbors_skipped, 21, "{what}");
        assert_eq!(json_of(&b)["vector_neighbors_skipped"], json!(21), "{what}");
        assert!(json_of(&b).get("vector_neighbors").is_none(), "{what}");
        assert_eq!(
            weight_stats(&st, &work()),
            WeightStats::default(),
            "{what}: no weight computed"
        );
        // a first page that shows fewer than 20 blocks counts the whole covered set
        let first = st.brief(&work(), &c, Some(300)).unwrap();
        assert!(first.truncated, "{what}");
        assert!(first.base.blocks.len() < 20, "{what}");
        assert!(first.vector_neighbors.is_empty(), "{what}");
        assert_eq!(first.vector_neighbors_skipped, 21, "{what}");
    }
}

#[test]
fn vxs11_wide_brief_explains_like_and_matches_cli() {
    let guidance = "For similar blocks, use rdy_search with like.";
    let s = Served::new();
    let address = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let mut ops = vec![
            json!({"op": "create", "type": "doc", "id": "$a", "fields": {"name": "alpha", "title": "Alpha", "doc_type": "overview"}}),
            json!({"op": "create", "type": "section", "id": "$sa", "parent": "$a", "fields": {"title": "One"}}),
        ];
        for k in 0..21 {
            ops.push(json!({"op": "create", "type": "claim", "parent": "$sa", "body": format!("Zebras graze near walrus colony {k}.")}));
        }
        let w = write(&mut r, "work", Value::Array(ops)).unwrap();
        land(&mut r, "work").unwrap();
        r.close().unwrap();
        format!("{}#one", w.created["$a"])
    };
    let mut c = McpClient::start_proxy(&s);
    let reply = c.call(
        "rdy_session",
        json!({"actor": "agent:vxs11", "run": "v1", "role": "writer"}),
    );
    assert!(reply.code().is_none());
    let m = c
        .call("rdy_brief", json!({"ids": [address], "scope": "canon"}))
        .ok();
    let sch = c.call("rdy_schema", json!({})).ok();
    drop(c);
    assert!(m.get("vector_neighbors").is_none(), "{m}");
    assert_eq!(m["vector_neighbors_skipped"], json!(21), "{m}");
    for text in [&sch["brief"], &sch["edit_loop"]] {
        let text = text.as_str().unwrap();
        assert!(text.contains(guidance), "{text}");
    }
    let (ok, cli) = s.json(&["--json", "brief", &address]);
    assert!(ok, "{cli}");
    assert!(cli.get("vector_neighbors").is_none(), "{cli}");
    assert_eq!(
        cli["vector_neighbors_skipped"],
        m["vector_neighbors_skipped"]
    );
    let o = s.rdy(&["brief", &address]);
    assert!(o.status.success());
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.ends_with("vector neighbors:"))
        .unwrap_or_else(|| panic!("{text}"));
    let rest: Vec<&str> = lines[at + 1..at + 3]
        .iter()
        .map(|l| l.trim_start_matches(['#', ' ']))
        .collect();
    assert_eq!(rest, ["21 source(s) not scored", guidance], "{text}");
    s.stop();
}
