//! ap1: the authority policy is an engine block type, injected into every pack (D-ZWP7Q). Generic
//! validation checks its four top-level fields; `policy::check_values` checks what they hold
//! (O3, O5), and `policy_fields` reads a valid block.
use rdy_model::{Block, BlockId, FieldValue};
use rdy_schema::policy::{
    self, DecisionsRule, JustificationKind, Lifecycle, OwnerEntry, PolicyFields, Require, Rule,
    When,
};
use rdy_schema::*;
use std::collections::{BTreeMap, BTreeSet};

const POLICY: &str = "authority_policy";
const FIX: &str = "correct the authority_policy block: rdy_write / rdy edit";

fn standard() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

fn bare() -> Pack {
    let mut set = PackSet::with_builtin();
    let name = set.add_yaml("name: bare\nversion: \"1\"\n").unwrap();
    set.resolve(&name).unwrap()
}

/// The standard pack with two areas, `payments` and `identity`.
fn with_areas() -> Pack {
    let mut set = PackSet::with_builtin();
    let name = set
        .add_yaml(
            "name: team\nversion: \"1\"\nextends: rdy-standard\nareas:\n  payments:\n    paths: [payments/**]\n  identity:\n    paths: [identity/**]\n",
        )
        .unwrap();
    set.resolve(&name).unwrap()
}

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
}

fn list(v: &[&str]) -> FieldValue {
    FieldValue::List(v.iter().map(|x| s(x)).collect())
}

fn map(entries: Vec<(&str, FieldValue)>) -> FieldValue {
    FieldValue::Map(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

fn doc() -> Block {
    let mut d = block("d-D0C11", "doc", None);
    for (k, v) in [("name", "plan"), ("title", "Plan"), ("doc_type", "page")] {
        d.fields.insert(k.into(), s(v));
    }
    d
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

/// A policy block under `d-D0C11` with the engine's template fields.
fn policy_block() -> Block {
    let mut p = block("ap-APQ01", POLICY, Some("d-D0C11"));
    p.fields = policy::template_fields();
    p
}

/// A valid rule named `name`: agents of nick, draft content, implements only, no decision.
fn rule(name: &str) -> BTreeMap<String, FieldValue> {
    [
        ("name", s(name)),
        ("actors", list(&["agent:nick/*"])),
        ("lifecycle", s("draft")),
        ("justifications", list(&["implements"])),
        ("decisions", s("none")),
        ("max_blocks", FieldValue::Int(20)),
        ("max_items", FieldValue::Int(0)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

fn with_rules(rules: Vec<BTreeMap<String, FieldValue>>) -> Block {
    let mut p = policy_block();
    p.fields.insert(
        "agent_may_accept".into(),
        FieldValue::List(rules.into_iter().map(FieldValue::Map).collect()),
    );
    p
}

fn set(p: &mut Block, field: &str, v: FieldValue) {
    p.fields.insert(field.into(), v);
}

/// The error issues of `p` under `d-D0C11`, as (code, message).
fn errors(pack: &Pack, p: &Block) -> Vec<(&'static str, String)> {
    validate_block(pack, p, Some(&doc()))
        .into_iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| (i.code, i.message))
        .collect()
}

/// The one issue `p` has, which must be a policy issue with the policy fix.
fn only(pack: &Pack, p: &Block) -> (&'static str, String) {
    let issues: Vec<Issue> = validate_block(pack, p, Some(&doc()));
    assert_eq!(issues.len(), 1, "{issues:#?}");
    assert_eq!(issues[0].fix, FIX, "{issues:#?}");
    assert_eq!(issues[0].block.as_ref(), Some(&p.id));
    (issues[0].code, issues[0].message.clone())
}

fn issue(code: &'static str, path: &str, reason: &str) -> (&'static str, String) {
    (code, format!("authority policy {path}: {reason}"))
}

#[test]
fn ap1_every_pack_holds_the_policy_type() {
    for pack in [standard(), bare(), with_areas()] {
        let def = &pack.block_types[POLICY];
        assert_eq!(def.prefix, "ap");
        assert_eq!(pack.type_for_prefix("ap"), Some(POLICY));
        assert_eq!(def.class, Some(TypeClass::Supporting));
        assert_eq!(pack.class_of(POLICY), TypeClass::Supporting);
        assert_eq!(def.body, BodyRule::Forbidden);
        assert!(def.children.is_empty());
        assert_eq!(def.traits, BTreeSet::from([Trait::Structural]));
        let kinds: BTreeMap<&str, (FieldKind, bool)> = def
            .fields
            .iter()
            .map(|(k, f)| (k.as_str(), (f.kind, f.required)))
            .collect();
        assert_eq!(
            kinds,
            BTreeMap::from([
                ("agent_may_accept", (FieldKind::List, false)),
                ("owners", (FieldKind::List, false)),
                ("require", (FieldKind::List, false)),
                ("roles", (FieldKind::Map, false)),
            ])
        );
        // a doc lists it once, after the publication profile
        let children = &pack.block_types["doc"].children;
        assert_eq!(children.iter().filter(|c| *c == POLICY).count(), 1);
        assert_eq!(children.last().map(String::as_str), Some(POLICY));
        assert_eq!(
            children
                .iter()
                .filter(|c| *c == "publication_profile")
                .count(),
            1
        );
    }
    assert_eq!(policy::POLICY_TYPE, POLICY);
    assert_eq!(policy::POLICY_PREFIX, "ap");
    assert_eq!(policy::POLICY_KEY, "authority-policy");
}

#[test]
fn ap1_a_pack_that_declares_the_type_is_refused() {
    let mut set = PackSet::with_builtin();
    let name = set
        .add_yaml("name: t\nversion: \"1\"\nblock_types:\n  authority_policy:\n    prefix: ap\n")
        .unwrap();
    match set.resolve(&name) {
        Err(SchemaError::Invalid(msg)) => assert_eq!(
            msg,
            "authority_policy is an engine type and cannot be declared by a pack"
        ),
        other => panic!("{other:?}"),
    }
    // a pack type cannot take its prefix either
    let mut set = PackSet::with_builtin();
    let name = set
        .add_yaml("name: t\nversion: \"1\"\nblock_types:\n  memo:\n    prefix: ap\n")
        .unwrap();
    assert!(matches!(set.resolve(&name), Err(SchemaError::Invalid(_))));
}

#[test]
fn ap1_the_template_validates() {
    let want: BTreeMap<String, FieldValue> = [
        ("key", s("authority-policy")),
        ("agent_may_accept", FieldValue::List(vec![])),
        ("roles", FieldValue::Map(BTreeMap::new())),
        ("require", FieldValue::List(vec![])),
        ("owners", FieldValue::List(vec![])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    assert_eq!(policy::template_fields(), want);
    for pack in [standard(), bare(), with_areas()] {
        assert_eq!(errors(&pack, &policy_block()), vec![]);
        assert!(validate_block(&pack, &policy_block(), Some(&doc())).is_empty());
    }
    // it sits directly under a doc, as a publication profile does
    let mut section = block("s-SECT1", "section", Some("d-D0C11"));
    section.fields.insert("title".into(), s("Goals"));
    let mut under = policy_block();
    under.parent = Some(section.id.clone());
    let issues = validate_block(&standard(), &under, Some(&section));
    assert_eq!(
        issues.iter().map(|i| i.code).collect::<Vec<_>>(),
        ["E_CHILD_TYPE"]
    );
    let mut top = policy_block();
    top.parent = None;
    let issues = validate_block(&standard(), &top, None);
    assert_eq!(
        issues.iter().map(|i| i.code).collect::<Vec<_>>(),
        ["E_ORPHAN"]
    );
    // no body
    let mut bodied = policy_block();
    bodied.body = "text".into();
    let issues = validate_block(&standard(), &bodied, Some(&doc()));
    assert_eq!(
        issues.iter().map(|i| i.code).collect::<Vec<_>>(),
        ["E_BODY_FORBIDDEN"]
    );
}

#[test]
fn ap1_the_key_must_be_authority_policy() {
    let pack = standard();
    let mut p = policy_block();
    set(&mut p, "key", s("my-policy"));
    assert_eq!(
        only(&pack, &p),
        issue("E_FIELD_KIND", "key", "must be authority-policy")
    );
    p.fields.remove("key");
    assert_eq!(
        only(&pack, &p),
        issue("E_FIELD_REQUIRED", "key", "is required")
    );
}

#[test]
fn ap1_an_agent_or_ci_role_member_fails() {
    let pack = standard();
    let mut p = policy_block();
    let ci = format!("ci:{}/build", "0123456789abcdef0123456789abcdef");
    set(
        &mut p,
        "roles",
        map(vec![
            ("approvers", list(&["human:nick", "agent:a1"])),
            ("builders", list(&["team:core", &ci])),
            ("people", list(&["human:nick", "team:core"])),
        ]),
    );
    assert_eq!(
        errors(&pack, &p),
        vec![
            issue(
                "E_FIELD_KIND",
                "roles.approvers[1]",
                "names agent:a1; it holds people and teams only"
            ),
            issue(
                "E_FIELD_KIND",
                "roles.builders[1]",
                &format!("names {ci}; it holds people and teams only")
            ),
        ]
    );
    // the same rule holds in an owner entry
    let mut p = policy_block();
    set(
        &mut p,
        "owners",
        FieldValue::List(vec![map(vec![
            ("path", s("payments")),
            ("owners", list(&["agent:nick/a1"])),
        ])]),
    );
    assert_eq!(
        only(&pack, &p),
        issue(
            "E_FIELD_KIND",
            "owners[0].owners[0]",
            "names agent:nick/a1; it holds people and teams only"
        )
    );
}

#[test]
fn ap1_a_ruling_basis_fails() {
    let pack = with_areas();
    let mut r0 = rule("r0");
    r0.insert("decisions".into(), s("two_way"));
    r0.insert("two_way_areas".into(), list(&["payments"]));
    r0.insert("basis".into(), list(&["delegated", "direct_ruling"]));
    let mut r1 = r0.clone();
    r1.insert("name".into(), s("r1"));
    r1.insert("basis".into(), list(&["recorded_ruling"]));
    assert_eq!(
        errors(&pack, &with_rules(vec![r0, r1])),
        vec![
            issue(
                "E_FIELD_KIND",
                "agent_may_accept[0].basis[1]",
                "lists direct_ruling; a policy never accepts a person's ruling"
            ),
            issue(
                "E_FIELD_KIND",
                "agent_may_accept[1].basis[0]",
                "lists recorded_ruling; a policy never accepts a person's ruling"
            ),
        ]
    );
}

#[test]
fn ap1_two_equal_owner_paths_fail() {
    let pack = standard();
    let mut p = policy_block();
    let entry = |path: &str, who: &str| map(vec![("path", s(path)), ("owners", list(&[who]))]);
    set(
        &mut p,
        "owners",
        FieldValue::List(vec![
            entry("docs/a", "human:nick"),
            entry("docs/b", "human:nick"),
            entry("docs/a", "team:core"),
        ]),
    );
    assert_eq!(
        only(&pack, &p),
        issue("E_FIELD_KIND", "owners[2].path", "repeats docs/a")
    );
}

#[test]
fn ap1_two_way_needs_two_way_areas_and_none_takes_none() {
    let pack = with_areas();
    let mut two_way = rule("r0");
    two_way.insert("decisions".into(), s("two_way"));
    assert_eq!(
        only(&pack, &with_rules(vec![two_way.clone()])),
        issue(
            "E_FIELD_KIND",
            "agent_may_accept[0].two_way_areas",
            "must be 1 to 64 areas of the pack when decisions is two_way"
        )
    );
    two_way.insert("two_way_areas".into(), list(&[]));
    assert_eq!(
        only(&pack, &with_rules(vec![two_way.clone()])).1,
        issue(
            "E_FIELD_KIND",
            "agent_may_accept[0].two_way_areas",
            "must be 1 to 64 areas of the pack when decisions is two_way"
        )
        .1
    );
    two_way.insert("two_way_areas".into(), list(&["payments"]));
    assert_eq!(errors(&pack, &with_rules(vec![two_way])), vec![]);
    let mut none = rule("r0");
    none.insert("two_way_areas".into(), list(&["payments"]));
    assert_eq!(
        only(&pack, &with_rules(vec![none.clone()])),
        issue(
            "E_FIELD_KIND",
            "agent_may_accept[0].two_way_areas",
            "must be empty when decisions is none"
        )
    );
    none.insert("two_way_areas".into(), list(&[]));
    assert_eq!(errors(&pack, &with_rules(vec![none])), vec![]);
}

#[test]
fn ap1_an_unknown_area_fails() {
    let pack = with_areas();
    let mut r = rule("r0");
    r.insert("areas".into(), list(&["payments", "nowhere"]));
    r.insert("decisions".into(), s("two_way"));
    r.insert("two_way_areas".into(), list(&["elsewhere"]));
    let mut p = with_rules(vec![r]);
    set(
        &mut p,
        "roles",
        map(vec![("approvers", list(&["human:nick"]))]),
    );
    set(
        &mut p,
        "require",
        FieldValue::List(vec![
            map(vec![("when", s("area:identity")), ("role", s("approvers"))]),
            map(vec![("when", s("area:nowhere")), ("role", s("approvers"))]),
        ]),
    );
    let reason = "names no area of the pack";
    assert_eq!(
        errors(&pack, &p),
        vec![
            issue("E_FIELD_KIND", "agent_may_accept[0].areas[1]", reason),
            issue(
                "E_FIELD_KIND",
                "agent_may_accept[0].two_way_areas[0]",
                reason
            ),
            issue("E_FIELD_KIND", "require[1].when", reason),
        ]
    );
    // the standard pack declares no area, so every area name fails
    let mut r = rule("r0");
    r.insert("areas".into(), list(&["payments"]));
    assert_eq!(
        only(&standard(), &with_rules(vec![r])),
        issue("E_FIELD_KIND", "agent_may_accept[0].areas[0]", reason)
    );
}

#[test]
fn ap1_an_empty_basis_reads_as_accepting_no_decision() {
    let pack = with_areas();
    let mut r = rule("r0");
    r.insert("decisions".into(), s("two_way"));
    r.insert("two_way_areas".into(), list(&["payments"]));
    let absent = with_rules(vec![r.clone()]);
    assert_eq!(errors(&pack, &absent), vec![]);
    let fields = policy::policy_fields(&pack, &absent).expect("valid");
    assert!(fields.rules[0].basis.is_empty());
    r.insert("basis".into(), list(&[]));
    let empty = with_rules(vec![r]);
    assert_eq!(errors(&pack, &empty), vec![]);
    let fields = policy::policy_fields(&pack, &empty).expect("valid");
    assert!(fields.rules[0].basis.is_empty());
    assert_eq!(fields.rules[0].decisions, DecisionsRule::TwoWay);
}

#[test]
fn ap1_a_top_level_field_of_the_wrong_kind_is_reported_once() {
    let pack = standard();
    for (field, value, kind) in [
        ("roles", s("approvers"), "Map"),
        ("agent_may_accept", map(vec![]), "List"),
        ("require", FieldValue::Int(1), "List"),
        ("owners", s("docs"), "List"),
    ] {
        let mut p = policy_block();
        set(&mut p, field, value);
        let issues = validate_block(&pack, &p, Some(&doc()));
        assert_eq!(issues.len(), 1, "{field}: {issues:#?}");
        assert_eq!(issues[0].code, "E_FIELD_KIND");
        assert_eq!(issues[0].message, format!("field {field:?} must be {kind}"));
        assert!(policy::policy_fields(&pack, &p).is_none(), "{field}");
    }
    // an absent field is no issue: each of the four is optional
    let mut p = policy_block();
    for field in ["agent_may_accept", "roles", "require", "owners"] {
        p.fields.remove(field);
    }
    assert_eq!(errors(&pack, &p), vec![]);
    let fields = policy::policy_fields(&pack, &p).expect("valid");
    assert_eq!(
        fields,
        PolicyFields {
            rules: vec![],
            roles: BTreeMap::new(),
            require: vec![],
            owners: vec![],
        }
    );
}

#[test]
fn ap1_an_unknown_rule_key_names_the_near_key() {
    let pack = standard();
    let mut r = rule("r0");
    let max = r.remove("max_blocks").unwrap();
    r.insert("max_block".into(), max);
    r.insert("zzzzzz".into(), s("x"));
    assert_eq!(
        errors(&pack, &with_rules(vec![r])),
        vec![
            issue(
                "E_FIELD_UNKNOWN",
                "agent_may_accept[0].max_block",
                "is not a field; did you mean max_blocks?"
            ),
            issue(
                "E_FIELD_UNKNOWN",
                "agent_may_accept[0].zzzzzz",
                "is not a field"
            ),
            issue(
                "E_FIELD_REQUIRED",
                "agent_may_accept[0].max_blocks",
                "is required"
            ),
        ]
    );
    // the entries of require and owners name their near keys too
    let mut p = policy_block();
    set(
        &mut p,
        "roles",
        map(vec![("approvers", list(&["human:nick"]))]),
    );
    set(
        &mut p,
        "require",
        FieldValue::List(vec![map(vec![("when", s("any")), ("rol", s("approvers"))])]),
    );
    set(
        &mut p,
        "owners",
        FieldValue::List(vec![map(vec![
            ("path", s("docs")),
            ("owner", list(&["human:nick"])),
        ])]),
    );
    assert_eq!(
        errors(&pack, &p),
        vec![
            issue(
                "E_FIELD_UNKNOWN",
                "require[0].rol",
                "is not a field; did you mean role?"
            ),
            issue("E_FIELD_REQUIRED", "require[0].role", "is required"),
            issue(
                "E_FIELD_UNKNOWN",
                "owners[0].owner",
                "is not a field; did you mean owners?"
            ),
            issue("E_FIELD_REQUIRED", "owners[0].owners", "is required"),
        ]
    );
}

#[test]
fn ap1_a_policy_at_each_bound_passes_and_one_past_it_fails() {
    let pack = standard();
    // 64 rules, then 65
    let rules = |n: usize| (0..n).map(|i| rule(&format!("r{i}"))).collect::<Vec<_>>();
    assert_eq!(errors(&pack, &with_rules(rules(64))), vec![]);
    assert_eq!(
        only(&pack, &with_rules(rules(65))),
        issue("E_LIMIT", "agent_may_accept", "holds more than 64 rules")
    );
    // 64 patterns, then 65
    let patterns = |n: usize| {
        FieldValue::List(
            (0..n)
                .map(|i| FieldValue::Str(format!("agent:a{i}")))
                .collect(),
        )
    };
    let mut r = rule("r0");
    r.insert("actors".into(), patterns(64));
    assert_eq!(errors(&pack, &with_rules(vec![r.clone()])), vec![]);
    r.insert("actors".into(), patterns(65));
    assert_eq!(
        only(&pack, &with_rules(vec![r])),
        issue(
            "E_LIMIT",
            "agent_may_accept[0].actors",
            "holds more than 64 patterns"
        )
    );
    // 256 members, then 257
    let members = |n: usize| {
        FieldValue::List(
            (0..n)
                .map(|i| FieldValue::Str(format!("human:p{i}")))
                .collect(),
        )
    };
    let mut p = policy_block();
    set(&mut p, "roles", map(vec![("approvers", members(256))]));
    assert_eq!(errors(&pack, &p), vec![]);
    set(&mut p, "roles", map(vec![("approvers", members(257))]));
    assert_eq!(
        only(&pack, &p),
        issue("E_LIMIT", "roles.approvers", "holds more than 256 members")
    );
    // 10,000 owner entries, then 10,001
    let owners = |n: usize| {
        FieldValue::List(
            (0..n)
                .map(|i| {
                    map(vec![
                        ("path", FieldValue::Str(format!("docs/d{i}"))),
                        ("owners", list(&["human:nick"])),
                    ])
                })
                .collect(),
        )
    };
    let mut p = policy_block();
    set(&mut p, "owners", owners(10_000));
    assert_eq!(errors(&pack, &p), vec![]);
    assert_eq!(
        policy::policy_fields(&pack, &p).unwrap().owners.len(),
        10_000
    );
    set(&mut p, "owners", owners(10_001));
    assert_eq!(
        only(&pack, &p),
        issue("E_LIMIT", "owners", "holds more than 10000 entries")
    );
    // a 240-byte owner path, then 241
    let path_of = |n: usize| {
        let mut p = policy_block();
        set(
            &mut p,
            "owners",
            FieldValue::List(vec![map(vec![
                ("path", FieldValue::Str("p".repeat(n))),
                ("owners", list(&["team:core"])),
            ])]),
        );
        p
    };
    assert_eq!(errors(&pack, &path_of(240)), vec![]);
    assert_eq!(
        only(&pack, &path_of(241)),
        issue("E_LIMIT", "owners[0].path", "holds more than 240 bytes")
    );
    assert_eq!(policy::MAX_PATH_BYTES, 240);
    assert_eq!(
        (
            policy::MAX_RULES,
            policy::MAX_PATTERNS,
            policy::MAX_AREAS,
            policy::MAX_ROLES,
            policy::MAX_MEMBERS,
            policy::MAX_REQUIRE,
            policy::MAX_OWNERS
        ),
        (64, 64, 64, 64, 256, 64, 10_000)
    );
}

#[test]
fn ap1_a_require_role_must_name_a_role_and_owner_takes_none() {
    let pack = standard();
    let mut p = policy_block();
    set(
        &mut p,
        "roles",
        map(vec![("approvers", list(&["human:nick"]))]),
    );
    set(
        &mut p,
        "require",
        FieldValue::List(vec![
            map(vec![("when", s("one_way")), ("role", s("approvers"))]),
            map(vec![("when", s("any")), ("role", s("nobody"))]),
            map(vec![("when", s("owner")), ("role", s("approvers"))]),
            map(vec![("when", s("decision"))]),
            map(vec![("when", s("owner"))]),
            map(vec![("when", s("sometimes")), ("role", s("approvers"))]),
        ]),
    );
    assert_eq!(
        errors(&pack, &p),
        vec![
            issue("E_FIELD_KIND", "require[1].role", "must be a role of roles"),
            issue(
                "E_FIELD_KIND",
                "require[2].role",
                "must be absent when when is owner"
            ),
            issue("E_FIELD_REQUIRED", "require[3].role", "is required"),
            issue(
                "E_FIELD_KIND",
                "require[5].when",
                "must be one_way, decision, area:<area>, deferral, reaffirm, owner or any"
            ),
        ]
    );
    // each form of when reads
    let pack = with_areas();
    let mut p = policy_block();
    set(
        &mut p,
        "roles",
        map(vec![("approvers", list(&["team:core"]))]),
    );
    let whens = [
        "one_way",
        "decision",
        "area:payments",
        "deferral",
        "reaffirm",
        "any",
    ];
    let mut entries: Vec<FieldValue> = whens
        .iter()
        .map(|w| map(vec![("when", s(w)), ("role", s("approvers"))]))
        .collect();
    entries.push(map(vec![("when", s("owner"))]));
    set(&mut p, "require", FieldValue::List(entries));
    assert_eq!(errors(&pack, &p), vec![]);
    let fields = policy::policy_fields(&pack, &p).unwrap();
    let role = Some("approvers".to_string());
    assert_eq!(
        fields.require,
        vec![
            Require {
                when: When::OneWay,
                role: role.clone()
            },
            Require {
                when: When::Decision,
                role: role.clone()
            },
            Require {
                when: When::Area("payments".into()),
                role: role.clone()
            },
            Require {
                when: When::Deferral,
                role: role.clone()
            },
            Require {
                when: When::Reaffirm,
                role: role.clone()
            },
            Require {
                when: When::Any,
                role
            },
            Require {
                when: When::Owner,
                role: None
            },
        ]
    );
}

#[test]
fn ap1_patterns_match_by_prefix() {
    assert!(policy::is_pattern("agent:nick/*"));
    assert!(policy::is_pattern("agent:*"));
    assert!(policy::is_pattern("agent:a1"));
    assert!(policy::is_pattern("human:nick"));
    assert!(!policy::is_pattern("agent:*x"));
    assert!(!policy::is_pattern("agent:nick/**"));
    assert!(!policy::is_pattern("*"));
    assert!(!policy::is_pattern("agent:Nick/*"));
    assert!(!policy::is_pattern(""));
    assert!(policy::pattern_matches("agent:nick/*", "agent:nick/a1"));
    assert!(!policy::pattern_matches("agent:nick/*", "agent:nick2/a1"));
    assert!(!policy::pattern_matches("agent:nick/*", "agent:nick"));
    assert!(policy::pattern_matches("agent:*", "agent:nick/a1"));
    assert!(!policy::pattern_matches("agent:*", "human:nick"));
    assert!(policy::pattern_matches("agent:a1", "agent:a1"));
    assert!(!policy::pattern_matches("agent:a1", "agent:a10"));
    assert!(!policy::pattern_matches("agent:a1", "agent:a1/x"));
    assert!(!policy::pattern_matches("agent:*x", "agent:*x"));
    assert!(!policy::pattern_matches("agent:*x", "agent:ax"));
    // a rule's actors take patterns only
    let pack = standard();
    let mut r = rule("r0");
    r.insert("actors".into(), list(&["agent:nick/*", "agent:*x"]));
    assert_eq!(
        only(&pack, &with_rules(vec![r])),
        issue(
            "E_FIELD_KIND",
            "agent_may_accept[0].actors[1]",
            "must be an actor pattern, such as agent:nick/*"
        )
    );
}

#[test]
fn ap1_policy_fields_reads_a_valid_block_and_none_for_an_invalid_one() {
    let pack = with_areas();
    let mut r0 = rule("docs-agents");
    r0.insert("actors".into(), list(&["agent:nick/*", "agent:ci-bot"]));
    r0.insert("areas".into(), list(&["payments", "identity"]));
    r0.insert("lifecycle".into(), s("any"));
    r0.insert(
        "justifications".into(),
        list(&["reflects", "implements", "editorial"]),
    );
    r0.insert("decisions".into(), s("two_way"));
    r0.insert("two_way_areas".into(), list(&["identity"]));
    r0.insert("basis".into(), list(&["precedent", "delegated"]));
    r0.insert("max_blocks".into(), FieldValue::Int(100_000));
    r0.insert("max_items".into(), FieldValue::Int(100_000));
    let r1 = rule("small");
    let mut p = with_rules(vec![r0, r1]);
    set(
        &mut p,
        "roles",
        map(vec![
            ("approvers", list(&["human:nick", "team:core"])),
            ("leads", list(&["human:ana"])),
        ]),
    );
    set(
        &mut p,
        "require",
        FieldValue::List(vec![
            map(vec![("when", s("one_way")), ("role", s("leads"))]),
            map(vec![("when", s("owner"))]),
            map(vec![("when", s("any")), ("role", s("approvers"))]),
        ]),
    );
    set(
        &mut p,
        "owners",
        FieldValue::List(vec![
            map(vec![
                ("path", s("payments")),
                ("owners", list(&["human:ana", "team:pay"])),
            ]),
            map(vec![
                ("path", s("d-D0C11")),
                ("owners", list(&["human:nick"])),
            ]),
        ]),
    );
    assert_eq!(errors(&pack, &p), vec![]);
    let want = PolicyFields {
        rules: vec![
            Rule {
                name: "docs-agents".into(),
                actors: vec!["agent:nick/*".into(), "agent:ci-bot".into()],
                areas: vec!["payments".into(), "identity".into()],
                lifecycle: Lifecycle::Any,
                justifications: BTreeSet::from([
                    JustificationKind::Implements,
                    JustificationKind::Editorial,
                    JustificationKind::Reflects,
                ]),
                decisions: DecisionsRule::TwoWay,
                two_way_areas: vec!["identity".into()],
                basis: BTreeSet::from(["delegated".to_string(), "precedent".to_string()]),
                max_blocks: 100_000,
                max_items: 100_000,
            },
            Rule {
                name: "small".into(),
                actors: vec!["agent:nick/*".into()],
                areas: vec![],
                lifecycle: Lifecycle::Draft,
                justifications: BTreeSet::from([JustificationKind::Implements]),
                decisions: DecisionsRule::None,
                two_way_areas: vec![],
                basis: BTreeSet::new(),
                max_blocks: 20,
                max_items: 0,
            },
        ],
        roles: BTreeMap::from([
            (
                "approvers".to_string(),
                vec!["human:nick".to_string(), "team:core".to_string()],
            ),
            ("leads".to_string(), vec!["human:ana".to_string()]),
        ]),
        require: vec![
            Require {
                when: When::OneWay,
                role: Some("leads".into()),
            },
            Require {
                when: When::Owner,
                role: None,
            },
            Require {
                when: When::Any,
                role: Some("approvers".into()),
            },
        ],
        owners: vec![
            OwnerEntry {
                path: "payments".into(),
                owners: vec!["human:ana".into(), "team:pay".into()],
            },
            OwnerEntry {
                path: "d-D0C11".into(),
                owners: vec!["human:nick".into()],
            },
        ],
    };
    assert_eq!(policy::policy_fields(&pack, &p), Some(want));

    // an invalid block reads as None
    let mut bad = p.clone();
    set(&mut bad, "key", s("other"));
    assert_eq!(policy::policy_fields(&pack, &bad), None);
    let mut bad = p.clone();
    set(
        &mut bad,
        "roles",
        map(vec![("approvers", list(&["agent:a1"]))]),
    );
    assert_eq!(policy::policy_fields(&pack, &bad), None);
    let mut bad = with_rules(vec![{
        let mut r = rule("r0");
        r.insert("max_blocks".into(), FieldValue::Int(0));
        r
    }]);
    assert_eq!(
        errors(&pack, &bad),
        vec![issue(
            "E_FIELD_KIND",
            "agent_may_accept[0].max_blocks",
            "must be a whole number from 1 to 100000"
        )]
    );
    assert_eq!(policy::policy_fields(&pack, &bad), None);
    set(&mut bad, "agent_may_accept", s("x"));
    assert_eq!(policy::policy_fields(&pack, &bad), None);
    // a block of another type is no policy
    let mut other = doc();
    other.fields = policy::template_fields();
    assert_eq!(policy::policy_fields(&pack, &other), None);
}
