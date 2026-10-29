//! pub1: the publication profile is an ordinary supporting block type the engine injects into
//! every pack. Generic validation checks it; a few value checks and a typed parse sit on top.
use rdy_model::redaction::RedactedBlockIdentity;
use rdy_model::{Block, BlockId, CROCKFORD, FieldValue, Hash, Link, StateSnapshot, hash_of};
use rdy_schema::publication::{self, Audience, ProfileFields};
use rdy_schema::*;
use std::collections::{BTreeMap, BTreeSet};

const PROFILE: &str = "publication_profile";

fn standard() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

fn bare() -> Pack {
    let mut set = PackSet::with_builtin();
    let name = set.add_yaml("name: bare\nversion: \"1\"\n").unwrap();
    set.resolve(&name).unwrap()
}

/// A custom pack that also lists the profile under a section and adds its own doc child.
fn custom() -> Pack {
    let mut set = PackSet::with_builtin();
    let name = set
        .add_yaml(
            "name: team\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  memo:\n    prefix: mm\n    body: required\n  section:\n    prefix: s\n    traits: [structural]\n    children: [prose, publication_profile]\n",
        )
        .unwrap();
    set.resolve(&name).unwrap()
}

fn b(id: &str, ty: &str, parent: Option<&str>) -> Block {
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

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
}

fn ids(v: &[&str]) -> FieldValue {
    FieldValue::List(v.iter().map(|x| s(x)).collect())
}

fn doc() -> Block {
    let mut d = b("d-D0C11", "doc", None);
    for (k, v) in [("name", "plan"), ("title", "Plan"), ("doc_type", "page")] {
        d.fields.insert(k.into(), s(v));
    }
    d
}

fn section() -> Block {
    let mut x = b("s-SECT1", "section", Some("d-D0C11"));
    x.fields.insert("title".into(), s("Goals"));
    x
}

fn folder() -> Block {
    let mut f = b("fd-F0DR1", "folder", None);
    f.fields.insert("name".into(), s("notes"));
    f
}

/// A valid profile under `d-D0C11`.
fn profile() -> Block {
    let mut p = b("pf-PR0F1", PROFILE, Some("d-D0C11"));
    p.fields.insert("key".into(), s("public-site"));
    p.fields.insert("title".into(), s("Public site"));
    p.fields.insert("audience".into(), s("public"));
    p.fields
        .insert("select".into(), ids(&["d-D0C22", "fd-F0DR1"]));
    p
}

fn with(mut p: Block, field: &str, v: FieldValue) -> Block {
    p.fields.insert(field.into(), v);
    p
}

fn without(mut p: Block, field: &str) -> Block {
    p.fields.remove(field);
    p
}

fn check(pack: &Pack, block: &Block) -> Vec<Issue> {
    validate_block(pack, block, Some(&doc()))
}

/// (code, message, fix) of each issue, in order.
fn rows(issues: &[Issue]) -> Vec<(&'static str, String, String)> {
    issues
        .iter()
        .map(|i| (i.code, i.message.clone(), i.fix.clone()))
        .collect()
}

fn row(code: &'static str, message: &str, fix: &str) -> (&'static str, String, String) {
    (code, message.to_string(), fix.to_string())
}

/// The `n`-th distinct full block id with prefix `p`.
fn nth_id(n: usize) -> String {
    let mut body = String::new();
    let mut k = n;
    for _ in 0..5 {
        body.insert(0, CROCKFORD[k % 32] as char);
        k /= 32;
    }
    format!("p-{body}")
}

fn limit(field: &str, n: usize, unit: &str) -> (&'static str, String, String) {
    (
        "E_LIMIT",
        format!("Publication field \"{field}\" exceeds its limit."),
        format!("Use at most {n} {unit} in \"{field}\"."),
    )
}

#[test]
fn pub1_profile_schema_is_closed_and_pack_independent() {
    let packs = [bare(), standard(), custom()];
    let def = packs[0].block_types[PROFILE].clone();
    for p in &packs {
        assert_eq!(p.block_types[PROFILE], def, "pack {}", p.name);
    }
    assert_eq!(def.prefix, "pf");
    assert_eq!(def.class, Some(TypeClass::Supporting));
    assert_eq!(def.traits, BTreeSet::from([Trait::Structural]));
    assert_eq!(def.body, BodyRule::Forbidden);
    assert!(def.children.is_empty());
    assert!(def.tracking.is_empty());
    let f = |kind, required, values: &[&str]| FieldDef {
        kind,
        required,
        values: values.iter().map(|v| v.to_string()).collect(),
    };
    let want: BTreeMap<String, FieldDef> = [
        ("audience", f(FieldKind::Enum, true, &["org", "public"])),
        ("description", f(FieldKind::Str, false, &[])),
        ("select", f(FieldKind::List, true, &[])),
        ("title", f(FieldKind::Str, true, &[])),
        ("trail", f(FieldKind::Bool, false, &[])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    assert_eq!(def.fields, want);
    assert_eq!(packs[0].type_for_prefix("pf"), Some(PROFILE));

    // the doc holds the profile once, in every pack
    for p in &packs {
        let children = &p.block_types["doc"].children;
        assert_eq!(children.iter().filter(|c| *c == PROFILE).count(), 1);
    }

    // a pack cannot declare the type, under its own name or by taking its prefix
    let resolve = |yaml: &str| {
        let mut set = PackSet::with_builtin();
        let name = set.add_yaml(yaml).unwrap();
        set.resolve(&name)
    };
    for yaml in [
        "name: t\nversion: \"1\"\nblock_types:\n  publication_profile:\n    prefix: pf\n",
        "name: t\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  publication_profile:\n    prefix: pp\n    fields:\n      owner: { kind: str }\n",
    ] {
        assert_eq!(
            resolve(yaml).unwrap_err(),
            SchemaError::Invalid(
                "publication_profile is an engine type and cannot be declared by a pack"
                    .to_string()
            ),
            "{yaml}"
        );
    }
    let e =
        resolve("name: t\nversion: \"1\"\nblock_types:\n  memo:\n    prefix: pf\n").unwrap_err();
    assert_eq!(
        e,
        SchemaError::Invalid(
            "prefix \"pf\" used by both \"memo\" and \"publication_profile\"".to_string()
        )
    );

    // body, parent and empty-children rules hold in every pack
    for p in &packs {
        assert_eq!(check(p, &profile()), vec![], "pack {}", p.name);
        let mut body = profile();
        body.body = "words".into();
        assert_eq!(
            rows(&check(p, &body)),
            vec![row(
                "E_BODY_FORBIDDEN",
                "type publication_profile must not have a body",
                "move the text into a child prose block"
            )]
        );
        // a section never holds a profile, even where a pack lists it under one
        let under = b("pf-PR0F1", PROFILE, Some("s-SECT1"));
        let under = Block {
            fields: profile().fields,
            ..under
        };
        assert_eq!(
            rows(&validate_block(p, &under, Some(&section()))),
            vec![row(
                "E_CHILD_TYPE",
                "a section cannot contain a publication_profile",
                "set parent to a doc id"
            )],
            "pack {}",
            p.name
        );
        let in_folder = Block {
            parent: Some(BlockId::parse("fd-F0DR1").unwrap()),
            ..profile()
        };
        assert_eq!(
            rows(&validate_block(p, &in_folder, Some(&folder()))),
            vec![row(
                "E_CHILD_TYPE",
                "a folder cannot contain a publication_profile",
                "set parent to a doc id"
            )]
        );
        // nothing sits under a profile
        let mut child = b("p-PR0SE", "prose", Some("pf-PR0F1"));
        child.body = "text".into();
        if p.block_types.contains_key("prose") {
            assert_eq!(
                validate_block(p, &child, Some(&profile()))
                    .iter()
                    .map(|i| (i.code, i.message.as_str()))
                    .collect::<Vec<_>>(),
                vec![(
                    "E_CHILD_TYPE",
                    "a publication_profile cannot contain a prose"
                )]
            );
        }
    }
}

#[test]
fn pub1_profile_fields_have_exact_bounds_and_defaults() {
    let p = standard();
    let d = doc();
    let parse = |b: &Block| publication::parse(&p, b, Some(&d));

    // trail defaults by audience; an explicit value wins
    let public = parse(&profile()).unwrap();
    assert_eq!(
        public,
        ProfileFields {
            key: "public-site".into(),
            title: "Public site".into(),
            description: None,
            audience: Audience::Public,
            select: vec![
                BlockId::parse("d-D0C22").unwrap(),
                BlockId::parse("fd-F0DR1").unwrap()
            ],
            trail: false,
        }
    );
    let org = parse(&with(profile(), "audience", s("org"))).unwrap();
    assert_eq!((org.audience, org.trail), (Audience::Org, true));
    let org_off = parse(&with(
        with(profile(), "audience", s("org")),
        "trail",
        FieldValue::Bool(false),
    ))
    .unwrap();
    assert!(!org_off.trail);
    let public_on = parse(&with(profile(), "trail", FieldValue::Bool(true))).unwrap();
    assert!(public_on.trail);
    assert_eq!(serde_json::to_string(&Audience::Org).unwrap(), "\"org\"");
    assert_eq!(
        serde_json::to_string(&Audience::Public).unwrap(),
        "\"public\""
    );
    // author order is kept
    let order = parse(&with(profile(), "select", ids(&["fd-F0DR1", "d-D0C22"]))).unwrap();
    assert_eq!(
        order.select,
        vec![
            BlockId::parse("fd-F0DR1").unwrap(),
            BlockId::parse("d-D0C22").unwrap()
        ]
    );

    // title: 256 bytes pass, 257 fail; blank fails; Unicode counts in bytes
    let e2 = "é".repeat(128); // 256 bytes
    assert_eq!(check(&p, &with(profile(), "title", s(&e2))), vec![]);
    assert_eq!(
        rows(&check(&p, &with(profile(), "title", s(&format!("{e2}a"))))),
        vec![limit("title", 256, "bytes")]
    );
    assert_eq!(
        rows(&check(&p, &with(profile(), "title", s(&" ".repeat(257))))),
        vec![limit("title", 256, "bytes")],
        "length precedes the nonblank check"
    );
    for blank in ["", "   ", "\u{3000}\t\n"] {
        assert_eq!(
            rows(&check(&p, &with(profile(), "title", s(blank)))),
            vec![row(
                "E_BAD_INPUT",
                "Publication title must contain text.",
                "Set title to a nonblank string."
            )],
            "{blank:?}"
        );
    }
    assert_eq!(
        parse(&with(profile(), "title", s("Révision ✓ 公開")))
            .unwrap()
            .title,
        "Révision ✓ 公開"
    );

    // description: empty and 4096 bytes pass, 4097 fail
    assert_eq!(
        parse(&with(profile(), "description", s("")))
            .unwrap()
            .description,
        Some(String::new())
    );
    assert_eq!(
        check(&p, &with(profile(), "description", s(&"x".repeat(4096)))),
        vec![]
    );
    assert_eq!(
        rows(&check(
            &p,
            &with(profile(), "description", s(&"x".repeat(4097)))
        )),
        vec![limit("description", 4096, "bytes")]
    );

    // select: 1 and 1024 ids pass, 0 and 1025 fail
    let many: Vec<String> = (0..1025).map(nth_id).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(
        check(&p, &with(profile(), "select", ids(&refs[..1]))),
        vec![]
    );
    assert_eq!(
        check(&p, &with(profile(), "select", ids(&refs[..1024]))),
        vec![]
    );
    assert_eq!(
        rows(&check(&p, &with(profile(), "select", ids(&refs)))),
        vec![limit("select", 1024, "ids")]
    );
    assert_eq!(
        rows(&check(&p, &with(profile(), "select", ids(&[])))),
        vec![row(
            "E_BAD_INPUT",
            "Publication select must contain at least one id.",
            "Add a doc or folder id to select."
        )]
    );
    // an element of 64 bytes passes the bound (and is no id); 65 bytes fail the bound
    let bad_ids = row(
        "E_BAD_INPUT",
        "Publication selectors must be distinct full block ids.",
        "Use each doc or folder id once in select.",
    );
    assert_eq!(
        rows(&check(
            &p,
            &with(profile(), "select", ids(&["d-D0C22", &"x".repeat(64)]))
        )),
        vec![bad_ids.clone()]
    );
    assert_eq!(
        rows(&check(
            &p,
            &with(profile(), "select", ids(&["d-D0C22", &"x".repeat(65)]))
        )),
        vec![limit("select", 64, "bytes per id")]
    );
    // a nonstring element, then invalid ids and a duplicate
    assert_eq!(
        rows(&check(
            &p,
            &with(
                profile(),
                "select",
                FieldValue::List(vec![s("d-D0C22"), FieldValue::Int(3)])
            )
        )),
        vec![row(
            "E_FIELD_KIND",
            "field \"select\" must be a list of strings",
            "write select as a list of strings"
        )]
    );
    for bad in [
        "docs/plan",
        "FW-S1",
        "public-site",
        "d-*",
        "d-D0C",
        "d-d0c22",
        "",
    ] {
        assert_eq!(
            rows(&check(
                &p,
                &with(profile(), "select", ids(&["d-D0C22", bad]))
            )),
            vec![bad_ids.clone()],
            "{bad:?}"
        );
    }
    assert_eq!(
        rows(&check(
            &p,
            &with(
                profile(),
                "select",
                ids(&["d-D0C22", "fd-F0DR1", "d-D0C22"])
            )
        )),
        vec![bad_ids.clone()]
    );
    // the select stages stop at the first one that fails: a count issue hides the rest
    let mut stages: Vec<FieldValue> = (0..1025).map(|i| s(&nth_id(i))).collect();
    stages[0] = FieldValue::Int(1);
    assert_eq!(
        rows(&check(
            &p,
            &with(profile(), "select", FieldValue::List(stages))
        )),
        vec![limit("select", 1024, "ids")]
    );
    // element kinds before element lengths
    assert_eq!(
        rows(&check(
            &p,
            &with(
                profile(),
                "select",
                FieldValue::List(vec![s(&"x".repeat(65)), FieldValue::Bool(true)])
            )
        )),
        vec![row(
            "E_FIELD_KIND",
            "field \"select\" must be a list of strings",
            "write select as a list of strings"
        )]
    );

    // the key: 32 characters pass; 33 give exactly one E_BAD_KEY and no E_LIMIT
    assert_eq!(
        check(&p, &with(profile(), "key", s(&"k".repeat(32)))),
        vec![]
    );
    let long = check(&p, &with(profile(), "key", s(&"k".repeat(33))));
    assert_eq!(
        long.iter().map(|i| i.code).collect::<Vec<_>>(),
        vec!["E_BAD_KEY"]
    );

    // every supplemental issue at once, in key, title, description, select order
    let all = with(
        with(
            with(without(profile(), "key"), "title", s(" ")),
            "description",
            s(&"x".repeat(4097)),
        ),
        "select",
        ids(&[]),
    );
    assert_eq!(
        rows(&check(&p, &all)),
        vec![
            row(
                "E_FIELD_REQUIRED",
                "type publication_profile requires field \"key\"",
                "add key (str)"
            ),
            row(
                "E_BAD_INPUT",
                "Publication title must contain text.",
                "Set title to a nonblank string."
            ),
            limit("description", 4096, "bytes"),
            row(
                "E_BAD_INPUT",
                "Publication select must contain at least one id.",
                "Add a doc or folder id to select."
            ),
        ]
    );
    // parse refuses with the same issues
    assert_eq!(parse(&all).unwrap_err(), check(&p, &all));
    // and refuses a block that is no profile
    assert_eq!(
        rows(&parse(&section()).unwrap_err()),
        vec![row(
            "E_BAD_INPUT",
            "Block s-SECT1 is not a publication profile.",
            "Use a publication profile id or key."
        )]
    );

    // a removed profile's read stub passes the read schema with no supplemental issue
    let stub = Block::redacted(
        BlockId::parse("pf-PR0F1").unwrap(),
        PROFILE.into(),
        Some(BlockId::parse("d-D0C11").unwrap()),
        "V".into(),
        RedactedBlockIdentity {
            version: Hash::of(b"version"),
            content_hash: Hash::of(b"content"),
            frozen: false,
            request: Hash::of(b"the request"),
        },
    );
    assert_eq!(validate_read_block(&p, &stub, Some(&d)), vec![]);
    // the write schema still refuses it
    assert!(has_errors(&validate_block(&p, &stub, Some(&d))));
}

#[test]
fn pub1_profile_uses_generic_validation_codes_and_text() {
    let p = standard();

    // unknown ordinary field: generic text, the type's own fields, no nearest-key hint
    assert_eq!(
        rows(&check(&p, &with(profile(), "titel", s("x")))),
        vec![row(
            "E_FIELD_UNKNOWN",
            "type publication_profile has no field \"titel\"",
            "remove it, or use one of: audience, description, select, title, trail"
        )]
    );

    // wrong kinds, each with no supplemental issue on top
    for (field, v, kind_dbg, kind) in [
        ("audience", FieldValue::Int(1), "Enum", "enum"),
        ("description", FieldValue::Int(1), "Str", "str"),
        ("select", s("d-D0C22"), "List", "list"),
        ("title", FieldValue::Bool(true), "Str", "str"),
        ("trail", s("yes"), "Bool", "bool"),
    ] {
        assert_eq!(
            rows(&check(&p, &with(profile(), field, v))),
            vec![row(
                "E_FIELD_KIND",
                &format!("field {field:?} must be {kind_dbg}"),
                &format!("write {field} as a {kind} value")
            )],
            "{field}"
        );
    }
    assert_eq!(
        rows(&check(&p, &with(profile(), "key", FieldValue::Int(7)))),
        vec![row(
            "E_FIELD_KIND",
            "field \"key\" must be Str",
            "write key as a str value"
        )]
    );

    // bad audience, with Rust debug escaping of the value
    assert_eq!(
        rows(&check(&p, &with(profile(), "audience", s("team \"a\"")))),
        vec![row(
            "E_FIELD_ENUM",
            "field \"audience\" has value \"team \\\"a\\\"\"",
            "use one of: org, public"
        )]
    );

    // missing declared required fields, in definition order, with no supplemental issue
    let bare = without(without(without(profile(), "audience"), "select"), "title");
    assert_eq!(
        rows(&check(&p, &bare)),
        vec![
            row(
                "E_FIELD_REQUIRED",
                "type publication_profile requires field \"audience\"",
                "add audience (enum)"
            ),
            row(
                "E_FIELD_REQUIRED",
                "type publication_profile requires field \"select\"",
                "add select (list)"
            ),
            row(
                "E_FIELD_REQUIRED",
                "type publication_profile requires field \"title\"",
                "add title (str)"
            ),
        ]
    );

    // generic order: present fields, required fields, body, link kinds, parent, then
    // supplemental
    let mut mixed = without(with(profile(), "zzz", s("x")), "audience");
    mixed.body = "words".into();
    mixed.links.insert(
        "nope".into(),
        vec![Link {
            target: BlockId::parse("d-D0C22").unwrap(),
            part: None,
        }],
    );
    mixed.fields.insert("title".into(), s(""));
    let got = validate_block(&p, &mixed, Some(&section()));
    assert_eq!(
        got.iter().map(|i| i.code).collect::<Vec<_>>(),
        vec![
            "E_FIELD_UNKNOWN",
            "E_FIELD_REQUIRED",
            "E_BODY_FORBIDDEN",
            "E_LINK_TYPE",
            "E_CHILD_TYPE",
            "E_BAD_INPUT",
        ]
    );

    // no parent, and a missing parent block
    let orphan = Block {
        parent: None,
        ..profile()
    };
    assert_eq!(
        rows(&validate_block(&p, &orphan, None)),
        vec![row(
            "E_ORPHAN",
            "publication_profile must have a parent",
            "set parent to a doc id"
        )]
    );
    assert_eq!(
        rows(&validate_block(&p, &profile(), None)),
        vec![row(
            "E_MISSING_PARENT",
            "pf-PR0F1 has parent d-D0C11, which does not exist",
            "move pf-PR0F1 under an existing block, or recreate d-D0C11"
        )]
    );

    // key, key_auto and reflects keep their engine-field rules
    let bad_key = check(&p, &with(profile(), "key", s("d-D0C22")));
    assert_eq!(
        bad_key.iter().map(|i| i.code).collect::<Vec<_>>(),
        vec!["E_BAD_KEY"]
    );
    assert_eq!(
        rows(&check(
            &p,
            &with(profile(), "key_auto", FieldValue::Bool(true))
        )),
        vec![row(
            "E_FIELD_UNKNOWN",
            "type publication_profile cannot carry \"key_auto\": only a doc's or folder's key is derived from its path",
            "remove key_auto"
        )]
    );
    assert_eq!(
        rows(&check(&p, &with(profile(), "reflects", ids(&["src/*.rs"])))),
        vec![row(
            "E_FIELD_UNKNOWN",
            "type publication_profile cannot carry \"reflects\": reflects justifies content; a decision links to code through Implements: trailers",
            "remove reflects; a decision links to code through Implements: trailers on the commit"
        )]
    );

    // no issue appears twice
    for b in [&mixed, &bare, &orphan] {
        let issues = validate_block(&p, b, Some(&doc()));
        let mut seen = BTreeSet::new();
        for i in &issues {
            assert!(seen.insert((i.code, i.message.clone())), "{i:?}");
        }
    }
}

#[test]
fn pub1_profile_is_supporting_and_uses_the_existing_block_hash() {
    for p in [bare(), standard(), custom()] {
        assert_eq!(p.class_of(PROFILE), TypeClass::Supporting);
        assert_eq!(p.class_of_prefix("pf"), Some(TypeClass::Supporting));
        assert!(!p.may_retype(PROFILE, "prose"));
        assert!(!p.may_retype("prose", PROFILE));
    }
    let pf = profile();
    // the ordinary block encoding and hashes: nothing profile-specific
    assert_eq!(pf.version_hash(), hash_of(&pf));
    let json = serde_json::to_string(&pf).unwrap();
    let back: Block = serde_json::from_str(&json).unwrap();
    assert_eq!(back, pf);
    assert_eq!(back.version_hash(), pf.version_hash());
    assert_eq!(back.content_hash(), pf.content_hash());
    let mut edited = pf.clone();
    edited.fields.insert("title".into(), s("Other"));
    assert_ne!(edited.version_hash(), pf.version_hash());
    // a snapshot pins the profile's version like any block's: no new tag structure
    let snap = StateSnapshot {
        blocks: BTreeMap::from([(pf.id.clone(), pf.version_hash())]),
    };
    assert_eq!(snap.blocks[&pf.id], hash_of(&pf));
}
