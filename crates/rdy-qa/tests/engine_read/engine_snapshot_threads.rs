//! snap1 under threads: readers hold states while a writer commits and lands; every state a
//! reader holds is internally consistent and never changes; the writer reads its own writes.
use rdy_engine::Scope;
use rdy_qa::fixtures::{land, project, write};
use serde_json::json;
use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};

/// Sets the flag when dropped, so the readers stop even if the writer panics.
struct SetOnDrop(Arc<AtomicBool>);
impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn consistent(m: &rdy_engine::Materialized) {
    assert_eq!(m.len(), m.versions().count());
    for b in m.iter() {
        assert_eq!(m.version_of(&b.id), Some(b.version_hash()));
        if let Some(p) = &b.parent {
            assert!(m.get(p).is_some(), "{} has a missing parent {p}", b.id);
        }
    }
}

#[test]
fn snap1_readers_on_threads_see_consistent_unchanging_states_while_the_writer_runs() {
    let (_d, mut r0) = project();
    let res = write(&mut r0, "w", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "d", "title": "D", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}}
    ])).unwrap();
    let section = res.created["$s"].clone();
    let r = Arc::new(Mutex::new(r0));
    let start = Arc::new(Barrier::new(5));
    let writes = 150;
    let done = Arc::new(AtomicBool::new(false));
    let writer = {
        let (r, start, section, done) = (
            Arc::clone(&r),
            Arc::clone(&start),
            section.clone(),
            Arc::clone(&done),
        );
        std::thread::spawn(move || {
            let _done = SetOnDrop(done);
            start.wait();
            for i in 0..writes {
                let mut g = r.lock().unwrap();
                let res = write(&mut g, "w", json!([{"op": "create", "type": "claim", "parent": section, "body": format!("c{i}")}])).unwrap();
                let id: rdy_model::BlockId =
                    rdy_model::BlockId::parse(res.created.values().next().unwrap()).unwrap();
                let st = g.read();
                assert!(
                    st.materialized(&Scope::View("w".into()))
                        .unwrap()
                        .get(&id)
                        .is_some(),
                    "read-your-writes at {i}"
                );
                if i % 50 == 49 {
                    land(&mut g, "w").unwrap();
                }
                // std's Mutex is unfair: without letting go and pausing, the writer takes the lock
                // straight back and starves the readers until it is finished.
                drop(g);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };
    let readers: Vec<_> = (0..4)
        .map(|t| {
            let (r, start, done) = (Arc::clone(&r), Arc::clone(&start), Arc::clone(&done));
            std::thread::spawn(move || {
                start.wait();
                let mut last_seq = None;
                let mut seqs = BTreeSet::new();
                let mut heads = HashSet::new();
                let mut held: Vec<(
                    Arc<rdy_engine::ReadState>,
                    Scope,
                    usize,
                    Option<rdy_model::Hash>,
                )> = vec![];
                // Keep reading for as long as the writer runs (and at least 300 rounds), so the
                // states held early are re-checked after later commits and landings.
                let mut i = 0usize;
                while i < 300 || !done.load(Ordering::SeqCst) {
                    let st = r.lock().unwrap().read();
                    if let Some(s) = last_seq {
                        assert!(st.seq() >= s, "reader {t}: seq went backwards");
                    }
                    last_seq = Some(st.seq());
                    seqs.insert(st.seq());
                    heads.insert(st.canon_head());
                    let scope = if i.is_multiple_of(3) {
                        Scope::Canon
                    } else {
                        Scope::View("w".into())
                    };
                    let m = st.materialized(&scope).unwrap();
                    consistent(&m);
                    assert_eq!(m.len(), m.snapshot().blocks.len());
                    if let Some(b) = m.iter().find(|b| b.block_type == "claim") {
                        let _ = st.context(&scope, &b.id).unwrap();
                    }
                    st.graph(&scope).unwrap();
                    if i.is_multiple_of(40) {
                        held.push((Arc::clone(&st), scope.clone(), m.len(), st.canon_head()));
                    }
                    i += 1;
                }
                for (st, scope, len, head) in held {
                    assert_eq!(
                        st.materialized(&scope).unwrap().len(),
                        len,
                        "reader {t}: a held state changed"
                    );
                    assert_eq!(st.canon_head(), head);
                }
                (seqs, heads)
            })
        })
        .collect();
    writer.join().unwrap();
    let (mut seqs, mut heads) = (BTreeSet::new(), HashSet::new());
    for t in readers {
        let (s, h) = t.join().unwrap();
        seqs.extend(s);
        heads.extend(h);
    }
    eprintln!(
        "readers saw {} seqs, {} canon heads",
        seqs.len(),
        heads.len()
    );
    // The interleaving must be real, or the held-state assertions above are vacuous: the readers
    // together saw many distinct commit points and the landings happen mid-run.
    assert!(
        heads.len() >= 3,
        "readers saw {} canon heads: the landings did not overlap them",
        heads.len()
    );
    assert!(
        seqs.len() >= writes / 2,
        "readers saw only {} distinct seqs of ~{} publishes",
        seqs.len(),
        writes
    );
    let g = r.lock().unwrap();
    let st = g.read();
    assert_eq!(st.materialized(&Scope::Canon).unwrap().len(), 2 + writes);
    assert!(st.included("w").is_empty());
    drop(st);
    assert!(rdy_qa::invariants::check(&g, &Scope::Canon).is_ok());
}
