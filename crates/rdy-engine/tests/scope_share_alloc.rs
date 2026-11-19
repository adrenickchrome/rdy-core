//! A scope off canon holds only what differs from canon. Reading an old landing, a proposal
//! version or a view after a landing, with its graph and lexicon, adds a small part of what
//! canon's own build holds, not a private copy of the whole project. The vector weights a
//! briefing and a search fill count too (D-AT49D). Its own binary: the global allocator below
//! counts the live bytes of the whole process, and no other test shares it.
use rdy_engine::testkit::rebuilt_scope_for_tests;
use rdy_engine::vectors::{Doc, WeightScope};
use rdy_engine::*;
use rdy_model::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// A global allocator that keeps the live bytes of the process.
mod live_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicIsize, Ordering};

    pub struct Live;

    static LIVE: AtomicIsize = AtomicIsize::new(0);

    // SAFETY: every call goes to `System` with the caller's arguments unchanged.
    #[allow(unsafe_code)]
    unsafe impl GlobalAlloc for Live {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc_zeroed` requires.
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
            // SAFETY: `ptr` came from `System` with `layout` (every allocation here is System's).
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            LIVE.fetch_add(
                new_size as isize - layout.size() as isize,
                Ordering::Relaxed,
            );
            // SAFETY: `ptr` came from `System` with `layout`; the caller's new size.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    /// The bytes `f` left live: what it allocated and did not free.
    pub fn kept_by(f: impl FnOnce()) -> isize {
        let start = LIVE.load(Ordering::SeqCst);
        f();
        LIVE.load(Ordering::SeqCst) - start
    }
}

#[global_allocator]
static ALLOC: live_alloc::Live = live_alloc::Live;

/// The summary a version is proposed with.
const SUMMARY: &str = "test summary";

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "w".into(),
        },
    )
    .unwrap()
}

/// Proposes and lands view `view`; returns the landing's scope text and the version's.
fn land(r: &mut Rdy, view: &str) -> (String, String) {
    let p = r.propose(&writer(), view, "t", Some(SUMMARY)).unwrap();
    let landing = r.land(&approver(), &p.id).unwrap();
    (format!("landing:{landing}"), format!("proposal:{}", p.id))
}

/// A doc of `n` entities and `n` notes that name them, in one write.
fn doc_ops(doc: usize, n: usize) -> Value {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"title": format!("Guide {doc}"), "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}}),
    ];
    for i in 0..n {
        ops.push(
            json!({"op": "create", "type": "entity", "id": format!("$e{i}"), "parent": "$s",
            "fields": {"name": format!("Widget {doc} {i}")}}),
        );
        ops.push(json!({"op": "create", "type": "note", "id": format!("$n{i}"), "parent": "$s",
            "body": format!("The Widget {doc} {i} feeds the Gateway `code_{doc}_{i}` and the Ledger of part {i}.")}));
    }
    json!(ops)
}

/// The live bytes reading `scope` with its graph and lexicon adds to `st`.
fn read_cost(st: &ReadState, scope: &Scope) -> isize {
    live_alloc::kept_by(|| {
        st.materialized(scope).unwrap();
        st.graph(scope).unwrap();
        st.lexicon(scope).unwrap();
    })
}

/// The live bytes filling `scope`'s vector weights adds to `st`, the way a briefing and a search
/// read them. The briefing reads the blocks whose body starts with `source` (one doc's notes):
/// each one's two vectors, and the norms of every block that shares a seed word or a seed
/// target with it (every seed, a superset of the 64 heaviest a briefing takes). The search
/// ranks every block that holds a word of `query`, which reads each one's lexical norm.
fn weight_cost(st: &ReadState, scope: &Scope, source: &str, query: &[&str]) -> isize {
    let m = st.materialized(scope).unwrap();
    let lexicon = st.lexicon(scope).unwrap();
    let sources: Vec<BlockId> = m
        .iter()
        .filter(|b| b.body.starts_with(source))
        .map(|b| b.id.clone())
        .collect();
    assert!(!sources.is_empty(), "{source}: the briefing has sources");
    live_alloc::kept_by(|| {
        let w = st.vector_weights(WeightScope::Read(scope)).unwrap();
        let mut norms = 0;
        for id in &sources {
            let d = Doc::Block(id);
            w.lexical(d).unwrap();
            w.link(d).unwrap();
            let words: Option<Vec<String>> = w
                .counts(d)
                .map(|c| c.iter().map(|(k, _)| k.to_string()).collect());
            for word in words.unwrap_or_default() {
                if w.word_is_seed(&word) {
                    for cand in lexicon.word_blocks_of(&word).into_iter().flatten() {
                        w.lexical_norm(Doc::Block(cand)).unwrap();
                        norms += 1;
                    }
                }
            }
            for t in w
                .targets(d)
                .map(|t| t.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
            {
                if w.target_is_seed(&t) {
                    for cand in lexicon.pointing_at(&t).into_iter().flatten() {
                        w.link_norm(Doc::Block(cand)).unwrap();
                        norms += 1;
                    }
                }
            }
        }
        for word in query {
            for hit in lexicon.word_blocks_of(word).into_iter().flatten() {
                w.lexical_norm(Doc::Block(hit)).unwrap();
                norms += 1;
            }
        }
        assert!(
            norms > 1000,
            "the reads fill the weights: {norms} norm reads"
        );
    })
}

#[test]
fn scopes_off_canon_add_under_a_tenth_of_canon() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    // about 4000 blocks, landed
    let mut entities = vec![];
    for doc in 0..20 {
        let w = write(&mut r, "work", doc_ops(doc, 100));
        entities.push(w.created["$e0"].clone());
        entities.push(w.created["$e1"].clone());
    }
    land(&mut r, "work");
    // four landings that each change a few blocks; their landings are read below
    let mut landings = vec![];
    let mut versions = vec![];
    for (n, e) in entities.iter().take(4).enumerate() {
        let mut ops = doc_ops(100 + n, 2).as_array().unwrap().clone();
        ops.push(json!({"op": "update", "id": e, "fields": {"name": format!("Renamed {n}")}}));
        write(&mut r, "work", json!(ops));
        let (l, v) = land(&mut r, "work");
        landings.push(l);
        versions.push(v);
    }
    // four views, two of which conflict with the landing after them
    let contested = entities[10].clone();
    let views: Vec<String> = (0..4).map(|i| format!("v{i}")).collect();
    for (i, v) in views.iter().enumerate() {
        let mut ops = doc_ops(200 + i, 2).as_array().unwrap().clone();
        if i >= 2 {
            ops.push(
                json!({"op": "update", "id": contested, "fields": {"name": format!("View {i}")}}),
            );
        }
        write(&mut r, v, json!(ops));
    }
    let mut ops = doc_ops(300, 2).as_array().unwrap().clone();
    ops.push(json!({"op": "update", "id": contested, "fields": {"name": "Landed"}}));
    write(&mut r, "work", json!(ops));
    land(&mut r, "work");

    let st = r.read();
    // canon's build with its graph and lexicon, and what a build of it from the store holds
    st.graph(&Scope::Canon).unwrap();
    st.lexicon(&Scope::Canon).unwrap();
    let mut held = None;
    let canon = live_alloc::kept_by(|| {
        held = Some(rebuilt_scope_for_tests(&st, &Scope::Canon).unwrap());
    });
    drop(held);
    assert!(canon > 1_000_000, "canon holds {canon} bytes");
    let mut scopes: Vec<(String, Scope)> = vec![];
    for s in landings.iter().chain(&versions) {
        scopes.push((s.clone(), st.resolve_scope(s).unwrap()));
    }
    for v in &views {
        scopes.push((v.clone(), Scope::View(v.clone())));
    }
    let mut costs: BTreeMap<String, isize> = BTreeMap::new();
    for (what, scope) in &scopes {
        let added = read_cost(&st, scope);
        eprintln!("{what}: {added} bytes (canon {canon})");
        assert!(
            added * 10 < canon,
            "{what} added {added} bytes: canon holds {canon}"
        );
        costs.insert(what.clone(), added);
    }
    // D-AT49D: on an old landing and on views read after a landing (one that merges cleanly,
    // one that conflicts), a briefing and a search fill the weights. All the scope holds beyond
    // canon's build, its blocks, graph, lexicon and weights, stays under a tenth of canon's.
    let weighed = [
        (landings[0].clone(), "The Widget 100 "),
        ("v0".to_string(), "The Widget 200 "),
        ("v2".to_string(), "The Widget 202 "),
    ];
    for (what, source) in weighed {
        let scope = &scopes.iter().find(|(w, _)| *w == what).unwrap().1;
        let weights = weight_cost(&st, scope, source, &["gateway", "ledger"]);
        let added = costs[&what] + weights;
        eprintln!("{what}: {weights} weight bytes, {added} in all (canon {canon})");
        assert!(weights > 0, "{what}: the reads cached weights");
        assert!(
            added * 10 < canon,
            "{what} added {added} bytes with its weights: canon holds {canon}"
        );
    }
}
