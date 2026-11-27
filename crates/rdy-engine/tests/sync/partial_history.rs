//! r59018 (D-GGTYA, D-4PV25): a project whose history is still arriving. It opens at its canon
//! state for read, brief, write, propose and land at once; every call that needs records behind
//! the frontier refuses with E_HISTORY_PENDING, and every read that omits history says so once
//! with W_HISTORY_PENDING.
use rdy_engine::*;
use rdy_model::*;
use rdy_store::Store;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_state::SyncHistory;
use rdy_store::sync_stream::StreamPurpose;
use rdy_store::take_batch::FetchTarget;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::path::{Path, PathBuf};

fn qa_sync_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rdy-qa/fixtures/sync")
        .join(name)
}

fn expect(name: &str) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(qa_sync_fixture(name)).unwrap()).unwrap()
}

/// The frozen canon state's repo and root.
fn state_facts() -> (RepoId, Hash) {
    let e = expect("sync5-canon-state.expect.json");
    let repo = e["source"]["repo_id"].as_str().unwrap();
    let mut id = [0u8; 16];
    for (i, b) in id.iter_mut().enumerate() {
        *b = u8::from_str_radix(&repo[2 * i..2 * i + 2], 16).unwrap();
    }
    let root: Hash = e["source"]["root"]["hash"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    (RepoId(id), root)
}

/// The frozen canon history's keys, in its export order.
fn history_keys() -> Vec<ObjectKey> {
    let e = expect("sync4-canon-history.expect.json");
    e["stream"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| ObjectKey {
            kind: k["kind"].as_u64().unwrap() as u8,
            hash: k["hash"].as_str().unwrap().parse().unwrap(),
        })
        .collect()
}

// test-hygiene: allow port the fetch target records this URL; nothing connects to it
const URL: &str = "https://127.0.0.1:9/r/demo";

/// A project at `root` that took the frozen canon state: an initialized project whose store is
/// replaced by a new store of the state's repo, the state taken into it, and the history's
/// checked key file written.
fn partial_project(root: &Path) {
    let (repo, frontier) = state_facts();
    Rdy::init(root, "rdy-standard").unwrap().close().unwrap();
    let store = root.join(".rdy").join(rdy_store::layout::STORE_DIR);
    std::fs::remove_dir_all(&store).unwrap();
    let mut s = Store::create_fork_project(&store, repo).unwrap();
    let keys = history_keys();
    let history = SyncHistory {
        repo_id: repo,
        url: URL.into(),
        frontier,
        complete: false,
        records: keys.len() as u64,
    };
    let target = FetchTarget {
        remote: "origin".into(),
        url: URL.into(),
        purpose: StreamPurpose::CanonState,
        root: frontier,
        view: None,
    };
    let file = std::fs::File::open(qa_sync_fixture("sync5-canon-state.bin")).unwrap();
    s.take_canon_state::<Infallible>(&[file], &target, &history, |_, _| Ok(()))
        .unwrap();
    s.close().unwrap();
    write_keys(root, repo, frontier, keys);
}

/// Writes the history's checked key file of remote origin.
fn write_keys(root: &Path, repo: RepoId, frontier: Hash, keys: Vec<ObjectKey>) {
    let path = partial_history::key_file(root, "origin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let value = partial_history::HistoryKeys {
        repo_id: repo,
        frontier,
        keys,
    };
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn writer() -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, view: &str, ops: serde_json::Value) -> Result<WriteResult, RdyError> {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "r59018".into(),
        },
    )
}

const BLOCK: &str = "d-JQKR9";

fn pending(e: &RdyError, what: &str) {
    assert_eq!(e.code, E_HISTORY_PENDING, "{what}: {e:?}");
    assert_eq!(
        e.message, "the project's history is still arriving (4 of 19 records)",
        "{what}"
    );
    assert_eq!(
        e.fix, "wait for the clone to finish, or run rdy pull to resume it",
        "{what}"
    );
}

/// r59018: every surface that needs records behind the frontier refuses with the one
/// E_HISTORY_PENDING, before it creates or writes anything.
#[test]
fn r59018_pending_refusals_cover_all_surfaces() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    std::fs::create_dir(&root).unwrap();
    partial_project(&root);
    let mut r = Rdy::open(&root).unwrap();
    let st = r.read();
    pending(&st.require_complete_history().unwrap_err(), "the guard");
    // whole and incremental dump, before any byte
    let out = d.path().join("dump.rdy");
    pending(&st.dump(&out).unwrap_err(), "dump");
    assert!(!out.exists());
    // local clone, fork and fork-at, before the destination is made
    for (what, fork, at) in [("clone", false, None), ("fork", true, None)] {
        let dir = d.path().join(what);
        let e = clone::clone_at(clone::CloneSource::State(&st), &dir, fork, at).unwrap_err();
        pending(&e, what);
        assert!(!dir.exists(), "{what}");
    }
    let tags = st.versions().unwrap();
    let held = tags.first().expect("the state holds its tag").name.clone();
    let dir = d.path().join("fork-at");
    let e = clone::clone_at(clone::CloneSource::State(&st), &dir, true, Some(&held)).unwrap_err();
    pending(&e, "fork-at");
    assert!(!dir.exists());
    // a read at a version or landing the state does not hold
    pending(&st.resolve_scope("tag:0.0.1").unwrap_err(), "old tag");
    pending(
        &st.resolve_scope("landing:0000000000000000").unwrap_err(),
        "old landing",
    );
    // the held frontier tag stays readable
    let at = st.resolve_scope(&format!("tag:{held}")).unwrap();
    assert!(st.context(&at, &BLOCK.parse().unwrap()).is_ok());
    drop(st);
    // an explicit id that may name a block deleted behind the frontier
    let end = r.store().log_end();
    let e = write(
        &mut r,
        "w",
        serde_json::json!([{"op": "create", "type": "prose", "id": "p-AAAAA", "parent": BLOCK, "body": "Back."}]),
    )
    .unwrap_err();
    pending(&e, "explicit id");
    assert_eq!(r.store().log_end(), end, "a refusal writes nothing");
    // held local deletion evidence keeps the existing rule: a block made and deleted here is
    // re-created by its explicit id
    let made = write(
        &mut r,
        "m",
        serde_json::json!([{"op": "create", "type": "prose", "id": "$p", "parent": BLOCK, "body": "Made here."}]),
    )
    .unwrap();
    let made = made.created["$p"].clone();
    let p = r
        .propose(&writer(), "m", "m", Some(crate::common::TEST_SUMMARY))
        .unwrap();
    crate::common::confirm_exceptions(&mut r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
    write(
        &mut r,
        "x",
        serde_json::json!([{"op": "delete", "id": made}]),
    )
    .unwrap();
    let p = r
        .propose(&writer(), "x", "x", Some(crate::common::TEST_SUMMARY))
        .unwrap();
    crate::common::confirm_exceptions(&mut r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
    write(
        &mut r,
        "y",
        serde_json::json!([{"op": "create", "type": "prose", "id": made, "parent": BLOCK, "body": "Back."}]),
    )
    .unwrap();
}

/// r59018: read, write, propose and land work on the partial project; the history reads omit
/// what is behind the frontier and say so once with W_HISTORY_PENDING; the log stops there.
#[test]
fn r59018_read_warnings_do_not_hide_content() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    std::fs::create_dir(&root).unwrap();
    partial_project(&root);
    let mut r = Rdy::open(&root).unwrap();
    let id: BlockId = BLOCK.parse().unwrap();
    {
        let st = r.read();
        assert!(st.context(&Scope::Canon, &id).is_ok());
        let w = st.history_warning().unwrap().expect("the warning");
        assert_eq!(w.code, W_HISTORY_PENDING);
        assert_eq!(w.message, "history is still arriving (4 of 19 records)");
        assert_eq!(
            w.fix,
            "wait for the clone to finish, or run rdy pull to resume it"
        );
        // the last change of the root's block is behind the frontier: left out, no error
        assert!(
            st.last_changes(&Scope::Canon, std::slice::from_ref(&id))
                .unwrap()
                .is_empty()
        );
        assert!(
            st.history(&Scope::Canon, &id, 10)
                .unwrap()
                .entries
                .is_empty()
        );
        // the context, the brief and the export each say so once, after their own warnings
        let once = |w: &[rdy_schema::Issue], what: &str| {
            assert_eq!(
                w.iter().filter(|w| w.code == W_HISTORY_PENDING).count(),
                1,
                "{what}: {w:?}"
            );
            assert_eq!(w.last().unwrap().code, W_HISTORY_PENDING, "{what}");
        };
        once(&st.context(&Scope::Canon, &id).unwrap().warnings, "context");
        let cov = Coverage {
            ids: vec![id.to_string()],
            path: None,
        };
        once(
            &st.brief(&Scope::Canon, &cov, None).unwrap().warnings,
            "brief",
        );
        let out = d.path().join("export");
        let summary = st.export(&Scope::Canon, &out).unwrap();
        assert_eq!(
            summary
                .warnings
                .iter()
                .filter(|w| w.starts_with("W_HISTORY_PENDING: "))
                .count(),
            1,
            "{:?}",
            summary.warnings
        );
        assert_eq!(
            summary.warnings.last().unwrap(),
            "W_HISTORY_PENDING: history is still arriving (4 of 19 records); wait for the clone to finish, or run rdy pull to resume it"
        );
        // the canon walk stops at the frontier; the log lists the held frontier landing, whose
        // proposal is behind it, with its columns left empty
        assert_eq!(st.landings().unwrap().len(), 1);
        let log = st.log_page(None, None).unwrap();
        assert_eq!(log.len(), 1, "the frontier landing is logged");
        let (_, frontier) = state_facts();
        assert_eq!(log[0].landing, frontier.to_string());
        assert_eq!(log[0].title, "");
        assert_eq!(log[0].proposed_by.actor, "");
        assert!(log[0].approved.is_none());
    }
    // an ordinary edit of held content: the version is the frontier map's, whose producer is
    // behind the frontier; it is written, proposed and landed like any other edit
    write(
        &mut r,
        "u",
        serde_json::json!([{"op": "update", "id": BLOCK, "fields": {"title": "Edited locally"}}]),
    )
    .unwrap();
    let p = r
        .propose(&writer(), "u", "u", Some(crate::common::TEST_SUMMARY))
        .unwrap();
    crate::common::confirm_exceptions(&mut r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
    {
        let st = r.read();
        assert_eq!(st.landings().unwrap().len(), 2);
        let c = serde_json::to_value(st.context(&Scope::Canon, &id).unwrap()).unwrap();
        assert!(c.to_string().contains("Edited locally"), "{c}");
        // the local edit's provenance is held: its last change is the local one
        assert_eq!(
            st.last_changes(&Scope::Canon, std::slice::from_ref(&id))
                .unwrap()
                .len(),
            1
        );
    }
    // a fresh write, its proposal and its landing extend the frontier normally
    write(
        &mut r,
        "w",
        serde_json::json!([{"op": "create", "type": "prose", "id": "$p", "parent": BLOCK, "body": "A new paragraph."}]),
    )
    .unwrap();
    let p = r
        .propose(&writer(), "w", "w", Some(crate::common::TEST_SUMMARY))
        .unwrap();
    crate::common::confirm_exceptions(&mut r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
    let st = r.read();
    assert_eq!(st.landings().unwrap().len(), 3);
    assert_eq!(
        st.log_page(None, None).unwrap().len(),
        3,
        "the local landings are logged, then the frontier"
    );
    assert!(st.history_warning().unwrap().is_some(), "still partial");
    drop(st);
    // the local landings' deltas continue the chain from the frontier's map
    let report = r.fsck().unwrap();
    assert_eq!(report.landings, 3);
    assert!(!report.history.unwrap().complete);
    // the tension read (restatement drift over the held history) says so once
    let e = rdy_engine::engine::Local::new(r);
    let t =
        rdy_engine::tensions::api::run_tensions(&e as &dyn Engine, &writer(), Default::default())
            .unwrap();
    assert_eq!(t["warnings"].as_array().unwrap().len(), 1, "{t}");
    assert_eq!(t["warnings"][0]["code"], W_HISTORY_PENDING, "{t}");
    drop(e);
    // a delete of held content, in a second partial project: proposed and landed
    let root = d.path().join("q");
    std::fs::create_dir(&root).unwrap();
    partial_project(&root);
    let mut r = Rdy::open(&root).unwrap();
    write(
        &mut r,
        "x",
        serde_json::json!([{"op": "delete", "id": BLOCK}]),
    )
    .unwrap();
    let p = r
        .propose(&writer(), "x", "x", Some(crate::common::TEST_SUMMARY))
        .unwrap();
    crate::common::confirm_exceptions(&mut r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
    let st = r.read();
    assert_eq!(st.landings().unwrap().len(), 2);
    assert!(st.context(&Scope::Canon, &id).is_err(), "deleted");
    drop(st);
    let report = r.fsck().unwrap();
    assert_eq!(report.landings, 2);
}

/// r59018: the progress is exact (listed keys held plus staged trailer records, at most the
/// total) and a read appends nothing to the store.
#[test]
fn r59018_progress_is_exact_and_not_persisted() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    std::fs::create_dir(&root).unwrap();
    partial_project(&root);
    let r = Rdy::open(&root).unwrap();
    let end = r.store().log_end();
    let st = r.read();
    let (_, frontier) = state_facts();
    let p = st.history_progress().unwrap().unwrap();
    assert_eq!(
        p,
        partial_history::HistoryProgress {
            frontier,
            complete: false,
            held: 4,
            total: 19,
        }
    );
    // the listed keys the store holds, counted from the frozen expectations
    let held = history_keys()
        .iter()
        .filter(|k| st.reader().has_kind(k.kind, &k.hash).unwrap())
        .count() as u64;
    assert_eq!(held, p.held);
    assert_eq!(r.store().log_end(), end, "a progress read writes nothing");
    drop(st);
    // a damaged key file is refused, not guessed around
    let path = partial_history::key_file(&root, "origin");
    std::fs::write(
        &path,
        br#"{"repo_id":"00","frontier":"00","keys":[],"extra":1}"#,
    )
    .unwrap();
    drop(r);
    let r = Rdy::open(&root).unwrap();
    assert!(r.read().history_progress().is_err());
}

/// R2: two progress reads on one state open the key file once and hash no staged payload; a
/// staged batch counts by its trailer alone.
#[test]
fn r59018_a_read_hashes_no_staged_payload_and_reads_the_key_file_once() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    std::fs::create_dir(&root).unwrap();
    partial_project(&root);
    // a staged batch of the history: the frozen canon history stream
    std::fs::copy(
        qa_sync_fixture("sync4-canon-history.bin"),
        partial_history::history_dir(&root, "origin").join("000001.batch"),
    )
    .unwrap();
    let r = Rdy::open(&root).unwrap();
    let st = r.read();
    let (opens, hashes) = (
        partial_history::key_file_opens_for_tests(),
        partial_history::staged_payload_hashes_for_tests(),
    );
    let first = st.history_progress().unwrap().unwrap();
    let second = st.history_progress().unwrap().unwrap();
    assert!(st.history_warning().unwrap().is_some());
    assert_eq!(first, second);
    assert_eq!(partial_history::key_file_opens_for_tests() - opens, 1);
    assert_eq!(
        partial_history::staged_payload_hashes_for_tests() - hashes,
        0
    );
    // the trailer's 19 records reach the total, never past it
    assert_eq!(first.held, 19);
    assert!(!first.complete);
}

/// r59018: fsck of the partial project hashes every held frame and reports the exact partial
/// history; the full check refuses the missing history; a damaged key file fails it.
#[test]
fn r59018_partial_fsck_is_strict() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    std::fs::create_dir(&root).unwrap();
    partial_project(&root);
    let r = Rdy::open(&root).unwrap();
    let report = r.fsck().unwrap();
    assert_eq!(
        report.history,
        Some(FsckHistory {
            complete: false,
            held: 4,
            total: 19,
        })
    );
    assert_eq!(report.history.unwrap().line(), "history: partial (4 of 19)");
    let v = serde_json::to_value(&report).unwrap();
    assert_eq!(
        v["history"],
        serde_json::json!({"complete": false, "held": 4, "total": 19})
    );
    pending(&r.fsck_full().unwrap_err(), "full check");
    drop(r);
    // a key file of another frontier fails the check
    let path = partial_history::key_file(&root, "origin");
    let mut keys: partial_history::HistoryKeys =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    keys.frontier = Hash::of(b"elsewhere");
    std::fs::write(&path, serde_json::to_vec(&keys).unwrap()).unwrap();
    let r = Rdy::open(&root).unwrap();
    let e = r.fsck().unwrap_err();
    assert_eq!(e.code, E_BAD_RECORD, "{e:?}");
    drop(r);
    partial_fsck_strict_cases();
}

/// r59018 (fix round 1): the full check runs before completion. The frozen canon history taken
/// into the partial project holds every listed record; the full check then checks all of it,
/// with no partial-history exception, while the ref stays incomplete; ordinary fsck still stops
/// at the frontier until completion sets the flag.
#[test]
fn r59018_full_check_runs_before_completion() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    std::fs::create_dir(&root).unwrap();
    partial_project(&root);
    let (_, frontier) = state_facts();
    {
        let mut s = Store::open(&root.join(".rdy").join(rdy_store::layout::STORE_DIR)).unwrap();
        let target = FetchTarget {
            remote: "origin".into(),
            url: URL.into(),
            purpose: StreamPurpose::CanonHistory,
            root: frontier,
            view: None,
        };
        let file = std::fs::File::open(qa_sync_fixture("sync4-canon-history.bin")).unwrap();
        s.take_fetched::<Infallible>(&[file], &target, |_, _| Ok(()))
            .unwrap();
        s.close().unwrap();
    }
    let r = Rdy::open(&root).unwrap();
    let held = FsckHistory {
        complete: false,
        held: 19,
        total: 19,
    };
    assert_eq!(
        r.read().history_progress().unwrap().unwrap().held,
        19,
        "every listed record is held"
    );
    // the ordinary check keeps the partial rules while the ref is incomplete
    let partial = r.fsck().unwrap();
    assert_eq!(partial.landings, 1);
    assert_eq!(partial.history, Some(held));
    // the full check checks every landing of the history, and the ref stays incomplete
    let full = r.fsck_full().unwrap();
    assert_eq!(full.landings, 3);
    assert_eq!(full.history, Some(held));
    assert!(
        r.read().history_warning().unwrap().is_some(),
        "still partial"
    );
    let epoch = r.store().redaction_epoch();
    drop(r);
    {
        let mut s = Store::open(&root.join(".rdy").join(rdy_store::layout::STORE_DIR)).unwrap();
        s.complete_history("origin", frontier, epoch).unwrap();
        s.close().unwrap();
    }
    let r = Rdy::open(&root).unwrap();
    let report = r.fsck().unwrap();
    assert_eq!(report.landings, 3);
    assert_eq!(
        report.history,
        Some(FsckHistory {
            complete: true,
            held: 19,
            total: 19,
        })
    );
    assert_eq!(r.fsck_full().unwrap().landings, 3);
    assert!(r.read().history_warning().unwrap().is_none());
}

/// A source project: a section with two linked claims, landed; then an edit of the referenced
/// claim whose plan items are confirmed twice, landed with its sealed plan (a delta on a whole
/// plan version). Returns the project and the root landing.
fn sealed_plan_source(root: &Path) -> (Rdy, Hash) {
    let mut r = Rdy::init(root, "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "work",
        serde_json::json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$x", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$x", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$x", "body": "B builds on {{$a}}."}
        ]),
    )
    .unwrap();
    let claim = w.created["$a"].clone();
    let p = r
        .propose(&writer(), "work", "work", Some(crate::common::TEST_SUMMARY))
        .unwrap();
    crate::common::confirm_exceptions(&mut r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
    write(
        &mut r,
        "w-plan",
        serde_json::json!([{"op": "update", "id": claim, "body": "A holds 40 days."}]),
    )
    .unwrap();
    let items: Vec<String> = r
        .read()
        .view_plan("w-plan")
        .unwrap()
        .items
        .iter()
        .filter_map(|row| match row {
            rdy_engine::plan::api::PlanRow::Live(i) => Some(i.id.clone()),
            _ => None,
        })
        .collect();
    assert!(!items.is_empty(), "the edit opens a plan item");
    for reason in ["it still holds", "it still holds, checked again"] {
        let answers: Vec<rdy_engine::plan::PlanAnswerInput> = items
            .iter()
            .map(|i| rdy_engine::plan::PlanAnswerInput {
                item: i.clone(),
                act: rdy_model::plan::PlanAct::Confirm,
                reason: Some(reason.into()),
            })
            .collect();
        r.answer_plan(&writer(), "w-plan", &answers).unwrap();
    }
    let p = r
        .propose(
            &writer(),
            "w-plan",
            "w-plan",
            Some(crate::common::TEST_SUMMARY),
        )
        .unwrap();
    crate::common::confirm_exceptions(&mut r, &founder(), &p);
    let landing: Hash = r.land(&founder(), &p.id).unwrap().parse().unwrap();
    (r, landing)
}

/// r59018 (fix round 1): a valid carried sealed PLAN. A canon state whose root carries a sealed
/// plan (its `prev` chain to the Full record) is taken into a fresh project; partial fsck takes
/// the plan's frontier links (`stable`, `proposal`, `seal.version`) as deferred, as the take
/// does. Once the canon history arrives, the full check validates those links.
#[test]
fn r59018_a_carried_sealed_plan_passes_partial_fsck() {
    use rdy_engine::canon_state_export::PreparedCanonState;
    let d = tempfile::tempdir().unwrap();
    let (src, root) = sealed_plan_source(&d.path().join("src"));
    src.fsck().unwrap();
    let repo = src.store().repo_id().unwrap().unwrap();
    // the canon state of the root, one batch
    let state = PreparedCanonState::new(src.read(), root).unwrap();
    let keys = state.keys().to_vec();
    assert!(
        keys.iter().any(|k| k.kind == rdy_store::kinds::PLAN),
        "the state carries the plan"
    );
    let state_path = d.path().join("state.batch");
    state
        .write_keys(&keys, std::fs::File::create(&state_path).unwrap())
        .unwrap();
    // the canon history of the root, one format-4 batch, and its keys
    let reader = src.store().reader();
    let end = rdy_store::closure::END_OF_LOG;
    let found = rdy_store::closure::history_closure_before(
        &reader,
        StreamPurpose::CanonHistory,
        root,
        &Default::default(),
        None,
        end,
    )
    .unwrap();
    let header = rdy_store::sync_stream::StreamHeader::canon_history(
        found.features.clone(),
        repo,
        root,
        found.prerequisites.clone(),
    );
    let mut source = rdy_store::closure::StoreSource {
        reader: &reader,
        end,
    };
    let history_path = d.path().join("history.batch");
    rdy_store::closure::write_batch(
        &mut source,
        &header,
        &found.records,
        std::fs::File::create(&history_path).unwrap(),
    )
    .unwrap();
    let history_keys = found.records.clone();
    drop(reader);
    drop(state);
    drop(src);

    // a fresh project of the source's repo takes the state
    let dest = d.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    Rdy::init(&dest, "rdy-standard").unwrap().close().unwrap();
    let store_dir = dest.join(".rdy").join(rdy_store::layout::STORE_DIR);
    std::fs::remove_dir_all(&store_dir).unwrap();
    let mut s = Store::create_fork_project(&store_dir, repo).unwrap();
    let history = SyncHistory {
        repo_id: repo,
        url: URL.into(),
        frontier: root,
        complete: false,
        records: history_keys.len() as u64,
    };
    let target = FetchTarget {
        remote: "origin".into(),
        url: URL.into(),
        purpose: StreamPurpose::CanonState,
        root,
        view: None,
    };
    s.take_canon_state::<Infallible>(
        &[std::fs::File::open(&state_path).unwrap()],
        &target,
        &history,
        |_, _| Ok(()),
    )
    .unwrap();
    // the store's partial object check keeps the table's deferred PLAN links; its full check
    // takes no partial-history exception, so the missing history fails it
    s.reader().check_objects().unwrap();
    assert!(s.reader().full_history_reader().check_objects().is_err());
    s.close().unwrap();
    write_keys(&dest, repo, root, history_keys);
    let r = Rdy::open(&dest).unwrap();
    let report = r.fsck().unwrap();
    assert!(!report.history.unwrap().complete);
    pending_any(&r.fsck_full().unwrap_err());
    drop(r);

    // fix round 2: a completed ref keeps no partial exception. Completed with no history
    // taken (the caller's full check skipped), the records behind the frontier are missing,
    // and both store checks refuse the store
    {
        let bare = d.path().join("bare");
        copy_tree(&store_dir, &bare);
        let mut s = Store::open(&bare).unwrap();
        let epoch = s.redaction_epoch();
        s.complete_history("origin", root, epoch).unwrap();
        for reader in [s.reader(), s.reader().full_history_reader()] {
            assert!(reader.check_objects().is_err());
        }
        s.close().unwrap();
    }

    // the canon history arrives: the full check validates the plan's deferred links
    {
        let mut s = Store::open(&store_dir).unwrap();
        let target = FetchTarget {
            purpose: StreamPurpose::CanonHistory,
            ..target
        };
        s.take_fetched::<Infallible>(
            &[std::fs::File::open(&history_path).unwrap()],
            &target,
            |_, _| Ok(()),
        )
        .unwrap();
        s.close().unwrap();
    }
    let r = Rdy::open(&dest).unwrap();
    let full = r.fsck_full().unwrap();
    assert_eq!(full.landings, 2);
    assert!(!full.history.unwrap().complete);
    let epoch = r.store().redaction_epoch();
    drop(r);

    // fix round 2: completion keeps the carried chain. The completed ref still names it, so
    // its links may have arrived in the later history batch, and each must be held
    {
        let mut s = Store::open(&store_dir).unwrap();
        s.complete_history("origin", root, epoch).unwrap();
        s.reader().check_objects().unwrap();
        s.reader().full_history_reader().check_objects().unwrap();
        s.close().unwrap();
    }
    let r = Rdy::open(&dest).unwrap();
    let done = r.fsck().unwrap();
    assert_eq!(done.landings, 2);
    assert!(done.history.unwrap().complete);
    let full = r.fsck_full().unwrap();
    assert_eq!(full.landings, 2);
    assert!(full.history.unwrap().complete);
    assert!(r.read().history_warning().unwrap().is_none());
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

fn pending_any(e: &RdyError) {
    assert_eq!(e.code, E_HISTORY_PENDING, "{e:?}");
}

/// The canon state of `root` in `src`, one batch at `dir/state.batch`, and its canon history,
/// one batch at `dir/history.batch`, with the history's keys.
fn split_source(src: &Rdy, root: Hash, dir: &Path) -> (PathBuf, PathBuf, Vec<ObjectKey>) {
    use rdy_engine::canon_state_export::PreparedCanonState;
    let state = PreparedCanonState::new(src.read(), root).unwrap();
    let keys = state.keys().to_vec();
    let state_path = dir.join("state.batch");
    state
        .write_keys(&keys, std::fs::File::create(&state_path).unwrap())
        .unwrap();
    let repo = src.store().repo_id().unwrap().unwrap();
    let reader = src.store().reader();
    let end = rdy_store::closure::END_OF_LOG;
    let found = rdy_store::closure::history_closure_before(
        &reader,
        StreamPurpose::CanonHistory,
        root,
        &Default::default(),
        None,
        end,
    )
    .unwrap();
    let header = rdy_store::sync_stream::StreamHeader::canon_history(
        found.features.clone(),
        repo,
        root,
        found.prerequisites.clone(),
    );
    let mut source = rdy_store::closure::StoreSource {
        reader: &reader,
        end,
    };
    let history_path = dir.join("history.batch");
    rdy_store::closure::write_batch(
        &mut source,
        &header,
        &found.records,
        std::fs::File::create(&history_path).unwrap(),
    )
    .unwrap();
    (state_path, history_path, found.records.clone())
}

/// A fresh project at `dest` of repo `repo` that took the canon state at `state` (root `root`)
/// and wrote the checked key file of `keys`.
fn take_state(dest: &Path, repo: RepoId, root: Hash, state: &Path, keys: Vec<ObjectKey>) {
    std::fs::create_dir(dest).unwrap();
    Rdy::init(dest, "rdy-standard").unwrap().close().unwrap();
    let store_dir = dest.join(".rdy").join(rdy_store::layout::STORE_DIR);
    std::fs::remove_dir_all(&store_dir).unwrap();
    let mut s = Store::create_fork_project(&store_dir, repo).unwrap();
    let history = SyncHistory {
        repo_id: repo,
        url: URL.into(),
        frontier: root,
        complete: false,
        records: keys.len() as u64,
    };
    let target = FetchTarget {
        remote: "origin".into(),
        url: URL.into(),
        purpose: StreamPurpose::CanonState,
        root,
        view: None,
    };
    s.take_canon_state::<Infallible>(
        &[std::fs::File::open(state).unwrap()],
        &target,
        &history,
        |_, _| Ok(()),
    )
    .unwrap();
    s.reader().check_objects().unwrap();
    s.close().unwrap();
    write_keys(dest, repo, root, keys);
}

/// r59018 (D-4PV25, the carried row): a canon state that carries an active REDACTION_APPLY is
/// taken, and its removal proof waits only for the apply's landing, which is behind the
/// frontier. The store's partial check passes; its full check refuses the missing history.
#[test]
fn r59018_a_carried_removal_is_taken() {
    let w = crate::canon_state::rich();
    let root = w.root;
    let repo = w.r.store().repo_id().unwrap().unwrap();
    let d = tempfile::tempdir().unwrap();
    let (state, _, keys) = split_source(&w.r, root, d.path());
    let dest = d.path().join("dest");
    take_state(&dest, repo, root, &state, keys);
    let store_dir = dest.join(".rdy").join(rdy_store::layout::STORE_DIR);
    let s = Store::open(&store_dir).unwrap();
    s.reader().check_objects().unwrap();
    assert!(s.reader().full_history_reader().check_objects().is_err());
    s.close().unwrap();
    // (the source's signatures are not real, so the store's checks stand for fsck here)
    let r = Rdy::open(&dest).unwrap();
    assert!(r.read().history_warning().unwrap().is_some());
}

/// The sealed batches of the store at `dir`, in log order: each batch's frames (raw kind and
/// payload), its marker left out.
fn store_batches(dir: &Path) -> Vec<Vec<(u8, Vec<u8>)>> {
    use rdy_store::redaction::StoredFrame;
    let s = Store::open(dir).unwrap();
    let r = s.reader();
    let (mut out, mut cur) = (vec![], vec![]);
    let origin = rdy_store::segment::Location {
        segment: 0,
        offset: 0,
    };
    r.for_each_record_from(origin, s.log_end(), |_, f, _| {
        let StoredFrame::Present { kind, payload, .. } = f else {
            panic!("the fixture drops no payload");
        };
        if rdy_store::codec::logical_kind(kind) == rdy_store::kinds::BATCH_COMMIT {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push((kind, payload.to_vec()));
        }
        Ok(())
    })
    .unwrap();
    drop(r);
    s.close().unwrap();
    out
}

/// Writes `batches` as the whole log of the store at `dir`, each batch sealed; the derived files
/// (index and span map) are removed, so the open rebuilds them.
fn write_batches(dir: &Path, batches: &[Vec<(u8, Vec<u8>)>]) {
    let segs = dir.join("segments");
    std::fs::remove_dir_all(&segs).unwrap();
    std::fs::create_dir(&segs).unwrap();
    for derived in std::fs::read_dir(dir).unwrap() {
        let path = derived.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !["FORMAT", "FEATURES", "lock", "segments"].contains(&name.as_str()) {
            if path.is_dir() {
                std::fs::remove_dir_all(&path).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
        }
    }
    let (mut log, _) =
        rdy_store::segment::SegmentLog::open_marked(&segs, 64 * 1024 * 1024, None).unwrap();
    for b in batches.iter().filter(|b| !b.is_empty()) {
        log.append_batch(b).unwrap();
    }
    log.sync().unwrap();
}

/// Replaces every copy of `from`'s bytes with `to`'s in the payloads of `batches`, then every
/// copy of the hash of each payload that changed, until none changes; returns each moved hash.
fn move_hash(batches: &mut [Vec<(u8, Vec<u8>)>], from: Hash, to: Hash) -> BTreeMap<Hash, Hash> {
    let mut moved = BTreeMap::new();
    let mut todo = vec![(from, to)];
    while let Some((a, b)) = todo.pop() {
        moved.insert(a, b);
        for batch in batches.iter_mut() {
            for (_, p) in batch.iter_mut() {
                let old = Hash::of(p);
                if replace_bytes(p, &a.0, &b.0) {
                    todo.push((old, Hash::of(p)));
                }
            }
        }
    }
    moved
}

fn replace_bytes(p: &mut [u8], from: &[u8], to: &[u8]) -> bool {
    let mut hit = false;
    let mut i = 0;
    while i + from.len() <= p.len() {
        if &p[i..i + from.len()] == from {
            p[i..i + from.len()].copy_from_slice(to);
            hit = true;
            i += from.len();
        } else {
            i += 1;
        }
    }
    hit
}

/// The first frame of logical kind `kind` in `batches`: its batch, place and hash.
fn frame_of(batches: &[Vec<(u8, Vec<u8>)>], kind: u8) -> (usize, usize, Hash) {
    for (b, batch) in batches.iter().enumerate() {
        for (i, (k, p)) in batch.iter().enumerate() {
            if rdy_store::codec::logical_kind(*k) == kind {
                return (b, i, Hash::of(p));
            }
        }
    }
    panic!("no frame of kind {kind}");
}

/// The strict cases of [`r59018_partial_fsck_is_strict`]: every record the partial project
/// holds is checked, the frontier landing edge by edge against the canon state link table
/// (D-4PV25). Each case forges the fixture's taken state from its own frames, so only the named
/// damage differs; a forged copy with no damage passes.
fn partial_fsck_strict_cases() {
    use rdy_store::kinds;
    let d = tempfile::tempdir().unwrap();
    let base = d.path().join("base");
    std::fs::create_dir(&base).unwrap();
    partial_project(&base);
    let (repo, frontier) = state_facts();
    let store_of = |root: &Path| root.join(".rdy").join(rdy_store::layout::STORE_DIR);
    let original = store_batches(&store_of(&base));
    let rec = Store::open(&store_of(&base))
        .unwrap()
        .reader()
        .landing(&frontier)
        .unwrap();
    let (_, _, block) = frame_of(&original, kinds::BLOCK);
    // each case: its name, the damage, and whether the state's checks already refused it before
    // the frontier landing's edges were checked (a guard)
    type Damage = Box<dyn Fn(&mut Vec<Vec<(u8, Vec<u8>)>>) -> Hash>;
    let in_landing = |from: Hash, to: Hash| -> Damage {
        Box::new(move |b: &mut Vec<Vec<(u8, Vec<u8>)>>| {
            let (bi, i, _) = frame_of(b, kinds::LANDING);
            assert!(replace_bytes(&mut b[bi][i].1, &from.0, &to.0));
            let new = Hash::of(&b[bi][i].1);
            let moved = move_hash(b, frontier, new);
            moved[&frontier]
        })
    };
    let drop_kind = |kind: u8| -> Damage {
        Box::new(move |b: &mut Vec<Vec<(u8, Vec<u8>)>>| {
            let (bi, i, _) = frame_of(b, kind);
            b[bi].remove(i);
            frontier
        })
    };
    let cases: Vec<(&str, Damage)> = vec![
        (
            "none",
            Box::new(move |_: &mut Vec<Vec<(u8, Vec<u8>)>>| frontier),
        ),
        (
            "approved version held as a block",
            in_landing(rec.approved_version.unwrap(), block),
        ),
        ("change held as a block", in_landing(rec.changes[0], block)),
        (
            "assessment held as a block",
            in_landing(rec.assessment.unwrap(), block),
        ),
        ("prev held as a block", in_landing(rec.prev.unwrap(), block)),
        (
            "bad snapshot hash",
            Box::new(move |b: &mut Vec<Vec<(u8, Vec<u8>)>>| {
                let (bi, i, _) = frame_of(b, kinds::SNAPSHOT);
                let last = b[bi][i].1.len() - 1;
                b[bi][i].1[last] ^= 1;
                frontier
            }),
        ),
        (
            "noncanonical snapshot bytes",
            Box::new(move |b: &mut Vec<Vec<(u8, Vec<u8>)>>| {
                let (bi, i, old) = frame_of(b, kinds::SNAPSHOT);
                b[bi][i].1.push(0);
                let new = Hash::of(&b[bi][i].1);
                b[bi][i].1.pop();
                let moved = move_hash(b, old, new);
                b[bi][i].1.push(0);
                moved.get(&frontier).copied().unwrap_or(frontier)
            }),
        ),
        ("missing map block", drop_kind(kinds::BLOCK)),
        ("missing delta", drop_kind(kinds::SNAPSHOT_DELTA)),
        (
            "missing snapshot (never absent)",
            drop_kind(kinds::SNAPSHOT),
        ),
        ("missing tag (carried)", drop_kind(kinds::TAG)),
        (
            "wrong delta",
            Box::new(move |b: &mut Vec<Vec<(u8, Vec<u8>)>>| {
                let (bi, i, old) = frame_of(b, kinds::SNAPSHOT_DELTA);
                let mut p = b[bi][i].1.clone();
                let last = p.len() - 1;
                p[last] ^= 1;
                let new = Hash::of(&p);
                b[bi][i].1 = p;
                move_hash(b, old, new);
                frontier
            }),
        ),
        (
            "corrupt history value",
            Box::new(move |b: &mut Vec<Vec<(u8, Vec<u8>)>>| {
                for batch in b.iter_mut() {
                    for (k, p) in batch.iter_mut() {
                        // the value's `complete` flag, after its frontier, is no bool
                        if rdy_store::codec::logical_kind(*k) == kinds::REF_UPDATE
                            && p.windows(24).any(|w| w == b"meta/sync/origin/history")
                        {
                            let at = p.windows(32).position(|w| w == frontier.0).unwrap();
                            p[at + 32] = 7;
                        }
                    }
                }
                frontier
            }),
        ),
    ];
    let mut outcomes = vec![];
    for (name, damage) in cases {
        let root = d.path().join(name.replace(' ', "-"));
        copy_tree(&base, &root);
        let mut batches = original.clone();
        let new_frontier = damage(&mut batches);
        write_batches(&store_of(&root), &batches);
        write_keys(&root, repo, new_frontier, history_keys());
        let got = Rdy::open(&root).and_then(|r| r.fsck().map(|_| ()));
        outcomes.push((name, got.map_err(|e| format!("{}: {}", e.code, e.message))));
    }
    for (name, got) in outcomes {
        if name == "none" {
            assert!(got.is_ok(), "{name}: {got:?}");
        } else {
            assert!(got.is_err(), "{name}: the partial check passed");
        }
    }
}

/// r59018: the status's progress never fails on the key file. Missing or damaged, the history
/// is partial with the held count the status can prove, and the key file's error is returned
/// beside it as the corruption warning.
#[test]
fn r59018_status_progress_survives_a_bad_key_file() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    std::fs::create_dir(&root).unwrap();
    partial_project(&root);
    let (_, frontier) = state_facts();
    let path = partial_history::key_file(&root, "origin");
    for damage in [None, Some(&b"not a key file"[..])] {
        match damage {
            None => std::fs::remove_file(&path).unwrap(),
            Some(bytes) => std::fs::write(&path, bytes).unwrap(),
        }
        let r = Rdy::open(&root).unwrap();
        let st = r.read();
        assert!(st.history_progress().is_err(), "the strict progress fails");
        let (p, e) = st.history_status().unwrap().expect("a partial history");
        assert_eq!(
            (p.frontier, p.complete, p.held, p.total),
            (frontier, false, 0, 19)
        );
        let e = e.expect("the key file's error");
        assert_ne!(e.code, W_HISTORY_PENDING);
        assert_eq!(p.warning().code, W_HISTORY_PENDING);
    }
}
