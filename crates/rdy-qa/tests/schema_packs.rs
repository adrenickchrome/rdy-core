//! Pack loading: a pack name defined twice (pack1).
use rdy_qa::fixtures::*;
use rdy_schema::{PackSet, SchemaError};

#[test]
fn pack1_a_second_pack_with_a_taken_name_is_refused() {
    let mut set = PackSet::with_builtin();
    let e = set
        .add_yaml_from("name: rdy-standard\nversion: \"9\"\n", "x.yaml")
        .unwrap_err();
    assert!(matches!(&e, SchemaError::Duplicate { .. }), "{e}");
    assert!(
        e.to_string().contains("x.yaml") && e.to_string().contains("built-in"),
        "{e}"
    );
    set.add_yaml_from(
        "name: team\nversion: \"1\"\nextends: rdy-standard\n",
        "a.yaml",
    )
    .unwrap();
    let e = set
        .add_yaml_from("name: team\nversion: \"2\"\n", "b.yaml")
        .unwrap_err();
    assert!(
        e.to_string().contains("a.yaml") && e.to_string().contains("b.yaml"),
        "{e}"
    );
    // the first definition is kept
    assert_eq!(set.resolve("team").unwrap().version, "1");
}

#[test]
fn pack1_a_project_with_two_packs_of_one_name_does_not_open() {
    let (dir, r) = project();
    drop(r);
    let packs = dir.path().join(".rdy/packs");
    std::fs::write(
        packs.join("a.yaml"),
        "name: team\nversion: \"1\"\nextends: rdy-standard\n",
    )
    .unwrap();
    std::fs::write(
        packs.join("b.yaml"),
        "name: team\nversion: \"2\"\nextends: rdy-standard\n",
    )
    .unwrap();
    let e = rdy_engine::Rdy::open(dir.path()).err().expect("refused");
    assert_eq!(e.code, "E_CONFIG", "{e}");
    assert!(
        e.message.contains("a.yaml") && e.message.contains("b.yaml"),
        "{e}"
    );
    std::fs::remove_file(packs.join("b.yaml")).unwrap();
    std::fs::write(packs.join("c.yaml"), "name: rdy-standard\nversion: \"9\"\n").unwrap();
    let e = rdy_engine::Rdy::open(dir.path()).err().expect("refused");
    assert_eq!(e.code, "E_CONFIG", "{e}");
    assert!(
        e.message.contains("c.yaml") && e.message.contains("built-in"),
        "{e}"
    );
    assert!(e.fix.contains("extends"), "{e}");
}
