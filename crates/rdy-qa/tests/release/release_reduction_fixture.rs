//! r5906 (D-3PNRK, D-4JMPH, D-WWAK7): plan deltas against the released formats. Every released
//! dump still loads; the reduction fixtures (written once by the io area's
//! `r5906_write_reduction_fixtures_once`) load as a whole dump and as a chain to the same store;
//! the native store survives dump, load, fsck and reindex; `plan_tension` is found in a frame's
//! own answers and seal; PLAN 1 survives a dump and a sync stream; an unknown PLAN encoding is
//! refused; and a full history moves between the chat and command surfaces through a loopback
//! TLS hub with every plan version intact.
#[allow(clippy::duplicate_mod)]
#[path = "../support/sync_hub.rs"]
mod sync_hub;

use rdy_engine::Rdy;
use rdy_model::plan::{
    PlanAct, PlanAnswer, PlanKind, PlanOutcome, PlanSeal, SealedItem, item_hash,
};
use rdy_model::plan_encoding::{PlanDeltaV1, delta_body, full_body};
use rdy_model::{BlockId, Hash, Plan, Provenance, Role};
use rdy_qa::foreign_fixture;
use rdy_store::closure::history_closure_before;
use rdy_store::format::{
    FEATURE_PLAN_DELTA, FEATURE_PLAN_TENSION, baseline_features, required_features,
};
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use rdy_store::sync_stream::StreamPurpose;
use rdy_store::take_batch::FetchTarget;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The released dump the reduction world starts from, and its pinned blake3.
const SOURCE: (&str, &str) = (
    "v4-f1-e1-ported-change.rdydump",
    "4dc15e868554e8d8b6dc486cacbdcd8865493576f9f25bc433b1e543a3513b7e",
);
/// The span-map fixture of an earlier task, which stays as it was frozen.
const DEDUP: [(&str, &str); 2] = [
    (
        "v4-f1-dedup-spans.rdydump",
        "25ca2264bb1dfaab5f795818d751e33ed5072a8c26b712b0a36739d99cfe19cf",
    ),
    (
        "v4-f1-dedup-spans.expect.json",
        "cf7643c1e2ad43fbbfcb03c28e25295b816c55842e8abad33b7ce2aaebb50440",
    ),
];
const DUMP: &str = "v4-f1-reduction.rdydump";
const CHAIN: [&str; 3] = [
    "v4-f1-reduction-chain-full.rdydump",
    "v4-f1-reduction-chain-1.rdydump",
    "v4-f1-reduction-chain-2.rdydump",
];
const SYNC: &str = "sync4-reduction.bin";
const SYNC_EXPECT: &str = "sync4-reduction.expect.json";

fn qa_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn dump(name: &str) -> PathBuf {
    qa_dir().join("fixtures/dump").join(name)
}

fn native() -> PathBuf {
    qa_dir().join("fixtures/store/reduction-1")
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn blake3(p: &Path) -> String {
    foreign_fixture::digest_of(p).1
}

/// The facts of an opened store that a transfer or reload must keep: its object keys, refs and
/// manifest.
fn kept(r: &Rdy) -> Value {
    let f = foreign_fixture::store_facts(r);
    json!({"object_keys": f["object_keys"], "refs": f["refs"], "manifest": f["manifest"]})
}

/// Loads `files` (one dump, or a chain) into `to`, then reindexes and checks it; returns its
/// kept facts.
fn loaded(files: &[PathBuf], to: &Path) -> Value {
    rdy_engine::load::load_chain(files, to, None)
        .unwrap_or_else(|e| panic!("{}: {e:?}", files[0].display()));
    let mut r = if to.join(".rdy.yaml").exists() {
        Rdy::open(to).unwrap()
    } else {
        Rdy::open_holder(to).unwrap()
    };
    r.reindex().unwrap();
    let report = r.fsck().unwrap_or_else(|e| panic!("fsck failed: {e:?}"));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let v = kept(&r);
    r.close().unwrap();
    v
}

/// Each PLAN frame of the store at `root` by its id: its raw encoding.
fn plan_encodings(r: &Rdy) -> BTreeMap<Hash, u16> {
    let mut out = BTreeMap::new();
    let end = r.store().log_end();
    r.read()
        .reader()
        .for_each_record_from(
            Location {
                segment: 0,
                offset: 0,
            },
            end,
            |_, f, _| {
                if let StoredFrame::Present {
                    kind,
                    hash,
                    payload,
                } = f
                    && kind & !rdy_store::codec::VERSIONED == kinds::PLAN
                {
                    let o = rdy_store::codec::split(kind, payload).unwrap();
                    out.insert(hash, o.version);
                }
                Ok(())
            },
        )
        .unwrap();
    out
}

fn agent() -> Provenance {
    Provenance {
        actor: "agent:plan".into(),
        run: None,
        role: Role::Writer,
        at_ms: 2,
    }
}

fn answer(n: u8, act: PlanAct) -> PlanAnswer {
    let cause = BlockId::parse("b-00001").unwrap();
    PlanAnswer {
        item: item_hash(&cause, &format!("app:src/t{n}.rs"), PlanKind::Tension),
        act,
        reason: Some("the tension is settled".into()),
        subject: None,
        cause: Hash([0x71; 32]),
        by: agent(),
    }
}

fn sealed(kind: PlanKind, outcome: PlanOutcome) -> PlanSeal {
    PlanSeal {
        landing: Hash([0x72; 32]),
        version: Hash([0x73; 32]),
        decisions: vec![],
        items: vec![SealedItem {
            id: Hash([0x74; 32]),
            kind,
            subject: "p-00001".into(),
            cause: BlockId::parse("b-00001").unwrap(),
            edge: "reflects".into(),
            outcome,
            answer: None,
        }],
    }
}

fn plan(answers: Vec<PlanAnswer>, seal: Option<PlanSeal>) -> Plan {
    Plan {
        stable: Some(Hash([0x75; 32])),
        prev: Some(Hash([0x76; 32])),
        view: "work".into(),
        proposal: None,
        answers,
        heads: vec![],
        provenance: agent(),
        seal,
        read_identity: None,
    }
}

/// The PLAN 1 frame of `body`, an encoding-1 body.
fn frame(body: Vec<u8>) -> (u8, Vec<u8>) {
    let mut payload = vec![1u8, 0];
    payload.extend(body);
    (kinds::PLAN | rdy_store::codec::VERSIONED, payload)
}

fn delta_of(p: &Plan) -> Vec<u8> {
    delta_body(&PlanDeltaV1 {
        stable: p.stable.unwrap(),
        prev: p.prev.unwrap(),
        view: p.view.clone(),
        proposal: p.proposal,
        answers_set: p.answers.clone(),
        answers_remove: vec![],
        heads: p.heads.clone(),
        provenance: p.provenance.clone(),
        seal: p.seal.clone(),
    })
}

fn names(raw: u8, payload: &[u8]) -> Vec<String> {
    required_features(raw, payload)
        .into_iter()
        .map(|f| f.name)
        .collect()
}

/// r5906: every released dump loads (each single file, and each chain whole); the reduction
/// dump and its chain load to the same objects, refs and features; the native store survives
/// dump, load, fsck and reindex; plan_tension is found in a whole version's answers, a delta's
/// set answers and a seal's Tension and Dismissed items, and not otherwise; PLAN 1 survives a
/// dump and a sync stream; an unknown PLAN encoding is refused; the old fixture digests and
/// the span-map fixture stay as frozen.
#[test]
fn r5906_released_fixtures_and_reduction_chain() {
    let scratch = tempfile::tempdir().unwrap();
    // the old fixtures keep their bytes
    assert_eq!(blake3(&dump(SOURCE.0)), SOURCE.1);
    for (name, digest) in DEDUP {
        assert_eq!(blake3(&dump(name)), digest, "{name}");
    }
    // every released dump loads, a chain from its full dump with both increments
    let mut released: Vec<String> = std::fs::read_dir(qa_dir().join("fixtures/dump"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".rdydump") && !n.contains("holder") && !n.contains("reduction"))
        .collect();
    released.sort();
    let mut n = 0;
    for name in &released {
        let files: Vec<PathBuf> = if name.ends_with("-chain-full.rdydump") {
            let stem = name.trim_end_matches("-full.rdydump");
            ["full", "1", "2"]
                .iter()
                .map(|s| dump(&format!("{stem}-{s}.rdydump")))
                .collect()
        } else if name.contains("-chain-") {
            continue;
        } else {
            vec![dump(name)]
        };
        n += 1;
        loaded(&files, &scratch.path().join(format!("released-{n}")));
    }
    assert!(n >= 20, "{n} released loads");

    // the whole reduction dump and its chain hold the same store
    let whole = loaded(&[dump(DUMP)], &scratch.path().join("whole"));
    let chain: Vec<PathBuf> = CHAIN.iter().map(|n| dump(n)).collect();
    let chained = loaded(&chain, &scratch.path().join("chain"));
    assert_eq!(chained, whole, "the chain loads to the whole dump's store");
    assert!(
        whole["manifest"]["features"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["name"] == FEATURE_PLAN_DELTA),
        "{whole}"
    );

    // the native store: fsck, reindex, then a dump and its load keep every object
    let (_base, root) = foreign_fixture::project_copy(&native());
    let mut r = Rdy::open(&root).unwrap();
    r.reindex().unwrap();
    r.fsck().unwrap();
    let facts = kept(&r);
    assert_eq!(facts, whole, "the native store is the dump's store");
    let encodings = plan_encodings(&r);
    let again = scratch.path().join("again.rdydump");
    r.read().dump(&again).unwrap();
    r.close().unwrap();
    rdy_qa::reindex_check::reindex_reproduces(&root).unwrap_or_else(|v| panic!("reindex: {v:#?}"));
    assert_eq!(
        loaded(
            std::slice::from_ref(&again),
            &scratch.path().join("reloaded")
        ),
        facts
    );
    // PLAN 0 and PLAN 1 records keep their encodings through the dump
    let r = Rdy::open(&scratch.path().join("reloaded")).unwrap();
    assert_eq!(plan_encodings(&r), encodings);
    r.close().unwrap();
    assert!(encodings.values().any(|v| *v == 0) && encodings.values().any(|v| *v == 1));

    // PLAN 1 survives the sync stream: its fetch into the released history takes the plans
    let want = read_json(&qa_dir().join("fixtures/sync").join(SYNC_EXPECT));
    let recv = scratch.path().join("recv");
    rdy_engine::load::load(&dump(SOURCE.0), &recv).unwrap();
    let mut dst = Rdy::open(&recv).unwrap();
    let t = FetchTarget {
        remote: "origin".into(),
        // test-hygiene: allow port the sync state records this URL; nothing connects to it
        url: "https://127.0.0.1:9/r/reduction".into(),
        purpose: StreamPurpose::CanonHistory,
        root: want["root"].as_str().unwrap().parse().unwrap(),
        view: None,
    };
    let file = std::fs::File::open(qa_dir().join("fixtures/sync").join(SYNC)).unwrap();
    let got = rdy_engine::sync_fetch::fetch(&mut dst, &[file], &t).unwrap();
    assert_eq!(json!(got.records_taken), want["fetch"]["records_taken"]);
    let taken = plan_encodings(&dst);
    let r = Rdy::open(&root).unwrap();
    let mut synced = 0;
    for (h, v) in &taken {
        assert_eq!(encodings.get(h), Some(v), "{h}");
        assert_eq!(
            dst.store().reader().plan(h).unwrap(),
            r.store().reader().plan(h).unwrap(),
            "{h}"
        );
        synced += usize::from(*v == 1);
    }
    assert_eq!(json!(synced), want["fetch"]["plans_taken"]);
    dst.close().unwrap();

    // an unknown PLAN encoding is refused, and nothing is written
    let end = r.store().log_end();
    drop(r);
    let mut r = Rdy::open(&root).unwrap();
    let mut payload = vec![2u8, 0];
    payload.extend(full_body(&plan(vec![], None)));
    let e = r
        .store_mut()
        .write_guarded_for_tests(vec![(kinds::PLAN | rdy_store::codec::VERSIONED, payload)])
        .expect_err("an unknown PLAN encoding is refused");
    assert!(format!("{e:?}").contains("encoding"), "{e:?}");
    assert_eq!(r.store().log_end(), end, "nothing is written");
    r.close().unwrap();

    // plan_tension: a frame's own answers and seal, never a predecessor
    let tension = FEATURE_PLAN_TENSION.to_string();
    let delta_feature = FEATURE_PLAN_DELTA.to_string();
    let dismiss = plan(vec![answer(1, PlanAct::Dismiss)], None);
    let (raw, p) = frame(full_body(&dismiss));
    assert!(names(raw, &p).contains(&tension), "Full.answers Dismiss");
    assert!(names(raw, &p).contains(&delta_feature));
    let (raw, p) = frame(delta_of(&dismiss));
    assert!(
        names(raw, &p).contains(&tension),
        "Delta.answers_set Dismiss"
    );
    let seal_tension = plan(vec![], Some(sealed(PlanKind::Tension, PlanOutcome::Open)));
    let (raw, p) = frame(delta_of(&seal_tension));
    assert!(names(raw, &p).contains(&tension), "seal Tension");
    let seal_dismissed = plan(
        vec![],
        Some(sealed(PlanKind::Confirm, PlanOutcome::Dismissed)),
    );
    let (raw, p) = frame(full_body(&seal_dismissed));
    assert!(names(raw, &p).contains(&tension), "seal Dismissed");
    let none = plan(
        vec![answer(2, PlanAct::Confirm)],
        Some(sealed(PlanKind::Confirm, PlanOutcome::Confirmed)),
    );
    for (raw, p) in [frame(full_body(&none)), frame(delta_of(&none))] {
        let got = names(raw, &p);
        assert!(!got.contains(&tension), "{got:?}");
        assert!(got.contains(&delta_feature), "{got:?}");
    }

    // a new store does not write the feature: its first use does (baseline false)
    assert!(
        !baseline_features()
            .features
            .iter()
            .any(|f| f.name == FEATURE_PLAN_DELTA)
    );
    // the span-map fixture is as frozen
    for (name, digest) in DEDUP {
        assert_eq!(blake3(&dump(name)), digest, "{name}");
    }
}

// ---------------------------------------------------------------------------------------------
// the full history through the hub

/// A kit whose project is a copy of the reduction corpus, with the share remote `origin` on
/// the hub.
fn corpus_kit() -> sync_hub::Kit {
    use sync_hub::*;
    let hub = Hub::start();
    let home = Home::new(&hub);
    home.signed_in(hub.origin());
    let dir = rdy_qa::daemon::short_tempdir();
    let (_base, copy) = foreign_fixture::project_copy(&native());
    Rdy::open(&copy).unwrap().close().unwrap();
    copy_dir(&copy, dir.path());
    let _ = std::fs::remove_file(dir.path().join(".rdy/daemon.json"));
    let cfg = dir.path().join(".rdy.yaml");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("serve:\n  idle_timeout: 60\n");
    std::fs::write(&cfg, text).unwrap();
    let proj = Proj {
        dir,
        landings: vec![],
    };
    proj.remote(&home, &format!("{}{REMOTE_PATH}", hub.origin()));
    Kit { hub, home, proj }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// The canon history closure of the project at `root`, and each PLAN in it as it reads.
fn closure(root: &Path) -> (BTreeSet<ObjectKey>, BTreeMap<Hash, Plan>) {
    let r = Rdy::open(root).unwrap();
    let st = r.read();
    let tip = st.canon_head().expect("canon");
    let found = history_closure_before(
        st.reader(),
        StreamPurpose::CanonHistory,
        tip,
        &BTreeSet::new(),
        None,
        st.seq(),
    )
    .unwrap();
    let keys: BTreeSet<ObjectKey> = found.records.iter().copied().collect();
    let plans = keys
        .iter()
        .filter(|k| k.kind == kinds::PLAN)
        .map(|k| (k.hash, st.reader().plan(&k.hash).unwrap()))
        .collect();
    drop(st);
    r.close().unwrap();
    (keys, plans)
}

/// Checks what a push sent, then serves the pull. Every put batch passes core's stream check
/// and the batches hold every key of `want`, the source's canon closure, and hub canon is the
/// source's canon. As the sync hub fixture does for every pull, the hub then serves known and
/// get from a copy of the project. Keep the returned copy while the hub serves it.
fn serve_pushed(k: &sync_hub::Kit, want: &BTreeSet<ObjectKey>) -> (tempfile::TempDir, Rdy) {
    let puts = k.hub.of("/objects/put");
    assert!(!puts.is_empty(), "the push sent its batches");
    let mut sent: BTreeSet<ObjectKey> = BTreeSet::new();
    for req in &puts {
        let report = rdy_engine::stream_check::check_batch(&req.body[..], None).unwrap();
        sent.extend(report.keys.iter().copied());
    }
    let missing: Vec<&ObjectKey> = want.difference(&sent).collect();
    assert!(
        missing.is_empty(),
        "the push sent every record: {missing:?}"
    );
    let (dir, h) = k.proj.copy(&k.home);
    let tip = h.read().canon_head().expect("canon");
    assert_eq!(
        k.hub.canon(),
        Some(tip.to_string()),
        "the push moved hub canon"
    );
    k.hub.serve(h.read());
    let repo = h.read().repo_id().unwrap().to_string();
    k.hub.set_canon(Some(tip), Some(repo));
    (dir, h)
}

/// The receiver's closure equals the source's: every object, and every plan version as it reads.
fn assert_same(source: &Path, receiver: &Path, what: &str) {
    let (keys, plans) = closure(source);
    let (got_keys, got_plans) = closure(receiver);
    assert_eq!(got_keys, keys, "{what}: closure objects");
    assert_eq!(got_plans, plans, "{what}: plan versions");
    let encodings = |root: &Path| {
        let r = Rdy::open(root).unwrap();
        let e = plan_encodings(&r);
        r.close().unwrap();
        e
    };
    let (from, to) = (encodings(source), encodings(receiver));
    for h in plans.keys() {
        assert_eq!(to.get(h), from.get(h), "{what}: {h} keeps its encoding");
    }
    assert!(
        plans.keys().any(|h| from[h] == 0) && plans.keys().any(|h| from[h] == 1),
        "{what}: a mixed-encoding history"
    );
}

/// r5906: the small mixed-encoding corpus (PLAN 0 and PLAN 1, whole and delta) goes to an
/// empty hub repo by a chat push and comes back by a command-line clone into an empty
/// directory; then the surfaces are reversed. Each receiver holds every closure object and
/// every logical plan version of the source, not only current canon.
#[test]
fn r5906_full_history_transfer_preserves_plans() {
    use sync_hub::*;
    // a chat push, a command-line pull
    let k = corpus_kit();
    let repo = k.proj.repo_id();
    let (want, _) = closure(k.proj.path());
    let mut c = chat(&k, Some("writer"));
    let v = mcp_ok(&mut c, "rdy_push", json!({}));
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    drop(c);
    k.proj.stop_daemon(&k.home);
    let (_hub_dir, _hub) = serve_pushed(&k, &want);
    let parent = tempfile::tempdir().unwrap();
    let to = parent.path().join("pulled");
    let v = cli_ok(&k, &["clone", &hub_url(&k), to.to_str().unwrap()]);
    assert_eq!(v["repo_id"], repo, "{v}");
    k.proj.stop_daemon(&k.home);
    assert_same(k.proj.path(), &to, "chat push, command pull");

    // a command-line push, a chat pull
    let k = corpus_kit();
    let repo = k.proj.repo_id();
    let (want, _) = closure(k.proj.path());
    let v = cli_ok(&k, &["push"]);
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    k.proj.stop_daemon(&k.home);
    let (_hub_dir, _hub) = serve_pushed(&k, &want);
    let mut c = chat(&k, Some("writer"));
    let v = mcp_ok(
        &mut c,
        "rdy_clone",
        json!({"dir": "pulled", "url": hub_url(&k)}),
    );
    assert_eq!(v["repo_id"], repo, "{v}");
    drop(c);
    k.proj.stop_daemon(&k.home);
    assert_same(
        k.proj.path(),
        &k.proj.path().join("pulled"),
        "command push, chat pull",
    );
}
