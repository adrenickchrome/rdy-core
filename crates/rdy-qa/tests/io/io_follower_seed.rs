//! Seeding a follower (D-AA4FD: "Seeding: `maintenance::seed_follower(dump, dir)` loads a full
//! dump of the dump format this rdy writes into an empty directory."): the log is the source's,
//! with no tail; a dump of another store format is refused; a failure leaves no unmarked store.
use rdy_engine::follower::{self, FollowerCore};
use rdy_engine::load::{PlaceFault, fail_next_placement_for_tests};
use rdy_engine::{Rdy, RdyError};
use rdy_qa::every_kind;
use rdy_store::dump_chain::ChainEnd;
use rdy_store::format::{FeatureClass, StoreFeature, StoreFeatures, inject_test_feature};
use rdy_store::fsutil::{PrivateWriteStage, with_private_write_probe_for_tests};
use rdy_store::kinds;
#[cfg(unix)]
use rdy_store::pio;
use rdy_store::upgrade::UpgradeStep;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FORMAT_MESSAGE: &str = "this dump's store format differs from the one this rdy writes";
const FORMAT_FIX: &str = "seed the follower from a dump that this rdy's store format wrote";

fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries {
        let p = e.unwrap().path();
        if std::fs::symlink_metadata(&p).unwrap().is_dir() {
            out.insert(p.clone(), None);
            out.extend(tree(&p));
        } else {
            out.insert(p.clone(), Some(std::fs::read(&p).unwrap()));
        }
    }
    out
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/dump")
        .join(name)
}

fn triple(e: &RdyError) -> (&str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

/// A small project, its full dump.
fn small(scratch: &Path) -> (PathBuf, PathBuf) {
    let root = scratch.join("small");
    let r = Rdy::init(&root, "rdy-standard").unwrap();
    let file = scratch.join("small.rdydump");
    r.read().dump(&file).unwrap();
    r.close().unwrap();
    (root, file)
}

#[test]
fn mh6_seed_is_the_source_log_without_a_tail() {
    let src = every_kind::project();
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("p.rdydump");
    let r = Rdy::open(src.path()).unwrap();
    let summary = r.read().dump(&file).unwrap();
    let source_facts = every_kind::facts(&r);
    r.close().unwrap();
    let dump_bytes = std::fs::read(&file).unwrap();
    let to = scratch.path().join("f");
    let end: ChainEnd = follower::seed(&file, &to).unwrap();
    assert_eq!(end, summary.chain_end());
    assert_eq!(
        std::fs::read(&file).unwrap(),
        dump_bytes,
        "the dump is unchanged"
    );
    // the segments, byte for byte: no upgrade, origin, reconcile or mark frame
    let segs = |p: &Path| tree(&p.join(".rdy/store/segments"));
    let strip = |t: BTreeMap<PathBuf, Option<Vec<u8>>>, root: &Path| {
        t.into_iter()
            .map(|(p, b)| (p.strip_prefix(root).unwrap().to_path_buf(), b))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(
        strip(segs(&to), &to),
        strip(segs(src.path()), src.path()),
        "the log"
    );
    // the mark: the dump's end and every FILE record's content hash, sorted by path
    let mark = rdy_store::follower::read_mark(&to.join(".rdy/store"))
        .unwrap()
        .unwrap();
    assert_eq!(mark.chain, end);
    assert_eq!(mark.chain.repo_id, summary.repo_id);
    let mut carried = vec![];
    let (mut reader, _, _) =
        rdy_store::dump::DumpReader::open(std::io::Cursor::new(&dump_bytes)).unwrap();
    while let Some(item) = reader.next().unwrap() {
        if let rdy_store::dump::DumpItem::File { path, bytes } = item {
            carried.push((path, rdy_model::Hash::of(&bytes)));
        }
    }
    carried.sort();
    assert!(!carried.is_empty());
    let listed: Vec<_> = mark
        .files
        .iter()
        .map(|f| (f.path.clone(), f.hash))
        .collect();
    assert_eq!(listed, carried);
    // the follower reads the same, and its dump is the source's: FOLLOWER is outside it
    let core = FollowerCore::open(&to).unwrap();
    assert_eq!(core.end().unwrap(), end.end);
    let again = scratch.path().join("again.rdydump");
    core.read().unwrap().dump(&again).unwrap();
    core.fsck().unwrap();
    drop(core);
    assert_eq!(std::fs::read(&again).unwrap(), dump_bytes, "the dump bytes");
    // the same facts: reopening the source shows nothing moved
    let r = Rdy::open(src.path()).unwrap();
    assert_eq!(every_kind::facts(&r), source_facts);
    r.close().unwrap();
}

#[test]
fn mh6_seed_accepts_current_feature_subsets_and_refuses_upgrades() {
    let d = tempfile::tempdir().unwrap();
    let (root, file) = small(d.path());
    // a current baseline subset, in a new path and in an existing empty directory
    follower::seed(&file, &d.path().join("new/deep")).unwrap();
    let empty = d.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    follower::seed(&file, &empty).unwrap();
    let _ = root;

    // a nonempty target, untouched
    let full = d.path().join("full");
    std::fs::create_dir(&full).unwrap();
    std::fs::write(full.join("x"), b"x").unwrap();
    let e = follower::seed(&file, &full).unwrap_err();
    assert_eq!(
        triple(&e),
        (
            "E_ALREADY_EXISTS",
            format!("{} is not empty", full.display()).as_str(),
            "choose an empty directory for the follower"
        )
    );
    assert_eq!(tree(&full).len(), 1);

    // an increment, whichever format it has, before the format cases
    let increment = (
        "E_DUMP_CHAIN",
        "a follower is seeded from a full dump, not an increment",
        "seed the follower from a full dump of the source store",
    );
    let target = d.path().join("no");
    for name in ["v3-s18-chain-1.rdydump", "v3-s18-holder-chain-1.rdydump"] {
        let e = follower::seed(&fixture(name), &target).unwrap_err();
        assert_eq!(triple(&e), increment, "{name}");
        assert!(!target.exists(), "{name}");
    }
    let (root2, full2) = {
        let root = d.path().join("inc");
        let mut r = Rdy::init(&root, "rdy-standard").unwrap();
        let f = d.path().join("inc-full.rdydump");
        let s = r.read().dump(&f).unwrap();
        r.store_mut()
            .set_refs(vec![("t/a".into(), vec![1])])
            .unwrap();
        r.seal_open().unwrap();
        (root, (s, r))
    };
    let (s, r) = full2;
    let inc = d.path().join("inc.rdydump");
    r.read().dump_after(&s.chain_end(), &inc).unwrap();
    r.close().unwrap();
    let _ = root2;
    assert_eq!(
        triple(&follower::seed(&inc, &target).unwrap_err()),
        increment
    );

    // every released older dump format
    for name in [
        "v2-s15.rdydump",
        "v2-s15-holder.rdydump",
        "v3-s18.rdydump",
        "v3-s18-chain-full.rdydump",
        "v3-s18-holder.rdydump",
    ] {
        let e = follower::seed(&fixture(name), &target).unwrap_err();
        assert_eq!(
            triple(&e),
            ("E_DUMP_CHAIN", FORMAT_MESSAGE, FORMAT_FIX),
            "{name}"
        );
        assert!(!target.exists(), "{name}");
    }
    // ...which a normal load still reads
    let loaded = d.path().join("loaded");
    let summary = rdy_engine::load::load(&fixture("v3-s18.rdydump"), &loaded).unwrap();
    assert_eq!(summary.dump_format, 3);

    // an unknown read-only or incompatible feature: this binary cannot write the manifest
    for (i, class) in [FeatureClass::ReadOnlyCompatible, FeatureClass::Incompatible]
        .into_iter()
        .enumerate()
    {
        let root = d.path().join(format!("feature{i}"));
        let file = d.path().join(format!("feature{i}.rdydump"));
        {
            let _known = inject_test_feature(class, kinds::BLOB_CHUNK);
            let mut r = Rdy::init(&root, "rdy-standard").unwrap();
            r.store_mut().put_blob(b"a first use").unwrap();
            r.seal_open().unwrap();
            let summary = r.read().dump(&file).unwrap();
            // the increment's refusal comes before its manifest's
            r.store_mut().put_blob(b"a later batch").unwrap();
            r.seal_open().unwrap();
            let later = d.path().join(format!("feature{i}-inc.rdydump"));
            r.read().dump_after(&summary.chain_end(), &later).unwrap();
            r.close().unwrap();
            drop(_known);
            assert_eq!(
                triple(&follower::seed(&later, &target).unwrap_err()),
                increment,
                "{class:?}"
            );
        }
        let e = follower::seed(&file, &target).unwrap_err();
        assert_eq!(
            triple(&e),
            ("E_DUMP_CHAIN", FORMAT_MESSAGE, FORMAT_FIX),
            "{class:?}"
        );
        assert!(!target.exists());
    }

    // a manifest that needs a registered upgrade
    let registry = vec![UpgradeStep {
        id: "add_zz".into(),
        from: StoreFeatures::of(vec![]),
        to: StoreFeatures::of(vec![StoreFeature {
            name: "zz_added".into(),
            class: FeatureClass::Compatible,
            required_rdy: "100.2.0".into(),
        }]),
    }];
    let e = follower::seed_with_registry_for_tests(&file, &target, &registry).unwrap_err();
    assert_eq!(triple(&e), ("E_DUMP_CHAIN", FORMAT_MESSAGE, FORMAT_FIX));
    assert!(!target.exists());

    // a malformed stream keeps its diagnostic; so does a damaged body of a current dump
    let bad = d.path().join("bad.rdydump");
    std::fs::write(&bad, b"not a dump").unwrap();
    assert_eq!(follower::seed(&bad, &target).unwrap_err().code, "E_DUMP");
    let mut body = std::fs::read(&file).unwrap();
    let last = body.len() - 200;
    body[last] ^= 0xff;
    std::fs::write(&bad, &body).unwrap();
    assert_eq!(follower::seed(&bad, &target).unwrap_err().code, "E_DUMP");
    assert!(!target.exists());
    // a missing file is the load's own refusal
    let gone = d.path().join("gone.rdydump");
    assert_eq!(
        follower::seed(&gone, &target).unwrap_err().code,
        "E_BAD_INPUT"
    );
}

#[test]
fn mh6_seed_failure_leaves_no_unmarked_store() {
    let d = tempfile::tempdir().unwrap();
    let (root, file) = small(d.path());
    let dump_bytes = std::fs::read(&file).unwrap();
    let source = tree(&root);
    // both shapes of a target come back as they were
    let check = |what: &str, seed: &dyn Fn(&Path) -> Result<ChainEnd, RdyError>| {
        for existing in [false, true] {
            let target = d.path().join(format!("t{}", u8::from(existing)));
            if existing {
                std::fs::create_dir(&target).unwrap();
            }
            assert!(seed(&target).is_err(), "{what}");
            if existing {
                assert!(tree(&target).is_empty(), "{what}: left empty");
                std::fs::remove_dir(&target).unwrap();
            } else {
                assert!(!target.exists(), "{what}: gone again");
            }
        }
        assert_eq!(std::fs::read(&file).unwrap(), dump_bytes, "{what}");
        assert_eq!(tree(&root), source, "{what}");
    };
    // the mark's write and its syncs, by the private replacement's own boundaries
    for stage in [
        PrivateWriteStage::Created,
        PrivateWriteStage::Written,
        PrivateWriteStage::Synced,
        PrivateWriteStage::Renamed,
        PrivateWriteStage::DirSynced,
    ] {
        check(&format!("{stage:?}"), &|target| {
            with_private_write_probe_for_tests(
                move |s, p| {
                    let name = p.file_name().map(|n| n.to_string_lossy().to_string());
                    let mark = matches!(name.as_deref(), Some(".FOLLOWER.tmp" | "FOLLOWER"))
                        || matches!(s, PrivateWriteStage::DirSynced);
                    if s == stage && mark {
                        Err(std::io::Error::other("injected"))
                    } else {
                        Ok(())
                    }
                },
                || follower::seed(&file, target),
            )
        });
    }
    // the rename into place and the directory's sync after it
    for fault in [PlaceFault::Rename, PlaceFault::SyncAfterRename] {
        check(&format!("{fault:?}"), &|target| {
            fail_next_placement_for_tests(Some(fault));
            let out = follower::seed(&file, target);
            fail_next_placement_for_tests(None);
            out
        });
    }
    // the store's creation: a target this user cannot write into (root can)
    if !runs_as_root() {
        let target = d.path().join("shut");
        std::fs::create_dir(&target).unwrap();
        let e = {
            let _shut = rdy_qa::platform::ReadOnlyDir::new(&target).unwrap();
            follower::seed(&file, &target)
        };
        assert_eq!(e.unwrap_err().code, "E_IO");
        assert!(tree(&target).is_empty());
    }
    // the check after placement: a dump whose fsck fails
    let forged = d.path().join("forged");
    let forged_file = d.path().join("forged.rdydump");
    {
        let mut r = Rdy::init(&forged, "rdy-standard").unwrap();
        r.store_mut()
            .set_refs_unchecked_for_tests(vec![("view/forged".into(), vec![0u8; 32])])
            .unwrap();
        r.close().unwrap();
        let r = Rdy::open(&forged).unwrap();
        r.read().dump(&forged_file).unwrap();
        r.close().unwrap();
    }
    let target = d.path().join("forged-target");
    let e = follower::seed(&forged_file, &target).unwrap_err();
    assert_eq!(e.code, "E_DUMP", "{e:?}");
    assert!(!target.exists(), "no placed store stays");
    // nothing failed for good: the same seed then succeeds
    let target = d.path().join("ok");
    follower::seed(&file, &target).unwrap();
    let mark = rdy_store::follower::read_mark(&target.join(".rdy/store")).unwrap();
    assert!(mark.is_some(), "a placed store has its mark");
    assert!(!target.join(".rdy/store.loading").exists());
}

/// Root ignores directory modes; Windows has no such account for an ordinary test run.
fn runs_as_root() -> bool {
    #[cfg(unix)]
    {
        pio::own_uid() == 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}
