use rdy_store::format::{Format, LOG_FORMAT, READER_FORMAT};

#[test]
fn fmt1_format_text_round_trips() {
    let t = Format::CURRENT.to_text();
    assert_eq!(t, "rdy store format\nlog_format 2\nmin_reader 2\n");
    assert_eq!(Format::parse(&t).unwrap(), Format::CURRENT);
    assert_eq!((LOG_FORMAT, READER_FORMAT), (2, 2));
}

#[test]
fn fmt1_format_text_ignores_unknown_keys_and_refuses_junk() {
    let f = Format::parse("rdy store format\nlog_format 3\nview_format 2\nmin_reader 3\n").unwrap();
    assert_eq!(
        f,
        Format {
            log_format: 3,
            min_reader: 3
        }
    );
    assert!(Format::parse("not a format file\n").is_err());
    assert!(
        Format::parse("rdy store format\nlog_format 2\n")
            .unwrap_err()
            .contains("min_reader")
    );
    assert!(Format::parse("rdy store format\nlog_format two\nmin_reader 2\n").is_err());
}
