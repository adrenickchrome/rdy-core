use rdy_engine::formats::{DumpFormatFacts, DumpPair, formats, inspect_dump};
use rdy_engine::{BatchMarker, DumpKind, E_BAD_INPUT, E_DUMP, Location, Rdy};
use std::path::Path;

fn make_dump(dir: &Path) -> std::path::PathBuf {
    make_dump_summary(dir).0
}

fn make_dump_summary(dir: &Path) -> (std::path::PathBuf, rdy_engine::DumpSummary) {
    let proj = dir.join("proj");
    let r = Rdy::init(&proj, "rdy-standard").unwrap();
    let out = dir.join("p.rdydump");
    let summary = r.read().dump(&out).unwrap();
    (out, summary)
}

#[test]
fn ha8_formats_match_current_constants_and_released_pairs() {
    let f = formats();
    assert_eq!(f.rdy, env!("CARGO_PKG_VERSION"));
    assert_eq!(f.store, rdy_store::format::STORE_FORMAT);
    assert_eq!(f.dump, rdy_store::dump::DUMP_FORMAT);
    assert_eq!(f.stream, 1);
    let pairs: Vec<(u32, u32)> = f.reads.iter().map(|p| (p.dump, p.store)).collect();
    assert_eq!(
        pairs,
        [
            (1, 10),
            (1, 11),
            (1, 12),
            (2, 13),
            (2, 14),
            (2, 15),
            (3, 16),
            (3, 17)
        ]
    );
    assert_eq!(pairs, rdy_store::dump::READS);
    assert_eq!(f.reads[0], DumpPair { dump: 1, store: 10 });
    // the facts carry exactly these fields: no manifest or recipe key
    let v = serde_json::to_value(&f).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    keys.sort();
    assert_eq!(keys, ["dump", "rdy", "reads", "store", "stream"]);
    let mut pair_keys: Vec<&str> = v["reads"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    pair_keys.sort();
    assert_eq!(pair_keys, ["dump", "store"]);
}

#[test]
fn ha8_inspect_reads_only_the_dump_header() {
    let t = tempfile::tempdir().unwrap();
    let (good, summary) = make_dump_summary(t.path());
    let facts = inspect_dump(&good).unwrap();
    assert_eq!(
        facts,
        DumpFormatFacts {
            dump: rdy_store::dump::DUMP_FORMAT,
            store: rdy_store::format::STORE_FORMAT,
            rdy: env!("CARGO_PKG_VERSION").to_string(),
            kind: Some(DumpKind::Full),
            repo_id: Some(summary.repo_id),
            base: Some(Location {
                segment: 0,
                offset: 0
            }),
            base_batch: Some(BatchMarker::ZERO),
            prev: Some(rdy_model::Hash([0; 32])),
            end: Some(summary.end),
            end_batch: Some(summary.end_batch),
        }
    );

    // a valid header with an invalid body: the tail (batches and trailer) is destroyed
    let mut bytes = std::fs::read(&good).unwrap();
    let n = bytes.len();
    assert!(n > 128);
    for b in &mut bytes[n - 96..] {
        *b = 0xff;
    }
    let bad = t.path().join("bad.rdydump");
    std::fs::write(&bad, &bytes).unwrap();
    assert_eq!(inspect_dump(&bad).unwrap(), facts);
    // the whole dump is not valid: a load refuses it
    let err = rdy_engine::load::load(&bad, &t.path().join("target")).unwrap_err();
    assert_eq!(err.code, E_DUMP);

    // file and header errors pass through the load's texts
    let missing = t.path().join("nope.rdydump");
    let e = inspect_dump(&missing).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.message.contains("does not exist"));
    let junk = t.path().join("junk");
    std::fs::write(&junk, b"not a dump at all").unwrap();
    assert_eq!(inspect_dump(&junk).unwrap_err().code, E_DUMP);
}

#[test]
fn ha8_format_calls_need_no_store_or_config() {
    let t = tempfile::tempdir().unwrap();
    let good = make_dump(t.path());
    // a directory that is no project: no .rdy, no config
    let bare = tempfile::tempdir().unwrap();
    let copy = bare.path().join("d.rdydump");
    std::fs::copy(&good, &copy).unwrap();
    let before: Vec<_> = std::fs::read_dir(bare.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let _ = formats();
    inspect_dump(&copy).unwrap();
    let after: Vec<_> = std::fs::read_dir(bare.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(before, after, "nothing is created beside the dump");
    assert!(!bare.path().join(".rdy").exists());
}
