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

#[test]
fn area1_a_project_pack_with_areas_loads_through_the_engine() {
    let (dir, r) = project();
    drop(r);
    std::fs::write(
        dir.path().join(".rdy/packs/team.yaml"),
        "name: team\nversion: \"1\"\nextends: rdy-standard\nareas:\n  payments: { paths: [wallets] }\ntensions: { fork: advisory }\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: team\n").unwrap();
    let r = rdy_engine::Rdy::open(dir.path()).unwrap();
    assert_eq!(r.pack().areas_for_path("wallets/x"), vec!["payments"]);
    assert_eq!(
        r.pack().tension_severity(rdy_schema::TensionKind::Fork),
        (rdy_schema::TensionSeverity::Advisory, true)
    );
}

#[test]
fn area1_a_project_pack_with_a_bad_area_does_not_open_and_names_it() {
    let (dir, r) = project();
    drop(r);
    std::fs::write(
        dir.path().join(".rdy/packs/team.yaml"),
        "name: team\nversion: \"1\"\nextends: rdy-standard\nareas:\n  Payments: { paths: [wallets] }\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: team\n").unwrap();
    let e = rdy_engine::Rdy::open(dir.path()).err().expect("refused");
    assert_eq!(e.code, "E_CONFIG", "{e}");
    assert!(
        e.message.contains("area \"Payments\" has an invalid name"),
        "{e}"
    );
}

#[test]
fn pres1_presentational_is_a_trait_and_excludes_normative_and_defines() {
    let ok = "name: p\nversion: '1'\nextends: rdy-standard\nblock_types:\n  aside: { prefix: as, traits: [presentational], body: required }\n";
    let mut set = PackSet::with_builtin();
    set.add_yaml(ok).unwrap();
    let pack = set.resolve("p").unwrap();
    assert!(pack.is_presentational("aside") && !pack.is_presentational("prose"));
    assert!(
        !pack.is_presentational("nope"),
        "an unknown type is not presentational"
    );
    for (traits, name) in [
        ("presentational, normative", "q"),
        ("presentational, defines", "r"),
    ] {
        let bad = format!(
            "name: {name}\nversion: '1'\nextends: rdy-standard\nblock_types:\n  aside: {{ prefix: as, traits: [{traits}], body: required }}\n"
        );
        let mut set = PackSet::with_builtin();
        set.add_yaml(&bad).unwrap();
        let e = set.resolve(name).unwrap_err();
        assert!(
            e.to_string()
                .contains("type aside: presentational excludes normative and defines"),
            "{e}"
        );
    }
}
