use rdy_engine::stream_error::to_rdy;
use rdy_engine::{
    E_BAD_RECORD, E_IO, E_LIMIT, E_REPO_MISMATCH, E_STORE_FORMAT, E_STREAM_PURPOSE,
    E_STREAM_TRUNCATED, E_TAG_EXISTS, E_UNRESOLVED_REF,
};
use rdy_model::Hash;
use rdy_schema::Severity;
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::{Reason, StreamError};

fn key() -> ObjectKey {
    ObjectKey {
        kind: 7,
        hash: Hash([0xab; 32]),
    }
}

/// Every stream error the store defines, with and without a position.
fn table() -> Vec<StreamError> {
    let io = std::io::Error::other("disk gone");
    vec![
        StreamError::bad_record(3, 120, Reason::HashMismatch),
        StreamError::bad_record(4, 9, Reason::UnknownKind).with_object(Some(key())),
        StreamError::unresolved(5, 77, key()),
        StreamError::truncated(None, 0),
        StreamError::truncated(Some(9), 4096),
        StreamError::purpose(None, 0, 2),
        StreamError::purpose(Some(0), 12, 9),
        StreamError::store_format(Some(0), 12, 14, 15),
        StreamError::limit(None, 0, "1000000 object records"),
        StreamError::limit(Some(2), 55, "2147483648 total bytes").with_object(Some(key())),
        StreamError::allocation(Some(6), 1),
        StreamError::io(None, 0, &io),
    ]
}

#[test]
fn ha8_stream_conversion_preserves_every_field() {
    let cases = table();
    assert_eq!(cases.len(), 12);
    for e in cases {
        let r = to_rdy(e.clone());
        assert_eq!(r.code, e.code, "{e}");
        assert_eq!(r.message, e.message);
        assert_eq!(r.fix, e.fix);
        assert!(r.corrected.is_none() && r.briefing.is_none());
        assert_eq!(r.issues.len(), 1, "exactly one positioned issue");
        let issue = &r.issues[0];
        assert_eq!(issue.severity, Severity::Error);
        assert_eq!(issue.code, e.code);
        assert!(issue.block.is_none());
        assert_eq!(issue.fix, e.fix);
        let record = e.record.map_or("none".to_string(), |n| n.to_string());
        let object = e.object.map_or("none".to_string(), |k| {
            format!("{}:{}", k.kind, "ab".repeat(32))
        });
        assert_eq!(
            issue.message,
            format!("record {record} at byte {}; object {object}", e.offset)
        );
    }
}

#[test]
fn ha8_conversion_prints_none_for_missing_record_and_object() {
    let r = to_rdy(StreamError::truncated(None, 0));
    assert_eq!(r.issues[0].message, "record none at byte 0; object none");
    let r = to_rdy(StreamError::unresolved(5, 77, key()));
    assert_eq!(r.issues[0].message, "record 5 at byte 77; object none");
    let r = to_rdy(StreamError::bad_record(1, 2, Reason::UnknownKind).with_object(Some(key())));
    assert_eq!(
        r.issues[0].message,
        format!("record 1 at byte 2; object 7:{}", "ab".repeat(32))
    );
}

#[test]
fn ha8_tag_exists_is_reused_and_no_hub_code_is_added() {
    assert_eq!(E_BAD_RECORD, "E_BAD_RECORD");
    assert_eq!(E_UNRESOLVED_REF, "E_UNRESOLVED_REF");
    assert_eq!(E_STREAM_TRUNCATED, "E_STREAM_TRUNCATED");
    assert_eq!(E_STREAM_PURPOSE, "E_STREAM_PURPOSE");
    assert_eq!(E_REPO_MISMATCH, "E_REPO_MISMATCH");
    // the codes the stream table uses that already exist keep their values
    assert_eq!(E_TAG_EXISTS, "E_TAG_EXISTS");
    assert_eq!(E_STORE_FORMAT, "E_STORE_FORMAT");
    assert_eq!(E_LIMIT, "E_LIMIT");
    assert_eq!(E_IO, "E_IO");
    // every code the store's stream errors carry is a constant of the engine
    let known = [
        E_BAD_RECORD,
        E_UNRESOLVED_REF,
        E_STREAM_TRUNCATED,
        E_STREAM_PURPOSE,
        E_STORE_FORMAT,
        E_LIMIT,
        E_IO,
    ];
    for e in table() {
        assert!(known.contains(&e.code), "{} has no engine constant", e.code);
    }
    // one definition of E_TAG_EXISTS, and no hub code (p-2P39F reserves them)
    let src = include_str!("../src/error.rs");
    assert_eq!(src.matches("pub const E_TAG_EXISTS").count(), 1);
    assert_eq!(src.matches("pub const E_REPO_MISMATCH").count(), 1);
    assert!(!src.contains("E_HUB_"));
}
