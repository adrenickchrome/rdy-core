//! ok14: the Release 1 contract inventory (D-TXFHR: the contract surfaces; D-E30RA: Release 1
//! ships after M3a; D-XTWM9; D-69AY3). One machine-readable fixture,
//! `fixtures/contracts/release-1.json`, holds thirteen rows, one for each surface the decision
//! lists. Every row is generated from the code that owns the surface (its registry, parser, schema
//! or an existing fixture), never typed by hand, and the tests compare the live generation with the
//! fixture. A public contract change after Release 1 must update a versioned fixture and satisfy
//! the upgrade or deprecation rule of D-TXFHR; this file is the baseline of the release candidate.
//!
//! - The `cli` row has two members. `cli.tree` is the real clap command tree: the test-only module
//!   `contract` in rdy-cli walks it and compares it (`ok14_cli_contract_from_clap_is_complete`).
//!   `cli.runtime` is what the release binary does: exit values and JSON field sets, generated
//!   here by running it.
//! - Objects have sorted keys. Where the declared order is the contract (an encoding, a header, a
//!   command's arguments) the value is an array in that order. A set-valued array is sorted.
//! - Absolute temporary paths, random ports and elapsed times are replaced by `<path>`, `<port>`
//!   and `<elapsed>`. No producer commit is in the output.
//! - A listed surface with no source fails the capture; a row cannot be marked covered without an
//!   assertion in its generator.
//!
//! To write a candidate baseline, run the test with `RDY_CONTRACT_CAPTURE=1`: it replaces every
//! row but `cli.tree` (rdy-cli writes that member the same way) and then compares as usual.
use rdy_engine::tensions::{Involved, SideRef, tension_id, tension_label, tension_vkey};
use rdy_engine::{Scope, config_levels};
use rdy_model::semantic::{PairItem, pair_key};
use rdy_model::{
    ActorRun, Answer, AnswerAct, Assurance, BlockId, Hash, Provenance, RepoId, ReviewSubject, Role,
    Verdict,
    signature::{StatementKind, StatementSource, statement},
};
use rdy_qa::fixtures::{output_within, project, rdy_bin};
use rdy_schema::{PackSet, TensionKind, TensionSeverity};
use rdy_store::format::{
    FeatureClass, MANIFEST_VERSION, MAX_FEATURES, MAX_MANIFEST_BYTES, REF_STORE_FEATURES,
    REF_UPGRADE, REGISTERED_FEATURES, baseline_features, registered_features, required_features,
};
use rdy_store::{codec, dump, kinds, sync_stream};
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The thirteen rows of the inventory, sorted: the surfaces of D-TXFHR. The tests pin this list.
const ROWS: [&str; 13] = [
    "cli",
    "config",
    "derived_identities",
    "dump_load",
    "errors_warnings",
    "markdown_exports",
    "mcp",
    "schema_packs",
    "signatures",
    "site_urls",
    "store",
    "sync",
    "working_copy",
];

/// The grammar of the inventory file itself.
const INVENTORY_VERSION: u64 = 1;

fn qa_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn workspace_root() -> PathBuf {
    qa_root()
        .join("../..")
        .canonicalize()
        .expect("the workspace root exists")
}

fn fixture_path() -> PathBuf {
    qa_root().join("fixtures/contracts/release-1.json")
}

fn capture_mode() -> bool {
    std::env::var_os("RDY_CONTRACT_CAPTURE").is_some()
}

fn blake3_of(bytes: &[u8]) -> String {
    Hash::of(bytes).to_string()
}

/// The length and blake3 of each file under `dir` (sub-directories walked), by relative path.
fn digests(dir: &Path, keep: impl Fn(&str) -> bool) -> Value {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).expect("read the fixture directory") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                out.push(
                    p.strip_prefix(base)
                        .expect("under the base")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut names = vec![];
    walk(dir, dir, &mut names);
    names.sort();
    let mut m = Map::new();
    for n in names.into_iter().filter(|n| keep(n)) {
        let bytes = std::fs::read(dir.join(&n)).expect("read a fixture file");
        m.insert(n, json!(format!("{} {}", bytes.len(), blake3_of(&bytes))));
    }
    assert!(!m.is_empty(), "no fixture file under {}", dir.display());
    Value::Object(m)
}

/// The first byte of the postcard encoding of `v`: its variant index for an enum.
fn ordinal<T: Serialize>(v: &T) -> u64 {
    u64::from(postcard::to_allocvec(v).expect("postcard encodes")[0])
}

/// The shape of a JSON value: objects as their sorted members, arrays as their first element's
/// shape, scalars as their type. No id, time or text of the run is in it.
fn shape(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
        Value::Array(a) => match a.first() {
            Some(first) => json!([shape(first)]),
            None => json!([]),
        },
        Value::String(_) => json!("string"),
        Value::Number(_) => json!("number"),
        Value::Bool(_) => json!("bool"),
        Value::Null => json!("null"),
    }
}

/// Replaces what varies between runs: temporary paths, block ids, hashes and 13-digit times.
fn normalize(text: &str, paths: &[&Path]) -> String {
    let mut out = text.to_string();
    for p in paths {
        let s = p.to_string_lossy();
        out = out.replace(s.as_ref(), "<path>");
        if let Ok(c) = p.canonicalize() {
            out = out.replace(c.to_string_lossy().as_ref(), "<path>");
        }
    }
    let hash = Regex::new(r"\b[0-9a-f]{64}\b").unwrap();
    let id = Regex::new(r"\b[A-Za-z]{1,3}-[0-9A-Z]{5}\b").unwrap();
    let time = Regex::new(r"\b1[0-9]{12}\b").unwrap();
    let port = Regex::new(r"127\.0\.0\.1:[0-9]+").unwrap();
    let out = hash.replace_all(&out, "<hash>");
    let out = id.replace_all(&out, "<id>");
    let out = time.replace_all(&out, "<elapsed>");
    port.replace_all(&out, "127.0.0.1:<port>").into_owned()
}

// ---- store ----

fn store_row() -> Value {
    // the activation rule: what a first write of each kind and encoding turns on
    let mut activation = Map::new();
    for kind in dump::current_frame_kinds() {
        for version in [0u16, 1] {
            let (raw, payload) = if version == 0 {
                (*kind, vec![])
            } else {
                (*kind | codec::VERSIONED, 1u16.to_le_bytes().to_vec())
            };
            let names: Vec<String> = required_features(raw, &payload)
                .into_iter()
                .map(|f| f.name)
                .collect();
            if !names.is_empty() {
                activation.insert(format!("kind {kind} encoding {version}"), json!(names));
            }
        }
    }
    assert!(
        activation.len() >= 5,
        "the activation rules of the carried kinds have a source"
    );
    let kind_ids: BTreeMap<String, u8> = [
        ("BLOCK", kinds::BLOCK),
        ("CHANGE", kinds::CHANGE),
        ("PROPOSAL", kinds::PROPOSAL),
        ("LANDING", kinds::LANDING),
        ("SNAPSHOT", kinds::SNAPSHOT),
        ("BLOB_CHUNK", kinds::BLOB_CHUNK),
        ("BLOB_MANIFEST", kinds::BLOB_MANIFEST),
        ("REF_UPDATE", kinds::REF_UPDATE),
        ("TAG", kinds::TAG),
        ("ANSWER", kinds::ANSWER),
        ("SIGNATURE", kinds::SIGNATURE),
        ("VIEW_OP", kinds::VIEW_OP),
        ("BATCH_COMMIT", kinds::BATCH_COMMIT),
        ("INTENT", kinds::INTENT),
        ("ALT_SET", kinds::ALT_SET),
        ("REVIEW", kinds::REVIEW),
        ("TENSION", kinds::TENSION),
        ("TRANSITION", kinds::TRANSITION),
        ("REDACTION", kinds::REDACTION),
        ("REDACTION_RESIDUE", kinds::REDACTION_RESIDUE),
        ("REDACTION_APPLY", kinds::REDACTION_APPLY),
        ("REDACTION_CARRIED", kinds::REDACTION_CARRIED),
        ("PLAN", kinds::PLAN),
        ("ASSESSMENT", kinds::ASSESSMENT),
        ("DISPUTE", kinds::DISPUTE),
        ("DISPUTE_RESOLUTION", kinds::DISPUTE_RESOLUTION),
        ("CHECK_REQUEST", kinds::CHECK_REQUEST),
    ]
    .into_iter()
    .map(|(n, k)| (n.to_string(), k))
    .collect();
    assert_eq!(
        kind_ids.values().max(),
        Some(&kinds::CHECK_REQUEST),
        "the kind ids end at CHECK_REQUEST"
    );
    let latest: BTreeMap<String, u16> = kind_ids
        .iter()
        .map(|(n, k)| (n.clone(), codec::latest(*k)))
        .collect();
    let first: BTreeMap<String, u16> = kind_ids
        .iter()
        .map(|(n, k)| (n.clone(), codec::first(*k)))
        .collect();
    let request = Hash([0; 32]);
    let subject = |s: ReviewSubject| ordinal(&s);
    let review_subjects = json!({
        "Tension": subject(ReviewSubject::Tension {
            id: request,
            vkey: request,
            kind: String::new(),
            severity: String::new(),
            sides: vec![],
            blocks: vec![],
        }),
        "Mention": subject(ReviewSubject::Mention {
            block: BlockId::parse("c-AAAAA").unwrap(),
            entity: BlockId::parse("c-AAAAA").unwrap(),
        }),
        "Pair": subject(ReviewSubject::Pair { key: request, items: vec![] }),
        "Check": subject(ReviewSubject::Check { request }),
    });
    assert_eq!(review_subjects["Check"], 3);
    let verdicts = json!({
        "Dismiss": ordinal(&Verdict::Dismiss),
        "Restore": ordinal(&Verdict::Restore),
        "Tension": ordinal(&Verdict::Tension),
        "NoTension": ordinal(&Verdict::NoTension),
        "Pass": ordinal(&Verdict::Pass),
        "Concern": ordinal(&Verdict::Concern),
        "Fail": ordinal(&Verdict::Fail),
    });
    assert_eq!(
        (
            verdicts["Pass"].clone(),
            verdicts["Concern"].clone(),
            verdicts["Fail"].clone()
        ),
        (json!(4), json!(5), json!(6))
    );
    let acts: Vec<(&str, AnswerAct)> = vec![
        ("RejectProposal", AnswerAct::RejectProposal),
        ("Later", AnswerAct::Later),
        ("Confirm", AnswerAct::Confirm),
        ("RejectDecision", AnswerAct::RejectDecision),
        ("AnswerQuestion", AnswerAct::AnswerQuestion),
        ("Dismiss", AnswerAct::Dismiss),
        ("Tag", AnswerAct::Tag),
        ("ViewOwn", AnswerAct::ViewOwn),
        ("CloseStale", AnswerAct::CloseStale),
        ("Propose", AnswerAct::Propose),
        ("KeepSuspect", AnswerAct::KeepSuspect),
        ("AcceptDeferrals", AnswerAct::AcceptDeferrals),
        ("RefuseDeferrals", AnswerAct::RefuseDeferrals),
        ("RejectReaffirm", AnswerAct::RejectReaffirm),
        ("Dispute", AnswerAct::Dispute),
        ("ResolveDispute", AnswerAct::ResolveDispute),
        ("AcceptVerdict", AnswerAct::AcceptVerdict),
    ];
    let answer_acts: Map<String, Value> = acts
        .iter()
        .map(|(n, a)| ((*n).to_string(), json!(ordinal(a))))
        .collect();
    assert_eq!(
        (
            answer_acts["Dispute"].clone(),
            answer_acts["ResolveDispute"].clone(),
            answer_acts["AcceptVerdict"].clone()
        ),
        (json!(14), json!(15), json!(16))
    );
    for (i, (name, _)) in acts.iter().enumerate() {
        assert_eq!(
            answer_acts[*name],
            json!(i as u64),
            "ordinals are declaration order: {name}"
        );
    }
    use rdy_model::plan::{PlanAct, PlanKind, PlanOutcome};
    let other_ordinals = json!({
        "Assurance": {
            "Policy": ordinal(&Assurance::Policy),
            "Relayed": ordinal(&Assurance::Relayed),
            "Local": ordinal(&Assurance::Local),
        },
        "PlanAct": {
            "Confirm": ordinal(&PlanAct::Confirm), "Defer": ordinal(&PlanAct::Defer),
            "Done": ordinal(&PlanAct::Done), "Dismiss": ordinal(&PlanAct::Dismiss),
        },
        "PlanKind": {
            "Update": ordinal(&PlanKind::Update), "Rejustify": ordinal(&PlanKind::Rejustify),
            "Reaffirm": ordinal(&PlanKind::Reaffirm), "Confirm": ordinal(&PlanKind::Confirm),
            "UpdateImplementation": ordinal(&PlanKind::UpdateImplementation),
            "Tension": ordinal(&PlanKind::Tension),
        },
        "PlanOutcome": {
            "Updated": ordinal(&PlanOutcome::Updated), "Relinked": ordinal(&PlanOutcome::Relinked),
            "Repinned": ordinal(&PlanOutcome::Repinned), "Reaffirmed": ordinal(&PlanOutcome::Reaffirmed),
            "Retired": ordinal(&PlanOutcome::Retired), "Committed": ordinal(&PlanOutcome::Committed),
            "Confirmed": ordinal(&PlanOutcome::Confirmed), "Done": ordinal(&PlanOutcome::Done),
            "Deferred": ordinal(&PlanOutcome::Deferred), "Open": ordinal(&PlanOutcome::Open),
            "Dismissed": ordinal(&PlanOutcome::Dismissed),
        },
    });
    // the stable object ids: the identity keys (kind and hash of every stored frame) that the
    // released fixtures pin, and the content domain every block hash derives from
    let fixtures = qa_root().join("fixtures");
    let mut object_ids = Map::new();
    for set in [
        "v3-s18.expect.json",
        "v4-f1-e0.expect.json",
        "v4-f1-e1.expect.json",
    ] {
        let text = std::fs::read_to_string(fixtures.join("dump").join(set)).expect("expect file");
        let v: Value = serde_json::from_str(&text).expect("expect JSON");
        let keys = &v["whole"]["store"]["identity"]["keys"];
        let list: Vec<&str> = keys
            .as_array()
            .unwrap_or_else(|| panic!("{set} holds no identity keys: {keys}"))
            .iter()
            .map(|k| k.as_str().expect("key text"))
            .collect();
        let mut by_kind: BTreeMap<String, u64> = BTreeMap::new();
        for k in &list {
            *by_kind
                .entry(k.split(':').next().unwrap().to_string())
                .or_default() += 1;
        }
        object_ids.insert(
            set.trim_end_matches(".expect.json").to_string(),
            json!({
                "objects": list.len(),
                "by_kind": by_kind,
                "keys_blake3": blake3_of(list.join("\n").as_bytes()),
            }),
        );
    }
    json!({
        "sources": [
            "rdy_store::format", "rdy_store::codec", "rdy_store::kinds", "rdy_model enums",
            "fixtures/store/feature-1", "fixtures/dump/*.expect.json",
        ],
        "manifest": {
            "version": MANIFEST_VERSION,
            "max_bytes": MAX_MANIFEST_BYTES,
            "max_features": MAX_FEATURES,
            "format_file": rdy_store::format::FORMAT_FILE,
            "format_first_line": "rdy store features 1",
            "authority_ref": REF_STORE_FEATURES,
            "upgrade_receipt_ref": REF_UPGRADE,
            "name_grammar": "[a-z][a-z0-9_]{0,63}",
            "required_rdy_grammar": "three decimal u32 components joined by dots",
        },
        "classes": [
            FeatureClass::Compatible.as_str(),
            FeatureClass::ReadOnlyCompatible.as_str(),
            FeatureClass::Incompatible.as_str(),
        ],
        "features": registered_features(),
        "registered_names": REGISTERED_FEATURES,
        "creation_baseline": baseline_features(),
        "creation_baseline_text": baseline_features().to_text(),
        "activation": activation,
        "feature_rules": {
            "assessment": "an ASSESSMENT, or a LANDING whose assessment is set",
            "check_request": "a CHECK_REQUEST, a REVIEW with subject Check or verdict Pass, Concern or Fail, or an ANSWER with act AcceptVerdict",
            "content_hash_v2": "baseline: every store this rdy writes",
            "dispute": "a DISPUTE, a DISPUTE_RESOLUTION, or an ANSWER with act Dispute or ResolveDispute",
            "object_encoding": "any frame of encoding 1 or more",
            "plan_tension": "a PLAN with a sealed Tension item or Dismissed outcome, or an answer with act Dismiss",
            "view_replay": "a CHANGE whose links.replayed_from is set",
        },
        "encoding_versions": {
            "frame_header_bytes": 37,
            "versioned_flag": codec::VERSIONED,
            "registered": codec::REGISTERED.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
            "latest": latest,
            "first": first,
        },
        "kind_ids": kind_ids,
        "current_frame_kinds": dump::current_frame_kinds(),
        "ref_families": rdy_qa::every_kind::FAMILIES,
        "ordinals": {
            "AnswerAct": answer_acts,
            "ReviewSubject": review_subjects,
            "Verdict": verdicts,
            "others": other_ordinals,
        },
        "content_domain": String::from_utf8_lossy(rdy_model::block::CONTENT_DOMAIN).trim_end_matches('\0'),
        "object_ids": object_ids,
        "native_fixture": digests(&fixtures.join("store/feature-1"), |_| true),
    })
}

// ---- dump and load ----

/// Runs the release binary in `dir` without a daemon; returns its exit value, stdout and stderr.
fn rdy_in(dir: &Path, env: &[(String, String)], args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(rdy_bin());
    cmd.args(["--no-daemon", "--project"])
        .arg(dir)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .current_dir(dir);
    let o = output_within(&mut cmd, Duration::from_secs(120)).expect("run rdy");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn dump_load_row() -> Value {
    let dump_dir = qa_root().join("fixtures/dump");
    let pairs: Vec<Value> = dump::READS
        .iter()
        .map(|(d, s)| {
            json!({
                "dump_format": d,
                "store_format": s,
                "frame_kinds": dump::frame_kinds(*d, *s).expect("a released pair has kinds"),
            })
        })
        .collect();
    assert_eq!(pairs.len(), 9, "nine released pairs");
    let probes: Vec<Value> = [
        ".rdy.yaml",
        ".rdyignore",
        ".rdy/packs/team.yaml",
        ".rdy/packs/.hidden.yaml",
        ".rdy/packs/x.txt",
        "docs/a.yaml",
    ]
    .iter()
    .map(|p| json!([p, dump::is_project_file(p)]))
    .collect();
    // empty-target behaviour: a load into an empty directory, then into the loaded one
    let mut empty_target = Map::new();
    let (cfg, env) = rdy_qa::fixtures::human_env("human:qa");
    let target = tempfile::tempdir().expect("tempdir");
    let source = dump_dir.join("v4-f1-e0.rdydump");
    let (code, out, _) = rdy_in(
        target.path(),
        &env,
        &["--json", "load", source.to_str().unwrap(), "."],
    );
    let loaded: Value = serde_json::from_str(out.trim()).unwrap_or(json!(null));
    empty_target.insert(
        "into_empty_directory".into(),
        json!({"exit": code, "reply": shape(&loaded)}),
    );
    let (code, _, err) = rdy_in(
        target.path(),
        &env,
        &["--json", "load", source.to_str().unwrap(), "."],
    );
    let refused: Value = serde_json::from_str(err.trim()).unwrap_or(json!(null));
    empty_target.insert(
        "into_a_directory_with_a_store".into(),
        json!({
            "exit": code,
            "code": refused["code"],
            "reply": shape(&refused),
        }),
    );
    drop(cfg);
    assert_eq!(
        empty_target["into_empty_directory"]["exit"], 0,
        "the load of a released fixture into an empty directory succeeds: {empty_target:?}"
    );
    json!({
        "sources": ["rdy_store::dump", "fixtures/dump", "fixtures/sync", "rdy load"],
        "magic": String::from_utf8_lossy(dump::MAGIC),
        "dump_format": dump::DUMP_FORMAT,
        "reads_features": dump::READS_FEATURES,
        "released_pairs": pairs,
        "current_frame_kinds": dump::current_frame_kinds(),
        "container_record_kinds": {
            "HEADER": dump::rec::HEADER, "FILE": dump::rec::FILE, "BATCH": dump::rec::BATCH,
            "DROPPED": dump::rec::DROPPED, "TRAILER": dump::rec::TRAILER,
        },
        "sizes": {
            "header_fixed": dump::HEADER_FIXED, "trailer": dump::TRAILER_LEN,
            "dropped": dump::DROPPED_LEN, "encoded_dropped": dump::ENCODED_DROPPED_LEN,
            "max_files": dump::MAX_FILES, "max_file_bytes": dump::MAX_FILE_BYTES,
        },
        "project_file_probes": probes,
        "fixture_digests": digests(&dump_dir, |_| true),
        "empty_target": empty_target,
    })
}

// ---- sync ----

fn sync_row() -> Value {
    let sync_dir = qa_root().join("fixtures/sync");
    let carrier = sync_dir.join("v2-f1-e1.rdysync");
    let report = rdy_engine::stream_check::check_batch(
        std::fs::File::open(&carrier).expect("the carrier fixture opens"),
        None,
    )
    .expect("the carrier fixture is a valid batch");
    let mut kinds_seen: BTreeMap<u8, u64> = BTreeMap::new();
    for k in &report.keys {
        *kinds_seen.entry(k.kind).or_default() += 1;
    }
    let header = serde_json::to_value(&report.header).expect("header JSON");
    json!({
        "sources": ["rdy_store::sync_stream", "fixtures/sync", "rdy_engine::stream_check"],
        "magic": String::from_utf8_lossy(sync_stream::MAGIC),
        "stream_format": sync_stream::STREAM_FORMAT,
        "record_kinds": {
            "HEADER": sync_stream::rec::HEADER,
            "DROPPED": sync_stream::rec::DROPPED,
            "TRAILER": sync_stream::rec::TRAILER,
        },
        "order": ["HEADER", "object frames and DROPPED records", "TRAILER"],
        "header_fields": [
            "features_len: u32", "features: canonical JSON", "repo_id: [u8; 16]",
            "purpose: u8", "tag: [u8; 32]", "prerequisites: u32", "keys: kind u8 + hash [u8; 32]",
        ],
        "purposes": [[0, "tag_history"]],
        "logical_keys": "kind: u8 || hash: [u8; 32]",
        "prerequisite_order": "strictly ascending logical keys",
        "trailer_fields": ["records: u64", "bytes: u64", "digest: [u8; 32]"],
        "sizes": {
            "header_fixed": sync_stream::HEADER_FIXED, "key": sync_stream::KEY_LEN,
            "trailer": sync_stream::TRAILER_LEN, "max_header": sync_stream::MAX_HEADER_BYTES,
            "max_prerequisites": sync_stream::MAX_PREREQUISITES,
            "max_stream_bytes": sync_stream::MAX_STREAM_BYTES,
            "max_records": sync_stream::MAX_RECORDS, "max_edges": sync_stream::MAX_EDGES,
            "max_history_depth": sync_stream::MAX_HISTORY_DEPTH,
            "max_batch_files": sync_stream::MAX_BATCH_FILES,
        },
        "sync_kinds": sync_stream::SYNC_KINDS,
        "carrier_fixture": {
            "name": "v2-f1-e1.rdysync",
            "header": header,
            "records": report.records,
            "bytes": report.bytes,
            "digest": report.digest.to_string(),
            "kinds": kinds_seen,
        },
        "fixture_digests": digests(&sync_dir, |_| true),
    })
}

// ---- working copy ----

/// The checked-out project of the working-copy row: a doc in a folder with a claim.
fn wc_project() -> (rdy_qa::daemon::Served, Vec<String>) {
    rdy_model::seed_ids_for_tests(14);
    let _clock = rdy_qa::golden::fixture_clock();
    let s = rdy_qa::daemon::Served::new();
    let mut r = rdy_engine::Rdy::open(s.dir.path()).expect("open the served project");
    let w = rdy_qa::fixtures::write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "guides", "title": "Guides"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "intro", "title": "Intro", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Start"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Hello."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "after": "$p", "body": "Claims hold.", "links": {"describes": [{"target": "$p"}]}}
        ]),
    )
    .expect("seed the tree");
    rdy_qa::fixtures::land(&mut r, "work").expect("land the seed");
    r.close().expect("close");
    rdy_model::unseed_ids_for_tests();
    let ids = ["$f", "$d", "$s", "$p", "$c"]
        .iter()
        .map(|k| w.created[*k].clone())
        .collect();
    (s, ids)
}

fn wc_row() -> Value {
    let (s, ids) = wc_project();
    let run = |args: &[&str]| -> (bool, Value, String) {
        let mut a = vec!["--no-daemon", "--json"];
        a.extend_from_slice(args);
        let o = s.rdy(&a);
        let text = String::from_utf8_lossy(&o.stdout).into_owned();
        let err = String::from_utf8_lossy(&o.stderr).into_owned();
        let v = serde_json::from_str(text.trim())
            .or_else(|_| serde_json::from_str(err.trim()))
            .unwrap_or(json!(null));
        (o.status.success(), v, err)
    };
    let (ok, checkout, _) = run(&["checkout"]);
    assert!(ok, "the checkout succeeds: {checkout}");
    // the sidecar state under .rdy/wc, by file, with the shape of each YAML file
    let wc_dir = s.dir.path().join(".rdy/wc");
    let mut state = Map::new();
    let mut files: Vec<String> = vec![];
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).expect("read dir") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                out.push(
                    p.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    walk(&wc_dir, &wc_dir, &mut files);
    files.sort();
    let mut state_names: Vec<String> = files
        .iter()
        .map(|f| {
            // a base copy or a stage file is named by the path it holds
            if f.starts_with("base/") || f.starts_with("stage/") {
                format!("{}<path>", &f[..f.find('/').unwrap() + 1])
            } else {
                f.clone()
            }
        })
        .collect();
    state_names.dedup();
    for f in files
        .iter()
        .filter(|f| f.ends_with(".yaml") && !f.starts_with("base/"))
    {
        let text = std::fs::read_to_string(wc_dir.join(f)).expect("state file");
        let v: Value = serde_yaml_ng::from_str(&text).unwrap_or(json!(null));
        state.insert(f.clone(), shape(&v));
    }
    // the member files, as the person edits them
    let mut members = Map::new();
    for rel in ["guides/_folder.yaml", "guides/intro.yaml"] {
        let text = std::fs::read_to_string(s.dir.path().join(rel)).expect("member file");
        let v: Value = serde_yaml_ng::from_str(&text).expect("member YAML");
        members.insert(rel.to_string(), shape(&v));
    }
    // the edit input grammar: an unknown key is refused, with the nearest known key named
    let intro = s.dir.path().join("guides/intro.yaml");
    let original = std::fs::read_to_string(&intro).expect("read the doc file");
    let edited = original.replacen("  body: Claims hold.", "  bodyy: Claims hold.", 1);
    let bad = if edited == original {
        original.replacen("body: Claims hold.", "bodyy: Claims hold.", 1)
    } else {
        edited
    };
    assert_ne!(
        bad, original,
        "the claim's body line is in the file:\n{original}"
    );
    std::fs::write(&intro, &bad).expect("write the edited file");
    let (_ok, refusal, err) = run(&["check"]);
    let refusal = if refusal.is_null() {
        json!({"stderr": normalize(&err, &[s.dir.path()])})
    } else {
        refusal
    };
    let refusal = serde_json::from_str::<Value>(&normalize(&refusal.to_string(), &[s.dir.path()]))
        .unwrap_or(refusal);
    std::fs::write(&intro, &original).expect("restore the doc file");
    let mut ids_seen = ids;
    ids_seen.sort();
    json!({
        "sources": ["rdy checkout", ".rdy/wc", "rdy check", "rdy_engine::wc"],
        "state_directory": ".rdy/wc",
        "state_files": state_names,
        "state_shapes": state,
        "member_shapes": members,
        "checkout_reply": shape(&checkout),
        "unknown_key_refusal": refusal,
        "state_constants": {
            "wc_dir": rdy_engine::wc::state::WC_DIR,
            "worktrees_dir": rdy_engine::wc::state::WORKTREES_DIR,
            "pointer_file": rdy_engine::wc::state::POINTER_FILE,
            "checkout_file": rdy_engine::wc::state::CHECKOUT_FILE,
            "update_file": rdy_engine::wc::state::UPDATE_FILE,
            "max_briefs": rdy_engine::wc::state::MAX_BRIEFS,
            "blobs_dir": rdy_engine::wc::layout::BLOBS_DIR,
            "theirs_suffix": rdy_engine::wc::layout::THEIRS_SUFFIX,
            "folder_theirs": rdy_engine::wc::layout::FOLDER_THEIRS,
            "max_member_bytes": rdy_engine::wc::layout::MAX_MEMBER_BYTES,
        },
        "seeded_blocks": ids_seen.len(),
    })
}

// ---- signatures ----

fn signatures_row() -> Value {
    use rdy_engine::signature_check::check_signature;
    let mut vectors = Map::new();
    for (name, sk) in [("plain_ed25519", false), ("security_key", true)] {
        let v = rdy_qa::signing::vector(sk);
        let c = check_signature(&v.record, &v.context, Some(&v.trust));
        assert_eq!(c.code, None, "the existing vector verifies: {c:?}");
        vectors.insert(
            name.into(),
            json!({
                "statement": String::from_utf8_lossy(&v.record.statement),
                "namespace": v.record.namespace,
                "subject": v.record.subject.to_string(),
                "signature_id": v.record.id().to_string(),
                "sshsig_len": v.record.sig.len(),
                "sshsig_blake3": blake3_of(&v.record.sig),
                "signed_by": {"actor": v.record.signed_by.actor, "role": format!("{:?}", v.record.signed_by.role)},
                "key": c.key,
                "check_code": c.code,
            }),
        );
    }
    // the statement grammar, built from fixed inputs for every source and version
    let by = |actor: &str, run: Option<&str>| Provenance {
        actor: actor.into(),
        run: run.map(str::to_string),
        role: Role::Approver,
        at_ms: 1,
    };
    let mut landing = match rdy_qa::signing::vector(false).context.subject {
        rdy_engine::signature_check::SignatureSubject::Landing { record, .. } => record,
        other => panic!("the vector signs a landing: {other:?}"),
    };
    let repo = RepoId([0x14; 16]);
    let key = "SHA256:8nEdi2dFYm+E9JRNIKxJNA5RGKdcAaVkhr4NVgAnE/4";
    let text = |b: Vec<u8>| String::from_utf8(b).expect("a statement is UTF-8");
    let mut statements = Map::new();
    statements.insert(
        "approval_local".into(),
        json!(text(
            statement(StatementSource::Approval(&landing), Some(repo), key).unwrap()
        )),
    );
    landing.assurance = Assurance::Relayed;
    landing.relayed_by = Some(ActorRun {
        actor: "agent:qa".into(),
        run: Some("r1".into()),
    });
    landing.relay_evidence = Some(rdy_model::RelayEvidence {
        quoted: "ship it".into(),
        session_ref: "session-1".into(),
        question: None,
    });
    landing.evidence = Some(rdy_model::ApprovalEvidence {
        kind: rdy_model::EvidenceKind::Text,
        options: vec![],
        choice: None,
        card_hash: None,
    });
    statements.insert(
        "approval_relayed".into(),
        json!(text(
            statement(StatementSource::Approval(&landing), Some(repo), key).unwrap()
        )),
    );
    for v in [1u8, 2] {
        let b = rdy_model::signature::statement_for_version(
            StatementSource::RelayApproval(&landing),
            Some(repo),
            key,
            v,
        )
        .unwrap();
        statements.insert(format!("relay_approve_v{v}"), json!(text(b)));
    }
    statements.insert(
        "relay_evidence_hash".into(),
        json!(
            rdy_model::signature::relay_evidence_hash(&landing)
                .unwrap()
                .to_string()
        ),
    );
    let tag = rdy_model::Tag {
        name: "v1.0.0".into(),
        landing: Hash([4; 32]),
        message: None,
        tagged_by: by("human:qa", None),
    };
    statements.insert(
        "tag".into(),
        json!(text(
            statement(
                StatementSource::Tag {
                    hash: tag.id(),
                    tag: &tag
                },
                Some(repo),
                key
            )
            .unwrap()
        )),
    );
    let relay_answer = Answer {
        item: "tag:v1.0.0".into(),
        act: AnswerAct::Tag,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![],
        person: by("human:qa", None),
        relayed_by: Some(ActorRun {
            actor: "agent:qa".into(),
            run: Some("r1".into()),
        }),
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    };
    for v in [1u8, 2] {
        let b = rdy_model::signature::statement_for_version(
            StatementSource::RelayTag {
                hash: tag.id(),
                tag: &tag,
                answer_hash: Hash([5; 32]),
                answer: &relay_answer,
            },
            Some(repo),
            key,
            v,
        )
        .unwrap();
        statements.insert(format!("relay_tag_v{v}"), json!(text(b)));
    }
    statements.insert(
        "keybind".into(),
        json!(text(
            statement(
                StatementSource::Keybind {
                    account: "acct-1",
                    challenge: "challenge-1"
                },
                None,
                key
            )
            .unwrap()
        )),
    );
    let namespaces: Map<String, Value> = [
        StatementKind::Approval,
        StatementKind::Relay,
        StatementKind::Tag,
        StatementKind::Keybind,
    ]
    .into_iter()
    .map(|k| (k.as_str().to_string(), json!(k.namespace())))
    .collect();
    json!({
        "sources": ["rdy_model::signature", "rdy_qa::signing vectors", "rdy_engine::signature_check"],
        "algorithm": "ssh-ed25519 and sk-ssh-ed25519@openssh.com in an SSHSIG envelope with sha512",
        "envelope": "SSHSIG blob over the statement bytes, in the namespace of the statement kind",
        "key_representation": "SHA256:<43 base64 characters> fingerprint of the OpenSSH public key",
        "statement_header": "rdy-statement: <1 | 2>",
        "namespaces": namespaces,
        "limits": {
            "statement": rdy_model::signature::MAX_STATEMENT,
            "value": rdy_model::signature::MAX_VALUE,
            "namespace": rdy_model::signature::MAX_NAMESPACE,
            "sig": rdy_model::signature::MAX_SIG,
            "signature": rdy_model::signature::MAX_SIGNATURE,
            "signer_text": rdy_model::signature::MAX_SIGNER_TEXT,
        },
        "statements": statements,
        "vectors": vectors,
    })
}

// ---- cli ----

/// A small landed project for the probes of the release binary.
fn seed(r: &mut rdy_engine::Rdy) {
    rdy_qa::fixtures::seed_doc(r, "seed");
    rdy_qa::fixtures::land(r, "seed").expect("land the seed");
}

/// The facts of the release binary: the exit value and the JSON field set of each probe.
fn cli_runtime() -> Value {
    let (d, mut r) = project();
    seed(&mut r);
    r.close().expect("close");
    let (cfg, env) = rdy_qa::fixtures::human_env("human:qa");
    let mut probes = Map::new();
    let list: &[&[&str]] = &[
        &["--json", "status"],
        &["--json", "tags"],
        &["--json", "ls"],
        &["--json", "log"],
        &["--json", "tensions"],
        &["--json", "suspects"],
        &["--json", "pending"],
        &["--json", "coverage"],
        &["--json", "fsck"],
        &["--json", "upgrade"],
        &["--json", "show", "c-ZZZZZ"],
        &["--json", "no-such-command"],
    ];
    for args in list {
        let (code, out, err) = rdy_in(d.path(), &env, args);
        let body = if out.trim().is_empty() {
            err.trim()
        } else {
            out.trim()
        };
        let v: Value = serde_json::from_str(body).unwrap_or(json!(null));
        probes.insert(
            args[1..].join(" "),
            json!({"exit": code, "stream": if out.trim().is_empty() { "stderr" } else { "stdout" }, "json": shape(&v)}),
        );
    }
    let (code, out, _) = rdy_in(d.path(), &env, &["--version"]);
    let version = out.trim().replace(env!("CARGO_PKG_VERSION"), "<version>");
    drop(cfg);
    json!({
        "exit_values": {"success": 0, "refusal": 1, "usage": 2},
        "version_text": {"exit": code, "text": version},
        "probes": probes,
    })
}

// ---- mcp ----

/// The code, message and fix of a refused call.
fn refusal_of(reply: rdy_qa::mcp::Reply) -> Value {
    match reply {
        rdy_qa::mcp::Reply::ToolError(v) => json!({
            "code": v["code"], "message": v["message"], "fix": v["fix"],
        }),
        other => panic!("the call is refused, not {:?}", other.is_ok()),
    }
}

/// The read tools whose result fields the row records: each takes no required parameter and
/// changes nothing.
const READ_PROBES: &[&str] = &[
    "rdy_overview",
    "rdy_schema",
    "rdy_tags",
    "rdy_suspects",
    "rdy_tensions",
    "rdy_pending",
    "rdy_coverage",
    "rdy_log",
    "rdy_read",
    "rdy_plan",
    "rdy_key_show",
    "rdy_upgrade",
];

fn mcp_row() -> Value {
    use rdy_qa::mcp::{McpClient, Reply};
    let (d, mut r) = project();
    seed(&mut r);
    r.close().expect("close");
    let mut c = McpClient::start(d.path());
    let mut tools = c.tools();
    tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    assert!(tools.len() > 20, "the server lists its tools");
    fn sort_required(v: &mut Value) {
        match v {
            Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    if k == "required" {
                        if let Some(a) = x.as_array_mut() {
                            a.sort_by_key(|e| e.as_str().map(str::to_string));
                        }
                    }
                    sort_required(x);
                }
            }
            Value::Array(a) => a.iter_mut().for_each(sort_required),
            _ => {}
        }
    }
    let mut schemas = Map::new();
    for t in &mut tools {
        sort_required(t);
        let name = t["name"].as_str().expect("tool name").to_string();
        schemas.insert(name, json!({"input_schema": t["inputSchema"]}));
    }
    // the result fields of the read tools that take no required parameter, called in an agent
    // session; a call with a required parameter missing is a refusal and shows
    // the envelope
    let session = c.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "qa", "role": "writer"}),
    );
    let mut results = Map::new();
    results.insert(
        "rdy_session".into(),
        match session {
            Reply::Ok(v) => json!({"ok": shape(&v)}),
            _ => panic!("the session opens"),
        },
    );
    for name in READ_PROBES {
        let facts = match c.call(name, json!({})) {
            Reply::Ok(v) => json!({"ok": shape(&v)}),
            Reply::ToolError(v) => json!({"refused": shape(&v)}),
            Reply::RpcError(_) => json!({"protocol_error": true}),
        };
        results.insert((*name).to_string(), facts);
    }
    // the refusal envelope: a tool error holds code, message and fix
    let refusal = match c.call("rdy_context", json!({"id": "c-ZZZZZ"})) {
        Reply::ToolError(v) => shape(&v),
        other => panic!("an unknown block is a tool error: {:?}", other.is_ok()),
    };
    assert!(
        schemas.len() >= 50 && results.len() > 5,
        "the MCP surface has a source"
    );
    drop(c);
    // the review writer refuses the carried verdicts and the check kind (no product writer of a
    // Check review, a Pass, a Concern or a Fail exists)
    let mut rv = McpClient::start(d.path());
    let opened = rv.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "qa", "role": "reviewer"}),
    );
    assert!(opened.is_ok(), "the reviewer session opens");
    let mut refusals = Map::new();
    for verdict in ["pass", "concern", "fail"] {
        let args =
            json!({"request": "rq-000000000000", "case_hash": "0".repeat(64), "verdict": verdict});
        refusals.insert(
            format!("rdy_review_submit {verdict}"),
            refusal_of(rv.call("rdy_review_submit", args)),
        );
    }
    refusals.insert(
        "rdy_review_next check".into(),
        refusal_of(rv.call("rdy_review_next", json!({"kind": "check"}))),
    );
    json!({
        "sources": ["rdy mcp tools/list", "tools/call on read-only tools"],
        "tool_names": tools.iter().map(|t| t["name"].clone()).collect::<Vec<_>>(),
        "tools": schemas,
        "result_fields": results,
        "carried_value_refusals": refusals,
        "refusal_envelope": refusal,
        "refusal_error_rows": rdy_mcp::tools::ERROR_ROWS.iter().map(|(c, _, _)| *c).collect::<Vec<_>>(),
    })
}

// ---- errors and warnings ----

/// Every `E_` and `W_` code constant in the sources, and every `_FIX` and `_MESSAGE` template
/// constant, by scanning the crates' source text.
fn errors_row() -> Value {
    let consts = Regex::new(
        r#"(?s)const\s+([A-Z][A-Z0-9_]*)\s*:\s*&(?:'static\s+)?str\s*=\s*("(?:[^"\\]|\\.)*")\s*;"#,
    )
    .unwrap();
    let mut codes: BTreeMap<String, String> = BTreeMap::new();
    let mut templates: BTreeMap<String, String> = BTreeMap::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).expect("read the sources") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut files = vec![];
    for krate in std::fs::read_dir(workspace_root().join("crates")).expect("crates") {
        let src = krate.expect("crate").path().join("src");
        if src.is_dir() {
            walk(&src, &mut files);
        }
    }
    files.sort();
    for f in files {
        // test modules and the QA crate are not product text
        if f.components().any(|c| c.as_os_str() == "rdy-qa") {
            continue;
        }
        let text = std::fs::read_to_string(&f).expect("read a source file");
        let product = text.split("#[cfg(test)]").next().unwrap_or("");
        for m in consts.captures_iter(product) {
            let (name, value) = (m[1].to_string(), m[2].to_string());
            let template = name.ends_with("_FIX") || name.ends_with("_MESSAGE");
            if template {
                templates.insert(name.clone(), value.clone());
            }
            if (name.starts_with("E_") || name.starts_with("W_")) && !template {
                codes.insert(name, value);
            }
        }
    }
    assert!(
        codes.len() > 80 && templates.len() > 10,
        "the sources hold the registry: {} codes, {} templates",
        codes.len(),
        templates.len()
    );
    // the registered rows rdy_schema reports, in their order
    let rows: Vec<Value> = rdy_mcp::tools::ERROR_ROWS
        .iter()
        .map(|(code, when, fix)| json!([code, when, fix]))
        .collect();
    // the envelope of an error value
    let e = rdy_engine::RdyError::new(rdy_engine::E_BAD_INPUT, "message", "fix");
    let envelope = shape(&serde_json::to_value(&e).expect("an error serializes"));
    json!({
        "sources": ["crates/*/src error and warning constants", "rdy_mcp::tools::ERROR_ROWS", "rdy_engine::RdyError"],
        "codes": codes,
        "templates": templates,
        "error_rows": rows,
        "envelope": envelope,
        "redaction_warning": {
            "copies": rdy_engine::W_REDACTION_COPIES_MESSAGE,
            "inventory": rdy_engine::W_REDACTION_INVENTORY_MESSAGE,
            "quotes": rdy_engine::W_REDACTION_QUOTES_MESSAGE,
            "reuse": rdy_engine::W_REDACTION_REUSE_MESSAGE,
            "quarantine": rdy_engine::W_REDACTION_QUARANTINE_MESSAGE,
            "secret": rdy_engine::W_REDACTION_SECRET_MESSAGE,
        },
    })
}

// ---- config ----

fn config_row() -> Value {
    let keys: Vec<Value> = config_levels::KEY_LEVELS
        .iter()
        .map(|k| {
            json!({
                "key": k.key,
                "levels": k.levels.iter().map(|l| serde_json::to_value(l).unwrap()).collect::<Vec<_>>(),
                "type": format!("{:?}", k.ty),
                "default": k.default,
            })
        })
        .collect();
    assert!(!keys.is_empty());
    // an unknown key warns and the project still opens
    let (d, r) = project();
    drop(r);
    let cfg = d.path().join(".rdy.yaml");
    let mut text = std::fs::read_to_string(&cfg).expect("config");
    text.push_str("bogus_key: 1\n");
    std::fs::write(&cfg, &text).expect("write config");
    let warnings: Vec<Value> = config_levels::config_warnings(d.path(), None)
        .iter()
        .map(|w| {
            json!({
                "code": w.code,
                "message": normalize(&w.message, &[d.path()]),
                "fix": normalize(&w.fix, &[d.path()]),
            })
        })
        .collect();
    assert!(
        warnings.iter().any(|w| w["code"] == "W_CONFIG_UNKNOWN"),
        "an unknown key warns: {warnings:?}"
    );
    // a doc file a person edits refuses an unknown key
    let doc = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        doc.path().join("page.yaml"),
        "- id: d-AAAAA\n  type: doc\n  order: a0\n  fields:\n    title: Page\n    doc_type: page\n  bogus: 1\n",
    )
    .expect("write a doc");
    let (cfgdir, env) = rdy_qa::fixtures::human_env("human:qa");
    let (code, out, err) = rdy_in(
        d.path(),
        &env,
        &[
            "--json",
            "check",
            doc.path().join("page.yaml").to_str().unwrap(),
        ],
    );
    drop(cfgdir);
    let body = if out.trim().is_empty() {
        err.trim()
    } else {
        out.trim()
    };
    let refusal: Value = serde_json::from_str(&normalize(body, &[doc.path(), d.path()]))
        .unwrap_or(json!({"text": normalize(body, &[doc.path(), d.path()])}));
    json!({
        "sources": ["rdy_engine::config_levels::KEY_LEVELS", "config_warnings", "rdy check"],
        "project_file": config_levels::PROJECT_FILE,
        "local_file": config_levels::LOCAL_FILE,
        "keys": keys,
        "propose_cadences": config_levels::PROPOSE_CADENCES,
        "sign_modes": config_levels::SIGN_MODES,
        "unknown_key_warning": warnings,
        "editable_file_unknown_key": {"exit": code, "reply": refusal},
    })
}

// ---- schema packs ----

fn packs_row() -> Value {
    let set = PackSet::with_builtin();
    let std_pack = set
        .resolve("rdy-standard")
        .expect("the built-in pack resolves");
    let pack_json = serde_json::to_value(&std_pack).expect("a pack serializes");
    let bad_key = PackSet::default()
        .add_yaml("name: x\nversion: \"1\"\nbogus: 1\n")
        .expect_err("an unknown pack key is refused")
        .to_string();
    let bad_field = PackSet::default()
        .add_yaml(
            "name: x\nversion: \"1\"\nblock_types:\n  t:\n    prefix: t\n    fields:\n      f:\n        kind: nope\n",
        )
        .expect_err("an unknown field kind is refused")
        .to_string();
    let duplicate = {
        let mut s = PackSet::with_builtin();
        s.add_yaml_from("name: rdy-standard\nversion: \"9\"\n", "x.yaml")
            .expect_err("a taken name is refused")
            .to_string()
    };
    let pack_keys: Vec<String> = pack_json.as_object().unwrap().keys().cloned().collect();
    json!({
        "sources": ["rdy_schema::Pack", "rdy_schema::PackSet", "crates/rdy-schema/packs"],
        "manifest_keys": pack_keys,
        "builtin_packs": [{"name": std_pack.name, "version": std_pack.version}],
        "rdy_standard": pack_json,
        "tension_kinds": TensionKind::ALL.iter().map(|k| k.name()).collect::<Vec<_>>(),
        "tension_table": std_pack
            .tension_table()
            .iter()
            .map(|(k, s, soft)| json!([k.name(), s.to_string(), soft]))
            .collect::<Vec<_>>(),
        "severities": [TensionSeverity::Blocking.to_string(), TensionSeverity::Advisory.to_string()],
        "store_only": {
            "link_kinds": rdy_schema::STORE_ONLY_LINK_KINDS,
            "target_types": rdy_schema::STORE_ONLY_TARGET_TYPES,
            "fields": rdy_schema::STORE_ONLY_FIELDS,
        },
        "limits": {
            "max_reflects_globs": rdy_schema::MAX_REFLECTS_GLOBS,
            "max_glob_len": rdy_schema::MAX_GLOB_LEN,
            "max_path_segments": rdy_schema::MAX_PATH_SEGMENTS,
        },
        "validation": {
            "unknown_pack_key": bad_key,
            "unknown_field_kind": bad_field,
            "duplicate_pack_name": duplicate,
        },
    })
}

// ---- markdown exports ----

fn markdown_row() -> Value {
    use rdy_render::print::md::{Layout, write_markdown};
    let (_d, r, _ids) = rdy_qa::golden::site_with_decision();
    let p = rdy_render::print::build(&r.read(), &Scope::Canon).expect("the print builds");
    let mut layouts = Map::new();
    for (name, layout) in [
        ("plain", Layout::Plain),
        ("mkdocs", Layout::Mkdocs),
        ("techdocs", Layout::Techdocs),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("md");
        write_markdown(&p, &out, layout).expect("write the tree");
        let mut files = Map::new();
        fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for e in std::fs::read_dir(dir).expect("read dir") {
                let p = e.expect("entry").path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    out.push((
                        p.strip_prefix(base)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                        std::fs::read(&p).expect("read"),
                    ));
                }
            }
        }
        let mut all = vec![];
        walk(&out, &out, &mut all);
        all.sort();
        let heading = Regex::new(r"(?m)^#{1,6} .*$").unwrap();
        let link = Regex::new(r"\]\(([^)\s]+)\)").unwrap();
        let anchor = Regex::new(r#"\{#[^}]+\}|<a (?:id|name)="[^"]+""#).unwrap();
        for (rel, bytes) in all {
            let entry = if rel.ends_with(".md") || rel.ends_with(".yml") {
                let text = normalize(&String::from_utf8_lossy(&bytes), &[dir.path()]);
                let front: Value = text
                    .strip_prefix("---\n")
                    .and_then(|r| r.split_once("\n---\n"))
                    .and_then(|(f, _)| serde_yaml_ng::from_str::<Value>(f).ok())
                    .unwrap_or(json!(null));
                json!({
                    "front_matter_keys": front.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()),
                    "headings": heading.find_iter(&text).map(|m| m.as_str().to_string()).collect::<Vec<_>>(),
                    "anchors": anchor.find_iter(&text).map(|m| m.as_str().to_string()).collect::<Vec<_>>(),
                    "link_targets": link.captures_iter(&text).map(|c| c[1].to_string()).collect::<Vec<_>>(),
                    "normalized_blake3": blake3_of(text.as_bytes()),
                })
            } else {
                json!({"len": bytes.len(), "blake3": blake3_of(&bytes)})
            };
            files.insert(rel, entry);
        }
        assert!(files.contains_key("index.md") || files.contains_key("docs/index.md"));
        layouts.insert(name.into(), json!({"files": files}));
    }
    json!({
        "sources": ["rdy_render::print::md::write_markdown", "rdy_qa::golden site_with_decision"],
        "layouts": layouts,
    })
}

// ---- site urls ----

fn site_row() -> Value {
    use rdy_engine::site_scope::{CHANGES_PAGE, SINCE_PAGE, ScopeRef};
    use rdy_engine::site_target::SiteTarget;
    let hash = Hash([0xab; 32]);
    let proposal = rdy_engine::site_scope::proposal_ref(&format!("P-{}@2", &hash.to_string()[..8]))
        .expect("a proposal reference");
    let scopes = vec![
        ("canon", ScopeRef::Canon),
        ("view", ScopeRef::View("editing".into())),
        ("view_with_slash", ScopeRef::View("team/a b".into())),
        ("proposal", ScopeRef::Proposal(proposal)),
        ("tag", ScopeRef::Tag("v1.2.0".into())),
        ("versions", ScopeRef::Versions),
    ];
    let mut scope_rows = Map::new();
    for (name, s) in &scopes {
        scope_rows.insert(
            (*name).to_string(),
            json!({"text": s.text(), "prefix": s.path_prefix()}),
        );
    }
    let mut targets = Map::new();
    for (name, s) in &scopes {
        let source = SiteTarget::Source(s.clone());
        targets.insert(
            format!("source_{name}"),
            json!({"root": source.root(), "menu": source.menu_url(), "events": source.events_query()}),
        );
    }
    let profile = SiteTarget::Profile {
        scope: ScopeRef::Tag("v1.2.0".into()),
        key: "acme.docs".into(),
    };
    let only = SiteTarget::ProfileOnly { key: "acme".into() };
    let menu = SiteTarget::Menu {
        scope: ScopeRef::View("editing".into()),
        profile: Some("acme".into()),
    };
    for (name, t) in [
        ("profile", &profile),
        ("profile_only", &only),
        ("menu", &menu),
    ] {
        targets.insert(
            name.into(),
            json!({"root": t.root(), "menu": t.menu_url(), "events": t.events_query()}),
        );
    }
    // the pages of a rendered site: canon pages, block permalinks and assets
    let (_d, r, ids) = rdy_qa::golden::site_with_decision();
    let out = tempfile::tempdir().expect("tempdir");
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).expect("render canon");
    let mut routes: Vec<String> = rdy_qa::golden::pages(out.path())
        .iter()
        .map(|p| {
            normalize(
                &p.strip_prefix(out.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
                &[],
            )
        })
        .collect();
    routes.sort();
    routes.dedup();
    let _ = ids;
    let has_go = routes.iter().any(|r| r.starts_with("go/"));
    assert!(has_go, "a block permalink route is in the site: {routes:?}");
    let tag_ok: Vec<Value> = ["v1.2.0", "1.0", ".", "..", "a/b"]
        .iter()
        .map(|t| json!([t, rdy_engine::site_scope::tag_name_ok(t)]))
        .collect();
    let view_ok: Vec<Value> = ["editing", "a:b", ""]
        .iter()
        .map(|t| json!([t, rdy_engine::site_scope::view_name_ok(t)]))
        .collect();
    json!({
        "sources": ["rdy_engine::site_scope", "rdy_engine::site_target", "rdy_render::render_site"],
        "scopes": scope_rows,
        "targets": targets,
        "summary_pages": {"changes": CHANGES_PAGE, "since": SINCE_PAGE},
        "tag_name_ok": tag_ok,
        "view_name_ok": view_ok,
        "percent_encode": rdy_engine::site_scope::percent_encode("a b/c.d"),
        "canon_routes": routes,
        "block_route": "go/<block id>.html",
        "asset_route": "assets/",
    })
}

// ---- derived identities ----

fn identities_row() -> Value {
    let block = |s: &str| BlockId::parse(s).expect("a block id");
    let h = |n: u8| Hash([n; 32]);
    let involved = vec![
        Involved {
            side: SideRef::Change(h(1)),
            block: block("c-AAAAA"),
            version: Some(h(2)),
            content: Some(h(3)),
        },
        Involved {
            side: SideRef::Canon {
                landing: Some(h(4)),
            },
            block: block("c-AAAAA"),
            version: Some(h(5)),
            content: Some(h(6)),
        },
    ];
    let id = tension_id(
        TensionKind::EditCollision,
        TensionSeverity::Blocking,
        &involved,
    );
    let mut reversed = involved.clone();
    reversed.reverse();
    assert_eq!(
        id,
        tension_id(
            TensionKind::EditCollision,
            TensionSeverity::Blocking,
            &reversed
        ),
        "the order of the sides does not matter"
    );
    let vkey = tension_vkey(
        TensionKind::EditCollision,
        TensionSeverity::Blocking,
        &involved,
    );
    let items = [
        PairItem {
            block: block("c-AAAAA"),
            content: h(7),
        },
        PairItem {
            block: block("c-BBBBB"),
            content: h(8),
        },
    ];
    let side = |change: u8, version: u8| rdy_store::merge::Side {
        change: Some(h(change)),
        version: Some(h(version)),
    };
    let conflict =
        rdy_store::merge::conflict_id(&block("c-AAAAA"), Some(h(9)), &[side(1, 2), side(3, 4)]);
    // the scenario: derived state of a project, before and after a reindex, from fixed ids and
    // a pinned clock
    rdy_model::seed_ids_for_tests(14);
    let _clock = rdy_qa::golden::fixture_clock();
    let (d, r) = rdy_qa::packs::qa_project();
    let e = rdy_engine::Local::new(r);
    rdy_qa::inbox::build_agent(&e);
    let before = rdy_qa::inbox::on(&e, |r| rdy_qa::reindex_check::derived(r).expect("derived"));
    rdy_qa::inbox::on(&e, |r| r.reindex().expect("reindex"));
    let after = rdy_qa::inbox::on(&e, |r| rdy_qa::reindex_check::derived(r).expect("derived"));
    rdy_model::unseed_ids_for_tests();
    drop(d);
    assert_eq!(before, after, "a reindex preserves the derived identities");
    let consequence =
        rdy_engine::consequence::consequence_id(&h(1), "premise_shift", &block("c-AAAAA"));
    json!({
        "sources": ["rdy_engine::tensions", "rdy_store::merge", "rdy_model::semantic", "rdy_engine::consequence", "rdy_qa::reindex_check"],
        "domains": {
            "tension": "rdy-tension-2",
            "tension_vkey": "rdy-tension-vkey-2",
            "pair": "rdy-pair-1",
            "consequence": "rdy-consequence",
            "case": "rdy-case-1",
            "content": "rdy-content-2",
        },
        "tension": {
            "id": id.to_string(),
            "vkey": vkey.to_string(),
            "label": tension_label(&id),
        },
        "pair_key": pair_key(&items).to_string(),
        "conflict_id": conflict.to_string(),
        "consequence_id": consequence,
        "scenario": derived_json(&after),
    })
}

fn derived_json(d: &rdy_qa::reindex_check::Derived) -> Value {
    json!({
        "tensions": d.tensions,
        "conflicts": d.conflicts,
        "inboxes": d.inboxes,
        "logged_tensions": d.logged_tensions,
        "logged_suspects": d.logged_suspects,
        "transitions": d.transitions,
        "events": d.events,
    })
}

// ---- the inventory ----

fn generate() -> BTreeMap<&'static str, Value> {
    let mut rows = BTreeMap::new();
    rows.insert("store", store_row());
    rows.insert("dump_load", dump_load_row());
    rows.insert("sync", sync_row());
    rows.insert("working_copy", wc_row());
    rows.insert("signatures", signatures_row());
    rows.insert("mcp", mcp_row());
    rows.insert("errors_warnings", errors_row());
    rows.insert("config", config_row());
    rows.insert("schema_packs", packs_row());
    rows.insert("markdown_exports", markdown_row());
    rows.insert("site_urls", site_row());
    rows.insert("derived_identities", identities_row());
    // the cli row is generated here only for its runtime member
    rows.insert("cli", json!({"runtime": cli_runtime()}));
    rows
}

fn read_fixture() -> Value {
    if capture_mode() {
        // the generation writes the baseline once, before any test reads it
        rows();
    }
    let path = fixture_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "the release contract baseline {} is absent ({e}); capture it with RDY_CONTRACT_CAPTURE=1",
            path.display()
        )
    });
    serde_json::from_str(&text).expect("the release contract fixture is JSON")
}

fn capture(rows: &BTreeMap<&'static str, Value>) {
    let mut doc: Value = std::fs::read_to_string(fixture_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    doc["inventory_version"] = json!(INVENTORY_VERSION);
    for (name, row) in rows {
        if *name == "cli" {
            if !doc["cli"].is_object() {
                doc["cli"] = json!({});
            }
            doc["cli"]["runtime"] = row["runtime"].clone();
        } else {
            doc[*name] = row.clone();
        }
    }
    std::fs::create_dir_all(fixture_path().parent().unwrap()).expect("create the directory");
    let text = serde_json::to_string_pretty(&doc).expect("encode") + "\n";
    std::fs::write(fixture_path(), text).expect("write the fixture");
}

/// A cached generation: the rows take a while, and three tests read them.
fn rows() -> &'static BTreeMap<&'static str, Value> {
    static ROWS_NOW: std::sync::OnceLock<BTreeMap<&'static str, Value>> =
        std::sync::OnceLock::new();
    ROWS_NOW.get_or_init(|| {
        let rows = generate();
        if capture_mode() {
            capture(&rows);
        }
        rows
    })
}

#[test]
fn ok14_release_contract_matches_all_surfaces() {
    let fixture = read_fixture();
    assert_eq!(fixture["inventory_version"], json!(INVENTORY_VERSION));
    let live = rows();
    for name in ROWS {
        let now = live
            .get(name)
            .unwrap_or_else(|| panic!("no generator for {name}"));
        let want = &fixture[name];
        if name == "cli" {
            assert!(
                want["tree"].is_object(),
                "the fixture's cli.tree is the command tree rdy-cli compares"
            );
            assert_eq!(
                &now["runtime"], &want["runtime"],
                "row {name}: the release binary's exit values or JSON fields changed"
            );
            continue;
        }
        assert!(want.is_object(), "the fixture has no row {name}");
        assert_eq!(
            now, want,
            "row {name} changed: a public contract change needs an upgrade or deprecation path (D-TXFHR)"
        );
    }
}

#[test]
fn ok14_contract_inventory_cannot_drop_a_surface() {
    let fixture = read_fixture();
    let mut have: Vec<String> = fixture
        .as_object()
        .expect("the inventory is an object")
        .keys()
        .filter(|k| *k != "inventory_version")
        .cloned()
        .collect();
    have.sort();
    assert_eq!(
        have,
        ROWS.to_vec(),
        "the inventory holds the 13 surface rows, no more and no fewer"
    );
    // a row with no content is not coverage
    for name in ROWS {
        let row = &fixture[name];
        let members = row
            .as_object()
            .unwrap_or_else(|| panic!("row {name} is not an object"))
            .len();
        assert!(members >= 2, "row {name} holds no facts");
        if name != "cli" {
            assert!(
                row["sources"].as_array().is_some_and(|s| !s.is_empty()),
                "row {name} names no source"
            );
        }
    }
    // every generator is wired: the live generation has the same 13 rows
    let mut live: Vec<&str> = rows().keys().copied().collect();
    live.sort();
    assert_eq!(live, ROWS.to_vec());
}

#[test]
fn ok14_reindex_preserves_release_derived_identities() {
    let fixture = read_fixture();
    let want = &fixture["derived_identities"];
    let now = &rows()["derived_identities"];
    // the identities of fixed inputs and of the seeded scenario, before and after a rebuild:
    // generation asserted equality; the fixture pins the values
    assert_eq!(
        &now["scenario"], &want["scenario"],
        "the derived identities of unchanged input moved"
    );
    assert_eq!(&now["tension"], &want["tension"]);
    assert_eq!(&now["pair_key"], &want["pair_key"]);
    assert_eq!(&now["conflict_id"], &want["conflict_id"]);
    assert_eq!(&now["consequence_id"], &want["consequence_id"]);
    let scenario = &want["scenario"];
    assert!(
        scenario["tensions"]
            .as_object()
            .is_some_and(|m| !m.is_empty()),
        "the scenario holds a tension: {scenario}"
    );
    assert!(
        scenario["conflicts"]
            .as_object()
            .is_some_and(|m| !m.is_empty()),
        "the scenario holds a conflict: {scenario}"
    );
    assert!(
        scenario["inboxes"]
            .as_object()
            .is_some_and(|m| !m.is_empty()),
        "the scenario holds an inbox: {scenario}"
    );
}

#[test]
fn ok14_future_record_writers_remain_absent() {
    let fixture = read_fixture();
    // the MCP tools: no name or parameter writes an assessment, a dispute, a resolution or a
    // check request, and no review or answer parameter accepts the carried values
    let forbidden_words = [
        "assessment",
        "dispute",
        "resolution",
        "resolve_dispute",
        "check_request",
        "accept_verdict",
        "check_verdict",
    ];
    let tools = fixture["mcp"]["tools"]
        .as_object()
        .expect("the MCP tools are pinned");
    for (name, t) in tools {
        let lower = name.to_lowercase();
        for w in forbidden_words {
            assert!(!lower.contains(w), "tool {name} names a future record: {w}");
        }
        // a writer's parameter schema may not accept the carried values
        let schema = t["input_schema"].to_string().to_lowercase();
        for w in [
            "dispute",
            "resolve_dispute",
            "accept_verdict",
            "check_request",
        ] {
            assert!(
                !schema.contains(&format!("\"{w}\"")),
                "tool {name} accepts the future act {w}"
            );
        }
        for verdict in ["\"pass\"", "\"concern\"", "\"fail\""] {
            assert!(
                !schema.contains(verdict),
                "tool {name} accepts the future verdict {verdict}"
            );
        }
    }
    // the review writer takes only the pair verdicts, and no review kind is a check
    let refusals = fixture["mcp"]["carried_value_refusals"]
        .as_object()
        .expect("the refusals are pinned");
    for v in ["pass", "concern", "fail"] {
        assert_eq!(
            refusals[&format!("rdy_review_submit {v}")]["code"],
            "E_REVIEW_INVALID",
            "the review writer accepts the verdict {v}"
        );
    }
    assert_eq!(
        refusals["rdy_review_next check"]["code"], "E_BAD_INPUT",
        "a review kind check exists"
    );
    // the CLI tree: no command or flag names a future record or act
    let mut seen = vec![];
    fn names(t: &Value, out: &mut Vec<String>) {
        out.push(t["name"].as_str().unwrap_or_default().to_string());
        for a in t["args"].as_array().into_iter().flatten() {
            out.push(a["id"].as_str().unwrap_or_default().to_string());
            for c in a["choices"].as_array().into_iter().flatten() {
                out.push(c.as_str().unwrap_or_default().to_string());
            }
        }
        for s in t["subcommands"].as_array().into_iter().flatten() {
            names(s, out);
        }
    }
    names(&fixture["cli"]["tree"], &mut seen);
    assert!(seen.len() > 100, "the CLI tree is pinned");
    for n in &seen {
        let lower = n.to_lowercase();
        for w in forbidden_words {
            assert!(
                !lower.contains(w),
                "the CLI names a future record or act: {n}"
            );
        }
    }
    // the product answer path: no product code constructs the carried acts. The source of each
    // writer crate is scanned outside its test modules.
    let acts = [
        "AnswerAct::Dispute",
        "AnswerAct::ResolveDispute",
        "AnswerAct::AcceptVerdict",
    ];
    let verdicts = [
        "Verdict::Pass",
        "Verdict::Concern",
        "Verdict::Fail",
        "ReviewSubject::Check",
    ];
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).expect("read the sources") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut files = vec![];
    for krate in ["rdy-engine", "rdy-mcp", "rdy-daemon", "rdy-cli"] {
        walk(
            &workspace_root().join("crates").join(krate).join("src"),
            &mut files,
        );
    }
    let mut offenders = vec![];
    // the string forms of the carried acts: no product code names them either
    let literals = ["\"resolve_dispute\"", "\"accept_verdict\""];
    for f in &files {
        let text = std::fs::read_to_string(f).expect("read");
        let product = text.split("#[cfg(test)]").next().unwrap_or("");
        for l in literals {
            if product.contains(l) {
                offenders.push(format!("{}: names the act {l}", f.display()));
            }
        }
    }
    for f in files {
        let rel = f
            .strip_prefix(workspace_root().join("crates"))
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&f).expect("read");
        let product = text.split("#[cfg(test)]").next().unwrap_or("");
        for needle in acts.iter().chain(verdicts.iter()) {
            for (i, line) in product.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") || !line.contains(needle) {
                    continue;
                }
                // a match arm or a comparison reads the value; only a construction writes it
                let reads = t.contains("=>")
                    || t.contains("==")
                    || t.contains("matches!")
                    || t.contains("!=");
                if !reads {
                    offenders.push(format!("{rel}:{}: {t}", i + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "product code writes a carried act, verdict or subject:\n{}",
        offenders.join("\n")
    );
}
