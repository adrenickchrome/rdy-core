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

#[test]
fn wx10_reserved_names_and_trailing_dots_stay_refused() {
    // Names are checked the same on every host, so a doc made on Unix opens on Windows.
    for bad in [
        "con", "NUL", "aux.txt", "com1", "LPT9.md", "prn", "a.", "report.", "x:y", r"a\b", "a/b",
        "C:", "a b",
    ] {
        assert!(rdy_model::tree::name_problem(bad).is_some(), "{bad}");
    }
    for ok in ["console", "com0", "com10", "lpt", "a.b", "wallets-2"] {
        assert!(rdy_model::tree::name_problem(ok).is_none(), "{ok}");
    }
}

// wck2: the module-rule structure (D-ZRXY0, D-WV7ZS), tested through `validate_rule_input` and
// `check_resolved_rules`. The public pack parser still refuses the module keys.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rdy_schema::{
    ArgValue, MAX_ARGS_BYTES, MAX_MODULE_BYTES, MAX_PACK_MODULE_BYTES, ModuleTest, RuleInput,
    RuleTestSpec, check_resolved_rules, validate_rule_input,
};
use serde_yaml_ng::{Mapping, Value};

/// The eight bytes of an empty WebAssembly module: its magic and version.
const MAGIC: &str = "AGFzbQEAAAA=";
const MAGIC_BYTES: &[u8] = b"\0asm\x01\0\0\0";

fn yaml(text: &str) -> Value {
    serde_yaml_ng::from_str(text).unwrap()
}

fn input(text: &str) -> Result<RuleInput, SchemaError> {
    validate_rule_input(&yaml(text))
}

fn input_refused(text: &str) -> String {
    match input(text) {
        Err(SchemaError::Invalid(detail)) => detail,
        other => panic!("expected an invalid rule, got {other:?}"),
    }
}

/// A module rule named `name` with `extra` YAML lines, the module given as `module`.
fn module_yaml(name: &str, module: &str, extra: &str) -> String {
    format!(
        "name: {}\nselect: type = decision\nmodule: {module}\n{extra}message: m\nfix: f\n",
        q(name)
    )
}

/// A module rule as a mapping, with `extra` keys added.
fn module_value(name: &str, module: &str, extra: Vec<(&str, Value)>) -> Value {
    let mut m = Mapping::new();
    m.insert("name".into(), name.into());
    m.insert("select".into(), "type = decision".into());
    m.insert("module".into(), module.into());
    for (k, v) in extra {
        m.insert(k.into(), v);
    }
    m.insert("message".into(), "m".into());
    m.insert("fix".into(), "f".into());
    Value::Mapping(m)
}

fn module_of(rule: &RuleInput) -> &ModuleTest {
    match &rule.test {
        RuleTestSpec::Module(m) => m,
        other => panic!("expected a module test, got {other:?}"),
    }
}

/// A checked module rule named `name` holding `bytes`.
fn module_input(name: &str, bytes: &[u8]) -> RuleInput {
    validate_rule_input(&module_value(name, &STANDARD.encode(bytes), vec![])).unwrap()
}

#[test]
fn wck2_rule_takes_exactly_one_test() {
    let filter =
        input("name: r\nselect: type = decision\nrequire: has fields.x\nmessage: m\nfix: f\n")
            .unwrap();
    assert_eq!(
        filter.test,
        RuleTestSpec::Filter {
            require: "has fields.x".to_string()
        }
    );
    assert_eq!(filter.severity, RuleSeverity::Advisory);

    let module = input(&module_yaml("r", MAGIC, "")).unwrap();
    let m = module_of(&module);
    assert_eq!(m.module, MAGIC_BYTES);
    assert_eq!(m.args, None);
    assert_eq!(m.depth, None);

    // both: refused before the module is decoded, so a bad module gives the same words
    for text in [MAGIC, "\"!!!!\""] {
        assert_eq!(
            input_refused(&module_yaml("r", text, "require: has fields.x\n")),
            "rule r takes require or module, not both"
        );
    }
    // neither
    assert_eq!(
        input_refused("name: r\nselect: type = decision\nmessage: m\nfix: f\n"),
        "rule r needs require or module"
    );
    assert_eq!(
        input_refused("name: r\nselect: type = decision\nseverity: blocking\nmessage: m\nfix: f\n"),
        "rule r needs require or module"
    );
    // the name in the detail is the rule's own
    assert_eq!(
        input_refused("name: \"two words\"\nselect: type = decision\nmessage: m\nfix: f\n"),
        "rule two words needs require or module"
    );
}

#[test]
fn wck2_module_only_keys_require_module() {
    let filter = |extra: &str| {
        format!(
            "name: r\nselect: type = decision\nrequire: has fields.x\n{extra}message: m\nfix: f\n"
        )
    };
    assert_eq!(
        input_refused(&filter("args: {}\n")),
        "rule r args applies to module rules only"
    );
    assert_eq!(
        input_refused(&filter("args: { a: 1 }\n")),
        "rule r args applies to module rules only"
    );
    assert_eq!(
        input_refused(&filter("depth: 1\n")),
        "rule r depth applies to module rules only"
    );
    // args is named first
    assert_eq!(
        input_refused(&filter("depth: 1\nargs: {}\n")),
        "rule r args applies to module rules only"
    );
    // both keys load on a module rule
    let m = input(&module_yaml("r", MAGIC, "args: { a: 1 }\ndepth: 2\n")).unwrap();
    assert_eq!(module_of(&m).depth, Some(2));
}

#[test]
fn wck2_explicit_rule_nulls_refuse() {
    let cases: Vec<(&str, String)> = vec![
        ("require", "name: r\nselect: type = decision\nrequire: null\nmessage: m\nfix: f\n".into()),
        ("require", format!("name: r\nselect: type = decision\nrequire: ~\nmodule: {MAGIC}\nmessage: m\nfix: f\n")),
        ("module", "name: r\nselect: type = decision\nmodule: null\nmessage: m\nfix: f\n".into()),
        ("module", "name: r\nselect: type = decision\nrequire: has fields.x\nmodule:\nmessage: m\nfix: f\n".into()),
        ("args", module_yaml("r", MAGIC, "args: null\n")),
        ("depth", module_yaml("r", MAGIC, "depth: ~\n")),
        // a null args on a filter rule is the null refusal
        ("args", "name: r\nselect: type = decision\nrequire: has fields.x\nargs: null\nmessage: m\nfix: f\n".into()),
    ];
    for (key, text) in &cases {
        assert_eq!(
            input_refused(text),
            format!("rule r {key}: null is not allowed"),
            "{text}"
        );
    }
    // null inside args stays allowed, in a mapping and in a list
    let m = input(&module_yaml(
        "r",
        MAGIC,
        "args: { a: null, b: [null, 1] }\n",
    ))
    .unwrap();
    let args = module_of(&m).args.clone().unwrap();
    assert_eq!(args["a"], ArgValue::Null);
    assert_eq!(
        args["b"],
        ArgValue::List(vec![ArgValue::Null, ArgValue::Int(1)])
    );
}

#[test]
fn wck2_base64_is_standard_and_bounded() {
    // spaces and YAML line breaks inside the text are ignored
    for text in [
        "AGFzbQEAAAA=",
        "\"AGFz bQEA AAA=\"",
        "|\n  AGFz\n  bQEA\n  AAA=",
        ">\n  AGFzbQ\n  EAAAA=",
        "\"AGFzbQEAAAA=\\r\\n\"",
    ] {
        let m = input(&module_yaml("r", text, "")).unwrap();
        assert_eq!(module_of(&m).module, MAGIC_BYTES, "{text}");
    }
    let not = "rule r module: not base64";
    for text in [
        // the URL-safe alphabet: standard `+/8=` written as `-_8=`
        "-_8=",
        // missing, extra and misplaced padding
        "AGFzbQEAAAA",
        "AGFzbQEAAAA==",
        "AGFz=QEAAAA=",
        "====",
        // trailing bits that are not zero
        "QR==",
        // a tab, and a character outside the alphabet
        "\"AGFz\\tbQEAAAA=\"",
        "AGFzbQEAAA\u{e9}",
        "\"AGFzbQEAAAA*\"",
    ] {
        assert_eq!(input_refused(&module_yaml("r", text, "")), not, "{text}");
    }
    assert_eq!(
        module_of(&input(&module_yaml("r", "+/8=", "")).unwrap()).module,
        [0xfb, 0xff]
    );
    // an empty text decodes to no bytes; the engine refuses the module later
    assert!(
        module_of(&input(&module_yaml("r", "\"\"", "")).unwrap())
            .module
            .is_empty()
    );

    // the decoded length is checked before decoding: a text whose length is past the limit gets
    // the size refusal although its last character is outside the alphabet
    let mut long = "A".repeat(1_398_104);
    long.pop();
    long.push('!');
    assert_eq!(
        validate_rule_input(&module_value("r", &long, vec![])),
        Err(SchemaError::Invalid(
            "rule r module is 1048578 bytes; a module takes at most 1048576".to_string()
        ))
    );
}

#[test]
fn wck2_module_bytes_at_and_past_limit() {
    assert_eq!(MAX_MODULE_BYTES, 1_048_576);
    let at = vec![7u8; MAX_MODULE_BYTES];
    assert_eq!(module_of(&module_input("r", &at)).module.len(), 1_048_576);
    for len in [
        MAX_MODULE_BYTES + 1,
        MAX_MODULE_BYTES + 2,
        MAX_MODULE_BYTES + 3,
    ] {
        let past = vec![7u8; len];
        assert_eq!(
            validate_rule_input(&module_value("r", &STANDARD.encode(&past), vec![])),
            Err(SchemaError::Invalid(format!(
                "rule r module is {len} bytes; a module takes at most 1048576"
            )))
        );
    }
}

#[test]
fn wck2_resolved_module_total_at_and_past_limit() {
    assert_eq!(MAX_PACK_MODULE_BYTES, 8_388_608);
    // a parent with filter rules, resolved; module rules follow it as a child's rules would
    let mut set = PackSet::with_builtin();
    let parent_rules: String = (0..32)
        .map(|i| rule(&format!("f{i:02}"), ANY, ANY))
        .collect();
    set.add_yaml(&format!(
        "name: parent\nversion: \"1\"\nextends: rdy-standard\nrules:\n{parent_rules}"
    ))
    .unwrap();
    let pack = set.resolve("parent").unwrap();
    let inherited: Vec<RuleInput> = pack.rules.iter().map(RuleInput::from).collect();

    // eight identical modules of 1 MiB fill the total exactly; each counts, none is merged
    let mib = module_input("m0", &vec![1u8; MAX_MODULE_BYTES]);
    let modules = |n: usize| -> Vec<RuleInput> {
        (0..n)
            .map(|i| {
                let mut r = mib.clone();
                r.name = format!("m{i}");
                r
            })
            .collect()
    };
    let mut all = inherited.clone();
    all.extend(modules(8));
    let checked = check_resolved_rules(&pack, &all).unwrap();
    assert_eq!(checked.len(), 40);
    // in exact name order, the filter rules with their parsed require, module rules with none
    assert_eq!(checked[0].input.name, "f00");
    assert_eq!(checked[0].require, Some(parse(ANY, &pack).unwrap()));
    assert_eq!(checked[39].input.name, "m7");
    assert_eq!(checked[39].require, None);
    assert_eq!(checked[39].select, parse("type = decision", &pack).unwrap());

    let mut past = all.clone();
    past.push(module_input("one more", &[0]));
    assert_eq!(
        check_resolved_rules(&pack, &past),
        Err(SchemaError::Invalid(
            "the pack's modules hold 8388609 bytes; a pack takes at most 8388608".to_string()
        ))
    );

    // the 64-rule limit counts both kinds across the parent and the child
    let small = |n: usize| -> Vec<RuleInput> {
        (0..n)
            .map(|i| module_input(&format!("s{i:02}"), MAGIC_BYTES))
            .collect()
    };
    let mut at = inherited.clone();
    at.extend(small(32));
    assert_eq!(check_resolved_rules(&pack, &at).unwrap().len(), 64);
    let mut over = inherited.clone();
    over.extend(small(33));
    assert_eq!(
        check_resolved_rules(&pack, &over),
        Err(SchemaError::Invalid(
            "a pack takes at most 64 rules".to_string()
        ))
    );

    // a name is unique across kinds; the names and texts keep their bounds
    let mut twice = inherited.clone();
    twice.push(module_input("f00", MAGIC_BYTES));
    assert_eq!(
        check_resolved_rules(&pack, &twice),
        Err(SchemaError::Invalid(
            "rule f00 is declared more than once".to_string()
        ))
    );
    assert_eq!(
        check_resolved_rules(&pack, &[module_input(&"n".repeat(65), MAGIC_BYTES)]),
        Err(SchemaError::Invalid(
            "rule name must have 1 to 64 characters".to_string()
        ))
    );
    let mut long_message = module_input("r", MAGIC_BYTES);
    long_message.message = "m".repeat(2001);
    assert_eq!(
        check_resolved_rules(&pack, &[long_message]),
        Err(SchemaError::Invalid(
            "rule r: message must have 1 to 2000 characters".to_string()
        ))
    );

    // a module rule's select uses the query parser and cannot read the landing clock
    let mut typo = module_input("r", MAGIC_BYTES);
    typo.select = "typ = decision".to_string();
    let what = parse("typ = decision", &pack).unwrap_err();
    assert_eq!(
        check_resolved_rules(&pack, &[typo]),
        Err(SchemaError::Invalid(format!("rule r select: {what}")))
    );
    let mut clock = module_input("r", MAGIC_BYTES);
    clock.select = "has fields.decided_at".to_string();
    assert_eq!(
        check_resolved_rules(&pack, &[clock]),
        Err(SchemaError::Invalid(
            "rule r select: a rule cannot read fields.decided_at, because the landing sets it"
                .to_string()
        ))
    );
}

#[test]
fn wck2_args_bytes_at_and_past_limit() {
    assert_eq!(MAX_ARGS_BYTES, 8_192);
    let refused = "rule r args: at most 8192 bytes";
    // `{"k":"<s>"}` is 8 bytes around the string's escaped form
    let one = |s: String| {
        let mut m = Mapping::new();
        m.insert("k".into(), Value::String(s));
        module_value("r", MAGIC, vec![("args", Value::Mapping(m))])
    };
    let run = |v: Value| validate_rule_input(&v);
    // plain, two-byte escapes, six-byte control escapes, and two-byte UTF-8
    for (unit, width) in [("a", 1), ("\"", 2), ("\\", 2), ("\u{1}", 6), ("\u{e9}", 2)] {
        let n = 8_184 / width;
        assert_eq!(n * width, 8_184, "{unit:?}");
        let at = run(one(unit.repeat(n))).unwrap();
        assert_eq!(
            module_of(&at).args.as_ref().unwrap()["k"],
            ArgValue::Str(unit.repeat(n))
        );
        assert_eq!(
            run(one(unit.repeat(n + 1))),
            Err(SchemaError::Invalid(refused.to_string())),
            "{unit:?}"
        );
    }
    // every JSON form counts: `{"a":[1,true,null,false,{}],"b":-12}` is 36 bytes
    let mixed = "{ a: [1, true, null, false, {}], b: -12, s: \"";
    let fill = |n: usize| format!("{mixed}{}\" }}", "x".repeat(n));
    // with `,"s":""` the fixed part is 43 bytes
    run(module_value(
        "r",
        MAGIC,
        vec![("args", yaml(&fill(8_192 - 43)))],
    ))
    .unwrap();
    assert_eq!(
        run(module_value(
            "r",
            MAGIC,
            vec![("args", yaml(&fill(8_192 - 42)))]
        )),
        Err(SchemaError::Invalid(refused.to_string()))
    );
    // a long text is refused by its length alone
    assert_eq!(
        run(one("x".repeat(1_000_000))),
        Err(SchemaError::Invalid(refused.to_string()))
    );
    // an empty mapping is two bytes
    let empty = run(module_value("r", MAGIC, vec![("args", yaml("{}"))])).unwrap();
    assert_eq!(module_of(&empty).args, Some(Default::default()));
}

#[test]
fn wck2_args_types_and_integer_edges() {
    let types = "rule r args: only strings, integers, booleans, null, lists and mappings";
    let args =
        |text: &str| validate_rule_input(&module_value("r", MAGIC, vec![("args", yaml(text))]));
    let int = |text: &str| {
        let r = args(&format!("{{ n: {text} }}")).unwrap();
        module_of(&r).args.as_ref().unwrap()["n"].clone()
    };
    assert_eq!(
        int("9007199254740992"),
        ArgValue::Int(9_007_199_254_740_992)
    );
    assert_eq!(
        int("-9007199254740992"),
        ArgValue::Int(-9_007_199_254_740_992)
    );
    assert_eq!(int("0"), ArgValue::Int(0));
    assert_eq!(int("-1"), ArgValue::Int(-1));
    for bad in [
        "{ n: 9007199254740993 }",
        "{ n: -9007199254740993 }",
        "{ n: 18446744073709551615 }",
        "{ n: 1.5 }",
        "{ n: 1.0 }",
        "{ n: 1e3 }",
        "{ n: .nan }",
        "{ n: [1, 2.5] }",
        "{ n: { m: -0.5 } }",
        // keys that are not strings, at the top and nested
        "{ 1: x }",
        "{ true: x }",
        "{ null: x }",
        "{ n: { 2: x } }",
        // tagged values
        "{ n: !custom 1 }",
        "!custom { n: 1 }",
        // args is a mapping
        "[1, 2]",
        "text",
        "7",
        "true",
    ] {
        assert_eq!(
            args(bad),
            Err(SchemaError::Invalid(types.to_string())),
            "{bad}"
        );
    }
    // every allowed form, keys in byte order
    let r = args("{ s: text, b: false, z: null, l: [x, 1, [true]], m: { y: 2, B: c } }").unwrap();
    let got = module_of(&r).args.clone().unwrap();
    assert_eq!(
        got.keys().map(String::as_str).collect::<Vec<_>>(),
        ["b", "l", "m", "s", "z"]
    );
    assert_eq!(got["s"], ArgValue::Str("text".into()));
    assert_eq!(got["b"], ArgValue::Bool(false));
    assert_eq!(got["z"], ArgValue::Null);
    assert_eq!(
        got["l"],
        ArgValue::List(vec![
            ArgValue::Str("x".into()),
            ArgValue::Int(1),
            ArgValue::List(vec![ArgValue::Bool(true)])
        ])
    );
    let ArgValue::Map(m) = &got["m"] else {
        panic!("expected a mapping")
    };
    assert_eq!(m.keys().map(String::as_str).collect::<Vec<_>>(), ["B", "y"]);
    // a quoted number is a string, kept as written
    assert_eq!(
        int("\"9007199254740993\""),
        ArgValue::Str("9007199254740993".into())
    );
}

#[test]
fn wck2_depth_edges_and_default() {
    let depth = |text: &str| input(&module_yaml("r", MAGIC, &format!("depth: {text}\n")));
    for d in 0..=3u8 {
        assert_eq!(module_of(&depth(&d.to_string()).unwrap()).depth, Some(d));
    }
    let absent = input(&module_yaml("r", MAGIC, "")).unwrap();
    assert_eq!(module_of(&absent).depth, None);
    assert_eq!(module_of(&absent).depth(), 1);
    assert_eq!(module_of(&depth("0").unwrap()).depth(), 0);
    assert_eq!(module_of(&depth("3").unwrap()).depth(), 3);
    for bad in [
        "-1",
        "4",
        "256",
        "1.5",
        "1.0",
        "\"1\"",
        "true",
        "[1]",
        "{}",
        "!custom 1",
    ] {
        assert_eq!(
            depth(bad),
            Err(SchemaError::Invalid(
                "rule r depth must be 0 to 3".to_string()
            )),
            "{bad}"
        );
    }
    assert_eq!(
        depth("null"),
        Err(SchemaError::Invalid(
            "rule r depth: null is not allowed".to_string()
        ))
    );
}

#[test]
fn wck2_new_rule_keys_are_closed_and_suggest_near_keys() {
    let yaml_error = |text: &str| match input(text) {
        Err(SchemaError::Yaml(t)) => t,
        other => panic!("expected the rule's key refusal, got {other:?}"),
    };
    for (typo, near) in [
        ("modul", Some("module")),
        ("modules", Some("module")),
        ("arg", Some("args")),
        ("dept", Some("depth")),
        ("depths", Some("depth")),
        ("requires", Some("require")),
        ("selct", Some("select")),
        ("zzzzzz", None),
    ] {
        let text = yaml_error(&module_yaml("r", MAGIC, &format!("{typo}: 1\n")));
        assert!(
            text.starts_with(&format!("unknown field `{typo}`")),
            "{text}"
        );
        for key in ["`module`", "`args`", "`depth`", "`require`"] {
            assert!(text.contains(key), "{text}");
        }
        match near {
            Some(near) => assert!(text.ends_with(&format!("; did you mean {near}?")), "{text}"),
            None => assert!(!text.contains("did you mean"), "{text}"),
        }
    }
    // a rule is a mapping with its required keys, as the pack's parser has it
    assert!(matches!(input("[1]"), Err(SchemaError::Yaml(_))));
    assert!(matches!(
        input(&module_yaml("r", MAGIC, "").replace("fix: f\n", "")),
        Err(SchemaError::Yaml(_))
    ));
    assert!(matches!(
        input(&module_yaml("r", "7", "")),
        Err(SchemaError::Yaml(_))
    ));

    // the public pack parser keeps refusing the new keys as unknown
    for key in ["module: AGFzbQEAAAA=", "args: {}", "depth: 1"] {
        let e = resolve(&format!(
            "  - {{ name: r, select: type = decision, require: has fields.x, {key}, message: m, fix: f }}\n"
        ))
        .unwrap_err();
        let SchemaError::Yaml(text) = e else {
            panic!("expected the pack's YAML error, got {e:?}")
        };
        let name = key.split(':').next().unwrap();
        assert!(text.contains(&format!("unknown field `{name}`")), "{text}");
    }
}

#[test]
fn wck2_unset_module_keys_preserve_pack_bytes_and_digest() {
    // the built-in pack: no rules, so no `rules` key either way
    let builtin = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    let inputs: Vec<RuleInput> = builtin.rules.iter().map(RuleInput::from).collect();
    assert!(inputs.is_empty());
    let json = serde_json::to_vec(&builtin).unwrap();
    assert!(!String::from_utf8_lossy(&json).contains("\"rules\""));

    // filter rules write the same bytes through the new structure, in JSON (the digest input)
    // and in YAML, the default severity included
    let pack = resolve(
        "  - { name: a, select: type = decision, require: has fields.x, message: m, fix: f }\n  - { name: b, select: type = decision, require: has fields.y, severity: blocking, message: \"m2\", fix: \"f2\" }\n",
    )
    .unwrap();
    for rule in &pack.rules {
        let new = RuleInput::from(rule);
        assert_eq!(
            serde_json::to_vec(&new).unwrap(),
            serde_json::to_vec(rule).unwrap()
        );
        assert_eq!(
            serde_yaml_ng::to_string(&new).unwrap(),
            serde_yaml_ng::to_string(rule).unwrap()
        );
        // and the structure reads them back as they were
        assert_eq!(
            validate_rule_input(&serde_yaml_ng::to_value(rule).unwrap()).unwrap(),
            new
        );
    }
    // the pack's JSON, the digest input, holds the same bytes for its rules either way
    let inputs: Vec<RuleInput> = pack.rules.iter().map(RuleInput::from).collect();
    let old_rules = serde_json::to_string(&pack.rules).unwrap();
    assert_eq!(serde_json::to_string(&inputs).unwrap(), old_rules);
    let pack_json = serde_json::to_string(&pack).unwrap();
    assert_eq!(
        pack_json.matches(&format!("\"rules\":{old_rules}")).count(),
        1
    );

    // a module rule writes only the keys it sets, in the rule's key order, the module as
    // standard base64 with no spaces
    let keys = |r: &RuleInput| -> Vec<String> {
        serde_json::to_value(r)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    };
    let plain = input(&module_yaml("r", "\"AGFz bQEA AAA=\"", "")).unwrap();
    let json = serde_json::to_string(&plain).unwrap();
    assert_eq!(
        json,
        "{\"name\":\"r\",\"select\":\"type = decision\",\"module\":\"AGFzbQEAAAA=\",\"severity\":\"advisory\",\"message\":\"m\",\"fix\":\"f\"}"
    );
    // explicit defaults stay written; omitted ones stay out
    let explicit = input(&module_yaml("r", MAGIC, "args: {}\ndepth: 1\n")).unwrap();
    let json = serde_json::to_string(&explicit).unwrap();
    assert!(
        json.contains(",\"module\":\"AGFzbQEAAAA=\",\"args\":{},\"depth\":1,\"severity\""),
        "{json}"
    );
    let only_depth = input(&module_yaml("r", MAGIC, "depth: 1\n")).unwrap();
    assert!(!keys(&only_depth).contains(&"args".to_string()));
    let only_args = input(&module_yaml("r", MAGIC, "args: { b: [1, null], a: x }\n")).unwrap();
    assert!(!keys(&only_args).contains(&"depth".to_string()));
    assert!(
        serde_json::to_string(&only_args)
            .unwrap()
            .contains("\"args\":{\"a\":\"x\",\"b\":[1,null]}")
    );
    // each round-trips through the structure
    for r in [&plain, &explicit, &only_depth, &only_args] {
        let back = validate_rule_input(&serde_yaml_ng::to_value(r).unwrap()).unwrap();
        assert_eq!(&back, r);
        assert_eq!(
            serde_yaml_ng::to_string(&back).unwrap(),
            serde_yaml_ng::to_string(r).unwrap()
        );
    }
    // the module, args and depth each move the bytes
    let bytes = |r: &RuleInput| serde_json::to_vec(r).unwrap();
    let other = input(&module_yaml("r", "AGFzbQEAAAE=", "")).unwrap();
    assert_ne!(bytes(&other), bytes(&plain));
    assert_ne!(bytes(&only_depth), bytes(&plain));
    assert_ne!(bytes(&explicit), bytes(&only_depth));
    let deeper = input(&module_yaml("r", MAGIC, "depth: 2\n")).unwrap();
    assert_ne!(bytes(&deeper), bytes(&only_depth));
}
