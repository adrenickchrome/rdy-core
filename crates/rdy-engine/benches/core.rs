use criterion::{Criterion, criterion_group, criterion_main};
use rdy_engine::{Rdy, Scope, Session, WriteRequest, testkit};
use rdy_model::Role;
use serde_json::json;

fn blocks() -> usize {
    std::env::var("RDY_BENCH_BLOCKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000)
}

fn bench(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let ids = testkit::generate(dir.path(), blocks());
    let mut r = Rdy::open(dir.path()).unwrap();
    let work = Scope::View("work".into());
    r.read().materialized(&work).unwrap();
    r.read().graph(&work).unwrap();
    let s = Session {
        actor: "agent:bench".into(),
        run: None,
        role: Role::Writer,
    };
    let parent = r
        .read()
        .block(&work, &ids[0])
        .unwrap()
        .parent
        .unwrap()
        .to_string();

    c.bench_function("write_commit_one_claim", |b| {
        b.iter(|| {
            r.write(&s, WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([{"op": "create", "type": "claim", "parent": parent, "body": "bench"}])).unwrap(), justification: None, message: String::new() }).unwrap()
        })
    });
    r.read().graph(&work).unwrap();
    let probe = ids[ids.len() / 2].clone();
    c.bench_function("context_query", |b| {
        b.iter(|| r.read().context(&work, &probe).unwrap())
    });
    c.bench_function("impact_query", |b| {
        b.iter(|| r.read().impact(&work, &ids[0], 3).unwrap())
    });
    drop(r);
    c.bench_function("cold_open_materialize_graph", |b| {
        b.iter(|| {
            let r = Rdy::open(dir.path()).unwrap();
            r.read().graph(&work).unwrap();
        })
    });
}

criterion_group! { name = benches; config = Criterion::default().sample_size(20); targets = bench }
criterion_main!(benches);
