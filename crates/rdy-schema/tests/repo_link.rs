//! rpl4: the engine type `repo_link` (prefix `rl`), a supporting block directly under a doc.
use rdy_model::{Block, BlockId, FieldValue};
use rdy_schema::*;
use std::collections::BTreeMap;

const FIX: &str = "correct the repo_link block: rdy_write / rdy edit";
const REPO: &str = "0123456789abcdef0123456789abcdef";

fn standard() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

fn resolve(yaml: &str) -> Result<Pack, SchemaError> {
    let mut set = PackSet::with_builtin();
    let name = set.add_yaml(yaml).unwrap();
    set.resolve(&name)
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
        foreign: Default::default(),
        read_identity: None,
    }
}

fn doc() -> Block {
    let mut d = block("d-D0C11", "doc", None);
    for (k, v) in [("name", "links"), ("title", "Links"), ("doc_type", "page")] {
        d.fields.insert(k.into(), FieldValue::Str(v.into()));
    }
    d
}

fn link(alias: &str, repo: &str, profile: &str) -> Block {
    let mut b = block("rl-AAAAA", "repo_link", Some("d-D0C11"));
    for (k, v) in [("alias", alias), ("repo", repo), ("profile", profile)] {
        b.fields.insert(k.into(), FieldValue::Str(v.into()));
    }
    b
}

fn check(pack: &Pack, b: &Block) -> Vec<Issue> {
    validate_block(pack, b, Some(&doc()))
}

#[test]
fn rpl4_repo_link_is_supporting_and_closed() {
    let p = standard();
    let def = &p.block_types["repo_link"];
    assert_eq!(def.prefix, "rl");
    assert_eq!(p.class_of("repo_link"), TypeClass::Supporting);
    assert!(def.traits.is_empty());
    assert_eq!(def.body, BodyRule::Forbidden);
    assert!(def.children.is_empty());
    assert!(
        p.block_types["doc"]
            .children
            .iter()
            .any(|c| c == "repo_link")
    );
    assert!(
        !p.block_types["section"]
            .children
            .iter()
            .any(|c| c == "repo_link")
    );
    let fields: Vec<&str> = def.fields.keys().map(String::as_str).collect();
    assert_eq!(fields, ["alias", "profile", "repo", "title"]);
    assert!(def.fields["title"].required == false);
    assert!(def.fields["alias"].required && def.fields["repo"].required);

    // a valid link, with and without a title
    let mut ok = link("hub", REPO, "pf-BBBBB");
    assert!(check(&p, &ok).is_empty(), "{:?}", check(&p, &ok));
    ok.fields
        .insert("title".into(), FieldValue::Str("Hub".into()));
    assert!(check(&p, &ok).is_empty());

    // a body, a key and a wrong parent are refused
    let mut with_body = link("hub", REPO, "pf-BBBBB");
    with_body.body = "text".into();
    assert!(
        check(&p, &with_body)
            .iter()
            .any(|i| i.code == "E_BODY_FORBIDDEN")
    );
    let mut with_key = link("hub", REPO, "pf-BBBBB");
    with_key
        .fields
        .insert("key".into(), FieldValue::Str("HUB".into()));
    assert!(
        check(&p, &with_key)
            .iter()
            .any(|i| i.code == "E_FIELD_UNKNOWN")
    );
    let mut sec = block("s-SSSSS", "section", Some("d-D0C11"));
    sec.fields
        .insert("title".into(), FieldValue::Str("S".into()));
    let under_section = validate_block(&p, &link("hub", REPO, "pf-BBBBB"), Some(&sec));
    assert!(under_section.iter().any(|i| i.code == "E_CHILD_TYPE"));

    // an extra field is refused
    let mut extra = link("hub", REPO, "pf-BBBBB");
    extra
        .fields
        .insert("note".into(), FieldValue::Str("x".into()));
    assert!(
        check(&p, &extra)
            .iter()
            .any(|i| i.code == "E_FIELD_UNKNOWN")
    );

    // a pack cannot declare the type
    let e = resolve(
        "name: t\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  repo_link: { prefix: rl }\n",
    )
    .unwrap_err();
    assert_eq!(
        e.to_string(),
        "invalid pack: repo_link is an engine type and cannot be declared by a pack"
    );
    // and no other type may take its prefix
    let e = resolve(
        "name: t\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  memo: { prefix: rl, body: required }\n",
    )
    .unwrap_err();
    assert!(e.to_string().contains("\"rl\""), "{e}");
}

#[test]
fn rpl4_alias_and_repo_grammar_are_exact() {
    let p = standard();
    let reasons = |b: &Block| -> Vec<(String, String)> {
        check(&p, b)
            .into_iter()
            .map(|i| {
                assert_eq!(i.code, "E_VALIDATION");
                (i.message, i.fix)
            })
            .collect()
    };
    let good_aliases = ["a", "hub", "a-b-c", "a0", &format!("a{}", "b".repeat(23))];
    for a in good_aliases {
        assert!(reasons(&link(a, REPO, "pf-BBBBB")).is_empty(), "{a}");
    }
    let long = format!("a{}", "b".repeat(24));
    let bad_aliases = [
        "",
        "A",
        "1a",
        "-a",
        "a_b",
        "a b",
        "ab.c",
        "é",
        long.as_str(),
    ];
    for a in bad_aliases {
        assert_eq!(
            reasons(&link(a, REPO, "pf-BBBBB")),
            [(format!("repo link {a}: invalid alias"), FIX.to_string())],
            "{a:?}"
        );
    }
    let upper = REPO.to_uppercase();
    let short = &REPO[..31];
    let long_repo = format!("{REPO}0");
    let nonhex = format!("{}g", &REPO[..31]);
    for r in [
        "",
        upper.as_str(),
        short,
        long_repo.as_str(),
        nonhex.as_str(),
        "d-D0C11",
    ] {
        assert_eq!(
            reasons(&link("hub", r, "pf-BBBBB")),
            [(
                "repo link hub: invalid repo id".to_string(),
                FIX.to_string()
            )],
            "{r:?}"
        );
    }
    for pr in [
        "",
        "BBBBB",
        "pf-",
        "d-D0C11",
        "pf-bbbbb",
        "pf-BBBBB-x",
        "rl-AAAAA",
    ] {
        assert_eq!(
            reasons(&link("hub", REPO, pr)),
            [(
                "repo link hub: invalid profile id".to_string(),
                FIX.to_string()
            )],
            "{pr:?}"
        );
    }
    // every bad field gets its own issue, in alias, repo, profile order
    let all = reasons(&link("A", "x", "y"));
    let msgs: Vec<&str> = all.iter().map(|m| m.0.as_str()).collect();
    assert_eq!(
        msgs,
        [
            "repo link A: invalid alias",
            "repo link A: invalid repo id",
            "repo link A: invalid profile id"
        ]
    );
    // the engine, not the schema, checks uniqueness and the project's own repo
    assert!(reasons(&link("hub", "00000000000000000000000000000000", "pf-BBBBB")).is_empty());
    // a wrong field kind is the generic validator's
    let mut b = link("hub", REPO, "pf-BBBBB");
    b.fields.insert("alias".into(), FieldValue::Bool(true));
    let issues = check(&p, &b);
    assert!(
        issues.iter().all(|i| i.code == "E_FIELD_KIND"),
        "{issues:?}"
    );
}
