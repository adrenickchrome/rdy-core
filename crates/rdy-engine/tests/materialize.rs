//! pmap1: Materialized is a persistent structure; `with` shares what it does not touch.
use rdy_engine::{Materialized, Rdy, Scope, testkit};
use rdy_model::{Block, BlockId, Op};
use std::sync::Arc;

fn assert_send_sync<T: Send + Sync>() {}

fn claim(parent: Option<BlockId>, body: &str) -> Block {
    Block {
        id: BlockId::new("c"),
        block_type: "claim".into(),
        parent,
        order: "V".into(),
        fields: Default::default(),
        body: body.into(),
        links: Default::default(),
        pins: Default::default(),
    }
}

#[test]
fn pmap1_materialized_is_send_sync() {
    assert_send_sync::<Materialized>();
}

#[test]
fn pmap1_with_leaves_the_receiver_untouched_and_shares_untouched_blocks() {
    let dir = tempfile::tempdir().unwrap();
    testkit::generate(dir.path(), 2_000);
    let r = Rdy::open(dir.path()).unwrap();
    let m = r.materialized(&Scope::View("work".into())).unwrap().clone();
    let before = m.snapshot();
    let victim: BlockId = m
        .iter()
        .find(|b| b.block_type == "claim")
        .unwrap()
        .id
        .clone();
    let mut edited: Block = m.get(&victim).unwrap().clone();
    edited.body = "changed by pmap1".into();
    let base = m.version_of(&victim).unwrap();
    let next = m.with(&[Op::Update {
        base,
        block: edited.clone(),
    }]);
    assert_eq!(m.snapshot(), before, "the receiver did not change");
    assert_eq!(next.get(&victim).unwrap().body, "changed by pmap1");
    assert_eq!(next.version_of(&victim), Some(edited.version_hash()));
    assert_eq!(next.len(), m.len());
    let mut shared = 0;
    for id in m.ids() {
        if id != &victim {
            assert!(
                Arc::ptr_eq(&m.block_arc(id).unwrap(), &next.block_arc(id).unwrap()),
                "{id} was copied"
            );
            shared += 1;
        }
    }
    assert_eq!(shared, m.len() - 1);
    let gone = next.with(&[Op::Delete {
        id: victim.clone(),
        base: edited.version_hash(),
    }]);
    assert!(gone.get(&victim).is_none());
    assert_eq!(gone.len(), m.len() - 1);
    assert!(
        next.get(&victim).is_some(),
        "a later with never reaches back"
    );
}

#[test]
fn pmap1_with_equals_build_of_the_applied_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    testkit::generate(dir.path(), 300);
    let mut r = Rdy::open(dir.path()).unwrap();
    let work = Scope::View("work".into());
    let before = r.materialized(&work).unwrap().clone();
    let section = before
        .iter()
        .find(|b| b.block_type == "section")
        .unwrap()
        .id
        .clone();
    let ops = serde_json::from_value(serde_json::json!([
        {"op": "create", "type": "claim", "parent": section.to_string(), "body": "new"},
        {"op": "delete", "id": before.iter().find(|b| b.block_type == "claim").unwrap().id.to_string()}
    ]))
    .unwrap();
    let s = rdy_engine::Session {
        actor: "a".into(),
        run: None,
        role: rdy_model::Role::Writer,
    };
    r.write(
        &s,
        rdy_engine::WriteRequest {
            view: "work".into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let change_ops: Vec<Op> = {
        let v = r.store().view("work").unwrap();
        let c = r.store().change(v.changes.last().unwrap()).unwrap();
        c.ops.clone()
    };
    let incremental = before.with(&change_ops);
    let rebuilt = {
        let snap = r.store().view_state("work").unwrap();
        let blocks = r.store().blocks(&snap).unwrap();
        Materialized::build(snap, blocks)
    };
    assert_eq!(incremental.snapshot(), rebuilt.snapshot());
    assert_eq!(
        incremental.iter().map(|b| b.id.clone()).collect::<Vec<_>>(),
        rebuilt.iter().map(|b| b.id.clone()).collect::<Vec<_>>()
    );
    assert_eq!(
        incremental.names_under(Some(&section)),
        rebuilt.names_under(Some(&section))
    );
    assert_eq!(
        incremental.children(Some(&section)).len(),
        rebuilt.children(Some(&section)).len()
    );
}

#[test]
#[cfg_attr(debug_assertions, ignore = "timing: run with --release")]
fn pmap1_with_on_ten_thousand_blocks_is_under_a_millisecond() {
    let dir = tempfile::tempdir().unwrap();
    testkit::generate(dir.path(), 10_000);
    let r = Rdy::open(dir.path()).unwrap();
    let m = r.materialized(&Scope::View("work".into())).unwrap().clone();
    let section = m
        .iter()
        .find(|b| b.block_type == "section")
        .unwrap()
        .id
        .clone();
    let mut times = vec![];
    for i in 0..50 {
        let b = claim(Some(section.clone()), &format!("t{i}"));
        let t = std::time::Instant::now();
        let _n = m.with(&[Op::Create { block: b }]);
        times.push(t.elapsed());
    }
    times.sort();
    assert!(
        times[49] < std::time::Duration::from_millis(1),
        "max {:?}",
        times[49]
    );
}
