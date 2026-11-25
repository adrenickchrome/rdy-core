//! D-Y1WEY, D-0ZPAP, D-5BM47: a write result lists the vector neighbors of the content blocks the
//! write creates, read from the state the write produced. The first ten created content blocks
//! that survive the write, in operation order, each list at most three rows `{for, block, score,
//! words, links, doc}` with a score of at least 0.25; a block the write creates or changes is
//! never a row. `vector_neighbors_skipped` counts the created content blocks past ten and the ones
//! a work guard stopped. The rows are advice: the write stands, and its consequences, briefing and
//! token do not change.
use rdy_engine::brief::BriefRecord;
use rdy_engine::consequence::write_and_report;
use rdy_engine::engine::{Engine, Local, WriteFn};
use rdy_engine::vector_neighbors::{AdviceCut, vector_neighbors_except};
use rdy_engine::vectors::{Doc, VectorBudget, WeightScope, round3, score_pair};
use rdy_engine::*;
use rdy_model::{Block, BlockId, CROCKFORD};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Barrier, Mutex};

const VIEW: &str = "w";

fn view() -> Scope {
    Scope::View(VIEW.into())
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

/// The empty doc `fresh` (d-0000N) with section s-0000N, where the writes create their blocks.
fn fresh() -> Vec<Block> {
    vec![
        doc("d-0000N", "fresh", "z1"),
        blk("s-0000N", "section", "d-0000N", "a1", ""),
    ]
}

/// The write of `ops` to view `w` as the person, through `e`, with its report.
fn report(e: &dyn Engine, ops: Value) -> Value {
    report_of(e, ops).unwrap()
}

fn report_of(e: &dyn Engine, ops: Value) -> Result<Value, RdyError> {
    let ops = parse_test_ops(ops).unwrap();
    write_and_report(e, VIEW, None, move |r| {
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
}

/// The created id of placeholder `p` in reply `v`.
fn made(v: &Value, p: &str) -> String {
    v["created"][p]
        .as_str()
        .unwrap_or_else(|| panic!("{p} in {v}"))
        .to_string()
}

fn rows(v: &Value) -> Vec<Value> {
    v["vector_neighbors"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The rows of `v` with each created id named by its placeholder.
fn named_rows(v: &Value) -> Vec<Value> {
    let names: BTreeMap<String, String> = v["created"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, x)| (x.as_str().unwrap().to_string(), k.clone()))
        .collect();
    rows(v)
        .into_iter()
        .map(|mut r| {
            for k in ["for", "block"] {
                if let Some(n) = r[k].as_str().and_then(|s| names.get(s)) {
                    r[k] = json!(n);
                }
            }
            r
        })
        .collect()
}

fn of_source<'a>(rows: &'a [Value], source: &str) -> Vec<&'a Value> {
    rows.iter().filter(|r| r["for"] == source).collect()
}

const ANIMALS: [&str; 12] = [
    "aardvark", "bison", "camel", "dingo", "egret", "ferret", "gecko", "hyena", "ibis", "jackal",
    "koala", "lemur",
];

/// Doc `corpus` (d-0000A), section s-0000A, with five claims for each animal `k`: c-<100k + j>
/// at order `a<k><j>`, body `<animal> herd`. Then [`fresh`].
fn animals(r: &mut Rdy) -> Vec<Vec<String>> {
    let mut blocks = vec![
        doc("d-0000A", "corpus", "a1"),
        blk("s-0000A", "section", "d-0000A", "a1", ""),
    ];
    let mut groups = vec![];
    for (k, a) in ANIMALS.iter().enumerate() {
        let mut g = vec![];
        for j in 0..5 {
            let c = nid("c", 100 * (k + 1) + j);
            blocks.push(blk(
                &c,
                "claim",
                "s-0000A",
                &format!("a{k:02}{j}"),
                &format!("{a} herd"),
            ));
            g.push(c);
        }
        groups.push(g);
    }
    blocks.extend(fresh());
    raw_commit(r, VIEW, blocks);
    groups
}

#[test]
fn vxs4_first_ten_creates_get_three_rows_in_operation_order() {
    let (_d, mut r) = project();
    let groups = animals(&mut r);
    let e = Local::new(r);
    // twelve created claims, each before the one made just before it, so the reading order is the
    // reverse of the operation order; a section between them is no source
    let mut ops = vec![
        json!({"op": "create", "type": "section", "id": "$sec", "parent": "s-0000N", "fields": {"title": "Inner"}}),
    ];
    for (k, a) in ANIMALS.iter().enumerate() {
        let mut op = json!({"op": "create", "type": "claim", "id": format!("${k}"),
                            "parent": "s-0000N", "body": format!("The {a}.")});
        if k > 0 {
            op["before"] = json!(format!("${}", k - 1));
        }
        ops.push(op);
    }
    let v = report(&e, json!(ops));
    let rows = named_rows(&v);
    let fors: BTreeSet<String> = rows
        .iter()
        .map(|r| r["for"].as_str().unwrap().to_string())
        .collect();
    let want: BTreeSet<String> = (0..10).map(|k| format!("${k}")).collect();
    assert_eq!(fors, want, "the first ten in operation order: {v:#}");
    assert_eq!(v["vector_neighbors_skipped"], json!(2), "{v:#}");
    for k in 0..10 {
        let mine = of_source(&rows, &format!("${k}"));
        // five neighbors of one score: the first three in reading order, two count nowhere
        let blocks: Vec<&str> = mine.iter().map(|r| r["block"].as_str().unwrap()).collect();
        let first: Vec<&str> = groups[k][..3].iter().map(String::as_str).collect();
        assert_eq!(blocks, first, "source {k}: {v:#}");
        for r in mine {
            let mut keys: Vec<&str> = r.as_object().unwrap().keys().map(String::as_str).collect();
            keys.sort();
            assert_eq!(
                keys,
                ["block", "doc", "for", "links", "score", "words"],
                "{r}"
            );
            assert_eq!(r["doc"], "d-0000A", "{r}");
            assert_eq!(r["words"], json!([ANIMALS[k]]), "{r}");
            assert!(r["score"].as_f64().unwrap() >= 0.25, "{r}");
        }
    }
    // the rows of all sources sort together by score
    let scores: Vec<f64> = rows.iter().map(|r| r["score"].as_f64().unwrap()).collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "{scores:?}");
}

#[test]
fn vxs4_changed_created_and_deleted_targets_are_excluded() {
    // heron claims at falling scores with the source: c-00001 (the best), c-00002, then
    // c-00003..c-00006
    let corpus = || {
        let mut b = vec![
            doc("d-0000A", "corpus", "a1"),
            blk("s-0000A", "section", "d-0000A", "a1", ""),
            blk(
                "c-00001",
                "claim",
                "s-0000A",
                "a1",
                "heron reed marsh pond bank",
            ),
            blk(
                "c-00002",
                "claim",
                "s-0000A",
                "a2",
                "heron reed marsh pond bank",
            ),
            blk("c-00003", "claim", "s-0000A", "a3", "heron reed marsh pond"),
            blk("c-00004", "claim", "s-0000A", "a4", "heron reed marsh"),
            blk("c-00005", "claim", "s-0000A", "a5", "heron reed"),
            blk("c-00006", "claim", "s-0000A", "a6", "heron"),
        ];
        b.extend(fresh());
        b
    };
    let (_d, mut r) = project();
    raw_commit(&mut r, VIEW, corpus());
    let e = Local::new(r);
    // the write creates two heron claims, changes the best neighbor (it stays a heron claim)
    // and deletes the second
    let v = report(
        &e,
        json!([
            {"op": "create", "type": "claim", "id": "$a", "parent": "s-0000N", "body": "Heron reed marsh pond bank."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "s-0000N", "body": "Heron reed."},
            {"op": "update", "id": "c-00001", "body": "Heron reed marsh pond bank!"},
            {"op": "delete", "id": "c-00002"}
        ]),
    );
    let rows = named_rows(&v);
    let a: Vec<&str> = of_source(&rows, "$a")
        .iter()
        .map(|r| r["block"].as_str().unwrap())
        .collect();
    // c-00001 and c-00002 would be the best two; the cap of three comes after the exclusion
    assert_eq!(a, ["c-00003", "c-00004", "c-00005"], "{v:#}");
    for r in &rows {
        let b = r["block"].as_str().unwrap();
        assert!(
            !["$a", "$b", "c-00001", "c-00002"].contains(&b),
            "a written block is no row: {r}"
        );
    }
    assert!(!of_source(&rows, "$b").is_empty(), "{v:#}");
    assert!(
        rows.iter().all(|r| r["for"] != "c-00001"),
        "a changed block is no source: {v:#}"
    );
    assert!(v.get("vector_neighbors_skipped").is_none(), "{v:#}");
    // without the write's own blocks, the briefing of the new claim would list c-00001 first
    let b = e
        .read()
        .brief(
            &view(),
            &rdy_engine::brief::Coverage {
                ids: vec![made(&v, "$a")],
                path: None,
            },
            Some(1_000_000),
        )
        .unwrap();
    assert_eq!(b.vector_neighbors[0].block, "c-00001");
    // a write that only changes an existing block lists none
    let u = report(
        &e,
        json!([{"op": "update", "id": "c-00003", "body": "Heron reed marsh pond."}]),
    );
    assert!(u.get("vector_neighbors").is_none(), "{u:#}");
    assert!(u.get("vector_neighbors_skipped").is_none(), "{u:#}");
}

/// An engine that, after the first write it is given is published and before it returns, waits
/// at `at_snapshot` while another thread writes, and goes on at `raced`: a write that lands
/// between a write and its report.
struct Racing {
    inner: Arc<Local>,
    at_snapshot: Arc<Barrier>,
    raced: Arc<Barrier>,
    armed: Mutex<bool>,
}

impl Engine for Racing {
    fn read(&self) -> Arc<ReadState> {
        self.inner.read()
    }
    fn write(&self, f: WriteFn) -> Result<Value, RdyError> {
        let out = self.inner.write(f);
        if std::mem::take(&mut *self.armed.lock().unwrap()) {
            self.at_snapshot.wait();
            self.raced.wait();
        }
        out
    }
    fn open_report(&self) -> OpenReport {
        self.inner.open_report()
    }
    fn root(&self) -> std::path::PathBuf {
        self.inner.root()
    }
    fn briefs(&self) -> Arc<rdy_engine::brief::BriefRegistry> {
        self.inner.briefs()
    }
    fn leases(&self) -> Arc<rdy_engine::semantic::lease::LeaseRegistry> {
        self.inner.leases()
    }
}

/// Doc `corpus` with five `otter` claims c-00001..c-00005 at falling scores, then [`fresh`].
fn otters() -> Vec<Block> {
    let mut b = vec![
        doc("d-0000A", "corpus", "a1"),
        blk("s-0000A", "section", "d-0000A", "a1", ""),
        blk("c-00001", "claim", "s-0000A", "a1", "otter"),
        blk("c-00002", "claim", "s-0000A", "a2", "otter river"),
        blk("c-00003", "claim", "s-0000A", "a3", "otter river bank"),
        blk(
            "c-00004",
            "claim",
            "s-0000A",
            "a4",
            "otter river bank stone",
        ),
        blk(
            "c-00005",
            "claim",
            "s-0000A",
            "a5",
            "otter river bank stone moss",
        ),
    ];
    b.extend(fresh());
    b
}

#[test]
fn vxs4_receipt_uses_its_produced_snapshot_under_a_racing_write() {
    let create = || json!([{"op": "create", "type": "claim", "id": "$n", "parent": "s-0000N", "body": "otter"}]);
    // the reference: the same write with nothing after it
    let (_d0, mut r0) = project();
    raw_commit(&mut r0, VIEW, otters());
    let alone = report(&Local::new(r0), create());
    assert_eq!(rows(&alone).len(), 3, "{alone:#}");
    // the same write, with a write between it and its report that deletes its best neighbor and
    // adds a better one
    let (_d, mut r) = project();
    raw_commit(&mut r, VIEW, otters());
    let e = Arc::new(Racing {
        inner: Arc::new(Local::new(r)),
        at_snapshot: Arc::new(Barrier::new(2)),
        raced: Arc::new(Barrier::new(2)),
        armed: Mutex::new(true),
    });
    let racer = {
        let (inner, at, done) = (
            Arc::clone(&e.inner),
            Arc::clone(&e.at_snapshot),
            Arc::clone(&e.raced),
        );
        std::thread::spawn(move || {
            at.wait();
            let ops = parse_test_ops(json!([
                {"op": "delete", "id": "c-00001"},
                {"op": "create", "type": "claim", "id": "$x", "parent": "s-0000A", "body": "otter otter"}
            ]))
            .unwrap();
            let v = inner
                .write(Box::new(move |r| {
                    let w = r.write(
                        &writer(),
                        WriteRequest {
                            view: VIEW.into(),
                            ops,
                            justification: None,
                            message: String::new(),
                        },
                    )?;
                    Ok(serde_json::to_value(w).unwrap())
                }))
                .expect("the racing write");
            done.wait();
            v
        })
    };
    let v = report(&*e, create());
    let raced = racer.join().unwrap();
    let x = raced["created"]["$x"].as_str().unwrap().to_string();
    let now = e.read().materialized(&view()).unwrap();
    assert!(now.get(&id("c-00001")).is_none(), "the racing write landed");
    assert!(now.get(&id(&x)).is_some(), "the racing write landed");
    // the receipt reads the state its write produced
    assert_eq!(named_rows(&v), named_rows(&alone), "{v:#}");
    assert_eq!(rows(&v)[0]["block"], "c-00001", "{v:#}");
    assert!(rows(&v).iter().all(|r| r["block"] != x.as_str()), "{v:#}");
}

/// Doc `bulk` (d-0000B) with `n` claims; claim `i` holds five of the words `g0..g63`
/// (spelled `gw<letter><letter>`), so each word is in about `5n / 64` claims. Then [`fresh`].
fn bulk(n: usize) -> Vec<Block> {
    let mut blocks = vec![
        doc("d-0000B", "bulk", "a1"),
        blk("s-0000B", "section", "d-0000B", "a1", ""),
    ];
    for i in 0..n {
        let words: Vec<String> = (0..5).map(|k| g_word((5 * i + k) % 64)).collect();
        blocks.push(blk(
            &nid("c", i + 1),
            "claim",
            "s-0000B",
            &format!("a{i:05}"),
            &words.join(" "),
        ));
    }
    blocks.extend(fresh());
    blocks
}

fn g_word(k: usize) -> String {
    let l = |x: usize| (b'a' + x as u8) as char;
    format!("gw{}{}", l(k / 8), l(k % 8))
}

#[test]
fn vxs4_vector_guard_keeps_the_acknowledged_write_successful() {
    // ten created claims, each with all 64 words: each seeds 64 words of about 200 postings, so
    // the receipt's 100,000 posting incidences stop the last sources
    let (_d, mut r) = project();
    raw_commit(&mut r, VIEW, bulk(2_400));
    let e = Local::new(r);
    let all: Vec<String> = (0..64).map(g_word).collect();
    let ops: Vec<Value> = (0..10)
        .map(|k| json!({"op": "create", "type": "claim", "id": format!("${k}"), "parent": "s-0000N", "body": all.join(" ")}))
        .collect();
    let v = report_of(&e, json!(ops)).expect("the write stands");
    let skipped = v["vector_neighbors_skipped"].as_u64().unwrap_or(0);
    assert!(
        skipped > 0 && skipped < 10,
        "a guard stopped some sources: {v:#}"
    );
    for k in 0..10 {
        assert!(
            e.read()
                .block(&view(), &id(&made(&v, &format!("${k}"))))
                .is_ok()
        );
    }
    assert!(v["consequences"].is_array(), "{v:#}");
    assert!(v["briefing"]["token"].is_string(), "{v:#}");
    // the sources a guard stopped give no rows: only the first ones have rows
    let rows = named_rows(&v);
    let fors: BTreeSet<String> = rows
        .iter()
        .map(|r| r["for"].as_str().unwrap().to_string())
        .collect();
    let scored = 10 - skipped as usize;
    let want: BTreeSet<String> = (0..scored).map(|k| format!("${k}")).collect();
    assert_eq!(fors, want, "{v:#}");
    // the same advice from the produced state with the receipt's budget; a briefing's larger
    // budget stops none
    let st = e.read();
    let (m, g) = (
        st.materialized(&view()).unwrap(),
        st.graph(&view()).unwrap(),
    );
    let w = st.vector_weights(WeightScope::Read(&view())).unwrap();
    let sources: Vec<BlockId> = (0..10).map(|k| id(&made(&v, &format!("${k}")))).collect();
    let written: BTreeSet<BlockId> = sources.iter().cloned().collect();
    let advice = |budget: &mut VectorBudget| {
        vector_neighbors_except(
            &m,
            &g,
            st.pack(),
            &w,
            &sources,
            AdviceCut::WRITE,
            budget,
            &|b| written.contains(b),
        )
    };
    assert_eq!(advice(&mut VectorBudget::write()).skipped, skipped as usize);
    assert_eq!(advice(&mut VectorBudget::briefing()).skipped, 0);
}

#[test]
fn vxs4_skipped_counts_sources_and_omits_zero_and_empty_fields() {
    let (_d, mut r) = project();
    let groups = animals(&mut r);
    let e = Local::new(r);
    // a structural block alone: no source, no member
    let v = report(
        &e,
        json!([{"op": "create", "type": "section", "parent": "s-0000N", "fields": {"title": "Inner"}}]),
    );
    for k in ["vector_neighbors", "vector_neighbors_skipped"] {
        assert!(v.get(k).is_none(), "{k}: {v:#}");
    }
    // a claim with no neighbor at the floor, and one with only numeric words (no seed): both
    // complete, nothing skipped
    let v = report(
        &e,
        json!([
            {"op": "create", "type": "claim", "parent": "s-0000N", "body": "Quokka."},
            {"op": "create", "type": "claim", "parent": "s-0000N", "body": "2026 12.5"}
        ]),
    );
    for k in ["vector_neighbors", "vector_neighbors_skipped"] {
        assert!(v.get(k).is_none(), "{k}: {v:#}");
    }
    // one source with five neighbors: three rows, the other two count nowhere
    let v = report(
        &e,
        json!([{"op": "create", "type": "claim", "id": "$n", "parent": "s-0000N", "body": "Lemur."}]),
    );
    let blocks: Vec<&str> = rows_ref(&v).map(|r| r["block"].as_str().unwrap()).collect();
    let first: Vec<&str> = groups[11][..3].iter().map(String::as_str).collect();
    assert_eq!(blocks, first, "{v:#}");
    assert!(v.get("vector_neighbors_skipped").is_none(), "{v:#}");
    // eleven created claims: the last is skipped, and counts once
    let ops: Vec<Value> = (0..11)
        .map(|k| json!({"op": "create", "type": "claim", "parent": "s-0000N", "body": format!("A {}.", ANIMALS[k])}))
        .collect();
    let v = report(&e, json!(ops));
    assert_eq!(v["vector_neighbors_skipped"], json!(1), "{v:#}");
    let fors: BTreeSet<&str> = rows_ref(&v).map(|r| r["for"].as_str().unwrap()).collect();
    assert_eq!(fors.len(), 10, "{v:#}");
}

fn rows_ref(v: &Value) -> impl Iterator<Item = &Value> {
    v["vector_neighbors"].as_array().into_iter().flatten()
}

/// The claims every surface creates in s-0000N, in order.
fn agree_ops() -> Value {
    json!([
        {"op": "create", "type": "claim", "id": "$a", "parent": "s-0000N", "body": "The aardvark herd."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "s-0000N", "body": "Bison camel herd."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "s-0000N", "body": "Quokka."}
    ])
}

/// Through `c`, in the session's own view: the briefing of s-0000N, then [`agree_ops`] citing
/// it; the reply.
fn mcp_write(c: &mut McpClient, run: &str) -> Value {
    let reply = c.call(
        "rdy_session",
        json!({"actor": format!("agent:{run}"), "run": run, "role": "writer"}),
    );
    assert!(reply.code().is_none());
    let b = c.call("rdy_brief", json!({"ids": ["s-0000N"]})).ok();
    let ts = b["base"]["token"].as_str().unwrap().to_string();
    c.call("rdy_write", json!({"brief": ts, "ops": agree_ops()}))
        .ok()
}

/// The rows of `v` with each created id named by its body.
fn by_body(v: &Value, st: &ReadState) -> Vec<Value> {
    let m = st.materialized(&view()).unwrap();
    rows(v)
        .into_iter()
        .map(|mut r| {
            let f = id(r["for"].as_str().unwrap());
            r["for"] = json!(m.get(&f).unwrap().body.clone());
            r
        })
        .collect()
}

#[test]
fn vxs4_local_daemon_mcp_and_import_receipts_agree() {
    // the corpus in canon, so an agent's own view reads it too
    let seed = |r: &mut Rdy| {
        animals(r);
        land_with_a_tie(r, VIEW);
    };
    // the person's local write
    let (_d, mut r) = project();
    seed(&mut r);
    let e = Local::new(r);
    let local = report(&e, agree_ops());
    let want = named_rows(&local);
    assert!(want.len() >= 6, "{local:#}");
    // in-process MCP (no daemon)
    let (_d2, mut r) = project();
    seed(&mut r);
    let dir = r.root().to_path_buf();
    r.close().unwrap();
    let mut c = McpClient::start(&dir);
    let v = mcp_write(&mut c, "local");
    drop(c);
    assert_eq!(named_rows(&v), want, "{v:#}");
    // MCP through the daemon
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    seed(&mut r);
    r.close().unwrap();
    let mut c = McpClient::start_proxy(&s);
    let v = mcp_write(&mut c, "daemon");
    drop(c);
    assert_eq!(named_rows(&v), want, "{v:#}");
    s.stop();
    // an import of a doc with the same claims, against the same doc and claims written locally
    let (_q, mut q) = project();
    write(&mut q, VIEW, import_ops()).unwrap();
    land(&mut q, VIEW).unwrap();
    let tree = q.root().join("export-tree");
    q.read().export(&Scope::Canon, &tree).unwrap();
    q.close().unwrap();
    let (_p, mut p) = project();
    seed(&mut p);
    let e = Local::new(p);
    let imported =
        rdy_engine::import_api::run_import(&e, &writer(), VIEW, &tree).expect("the import");
    let (_l, mut l) = project();
    seed(&mut l);
    let le = Local::new(l);
    let written = report(&le, import_ops());
    let want = by_body(&written, &le.read());
    assert!(want.len() >= 6, "{written:#}");
    assert_eq!(by_body(&imported, &e.read()), want, "{imported:#}");
    assert_eq!(
        imported.get("vector_neighbors_skipped"),
        written.get("vector_neighbors_skipped")
    );
}

/// A doc `import` with one section and the claims of [`agree_ops`].
fn import_ops() -> Value {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "import", "title": "Import", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Claims"}}),
    ];
    for mut op in agree_ops().as_array().unwrap().iter().cloned() {
        op["parent"] = json!("$s");
        ops.push(op);
    }
    json!(ops)
}

#[test]
fn vxs4_advice_changes_no_consequence_or_created_token() {
    let (_d, mut r) = project();
    animals(&mut r);
    let e = Local::new(r);
    let v = report(&e, agree_ops());
    assert!(!rows(&v).is_empty(), "{v:#}");
    let ids: BTreeSet<BlockId> = ["$a", "$b", "$c"]
        .iter()
        .map(|p| id(&made(&v, p)))
        .collect();
    // the token is the full briefing's of the created blocks, and its record covers them alone
    let full = e.read().brief_ids(&view(), &ids, None).unwrap();
    assert_eq!(
        v["briefing"],
        json!({"token": full.base.token, "covers": 3}),
        "{v:#}"
    );
    let rec: Arc<BriefRecord> = e.briefs().get(&full.base.token).unwrap();
    assert_eq!(rec.covers, ids);
    let advised: BTreeSet<String> = rows(&v)
        .iter()
        .map(|r| r["block"].as_str().unwrap().to_string())
        .collect();
    for a in &advised {
        assert!(!rec.recorded.contains_key(&id(a)), "{a} is recorded");
    }
    // no consequence, suspect or plan item names a row
    assert_eq!(v["consequences"], json!([]), "{v:#}");
    let text = serde_json::to_string(&json!({
        "consequences": v["consequences"],
        "on_change": v["on_change"],
        "suggestions": v["suggestions"],
        "coverage": v["coverage"],
    }))
    .unwrap();
    for a in &advised {
        assert!(!text.contains(a.as_str()), "{a}: {text}");
    }
    assert!(e.read().suspects(&view()).unwrap().is_empty());
}

#[test]
fn vxs4_write_advice_keeps_scores_at_the_point_two_five_boundary() {
    // every word of the source and of c-00002 is in three blocks, so all have one weight: the
    // source and c-00002 share one of four words, an exact score of 0.25; c-00003 adds a fifth
    // word and scores below it. The third holder of birch, cedar and dahlia is the write's own
    // second claim, never a row.
    let (_d, mut r) = project();
    let mut blocks = vec![
        doc("d-0000A", "corpus", "a1"),
        blk("s-0000A", "section", "d-0000A", "a1", ""),
        blk("c-00001", "claim", "s-0000A", "a1", "birch cedar dahlia"),
        blk(
            "c-00002",
            "claim",
            "s-0000A",
            "a2",
            "amber poplar quince rowan",
        ),
        blk(
            "c-00003",
            "claim",
            "s-0000A",
            "a3",
            "amber poplar quince rowan zinnia",
        ),
        blk("c-00004", "claim", "s-0000A", "a4", "poplar quince rowan"),
    ];
    blocks.extend(fresh());
    raw_commit(&mut r, VIEW, blocks);
    let e = Local::new(r);
    let v = report(
        &e,
        json!([
            {"op": "create", "type": "claim", "id": "$s", "parent": "s-0000N", "body": "amber birch cedar dahlia"},
            {"op": "create", "type": "claim", "id": "$t", "parent": "s-0000N", "body": "birch cedar dahlia"}
        ]),
    );
    let st = e.read();
    let w = st.vector_weights(WeightScope::Read(&view())).unwrap();
    let src = id(&made(&v, "$s"));
    let exact = |b: &str| {
        score_pair(
            &w,
            Doc::Block(&src),
            Doc::Block(&id(b)),
            &mut VectorBudget::write(),
        )
        .unwrap()
    };
    assert_eq!(exact("c-00002"), 0.25);
    assert!(exact("c-00003") < 0.25, "{}", exact("c-00003"));
    let rows = named_rows(&v);
    let mine: Vec<(&str, f64)> = of_source(&rows, "$s")
        .iter()
        .map(|r| (r["block"].as_str().unwrap(), r["score"].as_f64().unwrap()))
        .collect();
    assert_eq!(
        mine,
        [("c-00001", round3(exact("c-00001"))), ("c-00002", 0.25)],
        "{v:#}"
    );
    assert_eq!(
        of_source(&rows, "$s")[1]["words"],
        json!(["amber"]),
        "{v:#}"
    );
}
