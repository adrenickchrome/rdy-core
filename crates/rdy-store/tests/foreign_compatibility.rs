//! rpl33 (repo links; D-69AY3, D-DAPR8, D-C94DV): the stream reader takes every sync format
//! released from Release 1. The frozen sync fixtures are read as they are, by this crate's
//! reader alone:
//!
//! - format 2 (Release 1): every tag history reads to the header, records and totals its expect
//!   file records, and passes the batch check;
//! - format 3 (repo links): the frozen link delivery reads to the frames and the delivery its
//!   expect file records; its selection identity, recomputed from its versions and shells, is the
//!   frozen value; a batch check refuses it for its purpose;
//! - format 1 is older than Release 1 and stays refused by its own text.
//!
//! Every released sync fixture keeps the length and digest that the Release 1 contract records.
use rdy_model::Hash;
use rdy_store::sync_stream::{
    LINK_DELIVERY_FORMAT, READ_FORMATS, STREAM_FORMAT, StreamPurpose, StreamReader, StreamRecord,
    check_batch, check_delivery,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn qa_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rdy-qa/fixtures")
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// One record as a released sync expect file states it.
fn record_fact(r: &StreamRecord) -> Value {
    match r {
        StreamRecord::Present { key, payload, .. } => json!({
            "kind": key.kind,
            "hash": key.hash.to_string(),
            "form": "present",
            "len": payload.len(),
        }),
        StreamRecord::Dropped {
            key,
            original_len,
            apply,
            residue,
            ..
        } => json!({
            "kind": key.kind,
            "hash": key.hash.to_string(),
            "form": "dropped",
            "len": original_len,
            "apply": apply.to_string(),
            "residue": residue.map(|h| h.to_string()),
        }),
    }
}

/// One record as the link delivery's expect file states it: the released form plus its raw kind.
fn delivery_record_fact(r: &StreamRecord) -> Value {
    let (key, form, len) = match r {
        StreamRecord::Present { key, payload, .. } => (key, "present", payload.len() as u64),
        StreamRecord::Dropped {
            key, original_len, ..
        } => (key, "dropped", u64::from(*original_len)),
    };
    json!({
        "kind": key.kind,
        "hash": key.hash.to_string(),
        "raw_kind": r.raw_kind(),
        "form": form,
        "len": len,
    })
}

#[test]
fn rpl33_new_reader_takes_every_released_sync_format() {
    // the formats this rdy reads: Release 1's format 2 and the link delivery's format 3
    assert_eq!(READ_FORMATS, [2, 3]);
    assert_eq!(
        STREAM_FORMAT, 2,
        "a tag history stays in Release 1's format"
    );
    assert_eq!(LINK_DELIVERY_FORMAT, 3);

    // every released sync fixture keeps the length and digest Release 1 recorded
    let release_one = read_json(&qa_fixtures().join("contracts/release-1.json"));
    let released = release_one["sync"]["fixture_digests"].as_object().unwrap();
    assert!(released.len() >= 8, "{released:?}");
    assert_eq!(release_one["sync"]["stream_format"], json!(STREAM_FORMAT));
    for (name, recorded) in released {
        let bytes = std::fs::read(qa_fixtures().join("sync").join(name))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            &format!("{} {}", bytes.len(), Hash::of(&bytes)),
            recorded.as_str().unwrap(),
            "{name}"
        );
    }

    let dir = qa_fixtures().join("sync");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".rdysync"))
        .collect();
    names.sort();
    let mut formats = BTreeSet::new();
    let mut deliveries = 0;
    for name in &names {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        let expect = read_json(&dir.join(name.replace(".rdysync", ".expect.json")));
        let format = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        formats.insert(format);
        match format {
            // older than Release 1: refused by its own text
            1 => {
                let e = check_batch(&bytes[..]).unwrap_err();
                assert_eq!(
                    (e.code, e.message.as_str()),
                    ("E_STORE_FORMAT", "sync batch format 1 is not supported"),
                    "{name}"
                );
            }
            // a released tag history: its frames read as recorded, and its batch check passes
            2 => {
                let (mut rd, header) =
                    StreamReader::open(&bytes[..]).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(header.purpose, StreamPurpose::TagHistory, "{name}");
                let mut records = vec![];
                while let Some(r) = rd.next().unwrap() {
                    records.push(record_fact(&r));
                }
                let totals = rd.finish().unwrap();
                assert_eq!(
                    json!({"header": header, "records": records, "totals": totals}),
                    expect["stream"],
                    "{name}"
                );
                let report = check_batch(&bytes[..]).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(report.header, header, "{name}");
                assert_eq!(
                    check_delivery(&bytes[..]).unwrap_err().code,
                    "E_STREAM_PURPOSE",
                    "{name}: a tag history is no delivery"
                );
            }
            // the frozen link delivery: its frames, its delivery and its selection identity
            3 => {
                deliveries += 1;
                let report = check_delivery(&bytes[..]).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(
                    serde_json::to_value(&report).unwrap(),
                    expect["delivery"],
                    "{name}"
                );
                let (mut rd, header) =
                    StreamReader::open_for(&bytes[..], StreamPurpose::LinkDelivery)
                        .unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(header.purpose, StreamPurpose::LinkDelivery, "{name}");
                assert!(header.prerequisites.is_empty(), "{name}: no prerequisite");
                let mut records = vec![];
                while let Some(r) = rd.next().unwrap() {
                    // each object is the next selected version, under its BLOCK key
                    let v = &report.delivery.versions[records.len()];
                    assert_eq!(r.key().hash, v.version, "{name}");
                    records.push(delivery_record_fact(&r));
                }
                let totals = rd.finish().unwrap();
                assert_eq!(
                    json!({"header": header, "records": records, "totals": totals}),
                    expect["stream"],
                    "{name}"
                );
                // the selection identity is frozen from repo links on (D-C94DV)
                let d = &report.delivery;
                let selection = rdy_model::foreign::delivery_selection(
                    d.versions.iter().map(|v| (&v.id, &v.version)),
                    &d.shells,
                )
                .unwrap();
                assert_eq!(selection, d.selection, "{name}");
                assert_eq!(expect["selection"], json!(selection.to_string()), "{name}");
                // a tag-history check refuses it for its purpose, not as damage
                assert_eq!(
                    check_batch(&bytes[..]).unwrap_err().code,
                    "E_STREAM_PURPOSE",
                    "{name}"
                );
            }
            other => panic!("{name}: sync format {other} has no reader rule here"),
        }
    }
    assert_eq!(formats, BTreeSet::from([1, 2, 3]), "{names:?}");
    assert_eq!(deliveries, 1, "the frozen link delivery");
}
