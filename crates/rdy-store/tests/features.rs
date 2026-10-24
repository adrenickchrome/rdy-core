//! The store's feature manifest (D-WWAK7): compatibility by feature class instead of one format
//! number, first use of a feature in the batch that uses it, and hostile manifests and encodings
//! refused before anything is allocated from them or any file changes.
use rdy_model::Hash;
use rdy_store::bounded::RecordError;
use rdy_store::codec::{self, ObjectBytes};
use rdy_store::format::{
    FORMAT_FILE, FeatureClass, REF_STORE_FEATURES, StoreAccess, StoreFeature, StoreFeatures,
    TEST_FEATURE, baseline_features, check_features, inject_test_feature, registered_features,
};
use rdy_store::{Store, StoreError, kinds};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Every path under `dir` with its contents (None for directories).
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.insert(p.clone(), None);
            out.extend(tree(&p));
        } else {
            out.insert(p.clone(), Some(std::fs::read(&p).unwrap()));
        }
    }
    out
}

fn feature(name: &str, class: FeatureClass, required: &str) -> StoreFeature {
    StoreFeature {
        name: name.into(),
        class,
        required_rdy: required.into(),
    }
}

fn manifest(features: Vec<StoreFeature>) -> StoreFeatures {
    StoreFeatures {
        manifest_version: 1,
        features,
    }
}

fn format_error(e: StoreError) -> (String, String) {
    match e {
        StoreError::Format { message, fix } => (message, fix),
        other => panic!("not a format refusal: {other:?}"),
    }
}

/// The manifest a store's log holds now.
fn log_manifest(s: &Store) -> StoreFeatures {
    StoreFeatures::parse(
        &s.get_ref(REF_STORE_FEATURES)
            .unwrap()
            .expect("the features ref"),
    )
    .unwrap()
}

#[test]
fn ok1_feature_classes_choose_access() {
    assert_eq!(
        check_features(&baseline_features()).unwrap(),
        StoreAccess::ReadWrite
    );
    let one = |class| manifest(vec![feature(TEST_FEATURE, class, "99.0.0")]);
    assert_eq!(
        check_features(&one(FeatureClass::Compatible)).unwrap(),
        StoreAccess::ReadWrite,
        "an unknown compatible feature permits writes"
    );
    assert_eq!(
        check_features(&one(FeatureClass::ReadOnlyCompatible)).unwrap(),
        StoreAccess::ReadOnly,
        "an unknown read-only feature permits reads only"
    );
    assert_eq!(
        format_error(check_features(&one(FeatureClass::Incompatible)).unwrap_err()),
        (
            "store feature test_feature needs rdy 99.0.0".to_string(),
            "open the project with rdy 99.0.0 or later".to_string()
        )
    );
    // the first refusal by name, whatever else the manifest holds
    let mixed = manifest(vec![
        feature("alpha", FeatureClass::ReadOnlyCompatible, "3.0.0"),
        feature("beta", FeatureClass::Incompatible, "2.0.0"),
        feature("gamma", FeatureClass::Incompatible, "1.0.0"),
        feature("zeta", FeatureClass::Compatible, "1.0.0"),
    ]);
    assert_eq!(
        format_error(check_features(&mixed).unwrap_err()).0,
        "store feature beta needs rdy 2.0.0"
    );
    // a known feature is this rdy's, whatever its class
    let _known = inject_test_feature(FeatureClass::Incompatible, kinds::BLOB_CHUNK);
    assert_eq!(
        check_features(&one(FeatureClass::Incompatible)).unwrap(),
        StoreAccess::ReadWrite
    );
}

/// A store whose log activated `class`'s test feature, opened by an rdy that does not know it.
fn store_with_unknown(class: FeatureClass) -> (tempfile::TempDir, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    {
        let _known = inject_test_feature(class, kinds::BLOB_CHUNK);
        let mut s = Store::create(&dir).unwrap();
        s.put_blob(b"uses the feature").unwrap();
        assert_eq!(
            log_manifest(&s),
            manifest(vec![feature(TEST_FEATURE, class, "99.0.0")])
        );
    }
    (base, dir)
}

#[test]
fn ok1_feature_classes_choose_access_at_open() {
    // compatible: opens and writes
    let (_b, dir) = store_with_unknown(FeatureClass::Compatible);
    let mut s = Store::open(&dir).unwrap();
    s.put_blob(b"a later write").unwrap();
    drop(s);
    // read-only and incompatible: refused before anything under the store changes
    for (class, what) in [
        (FeatureClass::ReadOnlyCompatible, "read-only"),
        (FeatureClass::Incompatible, "incompatible"),
    ] {
        let (_b, dir) = store_with_unknown(class);
        let before = tree(&dir);
        let e = Store::open(&dir).err().expect(what);
        assert_eq!(
            format_error(e),
            (
                "store feature test_feature needs rdy 99.0.0".to_string(),
                "open the project with rdy 99.0.0 or later".to_string()
            ),
            "{what}"
        );
        assert_eq!(
            tree(&dir),
            before,
            "{what}: nothing under the store changed"
        );
    }
}

#[test]
fn ok1_test_feature_first_use_and_ref_are_one_batch() {
    assert!(
        registered_features().is_empty(),
        "production registers no feature"
    );
    assert_eq!(baseline_features(), manifest(vec![]));
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    assert_eq!(log_manifest(&s), baseline_features());
    // no first use without the feature: a blob writes no features ref
    s.put_blob(b"before").unwrap();
    assert!(s.last_batch().iter().all(|(k, _)| *k != kinds::REF_UPDATE));
    let _known = inject_test_feature(FeatureClass::Compatible, kinds::BLOB_CHUNK);
    let with = manifest(vec![feature(
        TEST_FEATURE,
        FeatureClass::Compatible,
        "99.0.0",
    )]);
    // a failed first use appends neither its objects nor the features ref
    let end = s.log_end();
    s.fail_next_append_after(40);
    assert!(s.put_blob(b"first use").is_err());
    assert_eq!(s.log_end(), end, "nothing appended");
    assert_eq!(log_manifest(&s), baseline_features());
    assert!(!s.has_blob(&Store::blob_hash(b"first use")).unwrap());
    // the first use: its chunk, its manifest and the features ref in one batch
    let h = s.put_blob(b"first use").unwrap();
    let batch: Vec<u8> = s.last_batch().iter().map(|(k, _)| *k).collect();
    assert_eq!(
        batch,
        [kinds::BLOB_CHUNK, kinds::BLOB_MANIFEST, kinds::REF_UPDATE]
    );
    assert_eq!(s.last_batch()[1].1, h);
    assert_eq!(log_manifest(&s), with);
    assert_eq!(
        std::fs::read_to_string(dir.join(FORMAT_FILE)).unwrap(),
        with.to_text(),
        "FORMAT mirrors the log"
    );
    // a later use adds nothing
    s.put_blob(b"second use").unwrap();
    assert!(s.last_batch().iter().all(|(k, _)| *k != kinds::REF_UPDATE));
    // the ref survives a reindex and a reopen
    s.reindex().unwrap();
    assert_eq!(log_manifest(&s), with);
    drop(s);
    let s = Store::open(&dir).unwrap();
    assert_eq!(log_manifest(&s), with);
}

/// One manifest text's refusal reason.
fn reason(text: &str) -> String {
    StoreFeatures::parse(text.as_bytes())
        .expect_err(text)
        .to_string()
}

#[test]
fn ok1_hostile_manifest_and_encoding_are_bounded() {
    // the manifest: every listed case, each refused with its exact reason
    let big = format!(
        "{{\"manifest_version\":1,\"features\":[],\"x\":\"{}\"}}",
        "a".repeat(65_536)
    );
    assert_eq!(reason(&big), "manifest exceeds 65536 bytes");
    assert_eq!(
        reason("{\"manifest_version\":2,\"features\":[]}"),
        "unsupported manifest version"
    );
    let many: Vec<String> = (0..257)
        .map(|i| {
            format!("{{\"name\":\"f{i:03}\",\"class\":\"compatible\",\"required_rdy\":\"1.0.0\"}}")
        })
        .collect();
    assert_eq!(
        reason(&format!(
            "{{\"manifest_version\":1,\"features\":[{}]}}",
            many.join(",")
        )),
        "more than 256 features"
    );
    let one = |name: &str, class: &str, required: &str| {
        format!("{{\"name\":\"{name}\",\"class\":\"{class}\",\"required_rdy\":\"{required}\"}}")
    };
    let list = |items: &[String]| {
        format!(
            "{{\"manifest_version\":1,\"features\":[{}]}}",
            items.join(",")
        )
    };
    let f = |name: &str| one(name, "compatible", "1.0.0");
    assert_eq!(reason(&list(&[f("a"), f("a")])), "duplicate feature");
    assert_eq!(reason(&list(&[f("b"), f("a")])), "features are not sorted");
    for name in ["", "A", "9a", "_a", "a-b", &"a".repeat(65)] {
        assert_eq!(
            reason(&list(&[f(name)])),
            "invalid feature name",
            "{name:?}"
        );
    }
    for required in [
        "1.0",
        "1.0.0.0",
        "01.0.0",
        "1.00.0",
        "-1.0.0",
        "4294967296.0.0",
        "1.0.x",
        "",
    ] {
        assert_eq!(
            reason(&list(&[one("a", "compatible", required)])),
            "invalid required_rdy",
            "{required:?}"
        );
    }
    assert_eq!(
        reason("{\"manifest_version\":1,\"features\":[],\"extra\":1}"),
        "unknown field extra"
    );
    assert_eq!(
        reason(&list(&[
            "{\"name\":\"a\",\"class\":\"compatible\",\"required_rdy\":\"1.0.0\",\"why\":0}"
                .to_string()
        ])),
        "unknown field why"
    );
    let long = "k".repeat(100);
    assert_eq!(
        reason(&format!(
            "{{\"manifest_version\":1,\"features\":[],\"{long}\":1}}"
        )),
        format!("unknown field {}\u{2026}", "k".repeat(80))
    );
    for text in [
        "",
        "{",
        "[]",
        "null",
        "{\"features\":[]}",
        "{\"manifest_version\":1}",
        "{\"manifest_version\":\"1\",\"features\":[]}",
        "{\"manifest_version\":1,\"features\":{}}",
        &list(&[one("a", "sometimes", "1.0.0")]),
        &list(&["{\"name\":\"a\",\"class\":\"compatible\"}".to_string()]),
    ] {
        assert_eq!(reason(text), "invalid JSON", "{text:?}");
    }
    {
        let _known = inject_test_feature(FeatureClass::Compatible, kinds::BLOB_CHUNK);
        assert_eq!(
            reason(&list(&[one(TEST_FEATURE, "incompatible", "99.0.0")])),
            "wrong feature class"
        );
    }
    // a canonical manifest reads back as itself
    let m = manifest(vec![
        feature("a", FeatureClass::Compatible, "0.0.0"),
        feature("b_2", FeatureClass::ReadOnlyCompatible, "4294967295.10.0"),
        feature("c", FeatureClass::Incompatible, "1.2.3"),
    ]);
    assert_eq!(
        String::from_utf8(m.to_json()).unwrap(),
        "{\"manifest_version\":1,\"features\":[{\"name\":\"a\",\"class\":\"compatible\",\"required_rdy\":\"0.0.0\"},{\"name\":\"b_2\",\"class\":\"read_only_compatible\",\"required_rdy\":\"4294967295.10.0\"},{\"name\":\"c\",\"class\":\"incompatible\",\"required_rdy\":\"1.2.3\"}]}"
    );
    assert_eq!(StoreFeatures::parse(&m.to_json()).unwrap(), m);

    // a store whose FORMAT holds a hostile manifest is refused before any file changes
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    Store::create(&dir).unwrap().put_blob(b"x").unwrap();
    for text in [
        format!("rdy store features 1\n{big}\n"),
        "rdy store features 1\n{\"manifest_version\":1,\"features\":[],\"extra\":1}\n".into(),
        "rdy store features 1\n{\"manifest_version\":2,\"features\":[]}\n".into(),
    ] {
        std::fs::write(dir.join(FORMAT_FILE), &text).unwrap();
        let before = tree(&dir);
        let (message, fix) = format_error(Store::open(&dir).err().expect("refused"));
        assert!(
            message.starts_with("store feature manifest is invalid: "),
            "{message}"
        );
        assert_eq!(fix, "restore the store from a backup");
        assert_eq!(tree(&dir), before);
    }

    // the encoding: a marked frame needs a known nonzero version and its two bytes
    let body = b"body".to_vec();
    assert_eq!(
        codec::split(kinds::TAG, &body).unwrap(),
        ObjectBytes {
            kind: kinds::TAG,
            version: 0,
            body: &body
        }
    );
    let marked = kinds::TAG | codec::VERSIONED;
    assert_eq!(codec::split(marked, &[]), Err(RecordError::Decode));
    assert_eq!(codec::split(marked, &[1]), Err(RecordError::Decode));
    assert_eq!(
        codec::split(marked, &[0, 0, b'x']),
        Err(RecordError::NoncanonicalVersion)
    );
    assert_eq!(
        RecordError::NoncanonicalVersion.to_string(),
        "noncanonical encoding version"
    );
    assert_eq!(
        codec::split(marked, &[1, 0, b'x']),
        Err(RecordError::UnknownVersion)
    );
    assert_eq!(
        RecordError::UnknownVersion.to_string(),
        "unknown encoding version"
    );
    assert_eq!(
        codec::split(marked, &[0xff, 0xff, b'x']),
        Err(RecordError::UnknownVersion)
    );
    for local in [kinds::REF_UPDATE, kinds::BATCH_COMMIT] {
        assert_eq!(
            codec::split(local | codec::VERSIONED, &[1, 0]),
            Err(RecordError::Kind)
        );
        assert_eq!(codec::frame(local, 1, vec![]), Err(RecordError::Kind));
    }
    assert_eq!(codec::frame(0x80, 0, vec![]), Err(RecordError::Kind));
    assert_eq!(
        codec::frame(kinds::TAG, 1, body.clone()),
        Err(RecordError::UnknownVersion),
        "no kind has encoding 1 yet"
    );
    assert_eq!(
        codec::frame(kinds::TAG, 0, body.clone()),
        Ok((kinds::TAG, body.clone()))
    );
    // a typed read of a pair the registry does not hold is refused
    let e = codec::decode_record::<rdy_model::Tag>(kinds::BLOCK, &body).unwrap_err();
    assert!(matches!(e, StoreError::Decode(_)), "{e:?}");
    let _ = Hash::of(&body);
}
