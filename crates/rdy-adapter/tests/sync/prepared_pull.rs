//! r59012 through the adapter: a hub prepares one pull of a snapshot's history and answers its
//! gets from it. A get reply is the batch's raw bytes, or those bytes as one zstd frame when the
//! caller accepts it; either way the checked bytes, keys, hashes and report counts are core's
//! `write_history_keys` of the same keys.

use crate::hub_world::{doc, land, project};
use rdy_adapter::stream::{BodyEncoding, ObjectKey, StreamPurpose, check_batch};
use rdy_adapter::{Hash, StoreHandle};
use std::io::Read;
use std::path::Path;

const CANON: StreamPurpose = StreamPurpose::CanonHistory;
/// The zstd frame magic, little-endian.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
const MAGIC: &[u8] = b"RDYSYNC\n";

/// A project of two landings; the second holds a long, repeating body that a frame shrinks.
fn world() -> (tempfile::TempDir, Hash) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().join("p");
    let mut r = project(&pd);
    doc(&mut r, "w0", "Canon", "The hub keeps one canon.");
    land(&mut r, "w0");
    let body = "A clause that repeats itself, and then repeats itself again. ".repeat(400);
    doc(&mut r, "w1", "Extend", &body);
    let tip = land(&mut r, "w1");
    r.close().unwrap();
    (dir, tip)
}

fn read_all(mut file: std::fs::File) -> Vec<u8> {
    let mut out = Vec::new();
    file.read_to_end(&mut out).unwrap();
    out
}

fn all_keys(pull: &rdy_adapter::stream::PreparedPull) -> Vec<ObjectKey> {
    let mut out = Vec::new();
    let mut after = None;
    loop {
        let page = pull.page(after, 7).unwrap();
        out.extend(page.keys.iter().copied());
        after = page.next;
        if after.is_none() {
            return out;
        }
    }
}

/// The segment files of the project at `dir`.
fn segments(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<_> = std::fs::read_dir(dir.join(".rdy").join("store").join("segments"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .collect();
    out.sort();
    out
}

#[test]
fn r59012_adapter_get_encoding_keeps_raw_checks() {
    let (dir, tip) = world();
    let pd = dir.path().join("p");
    let h = StoreHandle::open(&pd).unwrap();
    let snap = h.snapshot().unwrap();
    let reply = snap.prepare_pull_reply(CANON, tip).unwrap();
    let pull = snap.prepare_pull(CANON, tip).unwrap();
    let whole = all_keys(&pull);
    assert!(whole.len() > 12);
    // the reply's pages are the pull's pages
    assert_eq!(reply.page(None, 1000).unwrap().keys, whole);

    let mut sets: Vec<Vec<ObjectKey>> = whole.chunks(5).map(<[_]>::to_vec).collect();
    sets.push(whole.clone());
    sets.push(vec![whole[1], whole[1], whole[0]]);
    let mut framed = 0;
    for keys in &sets {
        // core's own export of the same keys is the oracle
        let mut raw = Vec::new();
        let want = snap.write_history_keys(CANON, tip, keys, &mut raw).unwrap();

        // identity: the batch's own bytes, with no frame
        let plain = reply.write_keys_encoded(keys, false).unwrap();
        assert_eq!(plain.encoding, BodyEncoding::Identity);
        assert_eq!(plain.wire_bytes, raw.len() as u64);
        assert_eq!(plain.report, want);
        let body = read_all(plain.body);
        assert_eq!(body, raw);
        assert!(body.starts_with(MAGIC) && !body.starts_with(&ZSTD_MAGIC));

        // accepted: one frame when it is smaller, else the same bytes as identity
        let coded = reply.write_keys_encoded(keys, true).unwrap();
        assert_eq!(coded.report, want, "the report counts the raw batch");
        assert_eq!(coded.report.bytes + 85, raw.len() as u64);
        let body = read_all(coded.body);
        assert_eq!(body.len() as u64, coded.wire_bytes);
        let decoded = match coded.encoding {
            BodyEncoding::Zstd => {
                framed += 1;
                assert!(body.starts_with(&ZSTD_MAGIC));
                assert!(coded.wire_bytes < raw.len() as u64);
                zstd::stream::decode_all(&body[..]).unwrap()
            }
            BodyEncoding::Identity => {
                assert_eq!(coded.wire_bytes, raw.len() as u64);
                body
            }
        };
        assert_eq!(decoded, raw, "the same bytes, hashes and order");
        let checked = check_batch(&decoded[..], None).unwrap();
        assert_eq!(checked, want);
        assert_eq!(checked.digest, want.digest);
        // the prepared pull's own write is the same batch
        let mut again = Vec::new();
        assert_eq!(pull.write_keys(keys, &mut again).unwrap(), want);
        assert_eq!(again, raw);
    }
    assert!(framed > 0, "a repeating body travels as a frame");

    // a refusal produces no reply, whichever the encoding
    let outside = ObjectKey {
        kind: whole[0].kind,
        hash: Hash([9; 32]),
    };
    for accept in [false, true] {
        let e = reply.write_keys_encoded(&[outside], accept).unwrap_err();
        assert_eq!(e.code, "E_UNRESOLVED_REF");
    }

    // a source record that no longer reads is no reply either: the prose changes on disk after
    // the pull was prepared
    let mut changed = 0;
    for f in segments(&pd) {
        let mut bytes = std::fs::read(&f).unwrap();
        if let Some(at) = bytes
            .windows(24)
            .position(|w| w == &b"A clause that repeats it"[..])
        {
            bytes[at] ^= 1;
            std::fs::write(&f, &bytes).unwrap();
            changed += 1;
        }
    }
    assert!(changed > 0);
    for accept in [false, true] {
        let e = reply.write_keys_encoded(&whole, accept).unwrap_err();
        assert_eq!(e.code, "E_IO", "{e}");
    }
    drop(pull);
    drop(reply);
    drop(snap);
    h.close().unwrap();
}
