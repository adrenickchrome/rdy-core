//! ck: the check-request fixtures of rdy 0.32.0 for the hub (TASK-1026). One repo where rdy's
//! own product writers made a CHECK_REQUEST (`rdy_review` run), a Check REVIEW with verdict
//! concern (`rdy_review_submit`) and a person's AcceptVerdict ANSWER (`rdy_answer`), and a later
//! landed, tagged version of the same change (`rdy_qa::check_fixture`). Three members, each with
//! its expect file, in the scheme of the complete sets (`io_feature_fixture`): dump format 4,
//! feature manifest grammar 1, object encoding 1, and the part `-ck`.
//!
//! - `v4-f1-e1-ck.rdydump`: a dump of the project. It holds all three records.
//! - `v4-f1-e1-ck-holder.rdydump`: a dump of a new holder of the same repo that took the sync
//!   batch. It holds the request and the review.
//! - `v2-f1-e1-ck.rdysync`: the tag's history as one sync format 2 batch, exported from the
//!   project. It holds the request and the review.
//!
//! The holder dump and the batch cannot hold the AcceptVerdict answer: a tag's closure takes an
//! ANSWER only when one of its targets is a closure landing or tag (`rdy_store::closure`,
//! `attach_answers`), and an AcceptVerdict answer has no targets (the store refuses one that
//! has, `carried_records.rs`). A holder takes only what a batch carries.
//!
//! Each expect file pins the formats, the file's facts, a whole load's facts (fsck, keys, store
//! features) and which of the three records the member holds. The members are written once by
//! [`ck_write_check_fixtures_once`]; their blake3 are pinned here.
use crate::io_dump_fixture_s15::fixture;
use crate::io_dump_fixture_s18 as s18;
use crate::io_sync_fixture::{stream_facts, sync_fixture, take_split};
use rdy_engine::Rdy;
use rdy_engine::stream_check::check_batch;
use rdy_model::check_request::CheckRequest;
use rdy_model::{Answer, AnswerAct, Hash, Review, ReviewSubject, Verdict};
use rdy_qa::chain_fixture;
use rdy_qa::check_fixture::{self, CK_TAG, CheckRecords, CheckWorld, records_json};
use rdy_store::kinds;
use rdy_store::sync_stream::{StreamReader, StreamRecord};
use serde_json::{Value, json};
use std::path::Path;

const CK_PROJECT: &str = "v4-f1-e1-ck.rdydump";
const CK_PROJECT_EXPECT: &str = "v4-f1-e1-ck.expect.json";
const CK_HOLDER: &str = "v4-f1-e1-ck-holder.rdydump";
const CK_HOLDER_EXPECT: &str = "v4-f1-e1-ck-holder.expect.json";
const CK_SYNC: &str = "v2-f1-e1-ck.rdysync";
const CK_SYNC_EXPECT: &str = "v2-f1-e1-ck.expect.json";

/// The dump members and their blake3, set when they were written.
const CK_DUMP_MEMBERS: [(&str, &str); 4] = [
    (
        CK_PROJECT,
        "2c576d7c7e1114d6a3ff0b955babb7c6c71d379e2f278e8fcd6bcacd7b8f2cce",
    ),
    (
        CK_PROJECT_EXPECT,
        "0a4881db4127014b6e49ee43ab7b207182ad16774075295d98fbd2b38b7b52f7",
    ),
    (
        CK_HOLDER,
        "f0fde8153ed1a1801bc8ad163c08e578684445086ba9de175c163e3b06616bae",
    ),
    (
        CK_HOLDER_EXPECT,
        "1c29f2167d9797a31ca3137d29db07e9d54c1112a0eeaae76324396b6e238aa1",
    ),
];

/// The sync members and their blake3, set when they were written.
const CK_SYNC_MEMBERS: [(&str, &str); 2] = [
    (
        CK_SYNC,
        "9ffaffb4df66be3b35b42e1acad1d96b9b855f6a31b52f58f299f462ee06bce3",
    ),
    (
        CK_SYNC_EXPECT,
        "c728a7deb8f066a523b990dd0266fe30b6bdbdc974cff005076dcbb62a0379e6",
    ),
];

/// The dump members: the on-disk inventory of `ha17_` adds them.
pub(crate) fn dump_members() -> Vec<&'static str> {
    CK_DUMP_MEMBERS.iter().map(|(n, _)| *n).collect()
}

/// The rdy whose writers made the members.
const CK_RDY: &str = "0.32.0";

/// The record names each member holds, in the order the expect files list them.
const ALL_THREE: [&str; 3] = ["check_request", "check_review", "accept_verdict_answer"];
const CARRIED_TWO: [&str; 2] = ["check_request", "check_review"];

/// Why the holder dump and the sync batch hold no AcceptVerdict answer.
const NO_ANSWER_WHY: &str = "a tag's closure takes an ANSWER only when one of its targets is a closure landing or tag (rdy_store::closure, attach_answers); an AcceptVerdict answer has no targets (carried_records.rs refuses one that has); a holder takes only what a batch carries";

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// The names of the records whose row in `rows` is present.
fn held(rows: &Value) -> Vec<String> {
    rows.as_array()
        .unwrap()
        .iter()
        .filter(|r| r["present"] == json!(true))
        .map(|r| r["record"].as_str().unwrap().to_string())
        .collect()
}

/// The three records as an expect file names them: kind and hash.
fn records_of(expect: &Value) -> CheckRecords {
    let h = |i: usize| -> Hash {
        expect["records"][i]["hash"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    };
    CheckRecords {
        request: h(0),
        review: h(1),
        answer: h(2),
    }
}

/// Whether the store `r` holds an object of `kind` and hash `h`.
fn store_holds(r: &Rdy, kind: u8, h: &Hash) -> bool {
    r.store().reader().has_kind(kind, h).unwrap()
}

/// The present records of the batch `bytes`: `(kind, hash, raw kind, payload)`.
fn batch_records(bytes: &[u8]) -> Vec<(u8, Hash, u8, Vec<u8>)> {
    let (mut rd, _) = StreamReader::open(bytes).unwrap();
    let mut out = vec![];
    while let Some(r) = rd.next().unwrap() {
        if let StreamRecord::Present { key, payload, .. } = &r {
            out.push((key.kind, key.hash, r.raw_kind(), payload.clone()));
        }
    }
    rd.finish().unwrap();
    out
}

/// The expect file of the dump `file` (named `name`): the formats, the file, a whole load, its
/// store features, the three records and which it holds.
fn dump_expect(
    file: &Path,
    name: &str,
    records: &CheckRecords,
    scratch: &Path,
    holder: bool,
) -> Value {
    let files = vec![file.to_path_buf()];
    let to = scratch.join("whole");
    let whole = s18::whole_facts(&files, &to, holder);
    let r = s18::open(&to, holder);
    let rows = records_json(records, |k, h| store_holds(&r, k, h));
    let features = r.store().reader().store_features().unwrap();
    r.close().unwrap();
    let mut v = json!({
        "formats": chain_fixture::current_formats(),
        "file": s18::file_facts(name, file),
        "mode": if holder { "holder" } else { "project" },
        "tag": CK_TAG,
        "store_features": features,
        "whole": whole,
        "records": rows,
    });
    v["holds"] = json!(held(&v["records"]));
    if holder {
        v["not_held"] = json!({"accept_verdict_answer": NO_ANSWER_WHY});
    }
    v
}

/// The expect file of the sync batch `bytes`: the formats, the tag, the stream's facts, a split
/// take's facts, the three records and which it holds.
fn sync_expect(bytes: &[u8], records: &CheckRecords, scratch: &Path) -> Value {
    let (h, _, taken) = take_split(bytes, &scratch.join("sync"));
    h.close().unwrap();
    let keys: Vec<(u8, Hash)> = batch_records(bytes)
        .into_iter()
        .map(|(k, h, ..)| (k, h))
        .collect();
    let mut v = json!({
        "formats": chain_fixture::current_formats(),
        "tag": CK_TAG,
        "stream": stream_facts(bytes),
        "taken": taken,
        "records": records_json(records, |k, h| keys.contains(&(k, *h))),
    });
    v["holds"] = json!(held(&v["records"]));
    v["not_held"] = json!({"accept_verdict_answer": NO_ANSWER_WHY});
    v
}

/// Fixture members by file name: `(name, bytes)`.
type Members = Vec<(&'static str, Vec<u8>)>;

/// The six members of a fresh build: the dump members, then the sync members.
fn build_members(w: &CheckWorld, scratch: &Path) -> (Members, Members) {
    let project = scratch.join("project.rdydump");
    std::fs::write(&project, &w.project_dump).unwrap();
    let holder = scratch.join("holder.rdydump");
    std::fs::write(&holder, &w.holder_dump).unwrap();
    let d = scratch.join("p");
    std::fs::create_dir_all(&d).unwrap();
    let pe = dump_expect(&project, CK_PROJECT, &w.records, &d, false);
    let d = scratch.join("h");
    std::fs::create_dir_all(&d).unwrap();
    let he = dump_expect(&holder, CK_HOLDER, &w.records, &d, true);
    let se = sync_expect(&w.sync_batch, &w.records, scratch);
    (
        vec![
            (CK_PROJECT, w.project_dump.clone()),
            (CK_PROJECT_EXPECT, s18::pretty(&pe)),
            (CK_HOLDER, w.holder_dump.clone()),
            (CK_HOLDER_EXPECT, s18::pretty(&he)),
        ],
        vec![
            (CK_SYNC, w.sync_batch.clone()),
            (CK_SYNC_EXPECT, s18::pretty(&se)),
        ],
    )
}

/// Writes the six members once. Run by hand, never by the gate: `cargo test -p rdy-qa --release
/// --test io ck_write_check_fixtures_once -- --ignored`. It refuses a member that exists.
#[test]
#[ignore]
fn ck_write_check_fixtures_once() {
    for (m, _) in CK_DUMP_MEMBERS {
        assert!(!fixture(m).exists(), "{m}: written once: never rewrite it");
    }
    for (m, _) in CK_SYNC_MEMBERS {
        assert!(
            !sync_fixture(m).exists(),
            "{m}: written once: never rewrite it"
        );
    }
    assert_eq!(
        env!("CARGO_PKG_VERSION"),
        CK_RDY,
        "the members are rdy 0.32.0's"
    );
    let w = check_fixture::build();
    let scratch = tempfile::tempdir().unwrap();
    let (dump, sync) = build_members(&w, scratch.path());
    for (name, bytes) in &dump {
        s18::write_new(&fixture(name), bytes);
        eprintln!("{name} {} blake3 {}", bytes.len(), Hash::of(bytes));
    }
    for (name, bytes) in &sync {
        s18::write_new(&sync_fixture(name), bytes);
        eprintln!("{name} {} blake3 {}", bytes.len(), Hash::of(bytes));
    }
}

// ---------------------------------------------------------------------------------------------
// the checks

/// Every member keeps its pinned blake3, and the writer refuses it.
#[test]
fn ck_check_fixture_members_are_immutable() {
    let paths = CK_DUMP_MEMBERS
        .iter()
        .map(|(n, d)| (fixture(n), *d))
        .chain(CK_SYNC_MEMBERS.iter().map(|(n, d)| (sync_fixture(n), *d)));
    for (p, digest) in paths {
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(Hash::of(&bytes).to_string(), digest, "{}", p.display());
        let refused = std::panic::catch_unwind(|| s18::write_new(&p, b"never"));
        assert!(refused.is_err(), "{}: the writer refuses it", p.display());
        assert_eq!(std::fs::read(&p).unwrap(), bytes, "{}", p.display());
    }
}

/// (a) The stream check accepts the batch for its repo; its header holds the feature
/// `check_request`; it carries the request and its concern review, which decode, and not the
/// AcceptVerdict answer.
#[test]
fn ck_sync_batch_passes_the_stream_check_and_carries_two_records() {
    let bytes = std::fs::read(sync_fixture(CK_SYNC)).unwrap();
    let expect = read_json(&sync_fixture(CK_SYNC_EXPECT));
    let report = check_batch(bytes.as_slice(), None).unwrap();
    let report = check_batch(bytes.as_slice(), Some(report.header.repo_id)).unwrap();
    assert_eq!(
        serde_json::to_value(&report.header).unwrap(),
        expect["stream"]["header"]
    );
    let names: Vec<&str> = report
        .header
        .features
        .features
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert!(names.contains(&"check_request"), "{names:?}");
    assert_eq!(stream_facts(&bytes), expect["stream"]);
    assert_eq!(expect["holds"], json!(CARRIED_TWO));
    let rec = records_of(&expect);
    let got = batch_records(&bytes);
    let find = |kind: u8, h: Hash| got.iter().find(|(k, x, ..)| *k == kind && *x == h);
    let (_, _, raw, payload) = find(kinds::CHECK_REQUEST, rec.request).expect("the request");
    let q: CheckRequest = rdy_store::codec::decode_bounded(*raw, payload).unwrap();
    assert_eq!(q.check, check_fixture::CK_CHECK);
    assert!(q.subject.change.is_some(), "the request names the change");
    let (_, _, raw, payload) = find(kinds::REVIEW, rec.review).expect("the review");
    let r: Review = rdy_store::codec::decode_bounded(*raw, payload).unwrap();
    assert_eq!(
        r.subject,
        ReviewSubject::Check {
            request: rec.request
        }
    );
    assert_eq!(r.verdict, Verdict::Concern);
    assert!(
        find(kinds::ANSWER, rec.answer).is_none(),
        "no AcceptVerdict answer"
    );
    // the request comes before its review
    let at = |h: Hash| got.iter().position(|(_, x, ..)| *x == h).unwrap();
    assert!(at(rec.request) < at(rec.review));
}

/// (b, c) A split take of the batch into a new holder gives the pinned take facts.
#[test]
fn ck_sync_batch_take_matches_expect() {
    let bytes = std::fs::read(sync_fixture(CK_SYNC)).unwrap();
    let expect = read_json(&sync_fixture(CK_SYNC_EXPECT));
    let scratch = tempfile::tempdir().unwrap();
    let (h, _, taken) = take_split(&bytes, scratch.path());
    let rec = records_of(&expect);
    assert!(store_holds(&h, kinds::CHECK_REQUEST, &rec.request));
    assert!(store_holds(&h, kinds::REVIEW, &rec.review));
    assert!(!store_holds(&h, kinds::ANSWER, &rec.answer));
    h.close().unwrap();
    assert_eq!(taken, expect["taken"]);
}

/// The load of the dump member `name` matches its expect file: the file, the whole load (fsck,
/// keys, features) and the records it holds, each of which decodes as written.
fn check_dump(name: &str, expect_name: &str, holder: bool, holds: &[&str]) {
    let file = fixture(name);
    let expect = read_json(&fixture(expect_name));
    assert_eq!(expect["formats"], chain_fixture::current_formats());
    assert_eq!(s18::file_facts(name, &file), expect["file"]);
    assert_eq!(expect["file"]["rdy"], json!(CK_RDY));
    assert_eq!(expect["holds"], json!(holds));
    let scratch = tempfile::tempdir().unwrap();
    let to = scratch.path().join("whole");
    let files = vec![file.clone()];
    assert_eq!(s18::whole_facts(&files, &to, holder), expect["whole"]);
    let r = s18::open(&to, holder);
    // fsck passes (a damaged store is an error); its report is in the whole facts
    r.fsck().unwrap();
    let features = r.store().reader().store_features().unwrap();
    assert_eq!(
        serde_json::to_value(&features).unwrap(),
        expect["store_features"]
    );
    assert!(
        features.features.iter().any(|f| f.name == "check_request"),
        "{features:?}"
    );
    let rec = records_of(&expect);
    let rows = records_json(&rec, |k, h| store_holds(&r, k, h));
    assert_eq!(rows, expect["records"]);
    let reader = r.store().reader();
    let q = reader.check_request(&rec.request).unwrap();
    assert_eq!(q.check, check_fixture::CK_CHECK);
    let review: Review = reader.get_typed(kinds::REVIEW, &rec.review).unwrap();
    assert_eq!(
        review.subject,
        ReviewSubject::Check {
            request: rec.request
        }
    );
    assert_eq!(review.verdict, Verdict::Concern);
    if holds.contains(&"accept_verdict_answer") {
        let a: Answer = reader.get_typed(kinds::ANSWER, &rec.answer).unwrap();
        assert_eq!(a.act, AnswerAct::AcceptVerdict);
        assert_eq!(a.item, format!("verdict:{}", rec.review));
        assert!(a.targets.is_empty());
    } else {
        assert!(!store_holds(&r, kinds::ANSWER, &rec.answer));
    }
    r.close().unwrap();
}

/// (c, d) The project dump loads; fsck, keys and features match; all three records decode.
#[test]
fn ck_project_dump_loads_with_all_three_records() {
    check_dump(CK_PROJECT, CK_PROJECT_EXPECT, false, &ALL_THREE);
}

/// (b, c, d) A holder loads the holder dump; fsck, keys and features match; the request and the
/// review decode, and the answer is absent.
#[test]
fn ck_holder_dump_loads_with_the_request_and_review() {
    check_dump(CK_HOLDER, CK_HOLDER_EXPECT, true, &CARRIED_TWO);
}

/// (e) Two builds give the same bytes for every member; while this rdy is the members' rdy, a
/// build gives the committed bytes too. (A later rdy writes its own version into a dump header,
/// so it compares only its two builds: the committed members stay frozen.)
#[test]
fn ck_check_fixtures_are_deterministic() {
    let a = check_fixture::build();
    let b = check_fixture::build();
    assert_eq!(a.records, b.records);
    assert_eq!(a.project_dump, b.project_dump, "the project dump");
    assert_eq!(a.sync_batch, b.sync_batch, "the sync batch");
    assert_eq!(a.holder_dump, b.holder_dump, "the holder dump");
    let s1 = tempfile::tempdir().unwrap();
    let s2 = tempfile::tempdir().unwrap();
    let (d1, y1) = build_members(&a, s1.path());
    let (d2, y2) = build_members(&b, s2.path());
    assert_eq!(d1, d2);
    assert_eq!(y1, y2);
    if env!("CARGO_PKG_VERSION") == CK_RDY {
        for (name, bytes) in &d1 {
            assert_eq!(&std::fs::read(fixture(name)).unwrap(), bytes, "{name}");
        }
        for (name, bytes) in &y1 {
            assert_eq!(&std::fs::read(sync_fixture(name)).unwrap(), bytes, "{name}");
        }
    }
}
