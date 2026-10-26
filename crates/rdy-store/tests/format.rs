//! The store's format is its feature manifest (D-WWAK7): FORMAT holds `rdy store format` and
//! `store_format 19`, so every released rdy reads it as a newer rdy's store; FEATURES beside it
//! mirrors the manifest's canonical JSON. The parser refuses what it does not read, scalar
//! formats included.
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
    // ok4: the baseline names content-2 identity and object encodings
    assert_eq!(
        baseline_features().to_text(),
        concat!(
            "rdy store features 1\n{\"manifest_version\":1,\"features\":[",
            "{\"name\":\"content_hash_v2\",\"class\":\"incompatible\",\"required_rdy\":\"0.26.0\"},",
            "{\"name\":\"object_encoding\",\"class\":\"incompatible\",\"required_rdy\":\"0.26.0\"}",
            "]}\n"
        )
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
        "rdy store format\nstore_format 19\n",
        "FORMAT holds exactly the two scalar lines"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("FEATURES")).unwrap(),
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
    // a feature store's FEATURES, beside FORMAT at 19
    let features = |text: &str| {
        std::fs::write(
            dir.path().join(FORMAT_FILE),
            "rdy store format\nstore_format 19\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("FEATURES"), text).unwrap();
        let e = format_error(read_format_file(dir.path()).unwrap_err());
        std::fs::remove_file(dir.path().join("FEATURES")).unwrap();
        e
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
        read("rdy store format\nstore_format 20\nview_format 2\n"),
        (
            "store format 20 is not supported".to_string(),
            "open the project with the rdy build that wrote it".to_string()
        )
    );
    // FORMAT at 19 with no FEATURES: the invalid-manifest refusal
    assert_eq!(
        read("rdy store format\nstore_format 19\n"),
        (
            "store feature manifest is invalid: invalid JSON".to_string(),
            "restore the store from a backup".to_string()
        )
    );
    // the manifest is never FORMAT's first line
    let (message, _) = read(&baseline_features().to_text());
    assert!(
        message.ends_with("is unreadable: the first line is not \"rdy store format\""),
        "{message}"
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
    // a FEATURES that is not the manifest line then one line of valid JSON
    for text in [
        "",
        "something else\n{}\n",
        "rdy store features 1\n",
        "rdy store features 1\n{\"manifest_version\":1,\"features\":[]}",
        "rdy store features 1\n{\"manifest_version\":1,\n\"features\":[]}\n",
        "rdy store features 1\nnot json\n",
    ] {
        assert_eq!(
            features(text),
            (
                "store feature manifest is invalid: invalid JSON".to_string(),
                "restore the store from a backup".to_string()
            ),
            "{text:?}"
        );
    }
    for text in [
        "rdy store features 1\n{\"manifest_version\":2,\"features\":[]}\n",
        "rdy store features 2\n{\"manifest_version\":2,\"features\":[]}\n",
    ] {
        assert_eq!(
            features(text).0,
            "store feature manifest is invalid: unsupported manifest version",
            "{text:?}"
        );
    }
}

/// FEATURES is renamed into place before FORMAT, each durably (final ruling 3, round 2), so a
/// crash between the two leaves the new manifest beside a FORMAT whose bytes never change.
#[test]
fn ok1_features_is_written_before_format() {
    let dir = tempfile::tempdir().unwrap();
    let ((), synced) = rdy_store::fsutil::record_syncs(|| {
        write_format_file(dir.path(), &baseline_features()).unwrap()
    });
    let names: Vec<String> = synced
        .iter()
        .filter(|p| p.parent() == Some(dir.path()))
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["FEATURES", "FORMAT"], "{synced:?}");
    // the state between the two renames: a new store's FEATURES with no FORMAT is no store
    std::fs::remove_file(dir.path().join(FORMAT_FILE)).unwrap();
    assert_eq!(read_format_file(dir.path()).unwrap(), None);
}
