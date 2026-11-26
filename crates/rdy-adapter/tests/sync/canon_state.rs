//! r59017 (D-GGTYA, D-4PV25): the format-5 fixture of a canon tip's state. Its source is the
//! frozen format-4 canon history `sync4-canon-history.bin`: a new holder takes it as a seed, and
//! the adapter's `Snapshot::prepare_canon_state` exports the state of the history's root as one
//! batch. The history's 19 keys are the full-history count a later completion reads.
//!
//! `sync5-canon-state.bin` and its expect file are frozen once by the ignored writer below; the
//! reader checks the export against them. The expect file's `receiver_features` is the manifest
//! the state stream's header lists: what a receiver is expected to hold after its take, which a
//! later take of the canon state checks.
use rdy_adapter::StoreHandle;
use rdy_adapter::stream::{ObjectKey, StreamPurpose, check_batch};
use rdy_adapter::sync::CanonMove;
use rdy_model::{Hash, RepoId};
use rdy_store::sync_stream::check_canon_state_batch;
use serde_json::{Value, json};
use std::fs::File;
use std::path::{Path, PathBuf};

/// The source history's root landing and repo.
const SOURCE_ROOT: &str = "d91a816b6a2eed457b598edf90b3684a457fe608f4b12be581201f49f34f4e75";
const SOURCE_REPO: &str = "8a55d733ad6ec262d2fd59d66db4ba8f";
const SOURCE_DIGEST: &str = "8d51365e1d9bd84bf9c00526e672d401298d0128cdf3590d1c09f50eb5f746e4";
/// The source history's keys: the full-history count.
const FULL_HISTORY_KEYS: usize = 19;

/// The frozen state fixtures: each file's length and blake3.
const SYNC5_BIN: (&str, usize, &str) = (
    "sync5-canon-state.bin",
    1403,
    "bdd142ebcf05c1d9ad83303718f45bf199f545cffa4f60caf961c654f8d5b8af",
);
const SYNC5_EXPECT: (&str, usize, &str) = (
    "sync5-canon-state.expect.json",
    4332,
    "4ab5dea998333f1ae3426497d7c055ecc097dbca19dc632691a0e7f07831784c",
);

fn sync_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rdy-qa/fixtures/sync")
        .join(name)
}

/// The state of the source's root as one batch, and the expect file's value of it: a new
/// holder seeded with the source history, the state prepared on its snapshot, every key written.
fn export() -> (Vec<u8>, Value) {
    let source = sync_fixture("sync4-canon-history.bin");
    let bytes = std::fs::read(&source).unwrap();
    assert_eq!(Hash::of(&bytes).to_string(), SOURCE_DIGEST);
    let history = check_batch(&bytes[..], None).unwrap();
    assert_eq!(history.keys.len(), FULL_HISTORY_KEYS);
    assert_eq!(history.header.purpose, StreamPurpose::CanonHistory);
    let root: Hash = SOURCE_ROOT.parse().unwrap();
    let repo: RepoId = SOURCE_REPO.parse().unwrap();
    assert_eq!(history.header.root().hash, root);
    assert_eq!(history.header.repo_id, repo);

    let d = tempfile::tempdir().unwrap();
    let holder = StoreHandle::create_holder(&d.path().join("h"), repo).unwrap();
    let checked = holder
        .snapshot()
        .unwrap()
        .check_quarantine(&[File::open(&source).unwrap()])
        .unwrap();
    holder.move_canon(&checked, CanonMove::Seed, None).unwrap();
    let snap = holder.snapshot().unwrap();
    assert_eq!(snap.canon(), Some(root));
    let state = snap.prepare_canon_state(root).unwrap();
    let mut out = Vec::new();
    let report = state.write_keys(state.keys(), &mut out).unwrap();
    assert_eq!(report.keys, state.keys());
    // a state is the tip of that history, and the SNAPSHOT the stream makes for it
    let snapshot = ObjectKey {
        kind: rdy_store::kinds::SNAPSHOT,
        hash: state.snapshot(),
    };
    for k in state.keys() {
        assert!(
            history.keys.contains(k) || *k == snapshot,
            "{k:?} is of the source history"
        );
    }
    assert!(state.keys().len() < FULL_HISTORY_KEYS);
    let expect = json!({
        "about": [
            "The state of one canon tip as a sync stream of format 5 (purpose canon_state, D-GGTYA, D-4PV25), frozen once.",
            "Its source is the frozen format-4 canon history sync4-canon-history.bin: a new holder of its repo takes it as a seed, and the state of its root is written as one batch with every key. The history's keys are the full-history count.",
            "receiver_features is the manifest the stream's header lists: what a receiver is expected to hold after its take of the state. It is expected only: a later take of the canon state checks it."
        ],
        "known_bytes": state.frame_bytes(),
        "purpose": "canon_state",
        "rdy": "0.59.0",
        "receiver_features": report.header.features,
        "receiver_features_status": "expected",
        "source": {
            "digest": SOURCE_DIGEST,
            "fixture": "sync4-canon-history.bin",
            "full_history_keys": FULL_HISTORY_KEYS,
            "repo_id": SOURCE_REPO,
            "root": {"hash": SOURCE_ROOT, "kind": rdy_store::kinds::LANDING},
        },
        "stream": report,
        "stream_format": 5,
    });
    (out, expect)
}

fn pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap();
    s.push('\n');
    s
}

/// Writes the two state fixtures once. Run by hand, never by the gate, after the reader fails:
/// `cargo test -p rdy-adapter --test sync r59017_write_state_fixture_once -- --ignored`. It
/// refuses a file that exists.
#[test]
#[ignore]
fn r59017_write_state_fixture_once() {
    for (name, _, _) in [SYNC5_BIN, SYNC5_EXPECT] {
        assert!(
            !sync_fixture(name).exists(),
            "{name}: the state fixtures are written once: never rewrite them"
        );
    }
    let (bin, expect) = export();
    std::fs::write(sync_fixture(SYNC5_BIN.0), &bin).unwrap();
    std::fs::write(sync_fixture(SYNC5_EXPECT.0), pretty(&expect)).unwrap();
    for name in [SYNC5_BIN.0, SYNC5_EXPECT.0] {
        let b = std::fs::read(sync_fixture(name)).unwrap();
        println!("{name}: {} bytes, blake3 {}", b.len(), Hash::of(&b));
    }
}

#[test]
fn r59017_state_fixture_reads_as_frozen() {
    for (name, len, digest) in [SYNC5_BIN, SYNC5_EXPECT] {
        let b = std::fs::read(sync_fixture(name)).unwrap();
        assert_eq!(
            (b.len(), Hash::of(&b).to_string()),
            (len, digest.to_string()),
            "{name}"
        );
    }
    let bin = std::fs::read(sync_fixture(SYNC5_BIN.0)).unwrap();
    let expect: Value =
        serde_json::from_slice(&std::fs::read(sync_fixture(SYNC5_EXPECT.0)).unwrap()).unwrap();
    // the export of this build makes the same bytes and the same expect file
    let (now, now_expect) = export();
    assert_eq!(now, bin, "the export makes the frozen bytes");
    assert_eq!(now_expect, expect);
    // the frozen batch checks as a canon state, in format 5
    assert_eq!(&bin[8..12], &5u32.to_le_bytes());
    let report = check_canon_state_batch(&bin[..]).unwrap();
    assert_eq!(serde_json::to_value(&report).unwrap(), expect["stream"]);
    assert_eq!(report.header.purpose, StreamPurpose::CanonState);
    assert!(
        report
            .header
            .features
            .has(rdy_store::format::FEATURE_PARTIAL_HISTORY)
    );
    assert_eq!(
        expect["receiver_features"],
        serde_json::to_value(&report.header.features).unwrap()
    );
    let reports = rdy_engine::stream_check::check_state_batches(
        vec![&bin[..]],
        Some(SOURCE_REPO.parse().unwrap()),
    )
    .unwrap();
    assert_eq!(reports, [report]);
    // the source keeps its bytes
    let source = std::fs::read(sync_fixture("sync4-canon-history.bin")).unwrap();
    assert_eq!(Hash::of(&source).to_string(), SOURCE_DIGEST);
}
