//! The store's format is its feature manifest (D-WWAK7): the FORMAT text mirrors the manifest's
//! canonical JSON, and the parser refuses what it does not read, scalar formats included.
use rdy_store::StoreError;
use rdy_store::format::{
    FORMAT_FILE, FeatureClass, LAST_SCALAR_STORE_FORMAT, StoreFeature, StoreFeatures,
    baseline_features, read_format_file, write_format_file,
};

fn format_error(e: StoreError) -> (String, String) {
    match e {
        StoreError::Format { message, fix } => (message, fix),
        other => panic!("not a format refusal: {other:?}"),
    }
}

#[test]
fn ok1_the_format_text_is_the_feature_manifest() {
    assert_eq!(LAST_SCALAR_STORE_FORMAT, 18);
    assert_eq!(
        baseline_features().to_text(),
        "rdy store features 1\n{\"manifest_version\":1,\"features\":[]}\n"
    );
    let m = StoreFeatures::of(vec![
        StoreFeature {
            name: "zeta".into(),
            class: FeatureClass::Compatible,
            required_rdy: "1.0.0".into(),
        },
        StoreFeature {
            name: "alpha".into(),
            class: FeatureClass::Incompatible,
            required_rdy: "0.23.0".into(),
        },
    ]);
    assert_eq!(m.features[0].name, "alpha", "sorted by name");
    let dir = tempfile::tempdir().unwrap();
    write_format_file(dir.path(), &m).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join(FORMAT_FILE)).unwrap(),
        format!(
            "rdy store features 1\n{}\n",
            String::from_utf8(m.to_json()).unwrap()
        )
    );
    assert_eq!(read_format_file(dir.path()).unwrap(), Some(m));
    let none = tempfile::tempdir().unwrap();
    assert_eq!(read_format_file(none.path()).unwrap(), None);
}

#[test]
fn ok1_the_format_text_refuses_what_it_does_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let read = |text: &str| {
        std::fs::write(dir.path().join(FORMAT_FILE), text).unwrap();
        format_error(read_format_file(dir.path()).unwrap_err())
    };
    // a scalar store: 10 to 18 moved with dump and load, a later number is no build's
    for n in [10, 17, 18] {
        assert_eq!(
            read(&format!("rdy store format\nstore_format {n}\n")),
            (
                format!("store format {n} predates feature-based stores"),
                "dump with the rdy that wrote this store, then load into a directory with no store"
                    .to_string()
            )
        );
    }
    assert_eq!(
        read("rdy store format\nstore_format 19\nview_format 2\n"),
        (
            "store format 19 is not supported".to_string(),
            "open the project with the rdy build that wrote it".to_string()
        )
    );
    // before dump existed: re-create
    let (message, fix) = read("rdy store format\nstore_format 9\n");
    assert!(message.ends_with("has store format 9, and this rdy uses feature-based stores"));
    assert_eq!(fix, "re-create it with this rdy");
    let (message, _) = read("rdy store format\nlog_format 2\nmin_reader 2\n");
    assert!(
        message.contains("has a FORMAT file with no store_format line"),
        "{message}"
    );
    // not a FORMAT file
    let (message, fix) = read("something else\n{}\n");
    assert!(message.contains("the first line is not"), "{message}");
    assert_eq!(
        fix,
        "restore FORMAT from backup, or re-create it with this rdy"
    );
    let (message, _) = read("rdy store format\nstore_format three\n");
    assert!(message.contains("bad line"), "{message}");
    // a manifest that is not one line of valid JSON
    for text in [
        "rdy store features 1\n",
        "rdy store features 1\n{\"manifest_version\":1,\"features\":[]}",
        "rdy store features 1\n{\"manifest_version\":1,\n\"features\":[]}\n",
        "rdy store features 1\nnot json\n",
    ] {
        assert_eq!(
            read(text),
            (
                "store feature manifest is invalid: invalid JSON".to_string(),
                "restore the store from a backup".to_string()
            ),
            "{text:?}"
        );
    }
    assert_eq!(
        read("rdy store features 1\n{\"manifest_version\":2,\"features\":[]}\n").0,
        "store feature manifest is invalid: unsupported manifest version"
    );
}
