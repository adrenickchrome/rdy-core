//! rpl4: the pack section `tensions_across` raises exactly five cross-project kinds; the local
//! `tensions` map stays strict.
use rdy_schema::*;

const FIVE: [TensionKind; 5] = [
    TensionKind::DuplicateDefinition,
    TensionKind::ValueDisagreement,
    TensionKind::RestatementDrift,
    TensionKind::DeprecatedReference,
    TensionKind::PremiseShift,
];

fn load(yaml: &str) -> (Pack, Vec<PackWarning>) {
    let mut set = PackSet::with_builtin();
    let name = set.add_yaml(yaml).unwrap();
    let pack = set.resolve(&name).unwrap();
    let warnings = set.warnings(&name).unwrap();
    (pack, warnings)
}

#[test]
fn rpl4_across_unknown_warns_local_unknown_refuses() {
    // the five kinds are accepted, default advisory, and inherited
    let mut set = PackSet::with_builtin();
    set.add_yaml("name: base\nversion: \"1\"\nextends: rdy-standard\ntensions_across: { duplicate_definition: blocking, premise_shift: advisory }\n").unwrap();
    set.add_yaml("name: child\nversion: \"1\"\nextends: base\ntensions_across: { value_disagreement: blocking, premise_shift: blocking }\n").unwrap();
    let p = set.resolve("child").unwrap();
    assert_eq!(p.tensions_across.len(), 3);
    assert_eq!(
        p.across_severity(TensionKind::DuplicateDefinition),
        TensionSeverity::Blocking
    );
    assert_eq!(
        p.across_severity(TensionKind::ValueDisagreement),
        TensionSeverity::Blocking
    );
    assert_eq!(
        p.across_severity(TensionKind::PremiseShift),
        TensionSeverity::Blocking
    );
    for k in [
        TensionKind::RestatementDrift,
        TensionKind::DeprecatedReference,
    ] {
        assert_eq!(p.across_severity(k), TensionSeverity::Advisory);
    }
    assert!(set.warnings("child").unwrap().is_empty());
    assert_eq!(FIVE.len(), CROSS_PROJECT_KINDS.len());
    for k in FIVE {
        assert!(CROSS_PROJECT_KINDS.contains(&k));
    }
    // a pack with none has an empty map and stays advisory
    let (plain, w) = load("name: plain\nversion: \"1\"\n");
    assert!(plain.tensions_across.is_empty() && w.is_empty());
    assert_eq!(
        plain.across_severity(TensionKind::Fork),
        TensionSeverity::Advisory
    );

    // unknown keys warn, in key order, and stay advisory: semantic, a local kind, a typo
    let (p, w) = load(
        "name: warny\nversion: \"1\"\ntensions_across: { semantic: blocking, fork: blocking, bogus: 3, premise_shift: blocking }\n",
    );
    assert_eq!(p.tensions_across.len(), 1);
    assert_eq!(
        p.across_severity(TensionKind::PremiseShift),
        TensionSeverity::Blocking
    );
    for k in [TensionKind::Semantic, TensionKind::Fork] {
        assert_eq!(p.across_severity(k), TensionSeverity::Advisory);
    }
    let got: Vec<(&str, &str, &str)> = w
        .iter()
        .map(|x| (x.code, x.message.as_str(), x.fix.as_str()))
        .collect();
    let fix = "use one of: duplicate_definition, value_disagreement, restatement_drift, deprecated_reference, premise_shift";
    assert_eq!(
        got,
        [
            (
                "W_CONFIG_UNKNOWN",
                "pack warny: tensions_across has no kind \"bogus\"; rdy ignores it",
                fix
            ),
            (
                "W_CONFIG_UNKNOWN",
                "pack warny: tensions_across has no kind \"fork\"; rdy ignores it",
                fix
            ),
            (
                "W_CONFIG_UNKNOWN",
                "pack warny: tensions_across has no kind \"semantic\"; rdy ignores it",
                fix
            ),
        ]
    );

    // a bad severity for a known kind is an error, like the local map
    let mut set = PackSet::with_builtin();
    let e = set
        .add_yaml("name: b\nversion: \"1\"\ntensions_across: { premise_shift: fatal }\n")
        .unwrap_err();
    assert!(matches!(e, SchemaError::Yaml(_)), "{e}");

    // the local map stays strict: an unknown kind, including a cross-project-only name, refuses
    let mut set = PackSet::with_builtin();
    assert!(matches!(
        set.add_yaml("name: c\nversion: \"1\"\ntensions: { collision: blocking }\n"),
        Err(SchemaError::Yaml(_))
    ));
    assert!(matches!(
        set.add_yaml("name: d\nversion: \"1\"\ntensions: { bogus: blocking }\n"),
        Err(SchemaError::Yaml(_))
    ));
    // semantic stays refused in the local map at resolve
    let mut set = PackSet::with_builtin();
    set.add_yaml("name: e\nversion: \"1\"\ntensions: { semantic: blocking }\n")
        .unwrap();
    assert!(set.resolve("e").is_err());
}

#[test]
fn rpl4_engine_names_are_reserved_against_pack_declarations() {
    let resolve = |yaml: &str| {
        let mut set = PackSet::with_builtin();
        let n = set.add_yaml(yaml).unwrap();
        set.resolve(&n)
    };
    let e = resolve("name: t\nversion: \"1\"\nlink_types: [deviates_from]\n").unwrap_err();
    assert_eq!(
        e.to_string(),
        "invalid pack: deviates_from is an engine link kind and cannot be declared by a pack"
    );
    for f in ["adopted_as", "deviates_from"] {
        let e = resolve(&format!(
            "name: t\nversion: \"1\"\nblock_types:\n  memo:\n    prefix: mm\n    body: required\n    fields:\n      {f}: {{ kind: str }}\n"
        ))
        .unwrap_err();
        assert_eq!(
            e.to_string(),
            format!("invalid pack: type memo: field {f} is reserved for the engine")
        );
    }
}
