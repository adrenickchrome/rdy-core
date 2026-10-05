//! One store format number (plan M2a1, fmt2): the FORMAT text, and a parser that ignores keys
//! a later rdy may add.
use rdy_store::format::{Format, STORE_FORMAT};

#[test]
fn fmt2_the_format_text_is_one_number() {
    assert_eq!(STORE_FORMAT, 3);
    assert_eq!(
        Format::CURRENT.to_text(),
        "rdy store format\nstore_format 3\n"
    );
    assert_eq!(
        Format::parse(&Format::CURRENT.to_text()),
        Ok(Some(Format::CURRENT))
    );
}

#[test]
fn fmt2_parse_ignores_unknown_keys_and_reports_a_missing_number() {
    let later = Format::parse("rdy store format\nstore_format 4\nview_format 2\n").unwrap();
    assert_eq!(later, Some(Format { store_format: 4 }));
    assert_eq!(
        Format::parse("rdy store format\nlog_format 2\nmin_reader 2\n"),
        Ok(None),
        "M2.0's text"
    );
    assert_eq!(Format::parse("rdy store format\n"), Ok(None));
    assert!(
        Format::parse("something else\nstore_format 3\n")
            .unwrap_err()
            .contains("first line")
    );
    assert!(
        Format::parse("rdy store format\nstore_format three\n")
            .unwrap_err()
            .contains("bad line")
    );
}
