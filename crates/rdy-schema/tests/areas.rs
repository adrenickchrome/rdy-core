//! The pack areas section (area1).
use rdy_schema::*;

fn team(areas: &str) -> Pack {
    let mut set = PackSet::with_builtin();
    let name = set
        .add_yaml(&format!(
            "name: team\nversion: \"1\"\nextends: rdy-standard\n{areas}"
        ))
        .unwrap();
    set.resolve(&name).unwrap()
}

#[test]
fn area1_the_standard_pack_declares_no_areas() {
    let p = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    assert!(p.areas.is_empty());
    assert!(p.areas_for_path("wallets/transactions").is_empty());
}

#[test]
fn area1_a_pack_declares_areas_with_descriptions_and_globs() {
    let p = team(
        "areas:\n  payments:\n    description: money in and out\n    paths: [wallets, payments/**]\n  identity:\n    paths: [identity/*]\n  glossary:\n    paths: ['**/glossary']\n",
    );
    assert_eq!(p.areas.len(), 3);
    assert_eq!(p.areas["payments"].description, "money in and out");
    assert_eq!(p.areas["identity"].description, "");
    assert_eq!(p.areas["payments"].paths, vec!["wallets", "payments/**"]);
    assert_eq!(p.areas_for_path("wallets"), vec!["payments"]);
    assert_eq!(p.areas_for_path("wallets/transactions"), vec!["payments"]);
    assert_eq!(
        p.areas_for_path("payments"),
        vec!["payments"],
        "'payments/**' matches the folder itself"
    );
    assert_eq!(p.areas_for_path("identity/users"), vec!["identity"]);
    assert_eq!(
        p.areas_for_path("identity"),
        Vec::<&str>::new(),
        "'identity/*' needs a child"
    );
    assert_eq!(
        p.areas_for_path("identity/users/glossary"),
        vec!["glossary", "identity"],
        "ascending by name"
    );
    assert_eq!(p.areas_for_path("other"), Vec::<&str>::new());
    assert_eq!(p.area_globs("payments").len(), 2);
    assert!(p.area_globs("nope").is_empty());
}

#[test]
fn area1_overlapping_areas_both_cover_a_path() {
    let p = team("areas:\n  a: { paths: [docs] }\n  b: { paths: ['docs/*'] }\n");
    assert_eq!(p.areas_for_path("docs/x"), vec!["a", "b"]);
    assert_eq!(p.areas_for_path("docs"), vec!["a"]);
}

#[test]
fn area1_an_area_with_no_paths_covers_nothing() {
    let p = team("areas:\n  later: { description: tbd }\n  none: {}\n");
    assert_eq!(p.areas.len(), 2);
    assert!(p.areas["none"].paths.is_empty());
    assert!(p.areas_for_path("anything").is_empty());
}

#[test]
fn area1_a_child_pack_replaces_an_area_by_name_and_keeps_the_others() {
    let mut set = PackSet::with_builtin();
    set.add_yaml("name: base\nversion: \"1\"\nextends: rdy-standard\nareas:\n  payments: { description: old, paths: [wallets] }\n  identity: { paths: [identity] }\n").unwrap();
    set.add_yaml("name: child\nversion: \"1\"\nextends: base\nareas:\n  payments: { paths: [payments] }\n  risk: { paths: [risk] }\n").unwrap();
    let p = set.resolve("child").unwrap();
    assert_eq!(
        p.areas.keys().cloned().collect::<Vec<_>>(),
        vec!["identity", "payments", "risk"]
    );
    assert_eq!(
        p.areas["payments"].paths,
        vec!["payments"],
        "replaced wholesale"
    );
    assert_eq!(
        p.areas["payments"].description, "",
        "not merged field by field"
    );
    assert!(p.areas_for_path("wallets").is_empty());
    assert_eq!(p.areas_for_path("identity/x"), vec!["identity"]);
}

#[test]
fn area1_invalid_names_and_globs_are_refused_with_the_reason() {
    let mut set = PackSet::with_builtin();
    let cases: Vec<(String, &str)> = vec![
        (
            "areas:\n  Payments: { paths: [a] }\n".into(),
            "area \"Payments\" has an invalid name",
        ),
        (
            "areas:\n  _x: { paths: [a] }\n".into(),
            "area \"_x\" has an invalid name",
        ),
        (
            "areas:\n  a b: { paths: [a] }\n".into(),
            "area \"a b\" has an invalid name",
        ),
        (
            format!("areas:\n  {}: {{ paths: [a] }}\n", "a".repeat(65)),
            "has an invalid name",
        ),
        (
            "areas:\n  ok: { paths: ['/a'] }\n".into(),
            "area \"ok\" path \"/a\": a path glob has no leading",
        ),
        (
            "areas:\n  ok: { paths: [a, 'b**'] }\n".into(),
            "area \"ok\" path \"b**\": '**' must be a whole segment",
        ),
        (
            "areas:\n  ok: { paths: [''] }\n".into(),
            "area \"ok\" path \"\": a path glob cannot be empty",
        ),
    ];
    for (i, (yaml, needle)) in cases.iter().enumerate() {
        let name = format!("p{i}");
        set.add_yaml(&format!(
            "name: {name}\nversion: \"1\"\nextends: rdy-standard\n{yaml}"
        ))
        .unwrap();
        let e = set.resolve(&name).unwrap_err();
        assert!(matches!(e, SchemaError::Invalid(_)), "{yaml}: {e}");
        assert!(e.to_string().contains(needle), "{yaml}: {e}");
    }
    let e = set
        .add_yaml("name: q\nversion: \"1\"\nareas:\n  a: { paths: [x], owner: me }\n")
        .unwrap_err();
    assert!(matches!(e, SchemaError::Yaml(_)), "unknown area field: {e}");
    assert!(e.to_string().contains("owner"), "{e}");
}

#[test]
fn area1_a_replacement_pack_without_extends_may_declare_areas() {
    let mut set = PackSet::with_builtin();
    set.add_yaml("name: solo\nversion: \"1\"\nareas:\n  all: { paths: ['**'] }\n")
        .unwrap();
    let p = set.resolve("solo").unwrap();
    assert_eq!(p.areas_for_path("x/y"), vec!["all"]);
}

#[test]
fn arn1_the_compiled_matcher_answers_as_the_pack_does() {
    let mut set = rdy_schema::PackSet::with_builtin();
    set.add_yaml("name: a\nversion: \"1\"\nextends: rdy-standard\nareas:\n  pay:\n    paths: [wallets, 'payments/**']\n  id:\n    paths: ['identity/*']\n").unwrap();
    let p = set.resolve("a").unwrap();
    let m = p.area_matcher();
    for path in [
        "wallets",
        "payments/x/y",
        "identity/login",
        "identity",
        "other",
    ] {
        assert_eq!(m.areas_for_path(path), p.areas_for_path(path), "{path}");
    }
    assert!(
        rdy_schema::PackSet::with_builtin()
            .resolve("rdy-standard")
            .unwrap()
            .area_matcher()
            .is_empty()
    );
}

#[test]
fn pkdup1_a_repeated_key_anywhere_in_a_pack_is_refused_with_its_line() {
    let mut set = rdy_schema::PackSet::with_builtin();
    let e = set
        .add_yaml(
            "name: a\nversion: \"1\"\nareas:\n  pay:\n    paths: [x]\n  pay:\n    paths: [y]\n",
        )
        .unwrap_err();
    assert!(matches!(e, rdy_schema::SchemaError::Yaml(_)), "{e}");
    let t = e.to_string();
    assert!(
        t.contains("duplicate entry with key \"pay\"") && t.contains("line 6"),
        "{t}"
    );
    let e = set
        .add_yaml("name: b\nversion: \"1\"\nversion: \"2\"\n")
        .unwrap_err();
    assert!(
        e.to_string()
            .contains("duplicate entry with key \"version\""),
        "{e}"
    );
    set.add_yaml("name: c\nversion: \"1\"\nareas:\n  pay:\n    paths: [x]\n")
        .unwrap();
}
