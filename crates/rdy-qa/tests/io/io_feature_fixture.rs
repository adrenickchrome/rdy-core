//! ok1: the complete fixture set of the written wire format (D-69AY3: "every later rdy reads every
//! dump format that rdy has released"; D-WWAK7: each object records its encoding version, and
//! feature compatibility replaces the format number; D-F0904). The set `v4-f1-e0` holds dump
//! format 4 and sync batch format 2 of a feature store of manifest grammar 1, with no feature and
//! every kind's latest encoding 0. The stem names the set; it is no store format number. Write it
//! once: never rewrite it, and never regenerate it to fix a later reader.
//!
//! The members follow the `(3, 18)` set (`io_dump_fixture_s18`), built by the same shared
//! builders through their current stage selector (`chain_fixture::project_chain_current`,
//! `holder_chain_current`; the standalone builders are shared):
//!
//! - `v4-f1-e0-chain-full`, `-chain-1` and `-chain-2` are a full dump and two increments of one
//!   project, and `v4-f1-e0` its standalone full fixture;
//! - `v4-f1-e0-holder-chain-*` and `v4-f1-e0-holder` are the same for a holder;
//! - `v2-f1-e0.rdysync` is one sync batch, the history of the holder's relayed tag
//!   (`io_sync_fixture::SYNC_TAG`), exported from the standalone holder.
//!
//! Every expect file pins what a load of its files must give, as the `(3, 18)` set pins it, and
//! the formats the set holds (`chain_fixture::current_formats`): the dump and sync formats, the
//! feature manifest and the latest-encoding map. A load of a dump of the current format appends
//! no batch of its own. A future dump format, newly registered feature or new latest encoding
//! needs a new complete set in its own task; [`ok1_written_wire_format_has_a_complete_fixture_set`]
//! refuses a written format without one.
use crate::io_dump_fixture_s15::{
    RELEASED, RELEASED_CHAINS, first_start_logs_moves, fixture, without_moves,
};
use crate::io_dump_fixture_s18 as s18;
use crate::io_sync_fixture::{SYNC_TAG, stream_facts, sync_fixture, take_split};
use rdy_engine::Rdy;
use rdy_engine::stream_check::check_batch;
use rdy_model::Hash;
use rdy_qa::chain_fixture;
use rdy_qa::every_kind;
use rdy_store::dump::{
    DUMP_FORMAT, DumpBatch, DumpItem, DumpReader, DumpStore, DumpWriter, ENCODED_DROPPED_LEN,
    Frame, READS, rec,
};
use rdy_store::format::baseline_features;
use rdy_store::segment::Location;
use rdy_store::sync_stream::STREAM_FORMAT;
use serde_json::{Value, json};
use std::io::BufReader;
use std::path::{Path, PathBuf};

const FULL: &str = "v4-f1-e0.rdydump";
const FULL_EXPECT: &str = "v4-f1-e0.expect.json";
const CHAIN_FULL: &str = "v4-f1-e0-chain-full.rdydump";
const CHAIN_1: &str = "v4-f1-e0-chain-1.rdydump";
const CHAIN_2: &str = "v4-f1-e0-chain-2.rdydump";
const CHAIN_EXPECT: &str = "v4-f1-e0-chain.expect.json";
const HOLDER: &str = "v4-f1-e0-holder.rdydump";
const HOLDER_EXPECT: &str = "v4-f1-e0-holder.expect.json";
const HOLDER_CHAIN_FULL: &str = "v4-f1-e0-holder-chain-full.rdydump";
const HOLDER_CHAIN_1: &str = "v4-f1-e0-holder-chain-1.rdydump";
const HOLDER_CHAIN_2: &str = "v4-f1-e0-holder-chain-2.rdydump";
const HOLDER_CHAIN_EXPECT: &str = "v4-f1-e0-holder-chain.expect.json";
const SYNC: &str = "v2-f1-e0.rdysync";
const SYNC_EXPECT: &str = "v2-f1-e0.expect.json";

/// Each dump member of the set `v4-f1-e0` and its blake3, set when it was written. A change here
/// needs a founder decision.
const MEMBERS: [(&str, &str); 12] = [
    (
        FULL,
        "98eae17a8266e141d81020985c5bd0e98b715fd73f04e01515227ac3bb80db38",
    ),
    (
        FULL_EXPECT,
        "76f66f75d566582ba3aad326a4494b43a635ddcee8cf95ba5be1f84090d59d02",
    ),
    (
        CHAIN_FULL,
        "7f23c524d0280e02100c61f66faf7631d0548f61b39166b77cb6400de14a2f2a",
    ),
    (
        CHAIN_1,
        "5b70a0ff2026dc669e0527f5cfd81284777eee7f30c671c6ffdadb8cfee5e339",
    ),
    (
        CHAIN_2,
        "0fbf09ec74745d52f8c0c948a866248616f6667184574f0bc8b30caec9bd76f8",
    ),
    (
        CHAIN_EXPECT,
        "1498668a30224e7ceb4e0c8ab4e79a2e7b74f785162e848688041742283375da",
    ),
    (
        HOLDER,
        "dbd9823bac8e2031afc9631285b8887f72d2e144d7823e8c9bf33ac2e3558565",
    ),
    (
        HOLDER_EXPECT,
        "9501975b36034f0a3da5d77b309f326c5d51fb871fe99b6065c9d0c98f173257",
    ),
    (
        HOLDER_CHAIN_FULL,
        "1f4f83c314d09cccd59474539c68f9147bb279940601d69c43f17ffcca6fad48",
    ),
    (
        HOLDER_CHAIN_1,
        "065b2827b5a7140929f40ccc9f74ad487b9bce2c098322ce5209f6c2e66b18f2",
    ),
    (
        HOLDER_CHAIN_2,
        "9dbd39c463411c5194f0461611dec57c14c7fb32cb44fc44ada8510e66e47172",
    ),
    (
        HOLDER_CHAIN_EXPECT,
        "5d9a15291ca62b211347d82997e939c3e40b9cd893c66e1914d379f64cf0b670",
    ),
];

/// The sync members of the set and their blake3, set when they were written.
const SYNC_MEMBERS: [(&str, &str); 2] = [
    (
        SYNC,
        "bd76ed879f89470f01b0c96734a5b43c8a4a41848f6184713edc279e7f568077",
    ),
    (
        SYNC_EXPECT,
        "41c739398eff4112f85e8b86bfd324110a4d8e9d97280dcdcaee62164156989c",
    ),
];

/// A complete fixture set of a written wire format: its dump members, its sync members, and the
/// expect file that records the formats it holds.
struct FeatureSet {
    dump: &'static [(&'static str, &'static str)],
    sync: &'static [(&'static str, &'static str)],
    formats_in: &'static str,
}

/// The registered complete sets, oldest first. A set, once registered, never leaves this list.
const SETS: [FeatureSet; 1] = [FeatureSet {
    dump: &MEMBERS,
    sync: &SYNC_MEMBERS,
    formats_in: FULL_EXPECT,
}];

/// The dump members of every registered set: the on-disk inventory of `ha17_` adds them to the
/// released pairs' members.
pub(crate) fn dump_members() -> Vec<&'static str> {
    SETS.iter()
        .flat_map(|s| s.dump.iter().map(|(n, _)| *n))
        .collect()
}

fn read_expect(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture(name)).unwrap()).unwrap()
}

fn read_sync_expect() -> Value {
    serde_json::from_str(&std::fs::read_to_string(sync_fixture(SYNC_EXPECT)).unwrap()).unwrap()
}

/// The current wire formats have a complete registered set, every member of it on disk with its
/// pinned blake3 (`ha17_` and `ok1_` both ask).
pub(crate) fn assert_written_set_is_registered() {
    let now = chain_fixture::current_formats();
    let set = SETS
        .iter()
        .find(|s| read_expect(s.formats_in)["formats"] == now)
        .unwrap_or_else(|| {
            panic!(
                "the written formats {now} have no complete fixture set: freeze one in this task"
            )
        });
    assert_eq!(
        set.dump.len(),
        12,
        "a project and a holder, full and chained"
    );
    assert_eq!(set.sync.len(), 2, "a sync batch and its expect file");
    for (name, digest) in set.dump {
        let bytes = std::fs::read(fixture(name)).unwrap();
        assert_eq!(Hash::of(&bytes).to_string(), *digest, "{name}");
    }
    for (name, digest) in set.sync {
        let bytes = std::fs::read(sync_fixture(name)).unwrap();
        assert_eq!(Hash::of(&bytes).to_string(), *digest, "{name}");
    }
}

/// Writes `bytes` to a new member `p`; refuses when `p` exists.
fn write_new(p: &Path, bytes: &[u8]) {
    s18::write_new(p, bytes);
}

/// The expect file of the chain `files`, as the `(3, 18)` set writes it, with the formats.
fn chain_expect(
    files: &[PathBuf],
    names: [&str; 3],
    c: &chain_fixture::Chain,
    scratch: &Path,
    holder: bool,
) -> Value {
    let mut v = s18::chain_expect(files, names, c, scratch, holder);
    v["formats"] = chain_fixture::current_formats();
    v
}

/// The expect file of a standalone full fixture, as the `(3, 18)` set writes it, with the
/// formats.
fn standalone_expect(
    file: &Path,
    name: &str,
    built_from: &str,
    stop: &str,
    scratch: &Path,
    holder: bool,
) -> Value {
    let mut v = s18::standalone_expect(file, name, built_from, stop, scratch, holder);
    v["formats"] = chain_fixture::current_formats();
    v
}

/// Builds the fourteen members in memory from the shared builders; scratch stores go under
/// `scratch`. The dump members first, then the sync members.
/// Fixture members by file name: `(name, bytes)`.
type Members = Vec<(&'static str, Vec<u8>)>;

fn build_members(scratch: &Path) -> (Members, Members) {
    let mut out: Vec<(&str, Vec<u8>)> = vec![];

    let c = chain_fixture::project_chain_current();
    let s = chain_fixture::project_standalone(&c);
    let d = scratch.join("project");
    std::fs::create_dir_all(&d).unwrap();
    let expect = chain_expect(&c.files(), [CHAIN_FULL, CHAIN_1, CHAIN_2], &c, &d, false);
    for (name, p) in [
        (CHAIN_FULL, &c.full),
        (CHAIN_1, &c.inc1),
        (CHAIN_2, &c.inc2),
    ] {
        out.push((name, std::fs::read(p).unwrap()));
    }
    out.push((CHAIN_EXPECT, s18::pretty(&expect)));
    let expect = standalone_expect(&s.dump, FULL, &c.missing_stop, &s.stop, &d, false);
    out.push((FULL, std::fs::read(&s.dump).unwrap()));
    out.push((FULL_EXPECT, s18::pretty(&expect)));

    let (hc, ups) = chain_fixture::holder_chain_current();
    let hs = chain_fixture::holder_standalone(&hc, &ups);
    let d = scratch.join("holder");
    std::fs::create_dir_all(&d).unwrap();
    let names = [HOLDER_CHAIN_FULL, HOLDER_CHAIN_1, HOLDER_CHAIN_2];
    let expect = chain_expect(&hc.files(), names, &hc, &d, true);
    for (name, p) in names.into_iter().zip(hc.files()) {
        out.push((name, std::fs::read(p).unwrap()));
    }
    out.push((HOLDER_CHAIN_EXPECT, s18::pretty(&expect)));
    let expect = standalone_expect(&hs.dump, HOLDER, &hc.missing_stop, &hs.stop, &d, true);
    out.push((HOLDER, std::fs::read(&hs.dump).unwrap()));
    out.push((HOLDER_EXPECT, s18::pretty(&expect)));

    // the sync batch: the relayed tag's history, exported from the standalone holder
    let bytes = export_sync_tag(&hs.root);
    let (h, _, taken) = take_split(&bytes, &scratch.join("sync"));
    h.close().unwrap();
    let expect = json!({
        "formats": chain_fixture::current_formats(),
        "tag": SYNC_TAG,
        "stream": stream_facts(&bytes),
        "taken": taken,
    });
    let sync = vec![(SYNC, bytes), (SYNC_EXPECT, s18::pretty(&expect))];
    (out, sync)
}

/// The sync batch of [`SYNC_TAG`]'s history, exported by this rdy from the holder at `root`.
fn export_sync_tag(root: &Path) -> Vec<u8> {
    let held = Rdy::open_holder(root).unwrap();
    let tag = held
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, ..)| n == SYNC_TAG)
        .unwrap()
        .1;
    let bytes = every_kind::export_tag(&held, tag);
    held.close().unwrap();
    bytes
}

/// Writes the fourteen members of the set `v4-f1-e0` once. Run by hand, never by the gate, after
/// the readers fail: `cargo test -p rdy-qa --release --test io ok1_write_feature_fixture_set_once
/// -- --ignored`. It refuses a member that exists.
#[test]
#[ignore]
fn ok1_write_feature_fixture_set_once() {
    for (m, _) in MEMBERS {
        assert!(
            !fixture(m).exists(),
            "{m}: the v4-f1-e0 fixtures are written once: never rewrite them"
        );
    }
    for (m, _) in SYNC_MEMBERS {
        assert!(
            !sync_fixture(m).exists(),
            "{m}: the v2-f1-e0 fixtures are written once: never rewrite them"
        );
    }
    assert_eq!(
        chain_fixture::current_formats(),
        json!({
            "dump_format": 4,
            "stream_format": 2,
            "features": baseline_features(),
            "encodings": chain_fixture::current_formats()["encodings"].clone(),
        }),
        "the v4-f1-e0 set holds dump format 4 and sync format 2"
    );
    assert!(
        chain_fixture::current_formats()["encodings"]
            .as_object()
            .unwrap()
            .values()
            .all(|v| *v == json!(0)),
        "every kind's latest encoding is 0"
    );
    let scratch = tempfile::tempdir().unwrap();
    let (dump, sync) = build_members(scratch.path());
    assert_eq!(dump.len(), MEMBERS.len());
    assert_eq!(sync.len(), SYNC_MEMBERS.len());
    for (name, bytes) in &dump {
        write_new(&fixture(name), bytes);
        eprintln!("{name} blake3 {}", Hash::of(bytes));
    }
    for (name, bytes) in &sync {
        write_new(&sync_fixture(name), bytes);
        eprintln!("{name} blake3 {}", Hash::of(bytes));
    }
}

// ---------------------------------------------------------------------------------------------
// the checks

const PROJECT_CHAIN: s18::Input = s18::Input {
    names: &[CHAIN_FULL, CHAIN_1, CHAIN_2],
    expect: CHAIN_EXPECT,
    holder: false,
    stops: &["held_stop", "missing_stop"],
    canaries: s18::PROJECT_CANARIES,
};
const HOLDER_CHAIN: s18::Input = s18::Input {
    names: &[HOLDER_CHAIN_FULL, HOLDER_CHAIN_1, HOLDER_CHAIN_2],
    expect: HOLDER_CHAIN_EXPECT,
    holder: true,
    stops: &["held_stop", "missing_stop"],
    canaries: s18::HOLDER_CANARIES,
};
const PROJECT_FULL: s18::Input = s18::Input {
    names: &[FULL],
    expect: FULL_EXPECT,
    holder: false,
    stops: &["missing_stop"],
    canaries: s18::PROJECT_CANARIES,
};
const HOLDER_FULL: s18::Input = s18::Input {
    names: &[HOLDER],
    expect: HOLDER_EXPECT,
    holder: true,
    stops: &["missing_stop"],
    canaries: s18::HOLDER_CANARIES,
};
const INPUTS: [s18::Input; 4] = [PROJECT_CHAIN, HOLDER_CHAIN, PROJECT_FULL, HOLDER_FULL];

/// The data frames past `end` in the store at `dir`: none, since a dump of the current format
/// needs no upgrade batch.
fn nothing_after(dir: &Path, end: Location, holder: bool, what: &str) {
    let r = s18::open(dir, holder);
    assert_eq!(
        r.store().log_end(),
        end,
        "{what}: a load of the current format appends nothing after the source log"
    );
    r.close().unwrap();
}

#[test]
fn ok1_feature_fixture_members_are_immutable() {
    // A change to any member needs a founder decision. The checksums are fixed here, apart from
    // any writer: a later writer that writes other bytes changes nothing in this check.
    let paths = MEMBERS
        .iter()
        .map(|(n, d)| (fixture(n), *d))
        .chain(SYNC_MEMBERS.iter().map(|(n, d)| (sync_fixture(n), *d)));
    for (p, digest) in paths {
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(Hash::of(&bytes).to_string(), digest, "{}", p.display());
        let refused = std::panic::catch_unwind(|| write_new(&p, b"never"));
        assert!(
            refused.is_err(),
            "{}: the writer refuses an existing member",
            p.display()
        );
        assert_eq!(std::fs::read(&p).unwrap(), bytes, "{}", p.display());
    }
}

#[test]
fn ok1_written_wire_format_has_a_complete_fixture_set() {
    let now = chain_fixture::current_formats();
    assert_eq!(now["dump_format"], json!(DUMP_FORMAT));
    assert_eq!(now["stream_format"], json!(STREAM_FORMAT));
    assert_eq!(now["features"], json!(baseline_features()));
    assert_written_set_is_registered();
    // every expect file of the set names the same formats
    for name in [
        FULL_EXPECT,
        CHAIN_EXPECT,
        HOLDER_EXPECT,
        HOLDER_CHAIN_EXPECT,
    ] {
        assert_eq!(read_expect(name)["formats"], now, "{name}");
    }
    assert_eq!(read_sync_expect()["formats"], now);
}

#[test]
fn ok1_complete_feature_fixture_set_loads() {
    let d = tempfile::tempdir().unwrap();
    let store = json!({"features": baseline_features()});
    for input in &INPUTS {
        let what = input.expect;
        let expect = read_expect(input.expect);
        let files: Vec<PathBuf> = input.names.iter().map(|n| fixture(n)).collect();
        // each file's envelope and body, read without a load
        let facts: Vec<Value> = files
            .iter()
            .zip(input.names)
            .map(|(p, n)| s18::file_facts(n, p))
            .collect();
        if files.len() == 1 {
            assert_eq!(facts[0], expect["file"], "{what}");
        } else {
            assert_eq!(json!(facts), expect["files"], "{what}");
        }
        for (i, f) in facts.iter().enumerate() {
            assert_eq!(f["dump_format"], json!(4), "{what}");
            assert_eq!(f["store_format"], store, "{what}: the feature manifest");
            assert_eq!(f["batches"], f["trailer"]["batches"]);
            assert_eq!(f["frames"], f["trailer"]["frames"]);
            if i > 0 {
                let prev = &facts[i - 1];
                assert_eq!(f["chain"]["kind"], "increment", "{what}");
                assert_eq!(f["chain"]["base_batch"], prev["chain"]["end_batch"]);
                assert_eq!(f["chain"]["prev"], prev["trailer"]["digest"]);
            }
        }
        // the whole load: the source log byte for byte, and no batch after it
        let to = d.path().join(format!("{what}-whole"));
        let w = s18::watch(input.canaries.to_vec());
        let s = s18::chain_load(&files, &to, None);
        assert!(
            !w.0.get(),
            "{what}: a write held a payload removed in the chain"
        );
        drop(w);
        let got = s18::whole_facts_of(&s, &files, &to, input.holder);
        // the set was written before content-2: its unlogged move count is analysis, which the
        // first writable start logs (`first_start_logs_moves`)
        assert_eq!(
            without_moves(&got),
            without_moves(&expect["whole"]),
            "{what}: the whole load"
        );
        nothing_after(&to, s18::chain_end(&files), input.holder, what);
        for c in input.canaries {
            assert!(
                !s18::holds(&to, c),
                "{what}: a removed payload is in the store"
            );
        }
        // every frame of every file under its own kind and hash
        let mut r = s18::open(&to, input.holder);
        let mut frames = 0;
        for p in &files {
            let (mut rd, _, _) =
                DumpReader::open(BufReader::new(std::fs::File::open(p).unwrap())).unwrap();
            while let Some(item) = rd.next().unwrap() {
                if let DumpItem::Batch(b) = item {
                    for f in &b.frames {
                        frames += 1;
                        assert!(r.store().has_kind(f.kind(), f.hash()).unwrap(), "{what}");
                    }
                }
            }
        }
        assert_eq!(json!(frames), s18::expect_frames(&expect), "{what}");
        // the plan tables a reindex rebuilds are the ones the load built
        let tables = r.store().index_dump_for_tests().unwrap();
        r.reindex().unwrap();
        assert_eq!(
            r.store().index_dump_for_tests().unwrap(),
            tables,
            "{what}: a reindex rebuilds every table"
        );
        r.fsck().unwrap();
        r.close().unwrap();
        first_start_logs_moves(&to, input.holder, what);
        let r = s18::open(&to, input.holder);
        if input.holder {
            r.close().unwrap();
        } else {
            rdy_qa::invariants::check_all(&r).unwrap_or_else(|v| panic!("{what}: {v:#?}"));
            r.close().unwrap();
            rdy_qa::reindex_check::reindex_reproduces(&to).unwrap_or_else(|v| panic!("{v:#?}"));
        }
        // a chain's full dump alone
        if files.len() > 1 {
            let to = d.path().join(format!("{what}-alone"));
            let got = s18::whole_facts(&files[..1], &to, input.holder);
            assert_eq!(
                without_moves(&got),
                without_moves(&expect["full_alone"]),
                "{what}: the full dump alone"
            );
            nothing_after(&to, s18::chain_end(&files[..1]), input.holder, what);
        }
        // each fixed stop: its position, its carried batch byte for byte, and its facts
        for name in input.stops {
            let what = format!("{}: {name}", input.expect);
            let want = &expect[*name];
            let to = d.path().join(format!("{}-{name}", input.expect));
            let missing: Vec<&'static [u8]> = input
                .canaries
                .iter()
                .copied()
                .filter(|c| !s18::stop_holds(want, input, c))
                .collect();
            let w = s18::watch(missing.clone());
            let selector = want["selector"].as_str().unwrap();
            let s = s18::chain_load(&files, &to, Some(selector));
            assert!(
                !w.0.get(),
                "{what}: a write held a payload removed at the stop"
            );
            drop(w);
            for c in &missing {
                assert!(!s18::holds(&to, c), "{what}");
            }
            let got = s18::stop_facts_of(&s, selector, &to, input.holder, None);
            assert_eq!(without_moves(&got), without_moves(want), "{what}");
            let r = s18::open(&to, input.holder);
            r.fsck().unwrap();
            r.close().unwrap();
        }
    }
    // the sync batch: its bytes and facts, its check, the current export of the same tag from
    // the loaded holder fixture, and its take
    let bytes = std::fs::read(sync_fixture(SYNC)).unwrap();
    let expect = read_sync_expect();
    assert_eq!(&bytes[8..12], &2u32.to_le_bytes(), "sync batch format 2");
    assert_eq!(stream_facts(&bytes), expect["stream"]);
    let report = check_batch(bytes.as_slice(), None).unwrap();
    assert_eq!(
        json!(report.keys),
        expect["stream"]["records"]
            .as_array()
            .map(|rs| {
                json!(
                    rs.iter()
                        .map(|r| json!({"kind": r["kind"], "hash": r["hash"]}))
                        .collect::<Vec<_>>()
                )
            })
            .unwrap()
    );
    let loaded = d.path().join("sync-holder");
    s18::chain_load(&[fixture(HOLDER)], &loaded, None);
    assert_eq!(
        export_sync_tag(&loaded),
        bytes,
        "the current export of the tag is the frozen batch, byte for byte"
    );
    let (h, _, taken) = take_split(&bytes, &d.path().join("sync-take"));
    h.close().unwrap();
    assert_eq!(taken, expect["taken"], "the take and what the holder reads");
}

#[test]
fn ok1_old_dump_pairs_keep_hashes() {
    // all nine released pairs remain accepted, each fixture of each with its pinned blake3
    let pairs: std::collections::BTreeSet<(u32, u32)> = RELEASED.iter().map(|f| f.pair).collect();
    assert_eq!(pairs.len(), 9);
    assert_eq!(
        READS
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        pairs
    );
    let files = RELEASED
        .iter()
        .map(|f| (f.dump, f.dump_blake3, f.pair))
        .chain(RELEASED.iter().map(|f| (f.expect, f.expect_blake3, f.pair)))
        .chain(RELEASED_CHAINS.iter().flat_map(|c| {
            c.files
                .iter()
                .chain([&c.expect])
                .zip(c.blake3)
                .map(move |(n, d)| (*n, d, c.pair))
        }));
    for (name, digest, pair) in files {
        let bytes = std::fs::read(fixture(name)).unwrap();
        assert_eq!(Hash::of(&bytes).to_string(), digest, "{name}");
        if name.ends_with(".rdydump") {
            let (_, format, h) = DumpReader::open(&bytes[..]).unwrap();
            assert_eq!(
                (format, h.store),
                (pair.0, DumpStore::Legacy(pair.1)),
                "{name}"
            );
        }
    }
}

/// The dropped frames of the dump files `files`, in order, as the reader gives them.
fn dropped_of(files: &[PathBuf]) -> Vec<Frame> {
    let mut out = vec![];
    for p in files {
        let (mut rd, _, _) =
            DumpReader::open(BufReader::new(std::fs::File::open(p).unwrap())).unwrap();
        while let Some(item) = rd.next().unwrap() {
            if let DumpItem::Batch(b) = item {
                out.extend(
                    b.frames
                        .into_iter()
                        .filter(|f| matches!(f, Frame::Dropped { .. })),
                );
            }
        }
    }
    out
}

#[test]
fn ok1_dropped_frames_keep_their_encoding() {
    // the released project and holder removals: a load of each (3, 18) chain dumped again by
    // this rdy keeps every dropped frame's kind, encoding 0, length and proof
    let d = tempfile::tempdir().unwrap();
    for (files, holder) in [
        (s18::chain_files(), false),
        (s18::holder_chain_files(), true),
    ] {
        let before = dropped_of(&files);
        assert!(!before.is_empty(), "the chain holds removals");
        let to = d.path().join(if holder { "holder" } else { "project" });
        s18::chain_load(&files, &to, None);
        let r = s18::open(&to, holder);
        let mut stored = vec![];
        r.store()
            .reader()
            .for_each_batch(r.store().log_end(), |b| {
                stored.extend(
                    b.frames
                        .into_iter()
                        .filter(|f| matches!(f, Frame::Dropped { .. })),
                );
                Ok(())
            })
            .unwrap();
        let out = d.path().join(if holder {
            "holder.rdydump"
        } else {
            "project.rdydump"
        });
        r.read().dump(&out).unwrap();
        r.close().unwrap();
        // the loaded log's removals, the released files' ones among them (a later file's apply
        // also removes what an earlier file still held), each kept as it was
        let after = dropped_of(std::slice::from_ref(&out));
        assert_eq!(after, stored);
        for f in &before {
            assert!(after.contains(f), "{f:?} is kept");
        }
        let before = after;
        let bytes = std::fs::read(&out).unwrap();
        // each DROPPED record: the logical kind, encoding 0, then the format-3 proof
        let mut at = 12usize;
        let mut seen = 0;
        while at < bytes.len() {
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            if bytes[at + 4] == rec::DROPPED {
                assert_eq!(len, ENCODED_DROPPED_LEN);
                assert_eq!(&bytes[at + 38..at + 40], &[0, 0], "encoding 0");
                seen += 1;
            }
            at += 37 + len;
        }
        assert_eq!(seen, before.len());
    }
    // a versioned removal has no residue adapter: the writer refuses it before a byte
    let frame = Frame::Dropped {
        kind: rdy_store::kinds::BLOCK | rdy_store::codec::VERSIONED,
        hash: Hash([1; 32]),
        len: 5,
        apply: Hash([2; 32]),
        residue: Some(Hash([3; 32])),
    };
    let b = DumpBatch {
        start: Location {
            segment: 0,
            offset: 0,
        },
        digest: rdy_store::kinds::batch_digest([frame.hash()]),
        frames: vec![frame],
    };
    let header = rdy_store::dump::DumpHeader {
        store: DumpStore::Features(baseline_features()),
        log_end: rdy_store::dump_chain::batch_end(&b).unwrap(),
        rdy: "test".into(),
        chain: Some(rdy_store::dump_chain::DumpChainHeader::full(
            rdy_model::RepoId([7; 16]),
            rdy_store::dump_chain::BatchMarker::of(&b),
        )),
    };
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    match w.batch(&b) {
        Err(rdy_store::StoreError::Refused(m)) => {
            assert!(
                m == "unknown encoding version" || m.ends_with("cannot stand here"),
                "{m}"
            )
        }
        other => panic!("{other:?}"),
    }
}
