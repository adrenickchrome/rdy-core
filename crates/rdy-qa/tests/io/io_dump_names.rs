//! A dump's pack file names are at most 255 bytes, their extension counted (D-69AY3, D-FR0C4):
//! a hash-valid dump whose FILE record names a longer pack file is refused as damaged by every
//! load surface (the engine, `rdy load` and `rdy_load`), before the target is created or
//! written.
use rdy_engine::{E_DUMP, Rdy, load::load};
use rdy_model::Hash;
use rdy_qa::daemon::{Served, short_tempdir};
use rdy_qa::mcp::{McpClient, Reply};
use rdy_store::dump::{DUMP_FORMAT, MAGIC, rec};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FIX: &str = "dump the project again; rdy never repairs a dump";

/// One record framed as a store frame, with its true hash.
fn record(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut r = (payload.len() as u32).to_le_bytes().to_vec();
    r.push(kind);
    r.extend_from_slice(&Hash::of(payload).0);
    r.extend_from_slice(payload);
    r
}

/// The records of `good` after its prelude, as (kind, payload).
fn records(good: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut at = 12usize;
    let mut out = vec![];
    while at < good.len() {
        let len = u32::from_le_bytes(good[at..at + 4].try_into().unwrap()) as usize;
        out.push((good[at + 4], good[at + 37..at + 37 + len].to_vec()));
        at += 37 + len;
    }
    out
}

/// A stream of `recs` with hash-valid records, the prelude in front.
fn forge(recs: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&DUMP_FORMAT.to_le_bytes());
    for (k, p) in recs {
        out.extend(record(*k, p));
    }
    out
}

/// The trailer digest over `recs` (the trailer left out): `Hash::of` over each record's
/// `kind byte || 8-byte payload length || hash`.
fn trailer_digest(recs: &[(u8, Vec<u8>)]) -> Hash {
    let mut b = vec![];
    for (k, p) in recs {
        b.push(*k);
        b.extend_from_slice(&(p.len() as u64).to_le_bytes());
        b.extend_from_slice(&Hash::of(p).0);
    }
    Hash::of(&b)
}

/// The format-3 trailer payload that closes `recs` (the trailer left out), with `digest` and no
/// removal facts (the source holds no apply).
fn trailer_with(recs: &[(u8, Vec<u8>)], digest: Hash) -> Vec<u8> {
    let batches = recs.iter().filter(|(k, _)| *k == rec::BATCH).count() as u64;
    let frames = recs.iter().filter(|(k, _)| *k < rec::HEADER).count() as u64;
    let files = recs.iter().filter(|(k, _)| *k == rec::FILE).count() as u32;
    let mut p = batches.to_le_bytes().to_vec();
    p.extend_from_slice(&frames.to_le_bytes());
    p.extend_from_slice(&files.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes()); // removals
    p.extend_from_slice(&[0u8; 13]); // no first removed span
    p.extend_from_slice(&digest.0);
    p
}

/// `recs` with its last record (the trailer) made true again.
fn reseal(recs: &mut [(u8, Vec<u8>)]) {
    let n = recs.len() - 1;
    let digest = trailer_digest(&recs[..n]);
    recs[n].1 = trailer_with(&recs[..n], digest);
}

/// The path a `FILE` record's payload names.
fn file_path(payload: &[u8]) -> &[u8] {
    let n = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    &payload[2..2 + n]
}

/// Every path under `dir` with its bytes (None for a directory); empty when `dir` is missing.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            }
            out.push(p);
        }
    }
    let mut paths = vec![];
    if dir.exists() {
        walk(dir, &mut paths);
    }
    paths
        .into_iter()
        .map(|p| {
            let rel = p.strip_prefix(dir).unwrap().to_path_buf();
            let bytes = (!p.is_dir()).then(|| std::fs::read(&p).unwrap());
            (rel, bytes)
        })
        .collect()
}

fn arg(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// A target's existence and contents.
fn state(dir: &Path) -> (bool, BTreeMap<PathBuf, Option<Vec<u8>>>) {
    (dir.exists(), tree(dir))
}

#[test]
fn plc3_oversized_pack_path_load_writes_nothing() {
    let s = Served::new();
    let scratch = short_tempdir();
    let good_file = scratch.path().join("good.rdydump");
    let r = Rdy::open(s.dir.path()).unwrap();
    r.read().dump(&good_file).unwrap();
    r.close().unwrap();
    let good = std::fs::read(&good_file).unwrap();
    let mut recs = records(&good);
    // the unbent copy, resealed, is the good stream
    let mut same = recs.clone();
    reseal(&mut same);
    assert_eq!(forge(&same), good);

    // a pack file whose name is 256 bytes, its extension counted, put where the closed order
    // places it among the dump's FILE records
    let path = format!(".rdy/packs/{}.yaml", "a".repeat(251));
    assert_eq!(path.len() - ".rdy/packs/".len(), 256);
    let mut payload = (path.len() as u16).to_le_bytes().to_vec();
    payload.extend_from_slice(path.as_bytes());
    payload.extend_from_slice(b"name: a\n");
    let i = recs
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, (k, p))| *k != rec::FILE || file_path(p) > path.as_bytes())
        .map(|(i, _)| i)
        .unwrap();
    assert!(
        recs[1..i].iter().all(|(k, _)| *k == rec::FILE),
        "only FILE records come before it"
    );
    recs.insert(i, (rec::FILE, payload));
    reseal(&mut recs);
    let bytes = forge(&recs);
    let at: usize = 12 + recs[..i].iter().map(|(_, p)| 37 + p.len()).sum::<usize>();
    let forged = scratch.path().join("forged.rdydump");
    std::fs::write(&forged, &bytes).unwrap();
    let message = format!(
        "{} is damaged at byte {at}: the project file path {path:?} is not one rdy carries",
        forged.display()
    );
    let want = (E_DUMP.to_string(), message.clone(), FIX.to_string());

    let mut m = McpClient::start_proxy(&s);
    m.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    for (name, empty) in [("missing", false), ("empty", true)] {
        // the engine
        let to = scratch.path().join(format!("engine-{name}"));
        if empty {
            std::fs::create_dir(&to).unwrap();
        }
        let before = state(&to);
        let e = load(&forged, &to).unwrap_err();
        assert_eq!(
            (e.code.to_string(), e.message.clone(), e.fix.to_string()),
            want,
            "engine, {name} target"
        );
        assert_eq!(state(&to), before, "engine, {name} target: unchanged");

        // `rdy load`
        let to = scratch.path().join(format!("cli-{name}"));
        if empty {
            std::fs::create_dir(&to).unwrap();
        }
        let before = state(&to);
        let (ok, e) = s.json(&["--json", "load", arg(&forged), arg(&to)]);
        assert!(!ok, "cli, {name} target: {e}");
        assert_eq!(
            (
                e["code"].as_str().unwrap_or_default().to_string(),
                e["message"].as_str().unwrap_or_default().to_string(),
                e["fix"].as_str().unwrap_or_default().to_string()
            ),
            want,
            "cli, {name} target: {e}"
        );
        assert_eq!(state(&to), before, "cli, {name} target: unchanged");

        // `rdy_load`
        let to = scratch.path().join(format!("mcp-{name}"));
        if empty {
            std::fs::create_dir(&to).unwrap();
        }
        let before = state(&to);
        let v = match m.call("rdy_load", json!({"file": arg(&forged), "dir": arg(&to)})) {
            Reply::ToolError(v) => v,
            other => panic!("mcp, {name} target: expected a tool error, got {other:?}"),
        };
        assert_eq!(
            (
                v["code"].as_str().unwrap_or_default().to_string(),
                v["message"].as_str().unwrap_or_default().to_string(),
                v["fix"].as_str().unwrap_or_default().to_string()
            ),
            want,
            "mcp, {name} target: {v}"
        );
        assert_eq!(state(&to), before, "mcp, {name} target: unchanged");
    }
    drop(m);
    s.stop();
    // the forged dump itself was only read
    assert_eq!(std::fs::read(&forged).unwrap(), bytes);
}
