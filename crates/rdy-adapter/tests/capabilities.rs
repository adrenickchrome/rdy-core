//! Feature-aware adapter capabilities (FW-UP1a D-69AY3): what this build writes and reads, as a
//! host reads it at build time. The public compile contract uses adapter imports only; the store
//! registries appear only inside oracle checks.
use rdy_adapter::maintenance::{
    self, Access, DumpFeaturePair, DumpPair, FeatureClass, FormatCapabilities, StoreFeature,
    StoreFeatures, format_capabilities,
};
use std::path::{Path, PathBuf};

const CHILD: &str = "RDY_RELC3_CHILD";

fn frozen(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rdy-qa/fixtures/dump")
        .join(name)
}

fn unknown(class: FeatureClass) -> StoreFeatures {
    StoreFeatures::of(vec![StoreFeature {
        name: "zz_future".into(),
        class,
        required_rdy: "99.0.0".into(),
    }])
}

fn access_of(features: &StoreFeatures) -> Access {
    match rdy_store::format::check_features(features) {
        Err(_) => Access::Refused,
        Ok(rdy_store::format::StoreAccess::ReadWrite) => Access::ReadWrite,
        Ok(rdy_store::format::StoreAccess::ReadOnly) => Access::ReadOnly,
    }
}

#[test]
fn relc3_capabilities_compile_with_only_adapter_imports() {
    let caps: FormatCapabilities = format_capabilities();
    let _: &[StoreFeature] = &caps.store_reads.known_features;
    let _: &[u32] = &caps.store_reads.manifest_versions;
    let _: &[DumpPair] = &caps.dump_reads.legacy;
    let _: &[DumpFeaturePair] = &caps.dump_reads.features;
    let _: Access = caps.store_reads.unknown_features.compatible;
    let _: Access = caps.store_reads.unknown_features.read_only_compatible;
    let _: Access = caps.store_reads.unknown_features.incompatible;
    let _ = maintenance::formats();
}

#[test]
fn relc3_written_formats_match_existing_format_facts() {
    let caps = format_capabilities();
    assert_eq!(caps.written, maintenance::formats());
    assert_eq!(caps.written.dump, 4);
    assert_eq!(caps.written.stream, 2);
    assert_eq!(caps.written.store, rdy_store::format::baseline_features());
}

#[test]
fn relc3_supported_features_are_not_only_new_store_features() {
    let caps = format_capabilities();
    let known = &caps.store_reads.known_features;
    assert_eq!(*known, {
        let mut v = rdy_store::format::registered_features();
        v.sort();
        v
    });
    // every feature a new store starts with is known, and known features reach beyond them
    for f in &caps.written.store.features {
        assert!(known.contains(f), "{}", f.name);
    }
    assert!(known.len() > caps.written.store.features.len());
    for name in [
        "assessment",
        "check_request",
        "dispute",
        "plan_tension",
        "view_replay",
    ] {
        assert!(known.iter().any(|f| f.name == name), "{name}");
        assert!(
            !caps.written.store.features.iter().any(|f| f.name == name),
            "{name}"
        );
    }
    assert_eq!(
        caps.store_reads.manifest_versions,
        [rdy_store::format::MANIFEST_VERSION]
    );
}

#[test]
fn relc3_dump_reads_cover_every_registered_pair_and_feature_grammar() {
    let caps = format_capabilities();
    let legacy: Vec<(u32, u32)> = caps
        .dump_reads
        .legacy
        .iter()
        .map(|p| (p.dump, p.store))
        .collect();
    assert_eq!(legacy, rdy_store::dump::READS);
    assert_eq!(caps.dump_reads.legacy, caps.written.reads);
    let features: Vec<(u32, u32)> = caps
        .dump_reads
        .features
        .iter()
        .map(|p| (p.dump, p.manifest_version))
        .collect();
    let expect: Vec<(u32, u32)> = rdy_store::dump::READS_FEATURES
        .iter()
        .map(|&m| (rdy_store::dump::DUMP_FORMAT, m))
        .collect();
    assert_eq!(features, expect);

    // the released fixture corpus: every file's formats are covered, and every full dump (a
    // project or a holder) loads through maintenance
    let dir = frozen("");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".rdydump"))
        .collect();
    names.sort();
    assert!(names.len() >= 20);
    let (mut seen_legacy, mut seen_features) = (vec![], false);
    for name in &names {
        let facts = maintenance::inspect_dump(&frozen(name)).unwrap();
        match facts.store {
            maintenance::DumpStore::Legacy(n) => {
                let pair = DumpPair {
                    dump: facts.dump,
                    store: n,
                };
                assert!(caps.dump_reads.legacy.contains(&pair), "{name}");
                if !seen_legacy.contains(&pair) {
                    seen_legacy.push(pair);
                }
            }
            maintenance::DumpStore::Features(m) => {
                let pair = DumpFeaturePair {
                    dump: facts.dump,
                    manifest_version: m.manifest_version,
                };
                assert!(caps.dump_reads.features.contains(&pair), "{name}");
                seen_features = true;
            }
        }
    }
    // the corpus holds every registered pair and a feature dump
    for p in &caps.dump_reads.legacy {
        assert!(seen_legacy.contains(p), "{p:?}");
    }
    assert!(seen_features);
    let mut holders = 0;
    let mut loaded = 0;
    for name in names
        .iter()
        .filter(|n| !n.contains("chain-1") && !n.contains("chain-2"))
    {
        let to = tempfile::tempdir().unwrap();
        let s = maintenance::load(&frozen(name), &to.path().join("t"))
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        holders += usize::from(name.contains("holder"));
        loaded += 1;
        drop(s);
    }
    assert!(loaded >= 10 && holders >= 2, "{loaded} {holders}");
}

#[test]
fn relc3_store_access_matches_unknown_feature_classes() {
    let caps = format_capabilities();
    let u = caps.store_reads.unknown_features;
    assert_eq!(u.compatible, access_of(&unknown(FeatureClass::Compatible)));
    assert_eq!(
        u.read_only_compatible,
        access_of(&unknown(FeatureClass::ReadOnlyCompatible))
    );
    assert_eq!(
        u.incompatible,
        access_of(&unknown(FeatureClass::Incompatible))
    );
    assert_eq!(
        (u.compatible, u.read_only_compatible, u.incompatible),
        (Access::ReadWrite, Access::ReadOnly, Access::Refused)
    );
    // writes follow the same oracle: only a compatible unknown feature permits them
    assert!(rdy_store::format::check_writable(&unknown(FeatureClass::Compatible)).is_ok());
    assert!(rdy_store::format::check_writable(&unknown(FeatureClass::ReadOnlyCompatible)).is_err());
    assert!(rdy_store::format::check_writable(&unknown(FeatureClass::Incompatible)).is_err());
    // a known feature is not unknown: a known name in the wrong class is a malformed manifest
    let wrong = StoreFeatures::of(vec![StoreFeature {
        name: "dispute".into(),
        class: FeatureClass::Compatible,
        required_rdy: "0.1.0".into(),
    }]);
    assert!(rdy_store::format::check_features(&wrong).is_err());
}

#[test]
fn relc3_legacy_dump_support_does_not_claim_legacy_store_open() {
    let caps = format_capabilities();
    assert!(!caps.dump_reads.legacy.is_empty());
    // the store side names no scalar format and no `opens` list
    let v = serde_json::to_value(&caps.store_reads).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["known_features", "manifest_versions", "unknown_features"]
    );
    assert!(!serde_json::to_string(&caps).unwrap().contains("opens"));
    // a scalar store does not open in place
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("p");
    rdy_engine::Rdy::init(&project, "rdy-standard")
        .unwrap()
        .close()
        .unwrap();
    let format = project.join(".rdy/store/FORMAT");
    let found = std::fs::read_dir(project.join(".rdy"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<Vec<_>>();
    let path = if format.exists() {
        format
    } else {
        find_format(&project).unwrap_or_else(|| panic!("no FORMAT in {found:?}"))
    };
    std::fs::write(&path, "rdy store format\nstore_format 18\n").unwrap();
    let e = rdy_adapter::StoreHandle::open(&project)
        .err()
        .expect("a scalar store is refused");
    assert_eq!(e.code, "E_STORE_FORMAT");
}

fn find_format(dir: &Path) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()? {
        let p = e.ok()?.path();
        if p.file_name().is_some_and(|n| n == "FORMAT") {
            return Some(p);
        }
        if p.is_dir()
            && let Some(f) = find_format(&p)
        {
            return Some(f);
        }
    }
    None
}

#[test]
fn relc3_capabilities_need_no_project_config_daemon_or_home() {
    if std::env::var_os(CHILD).is_some() {
        let caps = format_capabilities();
        assert_eq!(caps.written.dump, 4);
        return;
    }
    // run in a child with an empty environment and an empty working directory
    let cwd = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "relc3_capabilities_need_no_project_config_daemon_or_home",
            "--test-threads=1",
        ])
        .env_clear()
        .env(CHILD, "1")
        .current_dir(cwd.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"));
    assert_eq!(std::fs::read_dir(cwd.path()).unwrap().count(), 0);
}

#[test]
fn relc3_serialized_capabilities_are_sorted_and_stable() {
    let a = serde_json::to_string(&format_capabilities()).unwrap();
    let b = serde_json::to_string(&format_capabilities()).unwrap();
    assert_eq!(a, b);
    let caps = format_capabilities();
    let names: Vec<&str> = caps
        .store_reads
        .known_features
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(names, sorted);
    let pairs: Vec<(u32, u32)> = caps
        .dump_reads
        .legacy
        .iter()
        .map(|p| (p.dump, p.store))
        .collect();
    let mut s = pairs.clone();
    s.sort();
    assert_eq!(pairs, s);
    let v: serde_json::Value = serde_json::from_str(&a).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["dump_reads", "store_reads", "written"]);
    assert_eq!(
        v["store_reads"]["unknown_features"]["read_only_compatible"],
        "read_only"
    );
    assert_eq!(
        v["store_reads"]["unknown_features"]["incompatible"],
        "refused"
    );
    assert_eq!(
        v["store_reads"]["unknown_features"]["compatible"],
        "read_write"
    );
    assert_eq!(
        v["dump_reads"]["features"][0],
        serde_json::json!({"dump": 4, "manifest_version": 1})
    );
}
