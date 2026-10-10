use rdy_engine::*;
use rdy_model::*;
use std::collections::BTreeMap;

fn blk(id: &str, parent: Option<&str>, order: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: parent.map(|p| BlockId::parse(p).unwrap()),
        order: order.into(),
        fields: BTreeMap::new(),
        body: "x".into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
    }
}

#[test]
fn init_then_open() {
    let dir = tempfile::tempdir().unwrap();
    {
        let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        assert_eq!(r.pack().name, "rdy-standard");
    }
    assert!(dir.path().join(".rdy.yaml").exists());
    let r = Rdy::open(dir.path()).unwrap();
    assert_eq!(r.pack().name, "rdy-standard");
}

#[test]
fn open_without_project_and_locking_errors() {
    let dir = tempfile::tempdir().unwrap();
    let e = Rdy::open(dir.path()).err().unwrap();
    assert_eq!(e.code, "E_NO_PROJECT");
    assert!(e.fix.contains("rdy init"));
    let _r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    assert_eq!(Rdy::open(dir.path()).err().unwrap().code, "E_LOCKED");
}

#[test]
fn unknown_pack_is_a_config_error() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        Rdy::init(dir.path(), "nope").err().unwrap().code,
        "E_CONFIG"
    );
}

#[test]
fn custom_pack_files_are_loaded() {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(
        dir.path().join(".rdy/packs/team.yaml"),
        "name: team\nversion: \"1\"\nextends: rdy-standard\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: team\n").unwrap();
    assert_eq!(Rdy::open(dir.path()).unwrap().pack().name, "team");
}

#[test]
fn materialized_children_are_ordered_and_apply_is_incremental() {
    let mut m = Materialized::build(StateSnapshot::default(), BTreeMap::new());
    let a = blk("c-AAAAA", Some("s-PPPPP"), "W");
    let b = blk("c-BBBBB", Some("s-PPPPP"), "V");
    m = m.with(&[
        Op::Create { block: a.clone() },
        Op::Create { block: b.clone() },
    ]);
    let parent = BlockId::parse("s-PPPPP").unwrap();
    let ids: Vec<_> = m
        .children(Some(&parent))
        .iter()
        .map(|b| b.id.as_str().to_string())
        .collect();
    assert_eq!(ids, vec!["c-BBBBB", "c-AAAAA"]);
    let mut moved = a.clone();
    moved.order = "U".into();
    m = m.with(&[Op::Update {
        base: a.version_hash(),
        block: moved,
    }]);
    assert_eq!(m.children(Some(&parent))[0].id.as_str(), "c-AAAAA");
    m = m.with(&[Op::Delete {
        base: b.version_hash(),
        id: b.id.clone(),
    }]);
    assert_eq!(m.children(Some(&parent)).len(), 1);
    assert_eq!(m.snapshot().blocks.len(), 1);
}

#[test]
fn init_refuses_an_existing_project() {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let e = Rdy::init(dir.path(), "rdy-standard")
        .err()
        .expect("second init must fail");
    assert_eq!(e.code, "E_ALREADY_EXISTS");
}
