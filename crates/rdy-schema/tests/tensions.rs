//! The pack tensions section (tens1).
use TensionKind::*;
use TensionSeverity::*;
use rdy_schema::*;

#[test]
fn tens1_the_seven_kinds_have_the_spec_defaults_and_wire_names() {
    assert_eq!(
        TensionKind::ALL,
        [
            EditCollision,
            Fork,
            PremiseShift,
            AreaOverlap,
            DuplicateDefinition,
            UnjustifiedChange,
            DeprecatedReference
        ]
    );
    let expect = [
        (EditCollision, "edit_collision", Blocking),
        (Fork, "fork", Blocking),
        (PremiseShift, "premise_shift", Blocking),
        (AreaOverlap, "area_overlap", Advisory),
        (DuplicateDefinition, "duplicate_definition", Blocking),
        (UnjustifiedChange, "unjustified_change", Advisory),
        (DeprecatedReference, "deprecated_reference", Advisory),
    ];
    for (k, name, sev) in expect {
        assert_eq!(k.name(), name);
        assert_eq!(k.to_string(), name);
        assert_eq!(k.default_severity(), sev, "{name}");
        assert_eq!(name.parse::<TensionKind>().unwrap(), k);
        assert_eq!(serde_json::to_value(k).unwrap(), serde_json::json!(name));
    }
    let e = "collision".parse::<TensionKind>().unwrap_err();
    assert!(
        e.to_string().contains("unknown tension kind \"collision\"")
            && e.to_string().contains("deprecated_reference"),
        "{e}"
    );
    assert_eq!(Blocking.to_string(), "blocking");
    let mut sevs = vec![Advisory, Blocking];
    sevs.sort();
    assert_eq!(
        sevs,
        vec![Blocking, Advisory],
        "declaration order: blocking sorts first"
    );
}

#[test]
fn tens1_a_pack_without_the_section_has_only_defaults() {
    let p = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    assert!(p.tensions.is_empty());
    for k in TensionKind::ALL {
        assert_eq!(p.tension_severity(k), (k.default_severity(), false));
    }
    let t = p.tension_table();
    assert_eq!(t.len(), 7);
    assert_eq!(t[0], (EditCollision, Blocking, false));
    assert_eq!(t[5], (UnjustifiedChange, Advisory, false));
}

#[test]
fn tens1_a_pack_tunes_severities_per_kind() {
    let mut set = PackSet::with_builtin();
    set.add_yaml("name: team\nversion: \"1\"\nextends: rdy-standard\ntensions:\n  unjustified_change: blocking\n  fork: advisory\n  duplicate_definition: blocking\n").unwrap();
    let p = set.resolve("team").unwrap();
    assert_eq!(p.tensions.len(), 3);
    assert_eq!(p.tension_severity(UnjustifiedChange), (Blocking, true));
    assert_eq!(p.tension_severity(Fork), (Advisory, true));
    assert_eq!(
        p.tension_severity(DuplicateDefinition),
        (Blocking, true),
        "an explicit default is still an override"
    );
    assert_eq!(p.tension_severity(EditCollision), (Blocking, false));
    assert_eq!(p.tension_table()[1], (Fork, Advisory, true));
}

#[test]
fn tens1_a_child_pack_overrides_a_kind_and_keeps_the_rest() {
    let mut set = PackSet::with_builtin();
    set.add_yaml("name: base\nversion: \"1\"\nextends: rdy-standard\ntensions: { fork: advisory, area_overlap: blocking }\n").unwrap();
    set.add_yaml("name: child\nversion: \"1\"\nextends: base\ntensions: { fork: blocking }\n")
        .unwrap();
    let p = set.resolve("child").unwrap();
    assert_eq!(p.tension_severity(Fork), (Blocking, true));
    assert_eq!(
        p.tension_severity(AreaOverlap),
        (Blocking, true),
        "kept from the parent"
    );
    assert_eq!(p.tensions.len(), 2);
}

#[test]
fn tens1_unknown_kinds_and_values_are_refused() {
    let mut set = PackSet::with_builtin();
    let e = set
        .add_yaml("name: a\nversion: \"1\"\ntensions: { collision: blocking }\n")
        .unwrap_err();
    assert!(matches!(e, SchemaError::Yaml(_)), "{e}");
    assert!(e.to_string().contains("collision"), "{e}");
    let e = set
        .add_yaml("name: b\nversion: \"1\"\ntensions: { fork: fatal }\n")
        .unwrap_err();
    assert!(matches!(e, SchemaError::Yaml(_)), "{e}");
    assert!(e.to_string().contains("fatal"), "{e}");
    // D-QNGEJ carried into M2d1 (pkdup1): a repeated map key is refused at load.
    let e = set
        .add_yaml("name: c\nversion: \"1\"\ntensions:\n  fork: blocking\n  fork: advisory\n")
        .unwrap_err();
    assert!(matches!(e, SchemaError::Yaml(_)), "{e}");
    assert!(
        e.to_string().contains("duplicate entry with key \"fork\""),
        "{e}"
    );
    let e = set
        .add_yaml("name: d\nversion: \"1\"\ntensions: [fork]\n")
        .unwrap_err();
    assert!(
        matches!(e, SchemaError::Yaml(_)),
        "a list is not a map: {e}"
    );
}
