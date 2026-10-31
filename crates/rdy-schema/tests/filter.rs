//! The filter grammar's parser: its precedence, its tokens, its bounds and its refusals.

use rdy_schema::filter::{
    Dir, Expr, FILTER_GUIDE, Field, Filter, FilterError, LinkKind, MAX_FIELD_NAME,
    MAX_FILTER_CHARS, MAX_FILTER_DEPTH, MAX_LINK_NESTING, Needs, Op, Value, parse,
};
use rdy_schema::{Pack, PackSet};

fn standard() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

/// The standard pack with two areas, for the area checks.
fn with_areas() -> Pack {
    let mut set = PackSet::with_builtin();
    set.add_yaml(
        "name: p\nversion: \"1\"\nextends: rdy-standard\n\
         areas:\n  billing: { paths: [\"billing/**\"] }\n  search: { paths: [\"search/**\"] }\n",
    )
    .unwrap();
    set.resolve("p").unwrap()
}

fn ok(text: &str) -> Filter {
    parse(text, &standard()).unwrap_or_else(|e| panic!("{text:?} is refused: {e}"))
}

fn expr(text: &str) -> Expr {
    ok(text).expr
}

/// The whole refusal as `Display` prints it.
fn refused(text: &str) -> String {
    match parse(text, &standard()) {
        Ok(f) => panic!("{text:?} parses as {:?}", f.expr),
        Err(e) => e.to_string(),
    }
}

fn str_value(s: &str) -> Value {
    Value::Str(s.to_string())
}

fn key_is(s: &str) -> Expr {
    Expr::Compare {
        field: Field::Key,
        op: Op::Eq,
        value: str_value(s),
    }
}

fn int(value: i64) -> Value {
    Value::Int {
        value,
        text: value.to_string(),
    }
}

fn custom_n_is(value: Value) -> Expr {
    Expr::Compare {
        field: Field::Custom("n".to_string()),
        op: Op::Eq,
        value,
    }
}

#[test]
fn dq3_and_binds_before_or_and_not_binds_first() {
    assert_eq!(
        expr("key = a or key = b and not key = c"),
        Expr::Or(vec![
            key_is("a"),
            Expr::And(vec![key_is("b"), Expr::Not(Box::new(key_is("c")))]),
        ])
    );
    // A run of one operator is one flat list.
    assert_eq!(
        expr("key = a and key = b and key = c"),
        Expr::And(vec![key_is("a"), key_is("b"), key_is("c")])
    );
    assert_eq!(
        expr("key = a or key = b or key = c"),
        Expr::Or(vec![key_is("a"), key_is("b"), key_is("c")])
    );
    assert_eq!(
        expr("not not key = a"),
        Expr::Not(Box::new(Expr::Not(Box::new(key_is("a")))))
    );
}

#[test]
fn dq3_parentheses_group() {
    assert_eq!(
        expr("(key = a or key = b) and key = c"),
        Expr::And(vec![Expr::Or(vec![key_is("a"), key_is("b")]), key_is("c"),])
    );
    assert_eq!(expr("((key = a))"), key_is("a"));
    assert_eq!(
        expr("not (key = a or key = b)"),
        Expr::Not(Box::new(Expr::Or(vec![key_is("a"), key_is("b")])))
    );
    // Parentheses and ops are tokens, so no whitespace is needed around them.
    assert_eq!(
        expr("(key=a)or(key=b)"),
        Expr::Or(vec![key_is("a"), key_is("b")])
    );
}

#[test]
fn dq3_words_match_in_any_case_and_values_keep_theirs() {
    assert_eq!(
        expr("TYPE = decision AND Key = FW-X1"),
        Expr::And(vec![
            Expr::Compare {
                field: Field::Type,
                op: Op::Eq,
                value: str_value("decision"),
            },
            key_is("FW-X1"),
        ])
    );
    assert_eq!(
        expr("NOT HAS key"),
        Expr::Not(Box::new(Expr::Has(Field::Key)))
    );
    assert_eq!(
        expr("NO OUT DESCRIBES"),
        Expr::Linked {
            dir: Dir::Out,
            kind: LinkKind::Link("describes".to_string()),
            inner: None,
            none: true,
        }
    );
    assert_eq!(
        expr("FIELDS.Owner = Nick"),
        Expr::Compare {
            field: Field::Custom("Owner".to_string()),
            op: Op::Eq,
            value: str_value("Nick"),
        }
    );
}

#[test]
fn dq3_a_reserved_word_value_needs_quotes() {
    assert_eq!(refused("key = and"), "filter: and needs quotes at column 7");
    assert_eq!(expr("key = \"and\""), key_is("and"));
    for word in ["or", "not", "has", "no", "in", "out"] {
        assert_eq!(
            refused(&format!("key = {word}")),
            format!("filter: {word} needs quotes at column 7")
        );
        assert_eq!(expr(&format!("key = \"{word}\"")), key_is(word));
    }
    // The word is a word in any case, and the refusal writes it as it stands.
    assert_eq!(refused("key = AND"), "filter: AND needs quotes at column 7");
}

#[test]
fn dq3_quoted_strings_take_two_escapes() {
    assert_eq!(expr(r#"key = "a\"b""#), key_is(r#"a"b"#));
    assert_eq!(expr(r#"key = "a\\b""#), key_is(r"a\b"));
    assert_eq!(expr(r#"key = "a b (c) = d""#), key_is("a b (c) = d"));
    assert_eq!(expr(r#"key = """#), key_is(""));
    assert_eq!(
        refused(r#"key = "a\nb""#),
        r"filter: bad escape \n at column 9"
    );
    assert_eq!(
        refused(r#"key = "abc"#),
        "filter: unterminated string at column 7"
    );
    // A backslash at the end of the text leaves the string open.
    assert_eq!(
        refused(r#"key = "abc\"#),
        "filter: unterminated string at column 7"
    );
}

#[test]
fn dq3_numbers_and_booleans() {
    assert_eq!(
        expr("coverage < 40"),
        Expr::Compare {
            field: Field::Coverage,
            op: Op::Lt,
            value: int(40),
        }
    );
    assert_eq!(
        expr("suspect = true"),
        Expr::Compare {
            field: Field::Suspect,
            op: Op::Eq,
            value: Value::Bool {
                value: true,
                text: "true".to_string(),
            },
        }
    );
    assert_eq!(
        expr("tension != FALSE"),
        Expr::Compare {
            field: Field::Tension,
            op: Op::Ne,
            value: Value::Bool {
                value: false,
                text: "FALSE".to_string(),
            },
        }
    );
    assert_eq!(expr("fields.n = -3"), custom_n_is(int(-3)));
    assert_eq!(
        expr("key = 2024"),
        Expr::Compare {
            field: Field::Key,
            op: Op::Eq,
            value: int(2024),
        }
    );
    // The text stays as written, for the text comparison of id, key, path and doc.
    assert_eq!(
        expr("key = 007"),
        Expr::Compare {
            field: Field::Key,
            op: Op::Eq,
            value: Value::Int {
                value: 7,
                text: "007".to_string(),
            },
        }
    );
    // A quoted number or boolean is a string; a word with a digit in it is a word.
    assert_eq!(expr(r#"key = "2024""#), key_is("2024"));
    assert_eq!(expr(r#"key = "true""#), key_is("true"));
    assert_eq!(expr("key = 2024a"), key_is("2024a"));
    assert_eq!(expr("key = -"), key_is("-"));
}

#[test]
fn dq3_numbers_take_the_i64_range() {
    assert_eq!(
        expr("fields.n = -9223372036854775808"),
        custom_n_is(int(i64::MIN))
    );
    assert_eq!(
        expr("fields.n = 9223372036854775807"),
        custom_n_is(int(i64::MAX))
    );
    assert_eq!(
        refused("fields.n = 9223372036854775808"),
        "filter: number is outside the signed 64-bit range at column 12"
    );
    assert_eq!(
        refused("fields.n = -9223372036854775809"),
        "filter: number is outside the signed 64-bit range at column 12"
    );
    assert_eq!(
        refused("key = 99999999999999999999"),
        "filter: number is outside the signed 64-bit range at column 7"
    );
    assert_eq!(
        expr(r#"key = "99999999999999999999""#),
        key_is("99999999999999999999")
    );
}

#[test]
fn dq3_an_unknown_field_names_the_nearest() {
    assert_eq!(
        refused("typ = decision"),
        "filter: unknown field typ; did you mean type? at column 1"
    );
    assert_eq!(refused("zzz = 1"), "filter: unknown field zzz at column 1");
    // The nearest name ignores case, and the refusal writes the word as it stands.
    assert_eq!(
        refused("TYP = decision"),
        "filter: unknown field TYP; did you mean type? at column 1"
    );
    // `has` reads the same field names.
    assert_eq!(
        refused("has lifecycl"),
        "filter: unknown field lifecycl; did you mean lifecycle? at column 5"
    );
}

#[test]
fn dq3_an_unknown_link_names_the_nearest() {
    assert_eq!(
        refused("out describe"),
        "filter: unknown link describe; did you mean describes? at column 5"
    );
    assert_eq!(
        refused("in referencs"),
        "filter: unknown link referencs; did you mean references? at column 4"
    );
    assert_eq!(refused("out zzz"), "filter: unknown link zzz at column 5");
    assert_eq!(
        refused("no out zzz"),
        "filter: unknown link zzz at column 8"
    );
}

#[test]
fn dq3_link_kinds_are_the_packs_and_contains_and_references() {
    let pack = standard();
    let mut kinds: Vec<(String, LinkKind)> = vec![
        ("contains".to_string(), LinkKind::Contains),
        ("references".to_string(), LinkKind::References),
    ];
    for link in &pack.link_types {
        kinds.push((link.clone(), LinkKind::Link(link.clone())));
    }
    assert!(kinds.len() > 2, "the standard pack has link kinds");
    let inner = Box::new(Expr::Compare {
        field: Field::Type,
        op: Op::Eq,
        value: str_value("claim"),
    });
    for (word, kind) in kinds {
        for (dir_word, dir) in [("out", Dir::Out), ("in", Dir::In)] {
            let linked = |inner: Option<Box<Expr>>, none: bool| Expr::Linked {
                dir,
                kind: kind.clone(),
                inner,
                none,
            };
            assert_eq!(expr(&format!("{dir_word} {word}")), linked(None, false));
            assert_eq!(
                expr(&format!("{dir_word} {word} (type = claim)")),
                linked(Some(inner.clone()), false)
            );
            assert_eq!(expr(&format!("no {dir_word} {word}")), linked(None, true));
            assert_eq!(
                expr(&format!("no {dir_word} {word} (type = claim)")),
                linked(Some(inner.clone()), true)
            );
            // A link kind keeps the pack's spelling in any case.
            assert_eq!(
                expr(&format!("{dir_word} {}", word.to_ascii_uppercase())),
                linked(None, false)
            );
        }
    }
}

#[test]
fn dq3_values_of_closed_fields_are_checked() {
    assert_eq!(
        refused("type = decison"),
        "filter: no block type decison; did you mean decision? at column 8"
    );
    assert_eq!(
        refused("type = zzzzzzzzz"),
        "filter: no block type zzzzzzzzz at column 8"
    );
    assert_eq!(refused("area = x"), "filter: no area x at column 8");
    assert_eq!(
        parse("area = billin", &with_areas())
            .unwrap_err()
            .to_string(),
        "filter: no area billin; did you mean billing? at column 8"
    );
    assert_eq!(
        parse("area = zzzzzzzzz", &with_areas())
            .unwrap_err()
            .to_string(),
        "filter: no area zzzzzzzzz at column 8"
    );
    assert!(parse("area = billing", &with_areas()).is_ok());
    assert_eq!(
        refused("trait = loud"),
        "filter: loud is not a trait at column 9"
    );
    for name in [
        "normative",
        "informative",
        "defines",
        "describes",
        "asset",
        "structural",
        "presentational",
    ] {
        ok(&format!("trait = {name}"));
    }
    assert_eq!(
        refused("lifecycle = done"),
        "filter: done is not draft, stable or deprecated at column 13"
    );
    for name in ["draft", "stable", "deprecated"] {
        ok(&format!("lifecycle = {name}"));
    }
    assert_eq!(
        refused("status = alive"),
        "filter: alive is not a decision status at column 10"
    );
    for name in [
        "proposed",
        "rejected",
        "live",
        "partially_superseded",
        "superseded",
        "revoked",
    ] {
        ok(&format!("status = {name}"));
    }
    assert_eq!(
        refused("suspect = 1"),
        "filter: suspect takes true or false at column 11"
    );
    assert_eq!(
        refused(r#"tension = "true""#),
        "filter: tension takes true or false at column 11"
    );
    assert_eq!(
        refused("coverage = 101"),
        "filter: coverage takes a number from 0 to 100 at column 12"
    );
    assert_eq!(
        refused("coverage = -1"),
        "filter: coverage takes a number from 0 to 100 at column 12"
    );
    assert_eq!(
        refused("coverage = high"),
        "filter: coverage takes a number from 0 to 100 at column 12"
    );
    assert_eq!(
        refused(r#"coverage = "40""#),
        "filter: coverage takes a number from 0 to 100 at column 12"
    );
    ok("coverage = 0");
    ok("coverage = 100");
    // A value of a closed field keeps its case, so a wrong case is a wrong value.
    assert_eq!(
        refused("type = Decision"),
        "filter: no block type Decision; did you mean decision? at column 8"
    );
    // id, key, path, doc and fields.<name> take any value, and no pack check applies.
    ok("id = c-AAAAA");
    ok("path = a/b");
    ok("doc = true");
    ok("fields.anything = x");
}

#[test]
fn dq3_an_op_a_field_does_not_take_is_refused() {
    assert_eq!(
        refused("type < x"),
        "filter: < does not apply to type at column 6"
    );
    assert_eq!(
        refused("suspect ^= t"),
        "filter: ^= does not apply to suspect at column 9"
    );
    assert_eq!(
        refused("status > live"),
        "filter: > does not apply to status at column 8"
    );
    assert_eq!(
        refused("coverage ^= 4"),
        "filter: ^= does not apply to coverage at column 10"
    );
    for (field, value) in [("id", "x"), ("key", "x"), ("path", "x")] {
        for op in ["=", "!=", "^="] {
            ok(&format!("{field} {op} {value}"));
        }
        for op in ["<", "<=", ">", ">="] {
            let column = field.len() + 2;
            assert_eq!(
                refused(&format!("{field} {op} {value}")),
                format!("filter: {op} does not apply to {field} at column {column}")
            );
        }
    }
    for (field, value) in [
        ("type", "claim"),
        ("trait", "asset"),
        ("doc", "x"),
        ("lifecycle", "draft"),
        ("status", "live"),
        ("suspect", "true"),
        ("tension", "true"),
    ] {
        for op in ["=", "!="] {
            ok(&format!("{field} {op} {value}"));
        }
        for op in ["<", "<=", ">", ">=", "^="] {
            let column = field.len() + 2;
            assert_eq!(
                refused(&format!("{field} {op} {value}")),
                format!("filter: {op} does not apply to {field} at column {column}")
            );
        }
    }
    for op in ["=", "!=", "<", "<=", ">", ">="] {
        ok(&format!("coverage {op} 40"));
    }
    // fields.<name> takes each op here: the block's value decides whether a test holds.
    for op in ["=", "!=", "<", "<=", ">", ">=", "^="] {
        ok(&format!("fields.n {op} 4"));
    }
}

#[test]
fn dq3_has_takes_only_its_fields() {
    assert_eq!(expr("has key"), Expr::Has(Field::Key));
    assert_eq!(
        expr("has fields.n"),
        Expr::Has(Field::Custom("n".to_string()))
    );
    for field in ["path", "doc", "area", "lifecycle", "status", "coverage"] {
        ok(&format!("has {field}"));
    }
    assert_eq!(
        refused("has type"),
        "filter: has does not apply to type at column 5"
    );
    for field in ["id", "trait", "suspect", "tension"] {
        assert_eq!(
            refused(&format!("has {field}")),
            format!("filter: has does not apply to {field} at column 5")
        );
    }
}

#[test]
fn dq3_a_bad_custom_field_name_is_refused() {
    let text = "filter: fields.<name> takes a name of 1 to 64 characters from -, _, letters and digits at column 1";
    assert_eq!(refused("fields.a$b = 1"), text);
    assert_eq!(refused(&format!("fields.{} = 1", "a".repeat(65))), text);
    assert_eq!(refused("fields. = 1"), text);
    assert_eq!(
        refused("has fields.a$b").replace("column 5", "column 1"),
        text
    );
    ok("fields.A-z_09 = 1");
}

#[test]
fn dq3_each_bound_at_the_limit_passes_and_past_it_fails() {
    assert_eq!(MAX_FILTER_CHARS, 2_000);
    assert_eq!(MAX_FILTER_DEPTH, 16);
    assert_eq!(MAX_LINK_NESTING, 3);
    assert_eq!(MAX_FIELD_NAME, 64);

    // Length.
    let long = |chars: usize| format!("key = \"{}\"", "x".repeat(chars - 8));
    assert_eq!(long(2_000).chars().count(), 2_000);
    ok(&long(2_000));
    assert_eq!(
        refused(&long(2_001)),
        "filter: the filter is longer than 2000 characters at column 2001"
    );

    // Depth, through parentheses, through not and through a link's inner expression.
    let parens = |n: usize| format!("{}key = a{}", "(".repeat(n), ")".repeat(n));
    ok(&parens(16));
    assert_eq!(
        refused(&parens(17)),
        "filter: nesting deeper than 16 at column 17"
    );
    let nots = |n: usize| format!("{}key = a", "not ".repeat(n));
    ok(&nots(16));
    assert_eq!(
        refused(&nots(17)),
        "filter: nesting deeper than 16 at column 65"
    );
    let mixed = format!("out contains ({}key = a{})", "(".repeat(15), ")".repeat(15));
    ok(&mixed);
    let past = format!("out contains ({}key = a{})", "(".repeat(16), ")".repeat(16));
    assert_eq!(
        refused(&past),
        "filter: nesting deeper than 16 at column 30"
    );
    let inner_at_17 = format!("{}out contains (key = a){}", "(".repeat(16), ")".repeat(16));
    assert_eq!(
        refused(&inner_at_17),
        "filter: nesting deeper than 16 at column 30"
    );

    // Link tests nested in link tests. Each test without nesting counts on its own.
    let links = |n: usize| {
        format!(
            "{}out contains{}",
            "out contains (".repeat(n - 1),
            ")".repeat(n - 1)
        )
    };
    ok(&links(3));
    assert_eq!(
        refused(&links(4)),
        "filter: more than 3 nested link tests at column 43"
    );
    let no_links = "no out contains (no in contains (no out contains (no in contains)))";
    assert_eq!(
        refused(no_links),
        "filter: more than 3 nested link tests at column 51"
    );
    ok("out contains and out contains and out contains and out contains");
    ok("out contains (out contains) and out contains (out contains (out contains))");

    // A custom field's name.
    ok(&format!("fields.{} = 1", "a".repeat(64)));
    assert!(
        refused(&format!("fields.{} = 1", "a".repeat(65)))
            .starts_with("filter: fields.<name> takes a name of 1 to 64 characters")
    );
}

#[test]
fn dq3_nesting_past_16_is_refused_before_recursion() {
    // The worker's stack holds 17 levels and not 2,000, so a parser that recursed first would
    // overflow it and abort this test binary.
    let refusals = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let pack = standard();
            [
                "(".repeat(2_000),
                "not ".repeat(500),
                "out contains (".repeat(142),
                "(not ".repeat(400),
            ]
            .map(|text| parse(&text, &pack).unwrap_err())
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(
        refusals[0],
        FilterError {
            column: 17,
            what: "nesting deeper than 16".to_string(),
        }
    );
    assert_eq!(refusals[1].column, 65);
    assert_eq!(refusals[1].what, "nesting deeper than 16");
    assert_eq!(refusals[2].column, 43);
    assert_eq!(refusals[2].what, "more than 3 nested link tests");
    assert_eq!(refusals[3].what, "nesting deeper than 16");
}

#[test]
fn dq3_columns_count_characters() {
    assert_eq!(
        refused(r#"key = "éé" zzz"#),
        "filter: unexpected zzz at column 12"
    );
    assert_eq!(
        refused("ééééé = 1"),
        "filter: unknown field ééééé at column 1"
    );
    // A 4-byte character and a tab count as one each.
    assert_eq!(
        refused("key = \"🦀\"\tzzz"),
        "filter: unexpected zzz at column 11"
    );
    // The length bound counts characters too.
    let two_byte = format!("key = \"{}\"", "é".repeat(1_992));
    assert_eq!(two_byte.chars().count(), 2_000);
    assert!(two_byte.len() > 2_000);
    ok(&two_byte);
}

#[test]
fn dq3_an_empty_filter_expects_a_test() {
    assert_eq!(refused(""), "filter: expected a test at column 1");
    assert_eq!(refused("   "), "filter: expected a test at column 1");
    assert_eq!(refused("\t\n "), "filter: expected a test at column 1");
}

#[test]
fn dq3_needs_names_the_sets_a_filter_reads() {
    assert_eq!(
        ok("suspect = true or out describes (coverage < 40)").needs(),
        Needs {
            suspect: true,
            tension: false,
            coverage: true,
        }
    );
    assert_eq!(ok("type = claim").needs(), Needs::default());
    assert_eq!(
        ok("has coverage").needs(),
        Needs {
            coverage: true,
            ..Needs::default()
        }
    );
    assert_eq!(
        ok("not (key = a and no in implements (tension = false))").needs(),
        Needs {
            tension: true,
            ..Needs::default()
        }
    );
    // A custom field of one of those names reads no supplied set.
    assert_eq!(ok("fields.coverage = 1").needs(), Needs::default());
}

#[test]
fn dq3_the_error_text_is_exact() {
    let long = "x".repeat(50);
    let cut = "x".repeat(40);
    let cases: Vec<(String, String)> = vec![
        ("".into(), "expected a test at column 1".into()),
        ("key = a and".into(), "expected a test at column 12".into()),
        ("key = a and ) ".into(), "expected a test at column 13".into()),
        ("type".into(), "expected an operator at column 5".into()),
        ("type claim".into(), "expected an operator at column 6".into()),
        ("type =".into(), "expected a value at column 7".into()),
        ("type = )".into(), "expected a value at column 8".into()),
        ("out".into(), "expected a link kind at column 4".into()),
        ("in (key = a)".into(), "expected a link kind at column 4".into()),
        ("(type = claim".into(), "expected \")\" at column 14".into()),
        ("(type = claim x".into(), "expected \")\" at column 15".into()),
        ("type = claim)".into(), "unexpected ) at column 13".into()),
        ("type = claim x".into(), "unexpected x at column 14".into()),
        (format!("type = claim {long}"), format!("unexpected {cut} at column 14")),
        ("key ! a".into(), "unexpected ! at column 5".into()),
        ("key = a ^ b".into(), "unexpected ^ at column 9".into()),
        ("key = \"a".into(), "unterminated string at column 7".into()),
        ("key = \"a\\n\"".into(), "bad escape \\n at column 9".into()),
        ("key = or".into(), "or needs quotes at column 7".into()),
        ("typ = x".into(), "unknown field typ; did you mean type? at column 1".into()),
        ("zzz = 1".into(), "unknown field zzz at column 1".into()),
        (format!("{long} = 1"), format!("unknown field {cut} at column 1")),
        ("out describe".into(), "unknown link describe; did you mean describes? at column 5".into()),
        ("out zzz".into(), "unknown link zzz at column 5".into()),
        ("type = decison".into(), "no block type decison; did you mean decision? at column 8".into()),
        ("type = zzzzzzzzz".into(), "no block type zzzzzzzzz at column 8".into()),
        (format!("type = {long}"), format!("no block type {cut} at column 8")),
        ("area = x".into(), "no area x at column 8".into()),
        ("trait = loud".into(), "loud is not a trait at column 9".into()),
        ("lifecycle = done".into(), "done is not draft, stable or deprecated at column 13".into()),
        ("status = alive".into(), "alive is not a decision status at column 10".into()),
        ("type < x".into(), "< does not apply to type at column 6".into()),
        ("has type".into(), "has does not apply to type at column 5".into()),
        ("suspect = 1".into(), "suspect takes true or false at column 11".into()),
        ("coverage = high".into(), "coverage takes a number from 0 to 100 at column 12".into()),
        ("fields.a$b = 1".into(), "fields.<name> takes a name of 1 to 64 characters from -, _, letters and digits at column 1".into()),
        ("fields.n = 9223372036854775808".into(), "number is outside the signed 64-bit range at column 12".into()),
        ("x".repeat(2_001), "the filter is longer than 2000 characters at column 2001".into()),
        ("(".repeat(17), "nesting deeper than 16 at column 17".into()),
        ("out contains (out contains (out contains (out contains)))".into(), "more than 3 nested link tests at column 43".into()),
    ];
    for (text, what) in cases {
        assert_eq!(refused(&text), format!("filter: {what}"), "for {text:?}");
    }
    // The error's parts, as a reader takes them.
    let e = parse("zzz = 1", &standard()).unwrap_err();
    assert_eq!(e.column, 1);
    assert_eq!(e.what, "unknown field zzz");
    // The filter keeps its text as given.
    assert_eq!(ok("  KEY = a  ").text, "  KEY = a  ");
}

#[test]
fn dq3_the_guide_opens_and_closes_as_written() {
    assert!(FILTER_GUIDE.starts_with("A filter selects blocks. A test is <field> <op> <value>,"));
    assert!(
        FILTER_GUIDE.ends_with("Example: type = decision and status = live and no in implements.")
    );
    // A name after fields. keeps its case (TASK-956).
    assert!(FILTER_GUIDE.contains(
        "Keywords, built-in field names and link kinds are case-insensitive. A name after fields. keeps its case, and values keep their case."
    ));
    // Its example parses.
    ok("type = decision and status = live and no in implements");
}
