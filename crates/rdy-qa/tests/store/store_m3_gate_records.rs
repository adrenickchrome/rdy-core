//! M3 gate, durable records (Task 4): the M3 record kinds of one small corpus keep their bytes,
//! hashes, refs, index reads and dump bytes through a reopen, a reindex, a dump and a load, and
//! through a dry-run and a real compact. A write that fails at any frame of an M3 batch leaves no
//! record and no ref after a reopen. These tests read stored records and captured snapshots;
//! they never call the gate to prove the gate.
mod corpus;

use corpus::{dump_bytes, facts, segments};
use rdy_engine::Rdy;
use rdy_engine::answer::AnswerRequest;
use rdy_model::dispute::DisputeOutcome;
use rdy_model::{Hash, Verdict};
use rdy_qa::every_kind;
use rdy_qa::fixtures::approver;
use rdy_qa::intake::{PERSON, TEST_SUMMARY};
use rdy_qa::tensions::{ag, put};
use rdy_store::carried_records::CHECK_REF_PREFIX;
use rdy_store::compact::CompactReport;
use rdy_store::kinds;
use rdy_store::pio::{self, Punch};
use serde_json::json;
use std::path::Path;

#[test]
fn mg4_m3_records_survive_reopen_reindex_and_dump_load() {
    let c = corpus::build();
    let r = Rdy::open(&c.root).unwrap();
    let want = facts(&r);
    corpus::assert_holds_every_m3_record(&c, &want);
    let d = tempfile::tempdir().unwrap();
    let before = dump_bytes(&r, &d.path().join("before.rdydump"));
    r.close().unwrap();

    // a reopen reads every record, ref and index row as written
    let mut r = Rdy::open(&c.root).unwrap();
    assert_eq!(facts(&r), want, "reopen");
    // a reindex rebuilds the same rows from the log
    r.reindex().unwrap();
    assert_eq!(facts(&r), want, "reindex");
    assert_eq!(
        dump_bytes(&r, &d.path().join("reindexed.rdydump")),
        before,
        "reindex: the dump bytes"
    );
    r.close().unwrap();

    // a load of the dump is the same store: positions, bytes, refs and rows
    let to = d.path().join("loaded");
    rdy_engine::load::load(&d.path().join("before.rdydump"), &to).unwrap();
    let loaded = Rdy::open(&to).unwrap();
    assert_eq!(facts(&loaded), want, "load");
    assert_eq!(
        dump_bytes(&loaded, &d.path().join("loaded.rdydump")),
        before,
        "load: the dump bytes"
    );
    loaded.fsck().unwrap();
    loaded.close().unwrap();
}

/// The parts of a compact report that do not depend on the file system's allocation.
fn logical(r: &CompactReport) -> serde_json::Value {
    json!({"log_bytes": r.log_bytes, "removed_bytes": r.removed_bytes, "by_kind": r.by_kind})
}

/// Removes the punch backend when a test ends, also when it panics.
struct PunchHook;
impl Drop for PunchHook {
    fn drop(&mut self) {
        pio::on_punch(None);
    }
}

#[test]
fn mg4_compact_preserves_dump_bytes_with_removed_payloads() {
    let c = corpus::build();
    let d = tempfile::tempdir().unwrap();
    let r = Rdy::open(&c.root).unwrap();
    let want = facts(&r);
    let dump = dump_bytes(&r, &d.path().join("before.rdydump"));
    let log = segments(&c.root);
    let end = r.store().log_end();

    // dry run: the report alone
    let dry = r.compact(true).unwrap();
    assert!(dry.removed_bytes > 0, "the corpus holds removed payloads");
    assert_eq!(dry.punched_bytes, 0);
    assert_eq!(dry.notes, Vec::<String>::new());
    assert_eq!(segments(&c.root), log, "dry run: the log");

    // the real compact: the file system's own punch, best effort
    let real = r.compact(false).unwrap();
    assert_eq!(logical(&real), logical(&dry));
    // a second compact is logically the same report
    let again = r.compact(false).unwrap();
    assert_eq!(logical(&again), logical(&real));

    // a punch the file system refuses is a note, never an error, and changes nothing either
    let _hook = PunchHook;
    pio::on_punch(Some(Box::new(|_, _| Some(Ok(Punch::Unsupported)))));
    let unsupported = r.compact(false).unwrap();
    assert_eq!(logical(&unsupported), logical(&real));
    pio::on_punch(Some(Box::new(|_, _| {
        Some(Err(std::io::Error::other("injected punch failure")))
    })));
    let failed = r.compact(false).unwrap();
    assert_eq!(logical(&failed), logical(&real));
    assert!(!failed.notes.is_empty(), "a failed punch is reported");
    pio::on_punch(None);

    // no frame moved, no byte that rdy reads changed, and every M3 record reads the same
    assert_eq!(segments(&c.root), log, "the log after compact");
    assert_eq!(r.store().log_end(), end);
    assert_eq!(facts(&r), want, "the records after compact");
    assert_eq!(dump_bytes(&r, &d.path().join("after.rdydump")), dump);
    r.fsck().unwrap();
    r.close().unwrap();
    let r = Rdy::open(&c.root).unwrap();
    assert_eq!(facts(&r), want, "the records after a reopen");
    assert_eq!(dump_bytes(&r, &d.path().join("reopened.rdydump")), dump);
    r.close().unwrap();
}

/// What a failed act must leave as it was: the log end and every fact of [`facts`] (every
/// kept record, every count, ref and index row).
fn log_state(r: &Rdy) -> serde_json::Value {
    json!({"log_end": format!("{:?}", r.store().log_end()), "facts": facts(r)})
}

/// Runs `act` with a write fault at frame 0, 1, 2 and so on, until it writes whole: at no frame
/// before `frames`, the act's own data frames. Each failed act is refused, and after a reopen the
/// store is as it was: no frame, no ref and no record of the act. Returns the store and what the
/// whole act gave.
fn fail_at_each_frame<T>(
    root: &Path,
    mut r: Rdy,
    what: &str,
    frames: usize,
    mut act: impl FnMut(&mut Rdy) -> Result<T, String>,
) -> (Rdy, T) {
    let before = log_state(&r);
    for k in 0usize.. {
        r.store_mut().fail_append_at_frame_for_tests(Some((k, 3)));
        let out = act(&mut r);
        r.store_mut().fail_append_at_frame_for_tests(None);
        match out {
            Ok(v) => {
                assert!(k >= frames, "{what}: the act wrote whole at frame {k}");
                assert_ne!(log_state(&r), before, "{what}: the whole act writes");
                r.fsck().unwrap_or_else(|e| panic!("{what}: {e}"));
                return (r, v);
            }
            Err(e) => {
                drop(r);
                r = Rdy::open(root).unwrap_or_else(|o| panic!("{what} frame {k}: {e}: {o}"));
                assert_eq!(log_state(&r), before, "{what} frame {k}: {e}");
                r.fsck()
                    .unwrap_or_else(|o| panic!("{what} frame {k}: {e}: {o}"));
            }
        }
    }
    unreachable!()
}

/// A store writer's act as the engine's own writers seal it: the write, then the publish of the
/// open batch (`seal_open`, the `publish(Delta::Seq)` the dispute and check tools run), whose
/// failure is the act's.
fn sealed<T>(r: &mut Rdy, w: impl FnOnce(&mut Rdy) -> Result<T, String>) -> Result<T, String> {
    let v = w(r)?;
    r.seal_open().map_err(|e| e.to_string())?;
    r.invalidate();
    Ok(v)
}

#[test]
fn mg4_failed_m3_batch_has_no_acknowledged_half_record() {
    let c = corpus::build();
    let root = c.root.clone();
    let mut r = Rdy::open(&root).unwrap();
    let view = "mg4-fail";
    put(
        &mut r,
        view,
        json!([{"op": "create", "type": "claim", "parent": c.section.to_string(), "body": "A claim whose acts fail."}]),
    );

    // a ready version: the proposal, its refs and its method 2 assessment in one batch
    let (r, p) = fail_at_each_frame(&root, r, "propose", 3, |r| {
        r.propose(&ag(view), view, view, Some(TEST_SUMMARY))
            .map_err(|e| e.to_string())
    });
    let version: Hash = p.id.parse().unwrap();
    {
        let st = r.read();
        let a = st
            .reader()
            .assessment_of_proposal(&version, st.seq())
            .unwrap()
            .expect("the version's assessment");
        let a = st.reader().assessment(&a).unwrap();
        assert_eq!(a.method, 2, "a new assessment is method 2");
    }

    // a relayed answer and its ref
    let (r, later) = fail_at_each_frame(&root, r, "relayed answer", 2, |r| {
        let item = corpus::approve_item(r, &p.r#ref);
        r.answer(
            &corpus::relaying_agent(),
            PERSON,
            AnswerRequest {
                r#ref: item.r#ref.clone(),
                option: "later".into(),
                card_hash: Some(item.card_hash.clone()),
                note: None,
                relay: corpus::relay_words("Not yet.", "chat-mg4-fail"),
                brief: None,
            },
        )
        .map_err(|e| e.to_string())
    });
    assert_eq!(later.act, "later");

    // the landing and its canon ref
    let (r, landing) = fail_at_each_frame(&root, r, "land", 2, |r| {
        r.land(&approver(), &p.id).map_err(|e| e.to_string())
    });

    // a check request and its `check/` ref, then its Check review (a REVIEW with no ref)
    let q = every_kind::carried_check_request(c.carried.replay, c.carried.proposal, &c.subject);
    let q = rdy_model::check_request::CheckRequest {
        case_hash: Hash::of(b"mg4 failing check case"),
        ..q
    };
    let (r, request) = fail_at_each_frame(&root, r, "check request", 2, |r| {
        sealed(r, |r| {
            r.store_mut()
                .put_check_request(&q)
                .map_err(|e| e.to_string())
        })
    });
    let review = every_kind::carried_check_review(request, q.case_hash, Verdict::Pass, &c.subject);
    let (r, reviewed) = fail_at_each_frame(&root, r, "check review", 1, |r| {
        sealed(r, |r| {
            r.store_mut()
                .put_check_review(&review)
                .map_err(|e| e.to_string())
        })
    });

    // a relayed dispute of the new landing (the DISPUTE, its ANSWER and the answer's ref), then
    // its relayed resolution
    let relay = corpus::relay_words("Dispute it.", "chat-mg4-fail");
    let (r, opened) = fail_at_each_frame(&root, r, "dispute", 3, |r| {
        r.dispute_open_as(
            &corpus::relaying_agent(),
            PERSON,
            &relay,
            &landing,
            "the failing act's landing needs a look",
        )
        .map_err(|e| e.to_string())
    });
    assert!(opened.answer.is_some(), "the dispute's answer");
    let d = opened.dispute.id.clone();
    let (r, resolved) = fail_at_each_frame(&root, r, "resolution", 3, |r| {
        r.dispute_resolve_as(
            &corpus::relaying_agent(),
            PERSON,
            &relay,
            &d,
            DisputeOutcome::Dismissed,
            None,
        )
        .map_err(|e| e.to_string())
    });
    assert!(resolved.answer.is_some(), "the resolution's answer");

    // every whole act reads after a reopen
    r.close().unwrap();
    let r = Rdy::open(&root).unwrap();
    let st = r.read();
    let reader = st.reader();
    assert!(reader.has_kind(kinds::CHECK_REQUEST, &request).unwrap());
    assert!(
        reader
            .get_ref(&format!("{CHECK_REF_PREFIX}{request}"))
            .unwrap()
            .is_some()
    );
    assert!(reader.has_kind(kinds::REVIEW, &reviewed).unwrap());
    let d: Hash = d.parse().unwrap();
    assert!(reader.resolution_of(&d, st.seq()).unwrap().is_some());
    drop(st);
    r.fsck().unwrap();
    r.close().unwrap();
}
