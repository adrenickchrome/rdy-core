//! ok1, ok4: the complete fixture sets of the written wire formats (D-69AY3: "every later rdy
//! reads every dump format that rdy has released"; D-WWAK7: each object records its encoding
//! version, and feature compatibility replaces the format number; D-F0904). Each set holds dump
//! format 4 and sync batch format 2 of a feature store of manifest grammar 1. The stem names the
//! set; it is no store format number. Write a set once: never rewrite it, and never regenerate it
//! to fix a later reader.
//!
//! - `v4-f1-e0` and `v2-f1-e0` (ok1): no feature, every kind's latest encoding 0. They follow the
//!   `(3, 18)` set (`io_dump_fixture_s18`), built by the shared builders' store format 18 stages.
//! - `v4-f1-e1` and `v2-f1-e1` (ok4): object encoding 1. CHANGE, LANDING and REDACTION_RESIDUE
//!   are encoding 1, and kinds 25 to 28 are new at encoding 1. The project holds a load of the
//!   released `(3, 18)` chain, so its old changes, landings and residues stay encoding 0; the
//!   carried records follow (`chain_fixture`, the e1 sequence). Every manifest holds the seven
//!   registered features, each through its first use.
//!
//! The members of each set are built by the shared builders through their stage selector of
//! that time (`chain_fixture::project_chain_current`, `holder_chain_current`; the standalone
//! builders are shared):
//!
//! - `-chain-full`, `-chain-1` and `-chain-2` are a full dump and two increments of one project,
//!   and the bare stem its standalone full fixture;
//! - `-holder-chain-*` and `-holder` are the same for a holder;
//! - the `.rdysync` member is one sync batch, the history of one holder tag, exported from the
//!   standalone holder.
//!
//! Every expect file pins what a load of its files must give, as the `(3, 18)` set pins it, and
//! the formats the set holds (`chain_fixture::current_formats`): the dump and sync formats, the
//! feature manifest a new store writes and the latest-encoding map. A load of a dump of the
//! current format appends no batch of its own. A future dump format, newly registered feature or
//! new latest encoding needs a new complete set in its own task;
//! [`ok1_written_wire_format_has_a_complete_fixture_set`] refuses a written format without one.
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
use rdy_store::format::{StoreFeatures, baseline_features, registered_features};
use rdy_store::kinds;
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

const E1_FULL: &str = "v4-f1-e1.rdydump";
const E1_FULL_EXPECT: &str = "v4-f1-e1.expect.json";
const E1_CHAIN_FULL: &str = "v4-f1-e1-chain-full.rdydump";
const E1_CHAIN_1: &str = "v4-f1-e1-chain-1.rdydump";
const E1_CHAIN_2: &str = "v4-f1-e1-chain-2.rdydump";
const E1_CHAIN_EXPECT: &str = "v4-f1-e1-chain.expect.json";
const E1_HOLDER: &str = "v4-f1-e1-holder.rdydump";
const E1_HOLDER_EXPECT: &str = "v4-f1-e1-holder.expect.json";
const E1_HOLDER_CHAIN_FULL: &str = "v4-f1-e1-holder-chain-full.rdydump";
const E1_HOLDER_CHAIN_1: &str = "v4-f1-e1-holder-chain-1.rdydump";
const E1_HOLDER_CHAIN_2: &str = "v4-f1-e1-holder-chain-2.rdydump";
const E1_HOLDER_CHAIN_EXPECT: &str = "v4-f1-e1-holder-chain.expect.json";
const E1_SYNC: &str = "v2-f1-e1.rdysync";
const E1_SYNC_EXPECT: &str = "v2-f1-e1.expect.json";

/// Each dump member of the set `v4-f1-e1` and its blake3, set when it was written. A change here
/// needs a founder decision.
const E1_MEMBERS: [(&str, &str); 12] = [
    (
        E1_FULL,
        "6909115400534d16e7f24ee884e3a2b1f19678e325cbc7ce81290db87aaba0ee",
    ),
    (
        E1_FULL_EXPECT,
        "7887b6a7d1c61aae21ee1ea382c40cee4dd7f19486a30f12cbc470b225d3600e",
    ),
    (
        E1_CHAIN_FULL,
        "4277facc3f5211003da5dea0ee80bcf73883b3bfdcb38f8ff54a351935beba35",
    ),
    (
        E1_CHAIN_1,
        "567d23317260cb8243f33ae266d2403ac20d812d3a09048abe1e0753c391b757",
    ),
    (
        E1_CHAIN_2,
        "78c29157ebcc42752df39bfe3d822d39456419be8b6afe04df6a5566109e01eb",
    ),
    (
        E1_CHAIN_EXPECT,
        "30867bacc8c44b8ad4a59f1e89095c9ab845c09c336d6f95c6e965a73f21d972",
    ),
    (
        E1_HOLDER,
        "6d84b5ee1783cad5a351f22b6c7a20b34fbf81785ec7e0163c670eceb11088b6",
    ),
    (
        E1_HOLDER_EXPECT,
        "01af7230958b19c82974513dc5083e10577b10b2e238f7ea1cb9fbbf9dbcd320",
    ),
    (
        E1_HOLDER_CHAIN_FULL,
        "f62df037f738a9b62026d85d92963b4e0ceaaf8fc9f9ee114ca147624706e30b",
    ),
    (
        E1_HOLDER_CHAIN_1,
        "c52e42ea0d623f58fe6ab90df3a67bc8d5c71507374ed01d8f474b5c1019a571",
    ),
    (
        E1_HOLDER_CHAIN_2,
        "1023d2c2ee39431df418d9553aa3efde34fc691d8079ad981a91f2149bfc19a7",
    ),
    (
        E1_HOLDER_CHAIN_EXPECT,
        "b104926c782e9ef353d0262a05cc30c772deaee98ddd3f620a074630719b7c82",
    ),
];

/// The sync members of the set `v2-f1-e1` and their blake3, set when they were written.
const E1_SYNC_MEMBERS: [(&str, &str); 2] = [
    (
        E1_SYNC,
        "bd5a28a70c13b487f39ec28027396eee29982d83e711ed2eeb2cbca7acfe74c6",
    ),
    (
        E1_SYNC_EXPECT,
        "4843f554dfcffb506489e1dae57c07bc98c69698e29b513236dca3ad09e7af20",
    ),
];

/// The names of a complete set's members, and the holder tag its sync batch carries.
struct Names {
    full: &'static str,
    full_expect: &'static str,
    chain: [&'static str; 3],
    chain_expect: &'static str,
    holder: &'static str,
    holder_expect: &'static str,
    holder_chain: [&'static str; 3],
    holder_chain_expect: &'static str,
    sync: &'static str,
    sync_expect: &'static str,
    sync_tag: &'static str,
}

impl Names {
    fn expects(&self) -> [&'static str; 4] {
        [
            self.full_expect,
            self.chain_expect,
            self.holder_expect,
            self.holder_chain_expect,
        ]
    }
}

const E0: Names = Names {
    full: FULL,
    full_expect: FULL_EXPECT,
    chain: [CHAIN_FULL, CHAIN_1, CHAIN_2],
    chain_expect: CHAIN_EXPECT,
    holder: HOLDER,
    holder_expect: HOLDER_EXPECT,
    holder_chain: [HOLDER_CHAIN_FULL, HOLDER_CHAIN_1, HOLDER_CHAIN_2],
    holder_chain_expect: HOLDER_CHAIN_EXPECT,
    sync: SYNC,
    sync_expect: SYNC_EXPECT,
    sync_tag: SYNC_TAG,
};

const E1: Names = Names {
    full: E1_FULL,
    full_expect: E1_FULL_EXPECT,
    chain: [E1_CHAIN_FULL, E1_CHAIN_1, E1_CHAIN_2],
    chain_expect: E1_CHAIN_EXPECT,
    holder: E1_HOLDER,
    holder_expect: E1_HOLDER_EXPECT,
    holder_chain: [E1_HOLDER_CHAIN_FULL, E1_HOLDER_CHAIN_1, E1_HOLDER_CHAIN_2],
    holder_chain_expect: E1_HOLDER_CHAIN_EXPECT,
    sync: E1_SYNC,
    sync_expect: E1_SYNC_EXPECT,
    sync_tag: chain_fixture::E1_TAGS[2],
};

/// A complete fixture set of a written wire format: its names, its dump members and its sync
/// members with their blake3.
struct FeatureSet {
    names: &'static Names,
    dump: &'static [(&'static str, &'static str)],
    sync: &'static [(&'static str, &'static str)],
}

/// The registered complete sets, oldest first. A set, once registered, never leaves this list.
const SETS: [FeatureSet; 2] = [
    FeatureSet {
        names: &E0,
        dump: &MEMBERS,
        sync: &SYNC_MEMBERS,
    },
    FeatureSet {
        names: &E1,
        dump: &E1_MEMBERS,
        sync: &E1_SYNC_MEMBERS,
    },
];

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

fn read_sync_expect(n: &Names) -> Value {
    serde_json::from_str(&std::fs::read_to_string(sync_fixture(n.sync_expect)).unwrap()).unwrap()
}

/// The current wire formats have a complete registered set, every member of it on disk with its
/// pinned blake3 (`ha17_` and `ok1_` both ask).
pub(crate) fn assert_written_set_is_registered() {
    let now = chain_fixture::current_formats();
    let set = SETS
        .iter()
        .find(|s| {
            fixture(s.names.full_expect).exists()
                && read_expect(s.names.full_expect)["formats"] == now
        })
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

/// Fixture members by file name: `(name, bytes)`.
type Members = Vec<(&'static str, Vec<u8>)>;

/// Builds the fourteen members of the set named `n` in memory from the shared builders' current
/// stages; scratch stores go under `scratch`. The dump members first, then the sync members.
fn build_members(n: &Names, scratch: &Path) -> (Members, Members) {
    let mut out: Vec<(&str, Vec<u8>)> = vec![];

    let c = chain_fixture::project_chain_current();
    let s = chain_fixture::project_standalone(&c);
    let d = scratch.join("project");
    std::fs::create_dir_all(&d).unwrap();
    let expect = chain_expect(&c.files(), n.chain, &c, &d, false);
    for (name, p) in n.chain.into_iter().zip(c.files()) {
        out.push((name, std::fs::read(p).unwrap()));
    }
    out.push((n.chain_expect, s18::pretty(&expect)));
    let expect = standalone_expect(&s.dump, n.full, &c.missing_stop, &s.stop, &d, false);
    out.push((n.full, std::fs::read(&s.dump).unwrap()));
    out.push((n.full_expect, s18::pretty(&expect)));

    let (hc, ups) = chain_fixture::holder_chain_current();
    let hs = chain_fixture::holder_standalone(&hc, &ups);
    let d = scratch.join("holder");
    std::fs::create_dir_all(&d).unwrap();
    let expect = chain_expect(&hc.files(), n.holder_chain, &hc, &d, true);
    for (name, p) in n.holder_chain.into_iter().zip(hc.files()) {
        out.push((name, std::fs::read(p).unwrap()));
    }
    out.push((n.holder_chain_expect, s18::pretty(&expect)));
    let expect = standalone_expect(&hs.dump, n.holder, &hc.missing_stop, &hs.stop, &d, true);
    out.push((n.holder, std::fs::read(&hs.dump).unwrap()));
    out.push((n.holder_expect, s18::pretty(&expect)));

    // the sync batch: the tag's history, exported from the standalone holder
    let bytes = export_sync_tag(&hs.root, n.sync_tag);
    let (h, _, taken) = take_split(&bytes, &scratch.join("sync"));
    h.close().unwrap();
    let expect = json!({
        "formats": chain_fixture::current_formats(),
        "tag": n.sync_tag,
        "stream": stream_facts(&bytes),
        "taken": taken,
    });
    let sync = vec![(n.sync, bytes), (n.sync_expect, s18::pretty(&expect))];
    (out, sync)
}

/// The sync batch of `tag`'s history, exported by this rdy from the holder at `root`.
fn export_sync_tag(root: &Path, tag: &str) -> Vec<u8> {
    let held = Rdy::open_holder(root).unwrap();
    let t = held
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, ..)| n == tag)
        .unwrap()
        .1;
    let bytes = every_kind::export_tag(&held, t);
    held.close().unwrap();
    bytes
}

/// The latest encoding of each kind that object encoding 1 moved or added: every other kind's
/// is 0.
const E1_ENCODINGS: [u8; 7] = [
    kinds::CHANGE,
    kinds::LANDING,
    kinds::REDACTION_RESIDUE,
    kinds::ASSESSMENT,
    kinds::DISPUTE,
    kinds::DISPUTE_RESOLUTION,
    kinds::CHECK_REQUEST,
];

/// The latest-encoding map of the e1 set.
fn e1_encodings() -> Value {
    let m: serde_json::Map<String, Value> = rdy_store::dump::current_frame_kinds()
        .iter()
        .map(|k| (k.to_string(), json!(u16::from(E1_ENCODINGS.contains(k)))))
        .collect();
    json!(m)
}

/// The manifest of a store that has used every registered feature.
fn all_features() -> StoreFeatures {
    StoreFeatures {
        manifest_version: 1,
        features: registered_features(),
    }
}

/// Writes the fourteen members of the set `v4-f1-e1` once. Run by hand, never by the gate, after
/// the readers fail: `cargo test -p rdy-qa --release --test io ok4_write_encoding_fixture_set_once
/// -- --ignored`. It refuses a member that exists. (The e0 set's writer went with its stages:
/// the current builders write the e1 sequence.)
#[test]
#[ignore]
fn ok4_write_encoding_fixture_set_once() {
    for (m, _) in E1_MEMBERS {
        assert!(
            !fixture(m).exists(),
            "{m}: the v4-f1-e1 fixtures are written once: never rewrite them"
        );
    }
    for (m, _) in E1_SYNC_MEMBERS {
        assert!(
            !sync_fixture(m).exists(),
            "{m}: the v2-f1-e1 fixtures are written once: never rewrite them"
        );
    }
    assert_eq!(
        chain_fixture::current_formats(),
        json!({
            "dump_format": 4,
            "stream_format": 2,
            "features": baseline_features(),
            "encodings": e1_encodings(),
        }),
        "the v4-f1-e1 set holds dump format 4, sync format 2 and object encoding 1"
    );
    let scratch = tempfile::tempdir().unwrap();
    let (dump, sync) = build_members(&E1, scratch.path());
    assert_eq!(dump.len(), E1_MEMBERS.len());
    assert_eq!(sync.len(), E1_SYNC_MEMBERS.len());
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

/// The four inputs of a set: the project and holder chains, then their full fixtures.
fn inputs(
    n: &'static Names,
    canaries: [&'static [&'static [u8]]; 2],
    held: &'static [&'static [u8]],
) -> [s18::Input; 4] {
    [
        s18::Input {
            names: &n.chain,
            expect: n.chain_expect,
            holder: false,
            stops: &["held_stop", "missing_stop"],
            canaries: canaries[0],
            held,
        },
        s18::Input {
            names: &n.holder_chain,
            expect: n.holder_chain_expect,
            holder: true,
            stops: &["held_stop", "missing_stop"],
            canaries: canaries[1],
            held,
        },
        s18::Input {
            names: std::slice::from_ref(&n.full),
            expect: n.full_expect,
            holder: false,
            stops: &["missing_stop"],
            canaries: canaries[0],
            held,
        },
        s18::Input {
            names: std::slice::from_ref(&n.holder),
            expect: n.holder_expect,
            holder: true,
            stops: &["missing_stop"],
            canaries: canaries[1],
            held,
        },
    ]
}

/// The removed payloads of the e1 set: the released chain's and every e1 removal's, in both the
/// project and the holder.
const E1_CANARIES: &[&[u8]] = &[
    chain_fixture::EARLY_CANARY.as_bytes(),
    chain_fixture::LATE_CANARY.as_bytes(),
    chain_fixture::BLOB_CANARY,
    chain_fixture::STANDALONE_CANARY.as_bytes(),
    every_kind::CANARY_BODY.as_bytes(),
    every_kind::CANARY_FIELD.as_bytes(),
    chain_fixture::E1_EARLY_CANARY.as_bytes(),
    chain_fixture::E1_LATE_CANARY.as_bytes(),
    chain_fixture::E1_BLOB_CANARY,
    chain_fixture::E1_OLD_CANARY.as_bytes(),
];

/// The payloads an e1 chain's stops hold: the first e1 removal's (the holder takes no blob).
const E1_HELD: &[&[u8]] = &[
    chain_fixture::E1_EARLY_CANARY.as_bytes(),
    chain_fixture::E1_BLOB_CANARY,
    chain_fixture::E1_OLD_CANARY.as_bytes(),
];

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

/// Every member of `set` keeps its pinned blake3, and the writer refuses it.
fn members_are_immutable(set: &FeatureSet) {
    // A change to any member needs a founder decision. The checksums are fixed here, apart from
    // any writer: a later writer that writes other bytes changes nothing in this check.
    let paths = set
        .dump
        .iter()
        .map(|(n, d)| (fixture(n), *d))
        .chain(set.sync.iter().map(|(n, d)| (sync_fixture(n), *d)));
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
fn ok1_feature_fixture_members_are_immutable() {
    members_are_immutable(&SETS[0]);
}

#[test]
fn ok4_encoding_fixture_members_are_immutable() {
    members_are_immutable(&SETS[1]);
}

#[test]
fn ok1_written_wire_format_has_a_complete_fixture_set() {
    let now = chain_fixture::current_formats();
    assert_eq!(now["dump_format"], json!(DUMP_FORMAT));
    assert_eq!(now["stream_format"], json!(STREAM_FORMAT));
    assert_eq!(now["features"], json!(baseline_features()));
    // the latest-encoding map of object encoding 1
    assert_eq!(now["encodings"], e1_encodings());
    assert_written_set_is_registered();
    // every expect file of the set names the same formats
    for name in E1.expects() {
        assert_eq!(read_expect(name)["formats"], now, "{name}");
    }
    assert_eq!(read_sync_expect(&E1)["formats"], now);
    // the registered set's manifests hold every registered feature, through first use
    let all = json!({"features": all_features()});
    for name in [E1.full, E1.holder] {
        assert_eq!(
            s18::file_facts(name, &fixture(name))["store_format"],
            all,
            "{name}"
        );
    }
    for name in E1.chain.iter().chain(&E1.holder_chain) {
        let f = s18::file_facts(name, &fixture(name));
        assert!(
            f["store_format"]["features"]["features"]
                .as_array()
                .unwrap()
                .len()
                >= 2,
            "{name}"
        );
    }
    assert_eq!(
        s18::file_facts(E1.chain[2], &fixture(E1.chain[2]))["store_format"],
        all
    );
    assert_eq!(
        s18::file_facts(E1.holder_chain[2], &fixture(E1.holder_chain[2]))["store_format"],
        all
    );
}

/// ok13: the written format has a fixture. The dump format, the sync format, the feature
/// manifest and the latest-encoding map this rdy writes match one complete registered set, and
/// the native store fixture records the same manifest and map. This extends the current-set
/// assertion of `ok1_` and `ha17_`; it does not postpone it. A new dump format, a newly
/// registered feature or a new latest encoding fails here until the same task freezes a new
/// complete set.
#[test]
fn ok13_the_written_format_has_a_fixture() {
    let now = chain_fixture::current_formats();
    assert_eq!(now["dump_format"], json!(DUMP_FORMAT));
    assert_eq!(now["stream_format"], json!(STREAM_FORMAT));
    // exactly one registered set matches the written formats, and it is the newest
    let matching: Vec<&FeatureSet> = SETS
        .iter()
        .filter(|s| {
            fixture(s.names.full_expect).exists()
                && read_expect(s.names.full_expect)["formats"] == now
        })
        .collect();
    assert_eq!(matching.len(), 1, "one complete set matches {now}");
    let set = matching[0];
    assert!(std::ptr::eq(set, SETS.last().unwrap()), "the newest set");
    // twelve dump members and two sync members, each with a verified digest
    assert_eq!(set.dump.len(), 12);
    assert_eq!(set.sync.len(), 2);
    assert_written_set_is_registered();
    // the sync pair is current too: its expect file and its batch name the written formats
    assert_eq!(read_sync_expect(set.names)["formats"], now);
    // every expect file of the set names the written formats
    for name in set.names.expects() {
        assert_eq!(read_expect(name)["formats"], now, "{name}");
    }
    // the manifest: every registered feature, through first use, in the set's full fixtures
    let all = json!({"features": all_features()});
    for name in [set.names.full, set.names.holder] {
        assert_eq!(
            s18::file_facts(name, &fixture(name))["store_format"],
            all,
            "{name}"
        );
    }
    // the latest-encoding map covers every kind this rdy writes
    let map = now["encodings"].as_object().unwrap();
    let kinds_now: Vec<String> = rdy_store::dump::current_frame_kinds()
        .iter()
        .map(|k| k.to_string())
        .collect();
    assert_eq!(
        map.keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        kinds_now.into_iter().collect()
    );
    // the native store fixture records the same manifest and map
    let native = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/store/feature-1/expect.json");
    let native: Value = serde_json::from_str(&std::fs::read_to_string(native).unwrap()).unwrap();
    assert_eq!(native["store"]["manifest"], all["features"]);
    assert_eq!(native["store"]["encodings"], now["encodings"]);
    // no complete set is lost: e0 stays registered with all its members, and the nine legacy
    // pairs keep their dump and expect files
    for s in &SETS {
        for (name, _) in s.dump {
            assert!(fixture(name).exists(), "{name}");
        }
        for (name, _) in s.sync {
            assert!(sync_fixture(name).exists(), "{name}");
        }
    }
    assert_eq!(SETS[0].names.full, FULL);
    assert_eq!(SETS[0].dump.len(), 12);
    assert_eq!(SETS[0].sync.len(), 2);
    let pairs: std::collections::BTreeSet<(u32, u32)> = RELEASED.iter().map(|f| f.pair).collect();
    assert_eq!(pairs.len(), 9, "the nine legacy pairs");
    for f in &RELEASED {
        assert!(
            fixture(f.dump).exists() && fixture(f.expect).exists(),
            "{}",
            f.dump
        );
    }
    for c in &RELEASED_CHAINS {
        for name in c.files.iter().chain([&c.expect]) {
            assert!(fixture(name).exists(), "{name}");
        }
    }
}

#[test]
fn ok1_complete_feature_fixture_set_loads() {
    let store = StoreFeatures {
        manifest_version: 1,
        features: vec![],
    };
    let canaries = [s18::PROJECT_CANARIES, s18::HOLDER_CANARIES];
    let store = json!({ "features": store });
    check_set_loads(&E0, &inputs(&E0, canaries, s18::HELD), Some(&store), true);
}

#[test]
fn ok4_complete_encoding_fixture_set_loads() {
    let input = inputs(&E1, [E1_CANARIES, E1_CANARIES], E1_HELD);
    // each file's manifest is the one its expect file pins: the features used up to its end
    // (`ok1_written_wire_format_has_a_complete_fixture_set` pins all seven in the full fixtures
    // and the last increments)
    check_set_loads(&E1, &input, None, false);
    // the e1 records under their latest encodings, the old records under encoding 0: each kind
    // of each encoding the set promises is in its files
    for i in &input {
        let files: Vec<PathBuf> = i.names.iter().map(|n| fixture(n)).collect();
        let mut seen = std::collections::BTreeSet::new();
        let mut dropped = std::collections::BTreeSet::new();
        for p in &files {
            let (mut rd, _, _) =
                DumpReader::open(BufReader::new(std::fs::File::open(p).unwrap())).unwrap();
            while let Some(item) = rd.next().unwrap() {
                if let DumpItem::Batch(b) = item {
                    for f in &b.frames {
                        match f {
                            Frame::Present { kind, .. } => {
                                seen.insert(*kind);
                            }
                            Frame::Dropped { kind, .. } => {
                                dropped.insert(*kind);
                            }
                        }
                    }
                }
            }
        }
        let v = rdy_store::codec::VERSIONED;
        for k in [kinds::CHANGE, kinds::LANDING, kinds::REDACTION_RESIDUE] {
            assert!(seen.contains(&k), "{}: kind {k} encoding 0", i.expect);
            assert!(seen.contains(&(k | v)), "{}: kind {k} encoding 1", i.expect);
        }
        let new = [
            kinds::ASSESSMENT,
            kinds::DISPUTE,
            kinds::DISPUTE_RESOLUTION,
            kinds::CHECK_REQUEST,
        ];
        for k in new {
            assert!(seen.contains(&(k | v)), "{}: kind {k}", i.expect);
        }
        // a removed change of either encoding keeps its raw kind, in a project and a holder
        for k in [kinds::CHANGE, kinds::CHANGE | v] {
            assert!(dropped.contains(&k), "{}: a removed kind {k}", i.expect);
        }
    }
}

/// The checks of one complete set `n` over its four `inputs`: each file's facts and, when the set
/// has one manifest, that manifest (`store`), the whole load (byte for byte, no batch after it, no removed
/// payload, every frame, a reindex, fsck, the invariants), a chain's full dump alone, each fixed
/// stop, and the sync batch (its facts, its check, the current export and its take). `moves`:
/// the set was written before content-2, so its unlogged move count is analysis, which the first
/// writable start logs (`first_start_logs_moves`).
fn check_set_loads(n: &Names, inputs: &[s18::Input], store: Option<&Value>, moves: bool) {
    let d = tempfile::tempdir().unwrap();
    for input in inputs {
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
            if let Some(store) = store {
                assert_eq!(&f["store_format"], store, "{what}: the feature manifest");
            }
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
        if moves {
            assert_eq!(
                without_moves(&got),
                without_moves(&expect["whole"]),
                "{what}: the whole load"
            );
        } else {
            assert_eq!(got, expect["whole"], "{what}: the whole load");
        }
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
                        let kind = f.kind() & !rdy_store::codec::VERSIONED;
                        assert!(r.store().has_kind(kind, f.hash()).unwrap(), "{what}");
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
        if moves {
            first_start_logs_moves(&to, input.holder, what);
        }
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
            if moves {
                assert_eq!(
                    without_moves(&got),
                    without_moves(&expect["full_alone"]),
                    "{what}: the full dump alone"
                );
            } else {
                assert_eq!(got, expect["full_alone"], "{what}: the full dump alone");
            }
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
            if moves {
                assert_eq!(without_moves(&got), without_moves(want), "{what}");
            } else {
                assert_eq!(&got, want, "{what}");
            }
            let r = s18::open(&to, input.holder);
            r.fsck().unwrap();
            r.close().unwrap();
        }
    }
    // the sync batch: its bytes and facts, its check, the current export of the same tag from
    // the loaded holder fixture, and its take
    let bytes = std::fs::read(sync_fixture(n.sync)).unwrap();
    let expect = read_sync_expect(n);
    assert_eq!(&bytes[8..12], &2u32.to_le_bytes(), "sync batch format 2");
    assert_eq!(expect["tag"], json!(n.sync_tag));
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
    s18::chain_load(&[fixture(n.holder)], &loaded, None);
    assert_eq!(
        export_sync_tag(&loaded, n.sync_tag),
        bytes,
        "the current export of the tag is the frozen batch, byte for byte"
    );
    let (h, _, taken) = take_split(&bytes, &d.path().join("sync-take"));
    h.close().unwrap();
    if moves {
        // the take writes this rdy's baseline manifest into its new holder: the holder's
        // manifest ref and its ref updates differ from the frozen take's, which wrote none
        assert_eq!(
            without_manifest(&taken),
            without_manifest(&expect["taken"]),
            "the take and what the holder reads"
        );
    } else {
        assert_eq!(taken, expect["taken"], "the take and what the holder reads");
    }
}

/// `v` without the facts a new holder's feature manifest decides: its `meta/store_features`
/// ref and every ref update (kind 8) it holds.
fn without_manifest(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| k.as_str() != "meta/store_features")
                .map(|(k, v)| (k.clone(), without_manifest(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(
            a.iter()
                .filter(|x| {
                    !x.as_str()
                        .is_some_and(|s| s.starts_with(&format!("{}:", kinds::REF_UPDATE)))
                })
                .map(without_manifest)
                .collect(),
        ),
        _ => v.clone(),
    }
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
