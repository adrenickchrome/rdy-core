//! dl8: the dump promise (D-69AY3). Dump format 1 with store format 10 is released with this
//! file. Every later rdy loads it with every hash kept. Never rewrite the fixture: a new format
//! adds its own fixture beside it (F2).
use rdy_engine::{Rdy, load::load};
use rdy_qa::every_kind;
use rdy_store::dump::{DumpItem, DumpReader};
use std::path::PathBuf;

/// The fixture's blake3, set when it was written. A change here needs a founder decision.
const FIXTURE_BLAKE3: &str = "6dcb6184dc25b1edd7fa5cea22f23b5f18d536cba5a1f8e589cc3d51d512334c";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/dump")
        .join(name)
}

/// Writes the fixture once. Run by hand, never by the gate:
/// `cargo test -p rdy-qa --release --test io dl8_write_the_v1_fixture -- --ignored`.
#[test]
#[ignore]
fn dl8_write_the_v1_fixture() {
    let (dump, expect) = (fixture("v1-s10.rdydump"), fixture("v1-s10.expect.json"));
    assert!(
        !dump.exists() && !expect.exists(),
        "the v1 fixture is released: never rewrite it"
    );
    std::fs::create_dir_all(dump.parent().unwrap()).unwrap();
    let src = every_kind::project();
    let r = Rdy::open(src.path()).unwrap();
    r.read().dump(&dump).unwrap();
    let facts = every_kind::facts(&r);
    r.close().unwrap();
    std::fs::write(
        &expect,
        serde_json::to_string_pretty(&facts).unwrap() + "\n",
    )
    .unwrap();
    eprintln!(
        "blake3 {}",
        rdy_model::Hash::of(&std::fs::read(&dump).unwrap())
    );
}

#[test]
fn dl8_the_v1_fixture_is_never_rewritten() {
    let bytes = std::fs::read(fixture("v1-s10.rdydump")).unwrap();
    assert_eq!(rdy_model::Hash::of(&bytes).to_string(), FIXTURE_BLAKE3);
}

#[test]
fn dl8_the_v1_fixture_loads_with_every_hash() {
    let to = tempfile::tempdir().unwrap();
    let dir = to.path().join("p");
    let s = load(&fixture("v1-s10.rdydump"), &dir).unwrap();
    assert_eq!(s.dump_format, 1);
    let expect: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture("v1-s10.expect.json")).unwrap())
            .unwrap();
    let r = Rdy::open(&dir).unwrap();
    let got = every_kind::facts(&r);
    assert_eq!(
        got["landings"], expect["landings"],
        "every landing, in order"
    );
    assert_eq!(
        got["refs"], expect["refs"],
        "every ref: views, tags, proposals, inboxes, reviews"
    );
    assert_eq!(got["objects"], expect["objects"], "every kind, counted");
    // every object of the fixture is in the loaded store under its own hash and kind
    let (mut rd, _, _) =
        DumpReader::open(std::fs::File::open(fixture("v1-s10.rdydump")).unwrap()).unwrap();
    while let Some(item) = rd.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            for f in b.frames {
                assert!(
                    r.store().has_kind(f.kind, &f.hash).unwrap(),
                    "kind {} {} is gone",
                    f.kind,
                    f.hash
                );
            }
        }
    }
    // the reindex check opens the project itself, so the store lock is released first
    r.close().unwrap();
    rdy_qa::reindex_check::reindex_reproduces(&dir).unwrap_or_else(|v| panic!("{v:#?}"));
}

#[test]
fn dl8_the_v1_fixture_covers_every_kind_and_ref_family() {
    let expect: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture("v1-s10.expect.json")).unwrap())
            .unwrap();
    for k in every_kind::V1_KINDS {
        assert!(
            expect["objects"][k.to_string()].as_u64().unwrap_or(0) >= 1,
            "kind {k}"
        );
    }
    for f in every_kind::FAMILIES {
        assert!(
            expect["refs"]
                .as_object()
                .unwrap()
                .keys()
                .any(|n| n.starts_with(f)),
            "family {f}"
        );
    }
}

#[test]
fn dl8_the_v1_fixture_redumps_record_for_record_while_the_store_format_is_10() {
    // Only true while the store format is 10. A plan that changes the store format deletes this
    // one test (and keeps every other dl8_ test), with a note in its report. The header's `rdy`
    // text names the writer's version, so the comparison leaves it out.
    assert_eq!(rdy_store::format::STORE_FORMAT, 10, "see the comment above");
    let to = tempfile::tempdir().unwrap();
    let dir = to.path().join("p");
    load(&fixture("v1-s10.rdydump"), &dir).unwrap();
    let again = to.path().join("again.rdydump");
    let r = Rdy::open(&dir).unwrap();
    r.read().dump(&again).unwrap();
    r.close().unwrap();
    let items = |p: &std::path::Path| {
        let (mut rd, f, h) = DumpReader::open(std::fs::File::open(p).unwrap()).unwrap();
        let mut v = vec![];
        while let Some(i) = rd.next().unwrap() {
            v.push(i);
        }
        (f, h.store_format, h.log_end, v)
    };
    assert!(items(&again) == items(&fixture("v1-s10.rdydump")));
}
