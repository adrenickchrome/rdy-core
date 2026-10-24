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

/// A global allocator that records the largest single request of this thread, and refuses
/// every request while this thread asks it to.
mod alloc_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Probe;

    thread_local! {
        static LARGEST: Cell<usize> = const { Cell::new(0) };
        static REFUSE: Cell<bool> = const { Cell::new(false) };
    }

    /// Records a request of `n` bytes; false when it is to be refused.
    fn admit(n: usize) -> bool {
        let _ = LARGEST.try_with(|l| l.set(l.get().max(n)));
        !REFUSE.try_with(Cell::get).unwrap_or(false)
    }

    // SAFETY: every admitted call goes to `System` with the caller's arguments unchanged; a
    // refused one returns null, which `GlobalAlloc` allows for a failed allocation.
    #[allow(unsafe_code)]
    unsafe impl GlobalAlloc for Probe {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if !admit(layout.size()) {
                return std::ptr::null_mut();
            }
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: `ptr` came from `System` with `layout` (every allocation here is System's).
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            if !admit(new_size) {
                return std::ptr::null_mut();
            }
            // SAFETY: `ptr` came from `System` with `layout`; the caller's new size.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    /// `f`'s result and the largest single request this thread made while it ran.
    pub fn largest<T>(f: impl FnOnce() -> T) -> (T, usize) {
        LARGEST.with(|l| l.set(0));
        let out = f();
        (out, LARGEST.with(Cell::get))
    }

    /// `f`'s result with every allocation of this thread refused while it runs.
    pub fn refusing<T>(f: impl FnOnce() -> T) -> T {
        REFUSE.with(|r| r.set(true));
        let out = f();
        REFUSE.with(|r| r.set(false));
        out
    }
}

#[global_allocator]
static PROBE: alloc_probe::Probe = alloc_probe::Probe;

/// The general manifest parse as it stood before the bounded parser: a `serde_json::Value`,
/// then each rule in order. Its refusal texts are the contract the bounded parser keeps.
fn reference_reason(bytes: &[u8]) -> Result<StoreFeatures, String> {
    use serde_json::Value;
    let invalid = || "invalid JSON".to_string();
    if bytes.len() > 65_536 {
        return Err("manifest exceeds 65536 bytes".into());
    }
    let unknown = |o: &serde_json::Map<String, Value>, allowed: &[&str]| -> Result<(), String> {
        let mut u: Vec<&String> = o
            .keys()
            .filter(|k| !allowed.contains(&k.as_str()))
            .collect();
        u.sort();
        match u.first() {
            Some(k) => Err(format!(
                "unknown field {}",
                match k.char_indices().nth(80) {
                    Some((at, _)) => format!("{}\u{2026}", &k[..at]),
                    None => k.to_string(),
                }
            )),
            None => Ok(()),
        }
    };
    let v: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let top = v.as_object().ok_or_else(invalid)?;
    unknown(top, &["manifest_version", "features"])?;
    let version = top
        .get("manifest_version")
        .and_then(Value::as_u64)
        .ok_or_else(invalid)?;
    if version != 1 {
        return Err("unsupported manifest version".into());
    }
    let list = top
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    if list.len() > 256 {
        return Err("more than 256 features".into());
    }
    let mut features = vec![];
    for item in list {
        let o = item.as_object().ok_or_else(invalid)?;
        unknown(o, &["name", "class", "required_rdy"])?;
        let text = |k: &str| o.get(k).and_then(Value::as_str).ok_or_else(invalid);
        let name = text("name")?;
        let class = match text("class")? {
            "compatible" => FeatureClass::Compatible,
            "read_only_compatible" => FeatureClass::ReadOnlyCompatible,
            "incompatible" => FeatureClass::Incompatible,
            _ => return Err(invalid()),
        };
        features.push(feature(name, class, text("required_rdy")?));
    }
    let m = manifest(features);
    m.validate().map_err(|e| e.to_string())?;
    Ok(m)
}

/// Noncanonical manifests (D-WWAK7; limits before allocation): the bounded parser refuses each
/// text with the reason the general parse gave, a collection over its limit before any storage
/// grows for it, and a refused allocation as `Allocation`, never an abort.
#[test]
fn ok1_noncanonical_manifests_parse_within_their_bounds() {
    use alloc_probe::{largest, refusing};
    use rdy_store::format::{ManifestError, ParseFailure};
    // 30,000 zeros after leading whitespace: refused by count, with no storage grown for them
    let zeros = vec!["0"; 30_000].join(",");
    let hostile = format!(" {{\"manifest_version\":1,\"features\":[{zeros}]}}");
    assert_eq!(hostile.len(), 60_036);
    let (got, big) = largest(|| StoreFeatures::parse_fallible(hostile.as_bytes()));
    assert_eq!(
        got,
        Err(ParseFailure::Manifest(ManifestError(
            "more than 256 features".into()
        )))
    );
    assert!(big < 1024, "a request of {big} bytes");
    let (got, big) = largest(|| StoreFeatures::parse(hostile.as_bytes()));
    assert_eq!(got.unwrap_err().to_string(), "more than 256 features");
    assert!(big < 1024, "a request of {big} bytes");
    // a refused allocation is Allocation, canonical or not
    let one = manifest(vec![feature("a", FeatureClass::Compatible, "1.0.0")]);
    let spaced = "{ \"features\" : [ {\"required_rdy\":\"1.0.0\", \"class\":\"compatible\", \"name\":\"a\"} ],\n \"manifest_version\" : 1 }";
    assert_eq!(
        StoreFeatures::parse_fallible(spaced.as_bytes()),
        Ok(one.clone())
    );
    assert_eq!(
        refusing(|| StoreFeatures::parse_fallible(spaced.as_bytes())),
        Err(ParseFailure::Allocation)
    );
    let canonical = one.to_json();
    assert_eq!(
        refusing(|| StoreFeatures::parse_fallible(&canonical)),
        Err(ParseFailure::Allocation)
    );
    // an empty manifest needs no allocation at all
    assert_eq!(
        refusing(|| StoreFeatures::parse_fallible(b" {\"features\":[],\"manifest_version\":1} ")),
        Ok(baseline_features())
    );
    // every case reads as the general parse read it
    let f = |name: &str| {
        format!("{{\"name\":\"{name}\",\"class\":\"compatible\",\"required_rdy\":\"1.0.0\"}}")
    };
    let nested = |depth: usize| {
        format!(
            "{{\"manifest_version\":1,\"features\":[],\"deep\":{}{}}}",
            "[".repeat(depth),
            "]".repeat(depth)
        )
    };
    let many_objects = vec![f("a"); 300].join(",");
    let mut cases: Vec<String> = vec![
        "".into(),
        " ".into(),
        "{}".into(),
        "{\"manifest_version\":1,\"features\":[]}".into(),
        " \t\r\n{\"manifest_version\":1,\"features\":[]}\n\t ".into(),
        "{\"manifest\\u005fversion\":1,\"features\":[]}".into(),
        "{\"manifest_version\":2,\"manifest_version\":1,\"features\":[]}".into(),
        "{\"manifest_version\":1,\"manifest_version\":2,\"features\":[]}".into(),
        "{\"manifest_version\":1,\"features\":[{}],\"features\":[]}".into(),
        "{\"manifest_version\":1.0,\"features\":[]}".into(),
        "{\"manifest_version\":1e0,\"features\":[]}".into(),
        "{\"manifest_version\":-1,\"features\":[]}".into(),
        "{\"manifest_version\":-0,\"features\":[]}".into(),
        "{\"manifest_version\":01,\"features\":[]}".into(),
        "{\"manifest_version\":18446744073709551616,\"features\":[]}".into(),
        "{\"manifest_version\":18446744073709551615,\"features\":[]}".into(),
        "{\"manifest_version\":true,\"features\":[]}".into(),
        "{\"manifest_version\":null,\"features\":[]}".into(),
        "{\"manifest_version\":1,\"features\":[],}".into(),
        "{\"manifest_version\":1,\"features\":[1,]}".into(),
        "{\"manifest_version\":1,\"features\":[]} x".into(),
        "{\"manifest_version\":1,\"features\":[]}{}".into(),
        "{\"manifest_version\":1 \"features\":[]}".into(),
        "{\"manifest_version\":1,\"features\":[],\"b\":1,\"a\":[}".into(),
        "{\"manifest_version\":1,\"features\":[],\"b\":1,\"a\":2}".into(),
        "{\"manifest_version\":7,\"features\":3,\"zz\":{\"x\":[1,2,{\"y\":null}]}}".into(),
        "{\"manifest_version\":1,\"features\":[],\"\\u00e9t\\u00e9\":1}".into(),
        "{\"manifest_version\":1,\"features\":[],\"\\ud83d\\ude00\":1,\"\\u00ff\":1}".into(),
        "{\"manifest_version\":1,\"features\":[],\"\\ud800\":1}".into(),
        "{\"manifest_version\":1,\"features\":[],\"\\udc00x\":1}".into(),
        "{\"manifest_version\":1,\"features\":[],\"\\x\":1}".into(),
        "{\"manifest_version\":1,\"features\":[],\"\\u12\":1}".into(),
        "{\"manifest_version\":1,\"features\":[],\"a\tb\":1}".into(),
        "{\"manifest_version\":1,\"features\":[],\"a\\/b\\\\c\\\"d\\b\\f\\n\\r\\t\":1}".into(),
        "{\"manifest_version\":1,\"features\":[\"a\"]}".into(),
        "{\"manifest_version\":1,\"features\":[1,2,3]}".into(),
        format!("{{\"manifest_version\":1,\"features\":[{many_objects}]}}"),
        format!("{{\"manifest_version\":1,\"features\":[{many_objects},]}}"),
        format!(
            "{{\"manifest_version\":1,\"features\":[{},{{\"name\":1}}]}}",
            f("a")
        ),
        "{\"manifest_version\":1,\"features\":[{\"name\":1},{\"why\":1}]}".into(),
        "{\"manifest_version\":1,\"features\":[{\"name\":\"\\u0061\",\"class\":\"compatible\",\"required_rdy\":\"1.0.0\"}]}".into(),
        "{\"manifest_version\":1,\"features\":[{\"name\":\"a\",\"name\":\"b\",\"class\":\"incompatible\",\"required_rdy\":\"1.0.0\"}]}".into(),
        "{\"manifest_version\":1,\"features\":[{\"name\":\"a\",\"class\":\"compatible\",\"required_rdy\":\"1.0.0\",\"z\":1,\"y\":[}]}".into(),
        "{\"manifest_version\":1,\"features\":[{\"name\":\"a\",\"class\":\"compatibl\\u0065\",\"required_rdy\":\"1.0.0\"}]}".into(),
        "{\"manifest_version\":1,\"features\":[{\"name\":\"a\",\"class\":\"compatible\",\"required_rdy\":1}]}".into(),
        format!(
            "{{\"manifest_version\":1,\"features\":[{},{}]}}",
            f("b"),
            f("a")
        ),
        format!(
            "{{\"manifest_version\":1,\"features\":[],\"{}\":1}}",
            "\u{e9}".repeat(100)
        ),
        "\u{feff}{\"manifest_version\":1,\"features\":[]}".into(),
        "{\"manifest_version\":1,\"features\":[],\"k\":\"\u{e9}\"}".into(),
        "[]".into(),
        "null".into(),
        "\"text\"".into(),
        "1".into(),
        "{\"manifest_version\":1,\"features\":[],\"n\":[0,-0,1.5,-2e-3,1E+9,0.0]}".into(),
        "{\"manifest_version\":1,\"features\":[],\"n\":[1.]}".into(),
        "{\"manifest_version\":1,\"features\":[],\"n\":[.5]}".into(),
        "{\"manifest_version\":1,\"features\":[],\"n\":[+1]}".into(),
        "{\"manifest_version\":1,\"features\":[],\"n\":[1e]}".into(),
        "{\"manifest_version\":1,\"features\":[],\"n\":[tru]}".into(),
        "{\"manifest_version\":1,\"features\":[],\"n\":[true,false,null]}".into(),
        "{\"manifest_version\":1,\"features\":[],\"n\":1e400}".into(),
        "{\"manifest_version\":1,\"features\":[],\"n\":-1e400}".into(),
        "{\"manifest_version\":1,\"features\":[],\"n\":1e-400}".into(),
        format!(
            "{{\"manifest_version\":1,\"features\":[],\"n\":{}}}",
            "9".repeat(400)
        ),
        format!(
            "{{\"manifest_version\":{},\"features\":[]}}",
            "1".repeat(30)
        ),
    ];
    for depth in [1, 126, 127, 128, 129, 400] {
        cases.push(nested(depth));
    }
    let mut cases: Vec<Vec<u8>> = cases.into_iter().map(String::into_bytes).collect();
    cases.push(b"{\"manifest_version\":1,\"features\":[],\"\xff\":1}".to_vec());
    cases.push(b"{\"manifest_version\":1,\"features\":[]}\xff".to_vec());
    for case in &cases {
        let want = reference_reason(case);
        let got = StoreFeatures::parse(case).map_err(|e| e.to_string());
        assert_eq!(got, want, "{:?}", String::from_utf8_lossy(case));
        let fallible = StoreFeatures::parse_fallible(case).map_err(|e| match e {
            ParseFailure::Manifest(m) => m.to_string(),
            ParseFailure::Allocation => "allocation".into(),
        });
        assert_eq!(fallible, want, "{:?}", String::from_utf8_lossy(case));
    }
}

/// The manifest's refusals stay within their bounds too (limits before allocation): an overlong
/// name, required version or unknown key is refused with the general parse's reason and no
/// request near its length, and every refusal's text grows fallibly, so a refused allocation is
/// `Allocation`, never an abort.
#[test]
fn ok1_manifest_refusals_allocate_within_their_bounds() {
    use alloc_probe::{largest, refusing};
    use rdy_store::format::{ManifestError, ParseFailure, valid_required_rdy};
    let one = |name: &str, required: &str, extra: &str| {
        format!(
            "{{\"manifest_version\":1,\"features\":[{{\"name\":\"{name}\",\"class\":\"compatible\",\"required_rdy\":\"{required}\"{extra}}}]}}"
        )
    };
    let dots = ".".repeat(60_000);
    let long_key = "k".repeat(60_000);
    let wide_key = "\u{e9}".repeat(20_000);
    let cases: Vec<(String, String)> = vec![
        (one("a", &dots, ""), "invalid required_rdy".into()),
        (
            one("a", &format!("1.2.{}", "3".repeat(60_000)), ""),
            "invalid required_rdy".into(),
        ),
        (
            one("a", &"\\u0031".repeat(10_000), ""),
            "invalid required_rdy".into(),
        ),
        (
            one(&"a".repeat(60_000), "1.0.0", ""),
            "invalid feature name".into(),
        ),
        (
            one(&"\\u0061".repeat(10_000), "1.0.0", ""),
            "invalid feature name".into(),
        ),
        (
            format!("{{\"manifest_version\":1,\"features\":[],\"{long_key}\":1}}"),
            format!("unknown field {}\u{2026}", "k".repeat(80)),
        ),
        (
            one("a", "1.0.0", &format!(",\"{wide_key}\":1")),
            format!("unknown field {}\u{2026}", "\u{e9}".repeat(80)),
        ),
        (
            format!(
                "{{\"manifest_version\":1,\"features\":[],\"{}\":1}}",
                "\\u00e9".repeat(10_000)
            ),
            format!("unknown field {}\u{2026}", "\u{e9}".repeat(80)),
        ),
    ];
    for (text, want) in &cases {
        let short = &text[..60];
        assert!(text.len() <= 65_536, "{short}");
        assert_eq!(
            reference_reason(text.as_bytes()),
            Err(want.clone()),
            "{short}"
        );
        let (got, big) = largest(|| StoreFeatures::parse_fallible(text.as_bytes()));
        assert_eq!(
            got,
            Err(ParseFailure::Manifest(ManifestError(want.clone()))),
            "{short}"
        );
        assert!(big < 1024, "{short}: a request of {big} bytes");
        let (got, big) = largest(|| StoreFeatures::parse(text.as_bytes()));
        assert_eq!(got.unwrap_err().to_string(), *want, "{short}");
        assert!(big < 1024, "{short}: a request of {big} bytes");
        // the refusal's own text is storage too: refused, it is Allocation
        assert_eq!(
            refusing(|| StoreFeatures::parse_fallible(text.as_bytes())),
            Err(ParseFailure::Allocation),
            "{short}"
        );
    }
    // every other refusal reason grows its text fallibly as well
    let zeros = vec!["0"; 300].join(",");
    let f = |name: &str| {
        format!("{{\"name\":\"{name}\",\"class\":\"compatible\",\"required_rdy\":\"1.0.0\"}}")
    };
    for text in [
        "{".to_string(),
        "{\"manifest_version\":2,\"features\":[]}".into(),
        format!("{{\"manifest_version\":1,\"features\":[{zeros}]}}"),
        format!(
            "{{\"manifest_version\":1,\"features\":[{},{}]}}",
            f("a"),
            f("a")
        ),
        format!(
            "{{\"manifest_version\":1,\"features\":[{},{}]}}",
            f("b"),
            f("a")
        ),
        format!("{{\"manifest_version\":1,\"features\":[{}]}}", f("A")),
        " ".repeat(65_537),
    ] {
        assert!(
            StoreFeatures::parse_fallible(text.as_bytes()).is_err(),
            "{text:.60}"
        );
        assert_eq!(
            refusing(|| StoreFeatures::parse_fallible(text.as_bytes())),
            Err(ParseFailure::Allocation),
            "{text:.60}"
        );
    }
    // the version rule itself collects nothing
    let (ok, big) = largest(|| valid_required_rdy(&dots));
    assert!(!ok);
    assert!(big < 1024, "a request of {big} bytes");
    for (v, want) in [
        ("1.0.0", true),
        ("0.23.0", true),
        ("4294967295.4294967295.4294967295", true),
        ("4294967296.0.0", false),
        ("1.0", false),
        ("1.0.0.0", false),
        ("01.0.0", false),
        ("1..0", false),
        ("", false),
        ("1.0.x", false),
    ] {
        assert_eq!(valid_required_rdy(v), want, "{v}");
    }
}

/// `n` unknown compatible features, `f000` on, sorted by name.
fn unknown_compatible(n: usize) -> Vec<StoreFeature> {
    (0..n)
        .map(|i| feature(&format!("f{i:03}"), FeatureClass::Compatible, "1.0.0"))
        .collect()
}

/// A project whose log and `FORMAT` list `features`, reopened.
fn project_with(features: &StoreFeatures) -> (tempfile::TempDir, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    s.set_refs_unchecked_for_tests(vec![(REF_STORE_FEATURES.into(), features.to_json())])
        .unwrap();
    rdy_store::format::write_format_file(&dir, features).unwrap();
    drop(s);
    (base, dir)
}

/// D-WWAK7: a first use whose merged manifest would break the manifest's bounds (more than 256
/// features) is refused before `FORMAT` or the log changes; one that reaches the bound lands.
#[test]
fn ok1_first_use_refuses_a_manifest_over_its_bounds() {
    let _known = inject_test_feature(FeatureClass::Compatible, kinds::BLOB_CHUNK);
    let full = manifest(unknown_compatible(256));
    let (_base, dir) = project_with(&full);
    let mut s = Store::open(&dir).unwrap();
    assert_eq!(s.features(), &full);
    let end = s.log_end();
    let before = tree(&dir.join("segments"));
    let mirror = std::fs::read(dir.join(FORMAT_FILE)).unwrap();
    let e = s.put_blob(b"first use").unwrap_err();
    assert_eq!(
        format_error(e),
        (
            "store feature manifest is invalid: more than 256 features".into(),
            "restore the store from a backup".into()
        )
    );
    assert_eq!(s.log_end(), end, "nothing appended");
    assert_eq!(log_manifest(&s), full);
    assert_eq!(s.features(), &full);
    assert_eq!(std::fs::read(dir.join(FORMAT_FILE)).unwrap(), mirror);
    drop(s);
    assert_eq!(tree(&dir.join("segments")), before);
    Store::open(&dir).unwrap();
    // 255 and the first use: exactly the bound
    let (_base, dir) = project_with(&manifest(unknown_compatible(255)));
    let mut s = Store::open(&dir).unwrap();
    s.put_blob(b"first use").unwrap();
    assert_eq!(log_manifest(&s).features.len(), 256);
    assert!(log_manifest(&s).has(TEST_FEATURE));
    drop(s);
    Store::open(&dir).unwrap();
}

/// D-WWAK7: only the upgrade transaction writes `meta/upgrade`. Every public write and seal of
/// a project or a holder refuses it, and nothing is appended.
#[test]
fn ok1_public_writes_refuse_the_upgrade_ref() {
    use rdy_store::format::REF_UPGRADE;
    let refused = "only the store upgrade writes ref meta/upgrade";
    for holder in [false, true] {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("s");
        let mut s = if holder {
            Store::create_holder(&dir, rdy_model::RepoId([3; 16])).unwrap()
        } else {
            Store::create(&dir).unwrap()
        };
        let item = |s: &Store| {
            (
                kinds::REF_UPDATE,
                postcard::to_allocvec(&kinds::RefUpdate {
                    name: REF_UPGRADE.into(),
                    value: b"not a receipt".to_vec(),
                    at: s.log_end(),
                })
                .unwrap(),
            )
        };
        let end = s.log_end();
        let refusal = |r: Result<(), StoreError>| match r {
            Err(StoreError::Refused(m)) => assert_eq!(m, refused, "holder {holder}"),
            other => panic!("holder {holder}: {other:?}"),
        };
        refusal(s.write(vec![item(&s)]).map(|_| ()));
        s.set_held(true).unwrap();
        refusal(s.write(vec![item(&s)]).map(|_| ()));
        refusal(s.seal(vec![item(&s)]));
        s.set_held(false).unwrap();
        assert_eq!(s.log_end(), end, "holder {holder}: nothing appended");
        assert_eq!(s.get_ref(REF_UPGRADE).unwrap(), None);
        s.check_meta().unwrap();
    }
}

/// D-WWAK7: the sealed log is the authority over its `FORMAT` mirror. A first use that fails
/// puts the mirror back, and an open after a crash that left the mirror ahead of the log (the
/// first use's batch never sealed) opens by the log's features and puts the mirror back, so
/// the store never needs a newer rdy for a feature it does not use.
#[test]
fn ok1_a_failed_first_use_needs_no_newer_build() {
    for class in [FeatureClass::Incompatible, FeatureClass::ReadOnlyCompatible] {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("s");
        let mut s = Store::create(&dir).unwrap();
        let mirror = std::fs::read_to_string(dir.join(FORMAT_FILE)).unwrap();
        assert_eq!(mirror, baseline_features().to_text());
        {
            let _known = inject_test_feature(class, kinds::BLOB_CHUNK);
            let end = s.log_end();
            s.fail_next_append_after(40);
            assert!(s.put_blob(b"first use").is_err(), "{class:?}");
            assert_eq!(s.log_end(), end, "{class:?}: nothing appended");
            assert_eq!(log_manifest(&s), baseline_features());
            assert_eq!(
                std::fs::read_to_string(dir.join(FORMAT_FILE)).unwrap(),
                mirror,
                "{class:?}: FORMAT is the log's again"
            );
        }
        drop(s);
        // an rdy that does not know the feature opens and writes
        let mut s = Store::open(&dir).unwrap();
        s.put_blob(b"a later write").unwrap();
        drop(s);
        // the crash window: FORMAT published ahead of a first use whose batch never sealed
        let ahead = manifest(vec![feature(TEST_FEATURE, class, "99.0.0")]);
        std::fs::write(dir.join(FORMAT_FILE), ahead.to_text()).unwrap();
        let s = Store::open(&dir).unwrap();
        assert_eq!(log_manifest(&s), baseline_features(), "{class:?}");
        assert_eq!(s.features(), &baseline_features());
        assert_eq!(
            std::fs::read_to_string(dir.join(FORMAT_FILE)).unwrap(),
            mirror,
            "{class:?}: the open puts FORMAT back to the log's"
        );
        drop(s);
        Store::open(&dir).unwrap();
    }
}

/// An upgrade receipt's steps are counted before storage grows for them: a receipt of 20,000
/// steps is refused for its step count with no large request, and its other faults keep their
/// order (a receipt that does not decode says so first).
#[test]
fn ok1_upgrade_receipt_steps_are_bounded() {
    use alloc_probe::largest;
    use rdy_store::format::UpgradeReceipt;
    let receipt = |steps: &str, extra: &str| {
        format!(
            "{{\"from\":\"{}\",\"to\":\"{}\",\"steps\":[{steps}],\"source_end\":{{\"segment\":0,\"offset\":0}},\"backup\":\".store-upgrade-{}-{}\"{extra}}}",
            "0".repeat(64),
            "1".repeat(64),
            "a".repeat(64),
            "b".repeat(64)
        )
    };
    let many = vec!["\"\""; 20_000].join(",");
    let text = receipt(&many, "");
    let (got, big) = largest(|| UpgradeReceipt::parse(text.as_bytes()));
    assert_eq!(got.unwrap_err(), "upgrade receipt has more than 256 steps");
    assert!(big < 16_384, "a request of {big} bytes");
    assert_eq!(
        UpgradeReceipt::parse(receipt(&many, ",\"x\":1").as_bytes()).unwrap_err(),
        "upgrade receipt does not decode"
    );
    let not_text = format!("{many},1");
    assert_eq!(
        UpgradeReceipt::parse(receipt(&not_text, "").as_bytes()).unwrap_err(),
        "upgrade receipt does not decode"
    );
    let two = UpgradeReceipt::parse(receipt("\"a\",\"b\"", "").as_bytes()).unwrap();
    assert_eq!(two.steps, ["a", "b"]);
}

/// Fix round 3: a first use whose batch is durable (marker and sync) and whose index update
/// then fails is rolled back only if the truncate works. When the truncate fails too, the
/// marked batch stays in the log, so `FORMAT` keeps the manifest it published and the store
/// reopens with the feature. Both the write path and a held batch's seal.
#[test]
fn ok1_a_first_use_that_cannot_roll_back_keeps_format() {
    let _known = inject_test_feature(FeatureClass::Compatible, kinds::BLOB_CHUNK);
    let with = manifest(vec![feature(
        TEST_FEATURE,
        FeatureClass::Compatible,
        "99.0.0",
    )]);
    let format_text = |dir: &Path| std::fs::read_to_string(dir.join(FORMAT_FILE)).unwrap();
    for held in [false, true] {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("s");
        let mut s = Store::create(&dir).unwrap();
        if held {
            s.set_held(true).unwrap();
            s.put_blob(b"first use").unwrap();
            s.fail_next_index_apply();
            s.fail_next_rollback();
            assert!(s.seal(vec![]).is_err(), "held: the seal fails");
        } else {
            s.fail_next_index_apply();
            s.fail_next_rollback();
            assert!(s.put_blob(b"first use").is_err(), "the write fails");
        }
        assert_eq!(
            format_text(&dir),
            with.to_text(),
            "held {held}: FORMAT keeps the published manifest"
        );
        drop(s);
        let s = Store::open(&dir).unwrap_or_else(|e| panic!("held {held}: reopen: {e:?}"));
        assert_eq!(
            log_manifest(&s),
            with,
            "held {held}: the marked batch is the log's"
        );
        assert_eq!(s.features(), &with);
        assert_eq!(format_text(&dir), with.to_text());
    }
}
