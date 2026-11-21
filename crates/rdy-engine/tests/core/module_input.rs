//! wck4: the input of a module rule, interface 1. One block and its neighbours to the rule's
//! depth, as canonical compact JSON: object keys in byte order, the minimal escapes with
//! lower-case `\u00xx` for every control character, and integers only. Each derived value is
//! the filter evaluator's, the neighbours come from one state at their fewest steps, and the
//! envelope never passes 1 MiB.
use rdy_engine::Materialized;
use rdy_engine::check::module_input::{
    InputError, ModuleState, block_value, encode_input, module_inputs_for_tests, module_neighbours,
};
use rdy_engine::filter::{FilterContext, FilterInputs};
use rdy_graph::Graph;
use rdy_model::{Block, BlockId, FieldValue, Link, StateSnapshot};
use rdy_schema::filter::parse;
use rdy_schema::{AreaDef, AreaMatcher, ArgValue, Pack, PackSet};
use std::collections::{BTreeMap, BTreeSet};

/// The live bytes this thread holds, and their peak: the probe that the root alone fails before
/// rdy allocates an envelope's buffer.
mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Counting;

    thread_local! {
        static NOW: Cell<usize> = const { Cell::new(0) };
        static PEAK: Cell<usize> = const { Cell::new(0) };
    }

    fn grow(n: usize) {
        let _ = NOW.try_with(|now| {
            now.set(now.get().saturating_add(n));
            let _ = PEAK.try_with(|p| p.set(p.get().max(now.get())));
        });
    }

    fn shrink(n: usize) {
        let _ = NOW.try_with(|now| now.set(now.get().saturating_sub(n)));
    }

    // SAFETY: every call goes to `System` with the caller's arguments unchanged; the counters
    // are this thread's own cells.
    #[allow(unsafe_code)]
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            grow(l.size());
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
            unsafe { System.alloc(l) }
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            grow(l.size());
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc_zeroed` requires.
            unsafe { System.alloc_zeroed(l) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, l: Layout) {
            shrink(l.size());
            // SAFETY: `ptr` came from `System` with `l` (every allocation here is System's).
            unsafe { System.dealloc(ptr, l) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
            shrink(l.size());
            grow(new_size);
            // SAFETY: the caller's arguments, as `GlobalAlloc::realloc` requires.
            unsafe { System.realloc(ptr, l, new_size) }
        }
    }

    /// The peak of this thread's live allocations during `f`, above what was live before it.
    pub fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
        let base = NOW.with(|n| n.get());
        PEAK.with(|p| p.set(base));
        let out = f();
        (out, PEAK.with(|p| p.get()).saturating_sub(base))
    }
}

#[global_allocator]
static COUNTING: counting::Counting = counting::Counting;

/// The complete input envelope's cap.
const INPUT_MAX: usize = 1_048_576;
const INT_EDGE: i64 = 9_007_199_254_740_992;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn blk(at: &str, block_type: &str, parent: Option<&str>, body: &str) -> Block {
    Block {
        id: id(at),
        block_type: block_type.into(),
        parent: parent.map(id),
        order: "a0".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn field(mut b: Block, name: &str, value: FieldValue) -> Block {
    b.fields.insert(name.into(), value);
    b
}

fn text(mut b: Block, name: &str, value: &str) -> Block {
    b.fields.insert(name.into(), FieldValue::Str(value.into()));
    b
}

fn link(mut b: Block, kind: &str, target: &str) -> Block {
    b.links.entry(kind.into()).or_default().push(Link {
        target: id(target),
        part: None,
    });
    b
}

fn state(blocks: Vec<Block>) -> (Materialized, Graph) {
    let m = Materialized::build(
        StateSnapshot::default(),
        blocks.into_iter().map(|b| (b.id.clone(), b)).collect(),
    );
    let g = Graph::build_from(m.iter());
    (m, g)
}

/// The standard pack with two areas over `guide` and one over another path.
fn pack() -> Pack {
    let mut pack = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    for (name, path) in [("docs", "guide"), ("handbook", "guide"), ("zzz", "other")] {
        pack.areas.insert(
            name.into(),
            AreaDef {
                description: String::new(),
                paths: vec![path.into()],
            },
        );
    }
    pack
}

fn no_args() -> BTreeMap<String, ArgValue> {
    BTreeMap::new()
}

fn input(
    pack: &Pack,
    areas: &AreaMatcher,
    (m, g): &(Materialized, Graph),
    root: &str,
    depth: u8,
) -> Result<Vec<u8>, InputError> {
    let st = ModuleState::new(pack, m, g, areas);
    encode_input(&st, "rule-a", &no_args(), &id(root), depth)
}

/// The golden state: a doc, a section that marks its content planned, and a claim under it with
/// every edge kind, duplicate edges, a self-link, a link to a missing block, control characters,
/// Unicode keys, large integers and the landing clock.
fn golden_state() -> Vec<Block> {
    let mut list = BTreeMap::new();
    list.insert("b".to_string(), FieldValue::Int(INT_EDGE));
    list.insert("a".to_string(), FieldValue::Str("x\u{1f}y".into()));
    let root = blk(
        "C-AAAAA",
        "claim",
        Some("S-AAAAA"),
        "Tab\there, line\nend, vt\u{b}, us\u{1f}, \"quoted\" \\ and é \u{1f600}; per {{D-BBBBB}} and {{D-BBBBB|@key}}, \\{{not}}.",
    );
    let root = text(root, "key", "CLAIM-1");
    let root = text(root, "decided_at", "2026-11-20T00:00:00Z");
    let root = field(root, "big", FieldValue::Int(INT_EDGE + 1));
    let root = field(root, "neg", FieldValue::Int(-INT_EDGE));
    let root = field(root, "min", FieldValue::Int(i64::MIN));
    let root = text(root, "digits", "9007199254740993");
    let root = text(root, "ключ", "значение");
    let root = field(root, "é", FieldValue::Bool(false));
    let root = field(
        root,
        "list",
        FieldValue::List(vec![
            FieldValue::Int(1),
            FieldValue::Null,
            FieldValue::Map(list),
            FieldValue::Int(-INT_EDGE - 1),
        ]),
    );
    let root = link(root, "decided_by", "D-BBBBB");
    let root = link(root, "decided_by", "D-BBBBB");
    let root = link(root, "decided_by", "D-ZZZZZ");
    let root = link(root, "depends_on", "C-AAAAA");
    let doc = text(
        text(blk("D-AAAAA", "doc", None, ""), "name", "guide"),
        "key",
        "GUIDE",
    );
    let section = field(
        text(
            text(
                blk("S-AAAAA", "section", Some("D-AAAAA"), ""),
                "title",
                "Intro",
            ),
            "lifecycle",
            "stable",
        ),
        "planned",
        FieldValue::Bool(true),
    );
    let decision = text(
        text(
            blk("D-BBBBB", "decision", Some("S-AAAAA"), "Per {{T-AAAAA}}."),
            "statement",
            "Wallets stay",
        ),
        "status",
        "accepted",
    );
    let other = link(
        blk("C-BBBBB", "claim", Some("S-AAAAA"), "Per {{C-AAAAA}}."),
        "depends_on",
        "C-AAAAA",
    );
    let term = link(
        text(
            blk("T-AAAAA", "term", Some("S-AAAAA"), ""),
            "name",
            "wallet",
        ),
        "uses",
        "C-BBBBB",
    );
    let note = blk("N-AAAAA", "note", Some("C-AAAAA"), "");
    vec![root, doc, section, decision, other, term, note]
}

#[test]
fn wck4_canonical_input_bytes_are_exact() {
    let pack = pack();
    let areas = pack.area_matcher();
    let s = state(golden_state());
    let st = ModuleState::new(&pack, &s.0, &s.1, &areas);
    let mut args = BTreeMap::new();
    args.insert("zeta".to_string(), ArgValue::Bool(true));
    args.insert("ключ".to_string(), ArgValue::Str("t\u{b}\n".into()));
    args.insert(
        "alpha".to_string(),
        ArgValue::List(vec![
            ArgValue::Int(-INT_EDGE),
            ArgValue::Int(INT_EDGE),
            ArgValue::Null,
            ArgValue::Map(BTreeMap::from([("b".into(), ArgValue::Int(0))])),
        ]),
    );
    let bytes = encode_input(&st, "short-\"sentences\"", &args, &id("C-AAAAA"), 2).unwrap();
    let golden: &[u8] = include_bytes!("../fixtures/wasm/input-v1.json");
    assert_eq!(
        String::from_utf8_lossy(&bytes),
        String::from_utf8_lossy(golden)
    );
    assert_eq!(bytes, golden);
    let text = std::str::from_utf8(&bytes).unwrap();
    // lower-case six-byte escapes for every control character, the short forms never
    for escape in [r"\u000b", r"\u001f", r"\u000a", r"\u0009"] {
        assert!(text.contains(escape), "{escape}");
    }
    for short in [r"\n", r"\t", r"\u000B", r"\u001F"] {
        assert!(!text.contains(short), "{short}");
    }
    assert!(text.contains(r#"\"quoted\" \\ and é 😀"#));
    // the bytes are one JSON value
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["interface"], 1);
    assert_eq!(value["block"]["fields"]["ключ"], "значение");
}

/// Whether `filter` matches block `b`.
fn holds(ctx: &FilterContext, pack: &Pack, b: &BlockId, filter: &str) -> bool {
    ctx.matches(&parse(filter, pack).unwrap(), b)
}

#[test]
fn wck4_derived_values_match_the_filter_evaluator() {
    let pack = pack();
    let areas = pack.area_matcher();
    let mut blocks = golden_state();
    // a decision is also in the areas of what it affects; a proposed one has a status too
    let affects = link(
        text(
            text(
                blk("D-CCCCC", "decision", None, "Moved."),
                "statement",
                "Moved",
            ),
            "status",
            "proposed",
        ),
        "affects",
        "S-AAAAA",
    );
    blocks.push(affects);
    blocks.push(link(
        blk("C-CCCCC", "claim", Some("S-AAAAA"), "Built."),
        "implements",
        "D-BBBBB",
    ));
    blocks.push(text(
        blk("C-DDDDD", "claim", None, "Loose."),
        "lifecycle",
        "draft",
    ));
    let (m, g) = state(blocks);
    let st = ModuleState::new(&pack, &m, &g, &areas);
    let empty = BTreeSet::new();
    let coverage = BTreeMap::new();
    let ctx = FilterContext::from_parts(FilterInputs {
        pack: &pack,
        m: &m,
        graph: &g,
        areas: &areas,
        suspects: &empty,
        tensions: &empty,
        coverage: &coverage,
    });
    let all_traits = [
        "normative",
        "informative",
        "defines",
        "describes",
        "asset",
        "structural",
        "presentational",
    ];
    let mut seen = BTreeSet::new();
    for b in m.iter() {
        let v = block_value(&st, b);
        let at = &b.id;
        assert_eq!(v.id, at.as_str());
        match v.lifecycle {
            Some(l) => assert!(holds(&ctx, &pack, at, &format!("lifecycle = {l}"))),
            None => assert!(!holds(&ctx, &pack, at, "has lifecycle")),
        }
        match &v.path {
            Some(p) => assert!(holds(&ctx, &pack, at, &format!("path = \"{p}\""))),
            None => assert!(!holds(&ctx, &pack, at, "has path")),
        }
        match v.doc {
            Some(d) => assert!(holds(&ctx, &pack, at, &format!("doc = {d}"))),
            None => assert!(!holds(&ctx, &pack, at, "has doc")),
        }
        match v.status {
            Some(s) => assert!(holds(&ctx, &pack, at, &format!("status = {s}"))),
            None => assert!(!holds(&ctx, &pack, at, "has status")),
        }
        for want in [true, false] {
            assert_eq!(
                holds(&ctx, &pack, at, &format!("planned = {want}")),
                v.planned == Some(want),
                "{at} planned"
            );
        }
        assert_eq!(holds(&ctx, &pack, at, "has area"), !v.area.is_empty());
        for a in ["docs", "handbook", "zzz"] {
            assert_eq!(
                holds(&ctx, &pack, at, &format!("area = {a}")),
                v.area.iter().any(|x| x == a),
                "{at} area {a}"
            );
        }
        for t in all_traits {
            assert_eq!(
                holds(&ctx, &pack, at, &format!("trait = {t}")),
                v.traits.contains(&t),
                "{at} trait {t}"
            );
        }
        assert_eq!(
            v.key.is_some(),
            holds(&ctx, &pack, at, "has key"),
            "{at} key"
        );
        // arrays are sorted and distinct
        let mut sorted = v.area.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, v.area);
        let mut sorted = v.traits.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, v.traits);
        seen.insert((
            v.lifecycle.map(str::to_string),
            v.status,
            v.planned,
            v.area.len(),
        ));
    }
    // the state exercises inherited, own and missing values of each fact
    let root = block_value(&st, m.get(&id("C-AAAAA")).unwrap());
    assert_eq!(root.lifecycle, Some("stable"));
    assert_eq!(root.planned, Some(true));
    assert_eq!(root.path.as_deref(), Some("guide"));
    assert_eq!(root.doc, Some("D-AAAAA"));
    assert_eq!(root.area, vec!["docs".to_string(), "handbook".to_string()]);
    assert_eq!(root.traits, vec!["describes", "normative"]);
    let proposed = block_value(&st, m.get(&id("D-CCCCC")).unwrap());
    assert_eq!(proposed.status, Some("proposed"));
    assert_eq!(
        proposed.area,
        vec!["docs".to_string(), "handbook".to_string()]
    );
    assert_eq!(proposed.path, None);
    let loose = block_value(&st, m.get(&id("C-DDDDD")).unwrap());
    assert_eq!(loose.planned, Some(false));
    assert_eq!(
        (loose.lifecycle, loose.doc, loose.parent),
        (Some("draft"), None, None)
    );
    assert!(seen.len() >= 4, "{seen:?}");
}

/// Each neighbour as `(id, hops, from, via)`.
fn walk(s: &(Materialized, Graph), root: &str, depth: u8) -> Vec<(String, u8, String, String)> {
    module_neighbours(&s.0, &s.1, &id(root), depth)
        .iter()
        .map(|n| (n.id.to_string(), n.hops, n.from.to_string(), n.via.label()))
        .collect()
}

fn row(at: &str, hops: u8, from: &str, via: &str) -> (String, u8, String, String) {
    (at.into(), hops, from.into(), via.into())
}

#[test]
fn wck4_neighbours_have_minimum_hops_and_stable_ties() {
    let s = state(golden_state());
    assert_eq!(
        walk(&s, "C-AAAAA", 2),
        vec![
            // one step: the referencing claim's link wins over its reference ("in depends_on" <
            // "in references"); the decision's link wins over the reference to it
            row("C-BBBBB", 1, "C-AAAAA", "in depends_on"),
            row("D-BBBBB", 1, "C-AAAAA", "out decided_by"),
            row("N-AAAAA", 1, "C-AAAAA", "child"),
            row("S-AAAAA", 1, "C-AAAAA", "parent"),
            // two steps: the term is near the other claim, the decision and the section; the
            // smallest near block wins
            row("D-AAAAA", 2, "S-AAAAA", "parent"),
            row("T-AAAAA", 2, "C-BBBBB", "in uses"),
        ]
    );
    // a near block's smaller id wins over a smaller label; the same pair twice is one neighbour
    let s = state(vec![
        blk("A-AAAAA", "claim", None, ""),
        link(blk("B-AAAAA", "claim", None, ""), "zz", "A-AAAAA"),
        link(blk("C-AAAAA", "claim", None, ""), "aa", "A-AAAAA"),
        link(
            link(blk("X-AAAAA", "claim", None, ""), "zz", "B-AAAAA"),
            "aa",
            "C-AAAAA",
        ),
        link(
            link(blk("Y-AAAAA", "claim", None, ""), "uses", "A-AAAAA"),
            "uses",
            "A-AAAAA",
        ),
    ]);
    assert_eq!(
        walk(&s, "A-AAAAA", 2),
        vec![
            row("B-AAAAA", 1, "A-AAAAA", "in zz"),
            row("C-AAAAA", 1, "A-AAAAA", "in aa"),
            row("Y-AAAAA", 1, "A-AAAAA", "in uses"),
            row("X-AAAAA", 2, "B-AAAAA", "in zz"),
        ]
    );
    // a cycle reaches each block once, at its fewest steps, and never the root again
    let s = state(vec![
        link(blk("A-AAAAA", "claim", None, ""), "next", "B-AAAAA"),
        link(blk("B-AAAAA", "claim", None, ""), "next", "C-AAAAA"),
        link(blk("C-AAAAA", "claim", None, ""), "next", "A-AAAAA"),
    ]);
    assert_eq!(
        walk(&s, "A-AAAAA", 3),
        vec![
            row("B-AAAAA", 1, "A-AAAAA", "out next"),
            row("C-AAAAA", 1, "A-AAAAA", "in next"),
        ]
    );
}

#[test]
fn wck4_depth_zero_to_three_and_missing_edges() {
    // a chain of five, a self-link, a link and a reference to missing blocks, and a parent that
    // the state lacks
    let chain = |deleted: bool| {
        let mut blocks = vec![
            link(
                link(
                    blk("A-AAAAA", "claim", Some("P-ZZZZZ"), "See {{Q-ZZZZZ}}."),
                    "next",
                    "B-AAAAA",
                ),
                "next",
                "A-AAAAA",
            ),
            link(
                link(blk("B-AAAAA", "claim", None, ""), "next", "C-AAAAA"),
                "gone",
                "G-AAAAA",
            ),
            link(blk("C-AAAAA", "claim", None, ""), "next", "D-AAAAA"),
            link(blk("D-AAAAA", "claim", None, ""), "next", "E-AAAAA"),
            blk("E-AAAAA", "claim", None, ""),
        ];
        if !deleted {
            blocks.push(blk("G-AAAAA", "claim", None, ""));
        }
        state(blocks)
    };
    let s = chain(true);
    assert_eq!(walk(&s, "A-AAAAA", 0), vec![]);
    assert_eq!(
        walk(&s, "A-AAAAA", 1),
        vec![row("B-AAAAA", 1, "A-AAAAA", "out next")]
    );
    assert_eq!(
        walk(&s, "A-AAAAA", 2),
        vec![
            row("B-AAAAA", 1, "A-AAAAA", "out next"),
            row("C-AAAAA", 2, "B-AAAAA", "out next"),
        ]
    );
    assert_eq!(
        walk(&s, "A-AAAAA", 3),
        vec![
            row("B-AAAAA", 1, "A-AAAAA", "out next"),
            row("C-AAAAA", 2, "B-AAAAA", "out next"),
            row("D-AAAAA", 3, "C-AAAAA", "out next"),
        ]
    );
    // the deleted block's link stays in its source's own links, and gives no neighbour
    let pack = pack();
    let areas = pack.area_matcher();
    let st = ModuleState::new(&pack, &s.0, &s.1, &areas);
    let b = block_value(&st, s.0.get(&id("B-AAAAA")).unwrap());
    assert_eq!(
        b.links,
        vec![("gone", vec!["G-AAAAA"]), ("next", vec!["C-AAAAA"])]
    );
    let a = block_value(&st, s.0.get(&id("A-AAAAA")).unwrap());
    assert_eq!(a.references, vec!["Q-ZZZZZ"]);
    assert_eq!(a.parent, Some("P-ZZZZZ"));
    // while it exists, it is a neighbour at two steps
    let s = chain(false);
    assert_eq!(
        walk(&s, "A-AAAAA", 2),
        vec![
            row("B-AAAAA", 1, "A-AAAAA", "out next"),
            row("C-AAAAA", 2, "B-AAAAA", "out next"),
            row("G-AAAAA", 2, "B-AAAAA", "out gone"),
        ]
    );
    // parent and child placement are steps in both directions
    let s = state(golden_state());
    assert_eq!(
        walk(&s, "D-AAAAA", 1),
        vec![row("S-AAAAA", 1, "D-AAAAA", "child")]
    );
    assert_eq!(
        walk(&s, "N-AAAAA", 1),
        vec![row("C-AAAAA", 1, "N-AAAAA", "parent")]
    );
    // the input's neighbours are this walk; a root the state lacks has no input
    let bytes = input(&pack, &areas, &chain(true), "A-AAAAA", 3).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let ids: Vec<&str> = value["neighbours"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["block"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["B-AAAAA", "C-AAAAA", "D-AAAAA"]);
    assert_eq!(value["neighbours_omitted"], 0);
    assert_eq!(
        input(&pack, &areas, &chain(true), "G-AAAAA", 1),
        Err(InputError::Missing)
    );
}

/// A root claim whose body is `x` and `pad` more bytes, with `children` claims under it, each
/// with a short body.
fn wide(pad: usize, children: usize) -> (Materialized, Graph) {
    let body = format!("x{}", "a".repeat(pad));
    let mut blocks = vec![blk("C-AAAAA", "claim", None, &body)];
    for i in 0..children {
        blocks.push(blk(&format!("N-{i:05}"), "claim", Some("C-AAAAA"), "child"));
    }
    state(blocks)
}

/// The envelope's length with no neighbour and `omitted` counted, for a root padded by `pad`:
/// what the root alone needs.
fn root_alone(pack: &Pack, areas: &AreaMatcher, pad: usize, omitted: usize) -> usize {
    let full = input(pack, areas, &wide(pad, 0), "C-AAAAA", 1).unwrap();
    full.len() - 1 + omitted.to_string().len()
}

/// The length of one neighbour entry of `wide`.
fn entry_len(pack: &Pack, areas: &AreaMatcher) -> usize {
    let one = input(pack, areas, &wide(0, 1), "C-AAAAA", 1).unwrap();
    one.len() - root_alone(pack, areas, 0, 0)
}

fn kept(bytes: &[u8]) -> (usize, u64) {
    let value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    (
        value["neighbours"].as_array().unwrap().len(),
        value["neighbours_omitted"].as_u64().unwrap(),
    )
}

#[test]
fn wck4_input_at_and_past_one_mib() {
    let pack = pack();
    let areas = pack.area_matcher();
    let entry = entry_len(&pack, &areas);
    let base = root_alone(&pack, &areas, 0, 0);
    // the root alone, all ten neighbours omitted: exactly 1 MiB fits, one byte more fails
    let pad = INPUT_MAX - root_alone(&pack, &areas, 0, 10);
    let at = input(&pack, &areas, &wide(pad, 10), "C-AAAAA", 1).unwrap();
    assert_eq!((at.len(), kept(&at)), (INPUT_MAX, (0, 10)));
    assert_eq!(
        input(&pack, &areas, &wide(pad + 1, 10), "C-AAAAA", 1),
        Err(InputError::TooLarge)
    );
    // with nine omitted the count has one digit fewer: the same root needs one byte less
    let nine = input(&pack, &areas, &wide(pad + 1, 9), "C-AAAAA", 1).unwrap();
    assert_eq!((nine.len(), kept(&nine)), (INPUT_MAX, (0, 9)));
    // a neighbour stays while the whole envelope, its omitted count included, fits
    for n in [10usize, 11] {
        let pad = INPUT_MAX - (base - 1) - entry - (n - 1).to_string().len();
        let fits = input(&pack, &areas, &wide(pad, n), "C-AAAAA", 1).unwrap();
        assert_eq!((fits.len(), kept(&fits)), (INPUT_MAX, (1, n as u64 - 1)));
        let over = input(&pack, &areas, &wide(pad + 1, n), "C-AAAAA", 1).unwrap();
        assert_eq!(kept(&over), (0, n as u64));
        assert!(over.len() <= INPUT_MAX);
    }
    // the omitted count gains a digit as the second neighbour drops: two fit at 1 MiB with
    // nine omitted, and one byte more keeps one with ten
    let pad = INPUT_MAX - (base - 1) - 2 * entry - 1 - 1;
    let two = input(&pack, &areas, &wide(pad, 11), "C-AAAAA", 1).unwrap();
    assert_eq!((two.len(), kept(&two)), (INPUT_MAX, (2, 9)));
    let one = input(&pack, &areas, &wide(pad + 1, 11), "C-AAAAA", 1).unwrap();
    assert_eq!((one.len(), kept(&one)), (INPUT_MAX + 1 - entry, (1, 10)));
    // every kept prefix is the walk's order
    let all = input(&pack, &areas, &wide(0, 3), "C-AAAAA", 1).unwrap();
    assert_eq!(kept(&all), (3, 0));
    let value: serde_json::Value = serde_json::from_slice(&one).unwrap();
    assert_eq!(value["neighbours"][0]["block"]["id"], "N-00000");
    // a root far over the cap fails without a buffer of its size
    let big = wide(2 * INPUT_MAX, 3);
    let st = ModuleState::new(&pack, &big.0, &big.1, &areas);
    let (result, peak) =
        counting::peak_during(|| encode_input(&st, "rule-a", &no_args(), &id("C-AAAAA"), 1));
    assert_eq!(result, Err(InputError::TooLarge));
    assert!(peak < 64 * 1024, "peak {peak}");
}

#[test]
fn wck4_fields_exclude_clock_and_preserve_integer_text() {
    let pack = pack();
    let areas = pack.area_matcher();
    let s = state(golden_state());
    let bytes = input(&pack, &areas, &s, "C-AAAAA", 0).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let fields = &value["block"]["fields"];
    assert!(fields.get("decided_at").is_none());
    assert!(!String::from_utf8_lossy(&bytes).contains("decided_at"));
    // integers within 2^53 stay numbers; others are strings of their decimal text
    assert_eq!(fields["neg"], serde_json::json!(-INT_EDGE));
    assert_eq!(fields["big"], "9007199254740993");
    assert_eq!(fields["min"], "-9223372036854775808");
    assert_eq!(fields["list"][2]["b"], serde_json::json!(INT_EDGE));
    assert_eq!(fields["list"][3], "-9007199254740993");
    // a string keeps its text, never read as a number
    assert_eq!(fields["digits"], "9007199254740993");
    assert_eq!(fields["key"], "CLAIM-1");
    let printed = String::from_utf8_lossy(&bytes);
    assert!(printed.contains(r#""big":"9007199254740993""#));
    assert!(printed.contains(r#""neg":-9007199254740992"#));
    // global facts and empty bodies are left out; a block without the fact lacks the key
    for global in ["suspect", "tension", "coverage", "since"] {
        assert!(value["block"].get(global).is_none(), "{global}");
    }
    let doc = input(&pack, &areas, &s, "D-AAAAA", 0).unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&doc).unwrap();
    let keys: Vec<&String> = doc["block"].as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        vec![
            "area",
            "doc",
            "fields",
            "id",
            "key",
            "links",
            "path",
            "references",
            "traits",
            "type"
        ]
    );
    // a decided decision's clock is no input either
    let decision = field(
        text(
            blk("D-BBBBB", "decision", None, "Body."),
            "status",
            "accepted",
        ),
        "decided_at",
        FieldValue::Int(1),
    );
    let s = state(vec![decision]);
    let bytes = input(&pack, &areas, &s, "D-BBBBB", 1).unwrap();
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        r#"{"args":{},"block":{"area":[],"body":"Body.","fields":{"status":"accepted"},"id":"D-BBBBB","links":{},"references":[],"status":"live","traits":["normative"],"type":"decision"},"interface":1,"neighbours":[],"neighbours_omitted":0,"rule":"rule-a"}"#
    );
}

#[test]
fn wck4_input_reads_one_snapshot_and_raw_references() {
    let pack = pack();
    let areas = pack.area_matcher();
    // the same ids in two states: the section's lifecycle and the claim's body differ
    let before = state(golden_state());
    let after = state(
        golden_state()
            .into_iter()
            .map(|b| match b.id.as_str() {
                "S-AAAAA" => text(b, "lifecycle", "deprecated"),
                "C-BBBBB" => {
                    let mut b = b;
                    b.body = "Changed {{C-AAAAA|the claim}}.".into();
                    b
                }
                _ => b,
            })
            .collect(),
    );
    for (s, lifecycle, body) in [
        (&before, "stable", "Per {{C-AAAAA}}."),
        (&after, "deprecated", "Changed {{C-AAAAA|the claim}}."),
    ] {
        let bytes = input(&pack, &areas, s, "C-AAAAA", 1).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["block"]["lifecycle"], lifecycle);
        let near: BTreeMap<&str, &serde_json::Value> = v["neighbours"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| (n["block"]["id"].as_str().unwrap(), &n["block"]))
            .collect();
        assert_eq!(near["S-AAAAA"]["lifecycle"], lifecycle);
        assert_eq!(near["C-BBBBB"]["lifecycle"], lifecycle);
        assert_eq!(near["C-BBBBB"]["body"], body);
        assert_eq!(
            near["C-BBBBB"]["references"],
            serde_json::json!(["C-AAAAA"])
        );
    }
    // the body keeps its markers as stored; references are the pinned ids, sorted and distinct
    let bytes = input(&pack, &areas, &before, "C-AAAAA", 0).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let body = v["block"]["body"].as_str().unwrap();
    assert!(body.contains("{{D-BBBBB}} and {{D-BBBBB|@key}}, \\{{not}}."));
    assert_eq!(v["block"]["references"], serde_json::json!(["D-BBBBB"]));
    assert_eq!(
        v["block"]["links"],
        serde_json::json!({"decided_by": ["D-BBBBB", "D-ZZZZZ"], "depends_on": ["C-AAAAA"]})
    );
    // what the input reads: the root, every neighbour to the depth whether or not the cap keeps
    // it, and the blocks their derived values read, as a filter rule's reach reads them: the
    // ancestors (lifecycle, path, doc, area, planned), the incoming links (status) and the
    // decisions, a missing one too (planned)
    let (m, g) = &before;
    let st = ModuleState::new(&pack, m, g, &areas);
    let reads = module_inputs_for_tests(&st, &id("C-AAAAA"), 0);
    let want: BTreeSet<BlockId> = [
        "C-AAAAA", "S-AAAAA", "D-AAAAA", "C-BBBBB", "D-BBBBB", "D-ZZZZZ",
    ]
    .into_iter()
    .map(id)
    .collect();
    assert_eq!(reads, want);
    // one step adds the child, and the term whose link reaches the other claim
    let reads = module_inputs_for_tests(&st, &id("C-AAAAA"), 1);
    let want: BTreeSet<BlockId> = [
        "C-AAAAA", "S-AAAAA", "D-AAAAA", "C-BBBBB", "D-BBBBB", "D-ZZZZZ", "N-AAAAA", "T-AAAAA",
    ]
    .into_iter()
    .map(id)
    .collect();
    assert_eq!(reads, want);
    let big = wide(2 * INPUT_MAX, 2);
    let st = ModuleState::new(&pack, &big.0, &big.1, &areas);
    let reads = module_inputs_for_tests(&st, &id("C-AAAAA"), 1);
    assert_eq!(reads.len(), 3);
}
