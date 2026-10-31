//! ck1: a pack's rules load after inheritance and engine types, stay inside their closed limits,
//! share the query parser, and cannot read the landing clock.

use rdy_schema::filter::{Expr, Field, Op, parse};
use rdy_schema::{Pack, PackSet, Rule, RuleSeverity, SchemaError, compile_rules};

/// `text` as a YAML double-quoted scalar.
fn q(text: &str) -> String {
    let inner = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{inner}\"")
}

/// One `rules:` entry, every text double-quoted.
fn rule(name: &str, select: &str, require: &str) -> String {
    format!(
        "  - name: {}\n    select: {}\n    require: {}\n    message: \"Needs a confirmation.\"\n    fix: \"Add one.\"\n",
        q(name),
        q(select),
        q(require)
    )
}

fn pack_with(rules: &str) -> String {
    format!("name: team\nversion: \"1\"\nextends: rdy-standard\nrules:\n{rules}")
}

fn resolve(rules: &str) -> Result<Pack, SchemaError> {
    let mut set = PackSet::with_builtin();
    set.add_yaml(&pack_with(rules))?;
    set.resolve("team")
}

/// The detail of a refused pack: what follows the loader's wrapper.
fn refused(rules: &str) -> String {
    match resolve(rules) {
        Err(SchemaError::Invalid(detail)) => detail,
        other => panic!("expected an invalid pack, got {other:?}"),
    }
}

fn names(pack: &Pack) -> Vec<String> {
    compile_rules(pack)
        .unwrap()
        .into_iter()
        .map(|c| c.rule.name)
        .collect()
}

const ANY: &str = "type = decision";

fn rule_with_texts(name: &str, message: &str, fix: &str) -> String {
    format!(
        "  - name: {}\n    select: {}\n    require: {}\n    message: {}\n    fix: {}\n",
        q(name),
        q(ANY),
        q(ANY),
        q(message),
        q(fix)
    )
}

#[test]
fn ck1_rules_load_after_inheritance_and_engine_types() {
    let mut set = PackSet::with_builtin();
    set.add_yaml(&format!(
        "name: parent\nversion: \"1\"\nextends: rdy-standard\n\
         block_types:\n  decision:\n    prefix: D\n    traits: [normative]\n    fields:\n      owner: {{ kind: str }}\n\
         link_types: [blesses]\n\
         rules:\n{}",
        rule("parent rule", "type = decision and has fields.owner", "out blesses")
    ))
    .unwrap();
    set.add_yaml(&format!(
        "name: child\nversion: \"1\"\nextends: parent\nrules:\n{}",
        rule(
            "child rule",
            "type = decision and fields.owner = nick",
            "has fields.confirmation"
        )
    ))
    .unwrap();
    let pack = set.resolve("child").unwrap();
    // parent first, then this pack, in the merged list
    let raw: Vec<&str> = pack.rules.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(raw, ["parent rule", "child rule"]);
    let compiled = compile_rules(&pack).unwrap();
    assert_eq!(compiled.len(), 2);
    assert_eq!(compiled[0].rule.name, "child rule");
    assert_eq!(compiled[1].rule.name, "parent rule");
    assert_eq!(compiled[0].rule.severity, RuleSeverity::Advisory);

    // an empty child list does not remove parent rules
    set.add_yaml("name: grandchild\nversion: \"1\"\nextends: child\nrules: []\n")
        .unwrap();
    assert_eq!(set.resolve("grandchild").unwrap().rules.len(), 2);

    // the engine type and its injected field are known with no pack declaring them
    resolve(&rule(
        "engine",
        "type = decision",
        "has fields.confirmation",
    ))
    .unwrap();
    // coverage, suspect and tension tests are not restricted
    resolve(&rule(
        "derived",
        "coverage < 50 or suspect = true",
        "tension = false",
    ))
    .unwrap();
}

#[test]
fn ck1_rule_severity_is_snake_case_and_advisory_by_default() {
    let pack = resolve(
        "  - { name: a, select: type = decision, require: has fields.x, message: m, fix: f }\n  - { name: b, select: type = decision, require: has fields.x, severity: blocking, message: m, fix: f }\n  - { name: c, select: type = decision, require: has fields.x, severity: advisory, message: m, fix: f }\n",
    )
    .unwrap();
    let severities: Vec<RuleSeverity> = pack.rules.iter().map(|r| r.severity).collect();
    assert_eq!(
        severities,
        [
            RuleSeverity::Advisory,
            RuleSeverity::Blocking,
            RuleSeverity::Advisory
        ]
    );
}

#[test]
fn ck1_rule_limits_are_closed() {
    // 64 rules pass, 65 fail
    let many = |n: usize| -> String {
        (0..n)
            .map(|i| rule(&format!("r{i:03}"), ANY, ANY))
            .collect()
    };
    assert_eq!(resolve(&many(64)).unwrap().rules.len(), 64);
    assert_eq!(refused(&many(65)), "a pack takes at most 64 rules");

    // the count holds across a parent and a child
    let mut set = PackSet::with_builtin();
    set.add_yaml(&format!(
        "name: parent\nversion: \"1\"\nextends: rdy-standard\nrules:\n{}",
        many(40)
    ))
    .unwrap();
    let child: String = (0..25)
        .map(|i| rule(&format!("c{i:03}"), ANY, ANY))
        .collect();
    set.add_yaml(&format!(
        "name: child\nversion: \"1\"\nextends: parent\nrules:\n{child}"
    ))
    .unwrap();
    assert_eq!(
        set.resolve("child"),
        Err(SchemaError::Invalid(
            "a pack takes at most 64 rules".to_string()
        ))
    );

    // names: 64 characters pass, 65 fail, in one-byte and in four-byte scalars
    const NAME_TEXT: &str = "rule name must have 1 to 64 characters";
    resolve(&rule(&"n".repeat(64), ANY, ANY)).unwrap();
    resolve(&rule(&"\u{1F600}".repeat(64), ANY, ANY)).unwrap();
    assert_eq!(refused(&rule(&"n".repeat(65), ANY, ANY)), NAME_TEXT);
    assert_eq!(refused(&rule(&"\u{1F600}".repeat(65), ANY, ANY)), NAME_TEXT);
    assert_eq!(refused(&rule("", ANY, ANY)), NAME_TEXT);
    assert_eq!(refused(&rule("   ", ANY, ANY)), NAME_TEXT);
    // the 256-byte bound comes first: 257 bytes that trim to one character
    assert_eq!(
        refused(&rule(&format!("x{}", " ".repeat(256)), ANY, ANY)),
        NAME_TEXT
    );
    // spaces around a name that fits
    const SPACE_TEXT: &str = "rule name must have no leading or trailing space";
    assert_eq!(refused(&rule(" name", ANY, ANY)), SPACE_TEXT);
    assert_eq!(refused(&rule("name ", ANY, ANY)), SPACE_TEXT);

    // texts: 2000 characters pass, 2001 fail; four-byte scalars reach exactly 8000 bytes
    for field in ["message", "fix"] {
        let want = format!("rule r: {field} must have 1 to 2000 characters");
        let with = |text: &str| {
            if field == "message" {
                rule_with_texts("r", text, "f")
            } else {
                rule_with_texts("r", "m", text)
            }
        };
        resolve(&with(&"m".repeat(2000))).unwrap();
        resolve(&with(&"\u{1F600}".repeat(2000))).unwrap();
        assert_eq!(refused(&with(&"m".repeat(2001))), want);
        assert_eq!(refused(&with(&"\u{1F600}".repeat(2001))), want);
        assert_eq!(refused(&with("")), want);
        assert_eq!(refused(&with("  \n ")), want);
        // 8001 bytes that trim to one character
        assert_eq!(refused(&with(&format!("x{}", " ".repeat(8000)))), want);
    }
    // an authored text keeps its bytes
    let kept = resolve(&rule_with_texts("r", "  spaced message \n", " spaced fix ")).unwrap();
    assert_eq!(kept.rules[0].message, "  spaced message \n");
    assert_eq!(kept.rules[0].fix, " spaced fix ");
}

#[test]
fn ck1_rule_names_are_unique_across_parents() {
    let both = format!("{}{}", rule("same", ANY, ANY), rule("same", ANY, ANY));
    assert_eq!(refused(&both), "rule same is declared more than once");

    let mut set = PackSet::with_builtin();
    set.add_yaml(&format!(
        "name: parent\nversion: \"1\"\nextends: rdy-standard\nrules:\n{}",
        rule("same", ANY, ANY)
    ))
    .unwrap();
    set.add_yaml(&format!(
        "name: child\nversion: \"1\"\nextends: parent\nrules:\n{}",
        rule("same", ANY, ANY)
    ))
    .unwrap();
    assert_eq!(
        set.resolve("child"),
        Err(SchemaError::Invalid(
            "rule same is declared more than once".to_string()
        ))
    );

    // case and normal form stay distinct; the order is exact name order
    let pack = resolve(&format!(
        "{}{}{}{}{}",
        rule("beta", ANY, ANY),
        rule("Beta", ANY, ANY),
        rule("alpha", ANY, ANY),
        // U+00E9, and e with a combining acute
        rule("caf\u{e9}", ANY, ANY),
        rule("cafe\u{301}", ANY, ANY),
    ))
    .unwrap();
    assert_eq!(
        names(&pack),
        ["Beta", "alpha", "beta", "cafe\u{301}", "caf\u{e9}"]
    );
}

#[test]
fn ck1_rule_expressions_use_the_query_parser() {
    let pack = resolve(&rule("r", "type = decision", "not has fields.owner")).unwrap();
    let compiled = compile_rules(&pack).unwrap();
    // both are the parser's own AST, with the text as written
    assert_eq!(compiled[0].select, parse("type = decision", &pack).unwrap());
    assert_eq!(
        compiled[0].require,
        parse("not has fields.owner", &pack).unwrap()
    );
    assert_eq!(
        compiled[0].select.expr,
        Expr::Compare {
            field: Field::Type,
            op: Op::Eq,
            value: rdy_schema::filter::Value::Str("decision".into()),
        }
    );

    // the parser's error, nearest name included, under the rule's name and field
    for (field, sel, req) in [
        ("select", "typ = decision", ANY),
        ("require", ANY, "typ = decision"),
    ] {
        let what = parse("typ = decision", &pack).unwrap_err();
        assert_eq!(
            refused(&rule("r", sel, req)),
            format!("rule r {field}: {what}")
        );
        assert!(what.to_string().contains("did you mean type?"), "{what}");
        assert!(what.to_string().ends_with("at column 1"), "{what}");
    }

    // depth 16 passes, one past fails with the parser's words
    let deep = |n: usize| format!("{}type = decision{}", "(".repeat(n), ")".repeat(n));
    let at_limit = (0..=64)
        .rev()
        .find(|n| parse(&deep(*n), &pack).is_ok())
        .unwrap();
    resolve(&rule("r", &deep(at_limit), ANY)).unwrap();
    let what = parse(&deep(at_limit + 1), &pack).unwrap_err();
    assert_eq!(
        refused(&rule("r", ANY, &deep(at_limit + 1))),
        format!("rule r require: {what}")
    );

    // a typo in a rule's key is the pack's unknown-key path
    let e = resolve(
        "  - { name: r, selct: type = decision, require: has fields.x, message: m, fix: f }\n",
    )
    .unwrap_err();
    match e {
        SchemaError::Yaml(text) => {
            assert!(text.contains("unknown field `selct`"), "{text}");
            assert!(text.contains("`select`"), "{text}");
        }
        other => panic!("expected the pack's YAML error, got {other:?}"),
    }
    // a required key left out is the same path
    assert!(matches!(
        resolve("  - { name: r, select: type = decision, require: has fields.x, message: m }\n"),
        Err(SchemaError::Yaml(_))
    ));
}

#[test]
fn ck1_rule_predicates_cannot_read_landing_clock() {
    for (field, sel, req) in [
        ("select", "fields.decided_at = x", ANY),
        ("require", ANY, "fields.decided_at = x"),
        ("select", "fields.decided_at >= 2026", ANY),
        ("select", "has fields.decided_at", ANY),
        ("require", ANY, "has fields.decided_at"),
        ("select", "not fields.decided_at = x", ANY),
        ("require", ANY, "not has fields.decided_at"),
        ("select", "type = decision and has fields.decided_at", ANY),
        ("require", ANY, "type = decision or has fields.decided_at"),
        ("select", "out depends_on (has fields.decided_at)", ANY),
        ("require", ANY, "in decided_by (fields.decided_at = x)"),
        (
            "require",
            ANY,
            "no out depends_on (not in refines (out answers (has fields.decided_at)))",
        ),
        // the prefix is case-insensitive in the grammar; the name is not
        ("select", "FIELDS.decided_at = x", ANY),
    ] {
        let got = refused(&rule("r", sel, req));
        assert_eq!(
            got,
            format!(
                "rule r {field}: a rule cannot read fields.decided_at, because the landing sets it"
            ),
            "{sel} / {req}"
        );
        assert!(!got.contains("column"), "{got}");
    }
    // another spelling reads no field, so it is not the clock
    resolve(&rule("r", "has fields.DECIDED_AT", ANY)).unwrap();
    // other custom fields still load, and so do other clock-like names
    resolve(&rule("r", "has fields.recorded_at", "fields.owner = nick")).unwrap();
    // the grammar is unchanged: a query keeps reading the field
    let pack = resolve(&rule("r", ANY, ANY)).unwrap();
    parse("has fields.decided_at", &pack).unwrap();
    parse("fields.decided_at >= 2026", &pack).unwrap();
}

#[test]
fn ck1_empty_rules_do_not_change_a_pack_serialization() {
    let pack = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    assert!(pack.rules.is_empty());
    let json = serde_json::to_value(&pack).unwrap();
    assert!(json.get("rules").is_none(), "an empty list is omitted");
    let yaml = serde_yaml_ng::to_string(&pack).unwrap();
    assert!(!yaml.contains("rules:"), "{yaml}");

    // a nonempty list is fully represented: every member, the default severity too
    let with = resolve(
        "  - { name: r, select: type = decision, require: has fields.x, message: m, fix: f }\n",
    )
    .unwrap();
    let json = serde_json::to_value(&with).unwrap();
    assert_eq!(
        json["rules"],
        serde_json::json!([{
            "name": "r",
            "select": "type = decision",
            "require": "has fields.x",
            "severity": "advisory",
            "message": "m",
            "fix": "f",
        }])
    );
    // no AST in the serialized form, and it round-trips
    let back: Pack = serde_json::from_value(json).unwrap();
    assert_eq!(back, with);
    let one: Rule = back.rules[0].clone();
    assert_eq!(one.name, "r");
    // every member moves the digest input
    let bytes = |p: &Pack| serde_json::to_vec(p).unwrap();
    for change in [
        |r: &mut Rule| r.name.push('x'),
        |r: &mut Rule| r.select.push('x'),
        |r: &mut Rule| r.require.push('x'),
        |r: &mut Rule| r.severity = RuleSeverity::Blocking,
        |r: &mut Rule| r.message.push('x'),
        |r: &mut Rule| r.fix.push('x'),
    ] {
        let mut other = with.clone();
        change(&mut other.rules[0]);
        assert_ne!(bytes(&other), bytes(&with));
    }
}
