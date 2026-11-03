//! M3 gate, durable records (Task 4), the io half: a removal reports the assessment, check
//! request, review and answer that quote its content as not erasable, and they stay whole
//! through the removal, a dump and a load; a follower fed the corpus's full dump and its two
//! increments grows its features as the source did and holds the source's records at each
//! completed prefix. `mh8_payload_and_removal_in_one_increment_cannot_reopen_inside_it` stays
//! the guard of the stated limit: an interrupted increment that holds a payload and its removal
//! is not claimed recoverable here. The corpus is the store half's (`store_m3_gate_records/corpus.rs`).
#[path = "../store/store_m3_gate_records/corpus.rs"]
mod corpus;

use corpus::{CANARY, dump_bytes, facts, facts_of, features_of, segments};
use rdy_engine::Rdy;
use rdy_engine::follower::{self, FollowerCore};
use rdy_model::redaction::ImpactKind;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;

/// Every quoted record's stored bytes, read as the corpus read them before the removal.
fn quoted_bytes(r: &Rdy, c: &corpus::Corpus) -> Vec<Vec<u8>> {
    let st = r.read();
    c.quoted
        .iter()
        .map(|(_, kind, h, _)| st.reader().get_kind(*kind, h).unwrap())
        .collect()
}

/// The hashes of the frames a removal dropped.
fn dropped(r: &Rdy) -> Vec<rdy_model::Hash> {
    let st = r.read();
    let mut out = vec![];
    let origin = Location {
        segment: 0,
        offset: 0,
    };
    st.reader()
        .for_each_record_from(origin, st.seq(), |_, f, _| {
            if let StoredFrame::Dropped { hash, .. } = f {
                out.push(hash);
            }
            Ok(())
        })
        .unwrap();
    out
}

#[test]
fn mg4_quoted_assessment_and_check_records_remain_reported_not_erasable() {
    let c = corpus::build();
    // the request's report names each record that quotes the removed claim, and none of them
    // can be erased by selecting it
    let kinds = [
        ImpactKind::Assessment,
        ImpactKind::CheckRequest,
        ImpactKind::Review,
        ImpactKind::Answer,
    ];
    for (kind, _, h, _) in &c.quoted {
        let row = c
            .report
            .quotes
            .iter()
            .find(|q| q.kind == *kind && q.record == h.to_string())
            .unwrap_or_else(|| panic!("{kind:?} {h} is a quote row: {:#?}", c.report.quotes));
        assert_eq!(row.quote, CANARY, "{kind:?} {h}");
        assert!(
            !row.erasable,
            "{kind:?} {h}: a quoted record is not erasable"
        );
    }
    for k in kinds {
        assert!(
            c.quoted.iter().any(|(q, ..)| *q == k),
            "the corpus quotes in a {k:?}"
        );
    }
    for q in c.report.quotes.iter().filter(|q| kinds.contains(&q.kind)) {
        assert!(!q.erasable, "{q:?}");
    }

    // the landing dropped the claim's version and the blob, and nothing it only quotes
    let r = Rdy::open(&c.root).unwrap();
    let gone = dropped(&r);
    assert!(
        gone.contains(&c.canary_version),
        "the removed version is dropped"
    );
    assert!(
        c.report.blobs.contains(&c.blob),
        "the request names the blob"
    );
    let manifest: rdy_store::kinds::BlobManifest = r
        .read()
        .reader()
        .get_typed(rdy_store::kinds::BLOB_MANIFEST, &c.blob)
        .unwrap();
    assert!(!manifest.chunks.is_empty());
    for chunk in &manifest.chunks {
        assert!(
            gone.contains(chunk),
            "the removed blob's chunk {chunk} is dropped"
        );
    }
    for (kind, _, h, _) in &c.quoted {
        assert!(!gone.contains(h), "{kind:?} {h} stays");
    }
    let after = quoted_bytes(&r, &c);
    for ((kind, _, h, before), now) in c.quoted.iter().zip(&after) {
        assert_eq!(now, before, "{kind:?} {h}: the bytes as written");
        let text = String::from_utf8_lossy(now);
        assert!(text.contains(CANARY), "{kind:?} {h} still quotes the claim");
    }
    let want = facts(&r);

    // a dump and a load keep the quoted records and the removal both
    let d = tempfile::tempdir().unwrap();
    let dump = d.path().join("post.rdydump");
    let bytes = dump_bytes(&r, &dump);
    r.close().unwrap();
    let to = d.path().join("loaded");
    rdy_engine::load::load(&dump, &to).unwrap();
    let loaded = Rdy::open(&to).unwrap();
    assert_eq!(quoted_bytes(&loaded, &c), after, "load: the quoted records");
    assert_eq!(dropped(&loaded), gone, "load: the dropped frames");
    assert_eq!(facts(&loaded), want, "load");
    assert_eq!(dump_bytes(&loaded, &d.path().join("again.rdydump")), bytes);
    loaded.close().unwrap();
}

#[test]
fn mg4_follower_feature_growth_preserves_source_records() {
    let c = corpus::build();
    let d = tempfile::tempdir().unwrap();
    let to = d.path().join("f");
    follower::seed(&c.full, &to).unwrap();
    let mut core = FollowerCore::open(&to).unwrap();
    let seeded = features_of(&core.read().unwrap());
    assert_eq!(
        seeded, c.features[0],
        "the seed has the full dump's features"
    );

    // increment 1 holds the first use of the check_request, dispute and view_replay features:
    // the follower grows as the source did, and holds the source's completed prefix
    let one = core.follow(&c.inc1).unwrap();
    assert!(!one.repeated);
    let st = core.read().unwrap();
    assert_eq!(features_of(&st), c.features[1], "increment 1: the features");
    let names = |f: &serde_json::Value| -> Vec<String> {
        f["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect()
    };
    for grown in ["check_request", "dispute", "view_replay"] {
        let grown = grown.to_string();
        assert!(!names(&c.features[0]).contains(&grown), "{grown}");
        assert!(names(&c.features[1]).contains(&grown), "{grown}");
    }
    assert_eq!(facts_of(&st), c.prefix_facts, "increment 1: the records");
    let prefix = d.path().join("prefix.rdydump");
    st.dump(&prefix).unwrap();
    assert_eq!(
        std::fs::read(&prefix).unwrap(),
        std::fs::read(&c.prefix).unwrap(),
        "increment 1: the source's dump at that end"
    );
    drop(st);

    // increment 2 holds the disputes, the verdicts and the removal
    let two = core.follow(&c.inc2).unwrap();
    assert!(!two.repeated);
    core.fsck().unwrap();
    let st = core.read().unwrap();
    assert_eq!(features_of(&st), c.features[2], "increment 2: the features");
    let source = Rdy::open(&c.root).unwrap();
    assert_eq!(facts_of(&st), facts(&source), "increment 2: the records");
    let whole = d.path().join("whole.rdydump");
    st.dump(&whole).unwrap();
    assert_eq!(
        std::fs::read(&whole).unwrap(),
        dump_bytes(&source, &d.path().join("source.rdydump")),
        "increment 2: the source's dump"
    );
    drop(st);
    source.close().unwrap();
    drop(core);
    // the log byte for byte, removed payloads and all
    assert_eq!(segments(&to), segments(&c.root));
    // a repeat of the last increment appends nothing
    let mut core = FollowerCore::open(&to).unwrap();
    assert!(core.follow(&c.inc2).unwrap().repeated);
    drop(core);
    assert_eq!(segments(&to), segments(&c.root));
}
