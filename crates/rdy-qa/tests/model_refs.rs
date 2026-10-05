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
