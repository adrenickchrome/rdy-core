//! cp1 (p-ZDH29, p-SG9AJ, O18, O22): `reaffirmed` and `reflects_at` are engine fields. No pack
//! defines either or marks it as tracking; both validators accept them on every type and check
//! their stored shapes, as for `key_auto`.
use rdy_model::engine_fields::{
    self, Against, MAX_ENGINE_ENTRIES, MAX_REASON_CHARS, REAFFIRMED, REFLECTS_AT, Reaffirmation,
    ReflectsAt,
};
use rdy_model::{Block, BlockId, FieldValue, Hash};
use rdy_schema::*;
use std::collections::BTreeMap;

fn pack() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn block(bid: &str, ty: &str, parent: Option<&str>) -> Block {
    Block {
        id: id(bid),
        block_type: ty.into(),
        parent: parent.map(id),
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
        .insert("title".into(), FieldValue::Str("Errors".into()));
    s
}

fn claim() -> Block {
    let mut c = block("c-AAAAA", "claim", Some("s-SECT1"));
    c.body = "Errors are listed.".into();
    c
}

fn str_v(s: &str) -> FieldValue {
    FieldValue::Str(s.into())
}

fn map(entries: &[(&str, FieldValue)]) -> FieldValue {
    FieldValue::Map(
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    )
}

fn list(items: &[&str]) -> FieldValue {
    FieldValue::List(items.iter().map(|s| str_v(s)).collect())
}

fn good_reaffirmed() -> FieldValue {
    engine_fields::encode_reaffirmed(&BTreeMap::from([
        (
            id("c-AAAAA"),
            Reaffirmation {
                against: Against::Retiring(vec![id("D-0AAAA"), id("D-W2HXR")]),
                reason: "Still true; see {{D-W2HXR|@not-a-ref}}.".into(),
            },
        ),
        (
            id("t-MEDGR"),
            Reaffirmation {
                against: Against::Deleted,
                reason: "Kept.".into(),
            },
        ),
        (
            id("c-2CCCC"),
            Reaffirmation {
                against: Against::Retiring(vec![]),
                reason: "Changed target.".into(),
            },
        ),
    ]))
}

fn good_reflects_at() -> FieldValue {
    engine_fields::encode_reflects_at(&ReflectsAt {
        files: Hash::of(b"files"),
        commits: BTreeMap::from([
            ("core".to_string(), Some("a".repeat(40))),
            ("hub".to_string(), None),
            ("web".to_string(), Some("0".repeat(64))),
        ]),
    })
}

/// One entry of `reaffirmed` with the given value.
fn one_target(entry: FieldValue) -> FieldValue {
    map(&[("c-AAAAA", entry)])
}

fn entry(against: FieldValue, reason: &str) -> FieldValue {
    map(&[("against", against), ("reason", str_v(reason))])
}

fn reflects(files: FieldValue, commits: FieldValue) -> FieldValue {
    map(&[("commits", commits), ("files", files)])
}

fn engine_issues(issues: &[Issue]) -> Vec<(&'static str, String, String)> {
    issues
        .iter()
        .filter(|i| i.message.contains(REAFFIRMED) || i.message.contains(REFLECTS_AT))
        .map(|i| (i.code, i.message.clone(), i.fix.clone()))
        .collect()
}

fn bad(field: &str) -> Vec<(&'static str, String, String)> {
    vec![(
        "E_FIELD_KIND",
        format!("field {field:?} has an invalid engine value"),
        "restore the block through rdy's write tools".to_string(),
    )]
}

/// Both validators' issues for `b` (under `section()` when it has a parent).
fn both(b: &Block) -> [Vec<Issue>; 2] {
    let p = section();
    let parent = b.parent.as_ref().map(|_| &p);
    [
        validate_block(&pack(), b, parent),
        validate_read_block(&pack(), b, parent),
    ]
}

#[test]
fn cp1_pack_cannot_own_engine_fields() {
    for field in [REAFFIRMED, REFLECTS_AT] {
        let mut set = PackSet::with_builtin();
        let name = set
            .add_yaml(&format!(
                "name: team\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  risk:\n    prefix: rk\n    traits: [normative]\n    fields:\n      {field}: {{ kind: map }}\n"
            ))
            .unwrap();
        let e = set.resolve(&name).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!("invalid pack: type risk: field {field} is reserved for the engine"),
        );
        // on an existing type too
        let mut set = PackSet::with_builtin();
        let name = set
            .add_yaml(&format!(
                "name: team\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  decision:\n    prefix: D\n    traits: [normative]\n    fields:\n      {field}: {{ kind: map }}\n    tracking: [{field}]\n"
            ))
            .unwrap();
        let e = set.resolve(&name).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!("invalid pack: type decision: field {field} is reserved for the engine"),
        );
        // a tracking declaration alone names no field of the type, and is refused as reserved
        let mut set = PackSet::with_builtin();
        let name = set
            .add_yaml(&format!(
                "name: team\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  decision:\n    prefix: D\n    traits: [normative]\n    tracking: [{field}]\n"
            ))
            .unwrap();
        let e = set.resolve(&name).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!(
                "invalid pack: type decision: tracking field {field} is derived or reserved by the engine"
            ),
        );
    }
    // both are store-only: a working copy never shows them
    assert_eq!(STORE_ONLY_FIELDS, ["key_auto", REAFFIRMED, REFLECTS_AT]);
}

#[test]
fn cp1_read_validation_checks_metadata_shapes() {
    // accepted on every type, by both validators, with no issue of their own
    let mut doc = block("d-AAAAA", "doc", None);
    for (k, v) in [
        ("name", "errors"),
        ("title", "Errors"),
        ("doc_type", "overview"),
    ] {
        doc.fields.insert(k.into(), str_v(v));
    }
    let mut decision = block("D-AAAAA", "decision", Some("s-SECT1"));
    decision
        .fields
        .insert("statement".into(), str_v("Use rdy."));
    let folder = block("fd-AAAAA", "folder", None);
    for plain in [claim(), section(), doc, decision, folder] {
        let mut meta = plain.clone();
        meta.fields.insert(REAFFIRMED.into(), good_reaffirmed());
        meta.fields.insert(REFLECTS_AT.into(), good_reflects_at());
        let [w0, r0] = both(&plain);
        let [w1, r1] = both(&meta);
        assert_eq!(w1, w0, "{}", plain.block_type);
        assert_eq!(r1, r0, "{}", plain.block_type);
    }

    let reason_cap = "é".repeat(MAX_REASON_CHARS);
    let ok_reaffirmed = [
        entry(list(&["D-W2HXR"]), "x"),
        entry(str_v("deleted"), "Gone."),
        entry(list(&[]), "Changed."),
        entry(list(&["D-0AAAA"]), &reason_cap),
    ];
    for v in ok_reaffirmed {
        let mut b = claim();
        b.fields.insert(REAFFIRMED.into(), one_target(v.clone()));
        for issues in both(&b) {
            assert!(engine_issues(&issues).is_empty(), "{v:?}: {issues:?}");
        }
    }

    let over_reason = "a".repeat(MAX_REASON_CHARS + 1);
    let many_against: Vec<String> = (0..=MAX_ENGINE_ENTRIES)
        .map(|i| format!("D-{}", five(i)))
        .collect();
    let many_against: Vec<&str> = many_against.iter().map(String::as_str).collect();
    let bad_reaffirmed = [
        // not a map, or a malformed target id
        ("not a map", str_v("x")),
        ("bad target id", map(&[("c-AAAA", entry(list(&[]), "x"))])),
        ("target is text", map(&[("hello", entry(list(&[]), "x"))])),
        ("entry not a map", one_target(str_v("x"))),
        // exactly against and reason
        (
            "extra key",
            one_target(map(&[
                ("against", list(&[])),
                ("reason", str_v("x")),
                ("by", str_v("y")),
            ])),
        ),
        ("missing reason", one_target(map(&[("against", list(&[]))]))),
        (
            "missing against",
            one_target(map(&[("reason", str_v("x"))])),
        ),
        // against: a sorted, distinct list of decision ids, or `deleted`
        (
            "unsorted",
            one_target(entry(list(&["D-W2HXR", "D-0AAAA"]), "x")),
        ),
        (
            "duplicate",
            one_target(entry(list(&["D-W2HXR", "D-W2HXR"]), "x")),
        ),
        (
            "not a decision id",
            one_target(entry(list(&["c-AAAAA"]), "x")),
        ),
        (
            "malformed decision id",
            one_target(entry(list(&["D-AAAA"]), "x")),
        ),
        ("against other text", one_target(entry(str_v("gone"), "x"))),
        (
            "against holds a non-string",
            one_target(entry(FieldValue::List(vec![FieldValue::Int(1)]), "x")),
        ),
        ("against null", one_target(entry(FieldValue::Null, "x"))),
        // reason: trimmed, 1 to 2000 scalar values
        ("empty reason", one_target(entry(list(&[]), ""))),
        ("blank reason", one_target(entry(list(&[]), "   "))),
        ("untrimmed reason", one_target(entry(list(&[]), " x"))),
        ("untrimmed end", one_target(entry(list(&[]), "x\n"))),
        ("long reason", one_target(entry(list(&[]), &over_reason))),
        (
            "reason not text",
            one_target(map(&[
                ("against", list(&[])),
                ("reason", FieldValue::Int(1)),
            ])),
        ),
        // limits
        (
            "too many retirements",
            one_target(entry(list(&many_against), "x")),
        ),
    ];
    for (what, v) in bad_reaffirmed {
        let mut b = claim();
        b.fields.insert(REAFFIRMED.into(), v.clone());
        for issues in both(&b) {
            assert_eq!(engine_issues(&issues), bad(REAFFIRMED), "{what}");
        }
        assert!(engine_fields::decode_reaffirmed(&v).is_err(), "{what}");
    }
    // too many targets
    let targets: BTreeMap<String, FieldValue> = (0..=MAX_ENGINE_ENTRIES)
        .map(|i| (format!("c-{}", five(i)), entry(list(&[]), "x")))
        .collect();
    let mut b = claim();
    b.fields
        .insert(REAFFIRMED.into(), FieldValue::Map(targets.clone()));
    assert_eq!(
        engine_issues(&validate_read_block(&pack(), &b, Some(&section()))),
        bad(REAFFIRMED)
    );
    let mut at_cap = targets;
    at_cap.pop_last();
    assert_eq!(at_cap.len(), MAX_ENGINE_ENTRIES);
    b.fields.insert(REAFFIRMED.into(), FieldValue::Map(at_cap));
    assert!(engine_issues(&validate_read_block(&pack(), &b, Some(&section()))).is_empty());

    let files = str_v(&Hash::of(b"f").to_string());
    let ok_reflects = [
        reflects(files.clone(), map(&[])),
        reflects(files.clone(), map(&[("core", FieldValue::Null)])),
        reflects(files.clone(), map(&[("core", str_v(&"f".repeat(40)))])),
        reflects(files.clone(), map(&[("core", str_v(&"9".repeat(64)))])),
    ];
    for v in ok_reflects {
        let mut b = claim();
        b.fields.insert(REFLECTS_AT.into(), v.clone());
        for issues in both(&b) {
            assert!(engine_issues(&issues).is_empty(), "{v:?}: {issues:?}");
        }
    }
    let many_commits: Vec<(String, FieldValue)> = (0..=MAX_ENGINE_ENTRIES)
        .map(|i| (format!("r{i}"), FieldValue::Null))
        .collect();
    let bad_reflects = [
        ("not a map", list(&[])),
        ("missing files", map(&[("commits", map(&[]))])),
        ("missing commits", map(&[("files", files.clone())])),
        (
            "extra key",
            map(&[
                ("commits", map(&[])),
                ("files", files.clone()),
                ("head", FieldValue::Null),
            ]),
        ),
        (
            "uppercase files",
            reflects(str_v(&Hash::of(b"f").to_string().to_uppercase()), map(&[])),
        ),
        ("short files", reflects(str_v("abcd"), map(&[]))),
        ("files not text", reflects(FieldValue::Null, map(&[]))),
        ("commits not a map", reflects(files.clone(), list(&[]))),
        (
            "short commit",
            reflects(files.clone(), map(&[("core", str_v(&"a".repeat(39)))])),
        ),
        (
            "uppercase commit",
            reflects(files.clone(), map(&[("core", str_v(&"A".repeat(40)))])),
        ),
        (
            "commit not text",
            reflects(files.clone(), map(&[("core", FieldValue::Int(0))])),
        ),
        (
            "too many repos",
            reflects(
                files.clone(),
                FieldValue::Map(many_commits.into_iter().collect()),
            ),
        ),
    ];
    for (what, v) in bad_reflects {
        let mut b = claim();
        b.fields.insert(REFLECTS_AT.into(), v.clone());
        for issues in both(&b) {
            assert_eq!(engine_issues(&issues), bad(REFLECTS_AT), "{what}");
        }
        assert!(engine_fields::decode_reflects_at(&v).is_err(), "{what}");
    }

    // the write validator's 29-level nesting bound still comes first
    let mut deep = str_v("x");
    for _ in 0..rdy_model::MAX_FIELD_NESTING {
        deep = map(&[("k", deep)]);
    }
    let mut b = claim();
    b.fields.insert(REAFFIRMED.into(), deep);
    let w = validate_block(&pack(), &b, Some(&section()));
    assert_eq!(
        w.iter().map(|i| i.code).collect::<Vec<_>>(),
        ["E_LIMIT"],
        "{w:?}"
    );
}

/// The `i`th five-digit Crockford id body, in sorted order.
fn five(mut i: usize) -> String {
    let digits = rdy_model::CROCKFORD;
    let mut out = vec![b'0'; 5];
    for slot in out.iter_mut().rev() {
        *slot = digits[i % 32];
        i /= 32;
    }
    String::from_utf8(out).unwrap()
}
