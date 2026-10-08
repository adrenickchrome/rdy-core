//! Display text on references and the key grammar (dtext1, key1).
use rdy_model::{RefMode, key, ref_spans};

#[test]
fn dtext1_display_text_splits_at_the_first_bar_and_is_trimmed() {
    let s = ref_spans(
        "see {{ c-WYF81 | the  rule | twice }} and {{c-WYF81}}",
        RefMode::Strict,
    );
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].token, "c-WYF81");
    assert_eq!(s[0].display.as_deref(), Some("the  rule | twice"));
    assert_eq!(s[1].token, "c-WYF81");
    assert_eq!(s[1].display, None);
}

#[test]
fn dtext1_blank_display_text_is_no_display_text() {
    let s = ref_spans("{{c-WYF81|}} {{c-WYF81|   }}", RefMode::Strict);
    assert_eq!(s.len(), 2);
    assert!(
        s.iter()
            .all(|x| x.token == "c-WYF81" && x.display.is_none())
    );
}

#[test]
fn dtext1_id_shaped_mode_judges_the_id_part_and_the_escape_still_wins() {
    let s = ref_spans(
        "A{{Decide|x}} B{{c-WYF81|Balance}} \\{{c-WYF81|no}}",
        RefMode::IdShaped,
    );
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].display.as_deref(), Some("Balance"));
    assert!(ref_spans("{{c-WYF81|x}}", RefMode::Off).is_empty());
}

#[test]
fn key1_the_grammar_accepts_human_keys_and_refuses_ids_and_syntax() {
    for ok in [
        "FW-S1",
        "REQ-12",
        "Q4",
        "cap1",
        "spec.guards",
        "A_b-3",
        "DEC-WILDCARD",
    ] {
        assert_eq!(key::problem(ok), None, "{ok}");
    }
    let long = "x".repeat(33);
    for bad in [
        "",
        "1abc",
        "c-WYF81",
        "P-12",
        "p-7KQ2M",
        "FW S1",
        "FW-S1|x",
        "a@b",
        "a/b",
        "$x",
        "a--b",
        "a-",
        "[a]",
        "{a}",
        long.as_str(),
    ] {
        assert!(key::problem(bad).is_some(), "{bad:?} should be refused");
    }
    assert_eq!(key::fold("Fw-s1"), key::fold("FW-S1"));
}

#[test]
fn key1_a_bar_at_key_marks_a_key_typed_reference_and_backslash_at_escapes_a_leading_at() {
    let s = ref_spans(
        "{{c-WYF81|@key}} {{ c-WYF81 | @key }} {{c-WYF81|\\@x}} {{c-WYF81|a@b}} {{c-WYF81}}",
        RefMode::Strict,
    );
    assert_eq!(s.len(), 5);
    for typed in &s[..2] {
        assert!(typed.key_typed, "{typed:?}");
        assert_eq!(typed.token, "c-WYF81");
        assert_eq!(typed.display, None);
    }
    assert!(!s[2].key_typed);
    assert_eq!(
        s[2].display.as_deref(),
        Some("@x"),
        "the escape is removed and the @ is literal"
    );
    assert!(!s[3].key_typed);
    assert_eq!(s[3].display.as_deref(), Some("a@b"));
    assert!(!s[4].key_typed && s[4].display.is_none());
}

#[test]
fn key1_encode_writes_the_stored_form_back_and_round_trips() {
    let text = "{{c-WYF81|@key}} {{c-WYF81|\\@x}} {{c-WYF81|plain}} {{c-WYF81}}";
    for s in ref_spans(text, RefMode::Strict) {
        let back = s.encode(&s.token);
        let again = ref_spans(&back, RefMode::Strict);
        assert_eq!(again.len(), 1);
        assert_eq!(
            (again[0].key_typed, &again[0].display),
            (s.key_typed, &s.display),
            "{back}"
        );
    }
    let s = &ref_spans("{{c-WYF81|\\@x}}", RefMode::Strict)[0];
    assert_eq!(s.encode("p-12345"), "{{p-12345|\\@x}}");
    let s = &ref_spans("{{c-WYF81|@key}}", RefMode::Strict)[0];
    assert_eq!(s.encode("p-12345"), "{{p-12345|@key}}");
}

#[test]
fn dtext1_a_span_says_whether_its_leading_at_was_escaped_and_encode_keeps_the_spelling() {
    let s = ref_spans(
        "{{c-WYF81|\\@x}} {{c-WYF81|@handle}} {{c-WYF81|plain}} {{c-WYF81|@key}}",
        RefMode::Strict,
    );
    assert_eq!(s.len(), 4);
    assert!(s[0].at_escaped && s[0].display.as_deref() == Some("@x"));
    assert!(!s[1].at_escaped && s[1].display.as_deref() == Some("@handle"));
    assert!(!s[2].at_escaped && !s[3].at_escaped);
    assert_eq!(s[0].encode("p-12345"), "{{p-12345|\\@x}}");
    assert_eq!(
        s[1].encode("p-12345"),
        "{{p-12345|@handle}}",
        "an unescaped @ is not silently escaped, so validation sees what the author wrote"
    );
}
