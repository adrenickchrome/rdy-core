//! rf1: the engine injects two supporting types into every pack (D-Z8P09, D-4WC6R, D-2WR0G):
//! a finding (`F-`) holds a statement and its evidence, a source (`S-`) names a release, commit
//! or spec. Packs may only add optional fields to them; `reflects` is an engine link kind.
use rdy_model::{Block, BlockId, FieldValue};
use rdy_schema::supporting::{
    EVIDENCE_FIX, EVIDENCE_MESSAGE, first_unknown_evidence_key, validate_evidence,
};
use rdy_schema::*;
use std::collections::{BTreeMap, BTreeSet};

fn standard() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

/// A minimal custom pack: no parent, no types of its own.
fn bare() -> Pack {
    let mut set = PackSet::with_builtin();
    let name = set.add_yaml("name: bare\nversion: \"1\"\n").unwrap();
    set.resolve(&name).unwrap()
}

/// A custom pack with its own doc children: one list holds decision, one does not.
fn custom() -> Pack {
    let mut set = PackSet::with_builtin();
    let name = set
        .add_yaml(
            "name: team\nversion: \"1\"\nblock_types:\n  doc:\n    prefix: d\n    children: [chapter, memo]\n  chapter:\n    prefix: ch\n    traits: [structural]\n    children: [memo, decision, finding]\n  memo:\n    prefix: mm\n    body: required\n",
        )
        .unwrap();
    set.resolve(&name).unwrap()
}

fn resolve(yaml: &str) -> Result<Pack, SchemaError> {
    let mut set = PackSet::with_builtin();
    let name = set.add_yaml(yaml).unwrap();
    set.resolve(&name)
}

/// A pack `t` extending the standard pack that declares `ty` as `def` (a YAML flow mapping).
fn declaring(ty: &str, def: &str) -> Result<Pack, SchemaError> {
    resolve(&format!(
        "name: t\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  {ty}: {def}\n"
    ))
}

fn redefine(ty: &str, what: &str) -> SchemaError {
    SchemaError::Invalid(format!(
        "pack t cannot redefine {ty} ({what}); it may only add optional fields"
    ))
}

fn fd(kind: FieldKind, required: bool, values: &[&str]) -> FieldDef {
    FieldDef {
        kind,
        required,
        values: values.iter().map(|s| s.to_string()).collect(),
    }
}

fn block(id: &str, ty: &str, parent: Option<&str>) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: ty.into(),
        parent: parent.map(|p| BlockId::parse(p).unwrap()),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn section() -> Block {
    let mut s = block("s-SECT1", "section", Some("d-D0C11"));
    s.fields
        .insert("title".into(), FieldValue::Str("Findings".into()));
    s
}

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
}

fn map(entries: &[(&str, FieldValue)]) -> FieldValue {
    FieldValue::Map(
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    )
}

fn quote(q: &str) -> FieldValue {
    map(&[("quote", s(q))])
}

fn finding(evidence: FieldValue) -> Block {
    let mut b = block("F-AAAAA", "finding", Some("s-SECT1"));
    b.fields.insert("statement".into(), s("Tasks pass."));
    b.fields.insert("evidence".into(), evidence);
    b
}

fn issues(b: &Block) -> Vec<(&'static str, String, String)> {
    validate_block(&standard(), b, Some(&section()))
        .into_iter()
        .map(|i| (i.code, i.message, i.fix))
        .collect()
}

fn bad_evidence() -> Vec<(&'static str, String, String)> {
    vec![(
        "E_FIELD_KIND",
        EVIDENCE_MESSAGE.to_string(),
        EVIDENCE_FIX.to_string(),
    )]
}

#[test]
fn rf1_types_are_injected_into_every_pack() {
    assert_eq!(
        EVIDENCE_MESSAGE,
        "field \"evidence\" must be a list of 1 to 20 evidence entries"
    );
    assert_eq!(
        EVIDENCE_FIX,
        "write evidence as a list of {quote, session_ref?, question?} or {source: \"{{S-…}}\"} entries"
    );
    let finding_fields: BTreeMap<String, FieldDef> = [
        ("evidence", fd(FieldKind::List, true, &[])),
        ("statement", fd(FieldKind::Str, true, &[])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let source_fields: BTreeMap<String, FieldDef> = [
        (
            "kind",
            fd(FieldKind::Enum, true, &["release", "commit", "spec"]),
        ),
        ("locator", fd(FieldKind::Str, true, &[])),
        ("title", fd(FieldKind::Str, true, &[])),
        ("version", fd(FieldKind::Str, false, &[])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for p in [standard(), bare(), custom()] {
        for (ty, prefix, fields) in [
            ("finding", "F", &finding_fields),
            ("source", "S", &source_fields),
        ] {
            let def = &p.block_types[ty];
            assert_eq!(def.prefix, prefix, "{} {ty}", p.name);
            assert_eq!(def.class, Some(TypeClass::Supporting), "{} {ty}", p.name);
            assert_eq!(p.class_of(ty), TypeClass::Supporting, "{} {ty}", p.name);
            assert_eq!(
                def.traits,
                BTreeSet::from([Trait::Informative]),
                "{} {ty}",
                p.name
            );
            assert_eq!(def.body, BodyRule::Optional, "{} {ty}", p.name);
            assert_eq!(&def.fields, fields, "{} {ty}", p.name);
            assert!(def.children.is_empty(), "{} {ty}", p.name);
            assert_eq!(p.type_for_prefix(prefix), Some(ty), "{}", p.name);
        }
        assert!(p.link_types.contains("reflects"), "{}", p.name);
    }
}

#[test]
fn rf1_children_and_optional_extensions_are_closed() {
    // every children list that holds decision gains finding then source, once
    let std = standard();
    for parent in ["doc", "section"] {
        let children = &std.block_types[parent].children;
        for ty in ["finding", "source"] {
            assert_eq!(
                children.iter().filter(|c| *c == ty).count(),
                1,
                "{parent}: {children:?}"
            );
        }
        let at = |t: &str| children.iter().position(|c| c == t).unwrap();
        assert!(at("decision") < at("finding") && at("finding") < at("source"));
        // the publication profile still closes a doc's list
        if parent == "doc" {
            assert_eq!(
                children.last().map(String::as_str),
                Some("publication_profile")
            );
        }
    }
    let p = custom();
    assert_eq!(
        p.block_types["chapter"].children,
        vec!["memo", "decision", "finding", "source"],
        "a declared finding is not repeated"
    );
    assert_eq!(
        p.block_types["doc"].children,
        vec!["chapter", "memo", "publication_profile"],
        "a parent without decision gains neither"
    );
    assert_eq!(
        bare().block_types["doc"].children,
        vec!["publication_profile"]
    );
    for pack in [bare(), custom()] {
        for (name, t) in &pack.block_types {
            let has = |c: &str| t.children.iter().any(|x| x == c);
            assert_eq!(has("finding"), has("decision"), "{} {name}", pack.name);
            assert_eq!(has("source"), has("decision"), "{} {name}", pack.name);
        }
    }

    // a pack may omit the core fields, repeat them exactly or add optional fields
    for (ty, def) in [
        ("finding", "{ prefix: F }"),
        ("finding", "{ prefix: F, traits: [informative] }"),
        (
            "finding",
            "{ prefix: F, fields: { statement: { kind: str, required: true }, confidence: { kind: int } } }",
        ),
        ("source", "{ prefix: S, body: optional }"),
        (
            "source",
            "{ prefix: S, fields: { kind: { kind: enum, required: true, values: [release, commit, spec] }, publisher: { kind: str } } }",
        ),
    ] {
        let p = declaring(ty, def).unwrap_or_else(|e| panic!("{ty} {def}: {e}"));
        let t = &p.block_types[ty];
        assert_eq!(t.class, Some(TypeClass::Supporting));
        assert_eq!(t.body, BodyRule::Optional);
        assert!(t.traits.contains(&Trait::Informative));
    }
    let p = declaring(
        "finding",
        "{ prefix: F, fields: { confidence: { kind: int } } }",
    )
    .unwrap();
    let f = &p.block_types["finding"];
    assert_eq!(
        f.fields.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["confidence", "evidence", "statement"]
    );
    assert!(!f.fields["confidence"].required);
    let p = declaring(
        "source",
        "{ prefix: S, fields: { publisher: { kind: str } } }",
    )
    .unwrap();
    assert!(p.block_types["source"].fields.contains_key("publisher"));

    // anything else is refused, checked in the order prefix, children, body, traits, fields
    for (ty, def, what) in [
        ("finding", "{ prefix: X }", "prefix"),
        ("finding", "{ prefix: X, children: [prose] }", "prefix"),
        ("finding", "{ prefix: F, children: [prose] }", "children"),
        (
            "finding",
            "{ prefix: F, children: [prose], body: required }",
            "children",
        ),
        ("finding", "{ prefix: F, body: required }", "body"),
        (
            "finding",
            "{ prefix: F, body: required, traits: [normative] }",
            "body",
        ),
        ("finding", "{ prefix: F, traits: [normative] }", "traits"),
        ("finding", "{ prefix: F, traits: [defines] }", "traits"),
        (
            "finding",
            "{ prefix: F, traits: [informative, presentational] }",
            "traits",
        ),
        (
            "finding",
            "{ prefix: F, traits: [normative], fields: { owner: { kind: str, required: true } } }",
            "traits",
        ),
        (
            "finding",
            "{ prefix: F, fields: { statement: { kind: list } } }",
            "field statement",
        ),
        (
            "finding",
            "{ prefix: F, fields: { evidence: { kind: list, required: false } } }",
            "field evidence",
        ),
        (
            "finding",
            "{ prefix: F, fields: { owner: { kind: str, required: true } } }",
            "field owner",
        ),
        (
            "finding",
            "{ prefix: F, fields: { statement: { kind: int }, aaa: { kind: str, required: true } } }",
            "field aaa",
        ),
        ("source", "{ prefix: s }", "prefix"),
        ("source", "{ prefix: S, children: [prose] }", "children"),
        ("source", "{ prefix: S, body: required }", "body"),
        (
            "source",
            "{ prefix: S, traits: [presentational] }",
            "traits",
        ),
        (
            "source",
            "{ prefix: S, fields: { kind: { kind: enum, required: true, values: [release] } } }",
            "field kind",
        ),
        (
            "source",
            "{ prefix: S, fields: { version: { kind: str, required: true } } }",
            "field version",
        ),
        (
            "source",
            "{ prefix: S, fields: { reviewer: { kind: str, required: true } } }",
            "field reviewer",
        ),
    ] {
        assert_eq!(
            declaring(ty, def).unwrap_err(),
            redefine(ty, what),
            "{ty} {def}"
        );
    }
}

#[test]
fn rf1_evidence_has_two_shapes_and_numeric_bounds() {
    let list = |entries: Vec<FieldValue>| FieldValue::List(entries);
    let ok = |b: &Block| {
        assert_eq!(issues(b), vec![], "{:?}", b.fields["evidence"]);
        assert_eq!(validate_evidence(b), vec![]);
    };
    let bad = |b: &Block| {
        assert_eq!(issues(b), bad_evidence(), "{:?}", b.fields["evidence"]);
        let direct = validate_evidence(b);
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].severity, Severity::Error);
        assert_eq!(direct[0].block, Some(b.id.clone()));
    };

    // entry counts: 1 and 20 pass, 0 and 21 fail
    ok(&finding(list(vec![quote("a")])));
    ok(&finding(list(
        (0..20).map(|i| quote(&format!("q{i}"))).collect(),
    )));
    bad(&finding(list(vec![])));
    bad(&finding(list(
        (0..21).map(|i| quote(&format!("q{i}"))).collect(),
    )));

    // value lengths in Unicode scalar values: 1 and 2000 pass, 0 and 2001 fail
    let wide = |n: usize| "\u{1F600}".repeat(n);
    for key in ["quote", "session_ref", "question"] {
        for (n, pass) in [(1, true), (2000, true), (0, false), (2001, false)] {
            let mut entry = vec![("quote", s("q")), (key, s(&wide(n)))];
            if key == "quote" {
                entry.remove(0);
            }
            let b = finding(list(vec![map(&entry)]));
            if pass { ok(&b) } else { bad(&b) }
        }
    }

    // the two shapes
    ok(&finding(list(vec![
        map(&[
            ("quote", s("x")),
            ("session_ref", s("run 4")),
            ("question", s("Q-AAAAA")),
        ]),
        map(&[("quote", s("y")), ("question", s("why?"))]),
        map(&[("source", s("{{S-ABCDE}}"))]),
    ])));
    for entry in [
        map(&[]),
        map(&[("quote", s("x")), ("source", s("{{S-ABCDE}}"))]),
        map(&[("source", s("{{S-ABCDE}}")), ("question", s("why?"))]),
        map(&[("source", s("{{S-ABCDE}}")), ("session_ref", s("run 4"))]),
        map(&[("session_ref", s("run 4"))]),
        map(&[("question", s("why?"))]),
        map(&[("quote", s("x")), ("note", s("extra"))]),
        map(&[("source", s("{{S-ABCDE}}")), ("note", s("extra"))]),
        // nonstrings and nested values
        map(&[("quote", FieldValue::Int(1))]),
        map(&[("quote", FieldValue::Bool(true))]),
        map(&[("quote", FieldValue::Null)]),
        map(&[("quote", FieldValue::List(vec![s("x")]))]),
        map(&[("quote", map(&[("text", s("x"))]))]),
        map(&[("quote", s("x")), ("question", FieldValue::Int(2))]),
        map(&[("source", FieldValue::List(vec![s("{{S-ABCDE}}")]))]),
        s("a bare quote"),
        FieldValue::List(vec![quote("x")]),
        FieldValue::Int(3),
    ] {
        bad(&finding(list(vec![quote("fine"), entry])));
    }
    // evidence that is not a list at all
    bad(&finding(s("a quote")));
    bad(&finding(quote("a quote")));

    // a source entry is exactly one full source reference
    for good in ["{{S-ABCDE}}", "{{S-00000}}", "{{S-ZZZZZ}}"] {
        ok(&finding(list(vec![map(&[("source", s(good))])])));
    }
    for wrong in [
        "",
        "S-ABCDE",
        "{{S-ABCDE|the release}}",
        "{{S-ABCDE|@key}}",
        "{{S-ABCDE|}}",
        "{{S-ABCDE}}{{S-BCDEF}}",
        "{{S-ABCDE}} and {{S-BCDEF}}",
        "see {{S-ABCDE}}",
        "{{S-ABCDE}} ",
        " {{S-ABCDE}}",
        "{{ S-ABCDE }}",
        "{{S-ABCDE}",
        "{S-ABCDE}",
        "{{S-ABCD}}",
        "{{S-ABCDEF}}",
        "{{S-abcde}}",
        "{{S-ABCDU}}",
        "{{s-ABCDE}}",
        "{{F-ABCDE}}",
        "{{SR-ABCDE}}",
        "{{release-1.2}}",
        "{{$placeholder}}",
        "\\{{S-ABCDE}}",
    ] {
        bad(&finding(list(vec![map(&[("source", s(wrong))])])));
    }

    // a missing field takes the existing required-field path
    let mut b = block("F-AAAAA", "finding", Some("s-SECT1"));
    let codes = |b: &Block| {
        issues(b)
            .into_iter()
            .map(|i| (i.0, i.1))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        codes(&b),
        vec![
            (
                "E_FIELD_REQUIRED",
                "type finding requires field \"evidence\"".to_string()
            ),
            (
                "E_FIELD_REQUIRED",
                "type finding requires field \"statement\"".to_string()
            ),
        ]
    );
    assert_eq!(validate_evidence(&b), vec![]);
    b.fields.insert("statement".into(), s("x"));
    assert_eq!(validate_evidence(&b), vec![]);

    // the evidence rule is the finding's alone
    let mut other = block("c-AAAAA", "claim", Some("s-SECT1"));
    other.body = "A claim.".into();
    other.fields.insert("evidence".into(), list(vec![]));
    assert_eq!(validate_evidence(&other), vec![]);

    // the generic nesting check comes first and stands alone
    let mut deep = s("x");
    for _ in 0..rdy_model::MAX_FIELD_NESTING {
        deep = FieldValue::List(vec![deep]);
    }
    assert_eq!(
        issues(&finding(deep)),
        vec![(
            "E_LIMIT",
            rdy_model::field_nesting_message(),
            rdy_model::field_nesting_fix()
        )]
    );

    // the first unknown key, by entry order then key order
    assert_eq!(
        first_unknown_evidence_key(&list(vec![
            quote("a"),
            map(&[("source", s("{{S-ABCDE}}"))])
        ])),
        None
    );
    assert_eq!(
        first_unknown_evidence_key(&list(vec![map(&[("quote", s("a")), ("source", s("x"))])])),
        None,
        "a mixed entry names no unknown key"
    );
    assert_eq!(
        first_unknown_evidence_key(&list(vec![
            quote("a"),
            map(&[("quote", s("b")), ("zeta", s("")), ("qoute", s(""))]),
            map(&[("alpha", s(""))]),
        ])),
        Some((1, "qoute"))
    );
    assert_eq!(
        first_unknown_evidence_key(&list(vec![s("x"), map(&[("sorce", s("{{S-ABCDE}}"))])])),
        Some((1, "sorce"))
    );
    assert_eq!(first_unknown_evidence_key(&s("x")), None);
}

#[test]
fn rf1_reflects_and_supporting_links_are_store_only() {
    assert_eq!(
        STORE_ONLY_LINK_KINDS,
        &["decided_by", "implements", "reflects"]
    );
    assert_eq!(
        STORE_ONLY_TARGET_TYPES,
        &["decision", "question", "finding", "source"]
    );
    assert_eq!(
        STORE_ONLY_FIELDS,
        &["key_auto", "reaffirmed", "reflects_at"]
    );

    // a pack cannot declare the reflects link kind, alone or beside its own
    let reserved = SchemaError::Invalid(
        "reflects is an engine link kind and cannot be declared by a pack".to_string(),
    );
    for yaml in [
        "name: t\nversion: \"1\"\nlink_types: [reflects]\n",
        "name: t\nversion: \"1\"\nextends: rdy-standard\nlink_types: [cites, reflects]\n",
    ] {
        assert_eq!(resolve(yaml).unwrap_err(), reserved, "{yaml}");
    }
    let p =
        resolve("name: t\nversion: \"1\"\nextends: rdy-standard\nlink_types: [cites]\n").unwrap();
    assert!(p.link_types.contains("cites") && p.link_types.contains("reflects"));

    // a finding or source cannot carry the reflects field
    let mut f = finding(FieldValue::List(vec![quote("x")]));
    let mut src = block("S-AAAAA", "source", Some("s-SECT1"));
    for (k, v) in [
        ("title", "rdy 0.18.0"),
        ("kind", "release"),
        ("locator", "v0.18.0"),
    ] {
        src.fields.insert(k.into(), s(v));
    }
    assert_eq!(issues(&src), vec![]);
    for b in [&mut f, &mut src] {
        b.fields.insert(
            REFLECTS_FIELD.into(),
            FieldValue::List(vec![s("crates/*/src/error.rs")]),
        );
        let found = issues(b);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, "E_FIELD_UNKNOWN");
        assert!(found[0].2.contains("Implements"), "{found:?}");
    }

    // a pack type that takes F or S meets the existing prefix check
    assert_eq!(
        declaring("memo", "{ prefix: F }").unwrap_err(),
        SchemaError::Invalid("prefix \"F\" used by both \"finding\" and \"memo\"".to_string())
    );
    assert_eq!(
        declaring("memo", "{ prefix: S }").unwrap_err(),
        SchemaError::Invalid("prefix \"S\" used by both \"memo\" and \"source\"".to_string())
    );
}

#[test]
fn rf1_delegated_is_an_enum_value() {
    let basis = &standard().block_types["decision"].fields["authority_basis"];
    assert_eq!(basis.kind, FieldKind::Enum);
    assert!(!basis.required);
    assert_eq!(
        basis.values,
        vec![
            "direct_ruling",
            "recorded_ruling",
            "delegated",
            "precedent",
            "elimination",
            "none"
        ]
    );
    let decision = |b: &str| {
        let mut d = block("D-AAAAA", "decision", Some("s-SECT1"));
        d.fields.insert("statement".into(), s("Ship it."));
        d.fields.insert("status".into(), s("proposed"));
        d.fields.insert("authority_basis".into(), s(b));
        d
    };
    for p in [standard(), bare()] {
        for b in [
            "direct_ruling",
            "recorded_ruling",
            "delegated",
            "precedent",
            "elimination",
            "none",
        ] {
            let d = decision(b);
            let found: Vec<_> = validate_block(&p, &d, Some(&section()))
                .into_iter()
                .filter(|i| i.code.starts_with("E_FIELD"))
                .collect();
            assert_eq!(found, vec![], "{} {b}", p.name);
        }
    }
    for b in ["external", "reconstructed", "unknown", "Delegated"] {
        assert_eq!(
            issues(&decision(b)),
            vec![(
                "E_FIELD_ENUM",
                format!("field \"authority_basis\" has value {b:?}"),
                "use one of: direct_ruling, recorded_ruling, delegated, precedent, elimination, none"
                    .to_string()
            )],
            "{b}"
        );
    }
}
