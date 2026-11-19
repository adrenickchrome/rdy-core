//! m4bc3: an import is one unsealed batch, sealed once. Every failure before the seal abandons
//! the whole batch, so no later finalizer (`Local::write`, the close, a republish after a panic)
//! can seal part of it; a refusal before append or a confirmed rollback leaves the project as
//! it was after reopen and reindex. A rollback that cannot be proved is the indeterminate
//! refusal, the handle stays poisoned, and a complete marked batch survives reopen and reindex.
//! After the seal, the import stands: a failed baseline is `coverage_error`.
//!
//! Every failure case imports two authors' blocks, one imported decision status and one blob
//! into an unowned view.
use rdy_engine::engine::{Engine, Local};
use rdy_engine::import_prepared::{ImportFault, fail_next_import_at_for_tests};
use rdy_engine::*;
use rdy_model::{ActorRun, Hash, Role};
use rdy_store::segment::Location;
use rdy_store::{Store, kinds};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

const FIGURE: &[u8] = b"m4bc3 figure bytes";
const VIEW: &str = "import";
const INDETERMINATE: (&str, &str, &str) = (
    E_IO,
    "import outcome is indeterminate",
    "reopen the project and inspect the target view before retrying",
);

fn importer() -> Session {
    Session {
        actor: "human:imp".into(),
        run: None,
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

fn figure() -> Hash {
    Store::blob_hash(FIGURE)
}

/// One block row of an export tree's doc file.
fn row(id: &str, ty: &str, parent: Option<&str>, order: &str, fields: &[(&str, &str)]) -> String {
    let mut s = format!("- id: {id}\n  type: {ty}\n");
    if let Some(p) = parent {
        s.push_str(&format!("  parent: {p}\n"));
    }
    s.push_str(&format!("  order: {order}\n"));
    if !fields.is_empty() {
        s.push_str("  fields:\n");
        for (k, v) in fields {
            s.push_str(&format!("    {k}: {v}\n"));
        }
    }
    s
}

/// The export tree at `out`: doc `atomic` with section, figure (the blob) and an accepted
/// decision. agent:a wrote the doc and section at 100 and the figure at 300; agent:b wrote the
/// decision at 200. `extra` is appended to the doc file.
fn tree(out: &Path, extra: &str) {
    std::fs::create_dir_all(out.join("docs")).unwrap();
    std::fs::create_dir_all(out.join("blobs")).unwrap();
    std::fs::write(
        out.join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: work\n",
    )
    .unwrap();
    std::fs::write(out.join("blobs").join(figure().to_string()), FIGURE).unwrap();
    let blob = format!("'{}'", figure());
    let mut docs = row(
        "d-MP001",
        "doc",
        None,
        "a1",
        &[("doc_type", "page"), ("key", "atomic"), ("title", "Atomic")],
    );
    docs.push_str(&row(
        "s-MP001",
        "section",
        Some("d-MP001"),
        "a1",
        &[("title", "Rulings")],
    ));
    docs.push_str(&row(
        "fg-MP001",
        "figure",
        Some("s-MP001"),
        "a1",
        &[
            ("alt", "bytes"),
            ("blob", &blob),
            ("media_type", "application/octet-stream"),
        ],
    ));
    docs.push_str(&row(
        "D-MP001",
        "decision",
        Some("s-MP001"),
        "a2",
        &[
            ("authority_basis", "precedent"),
            ("needs_content", "false"),
            ("reversibility", "two_way"),
            ("statement", "Imports are atomic"),
            ("status", "accepted"),
        ],
    ));
    docs.push_str(extra);
    std::fs::write(out.join("docs").join("atomic.yaml"), docs).unwrap();
    let author = |id: &str, actor: &str, run: &str, at: i64| {
        format!("- id: {id}\n  actor: {actor}\n  run: {run}\n  role: writer\n  at_ms: {at}\n")
    };
    let authors = [
        author("d-MP001", "agent:a", "r1", 100),
        author("s-MP001", "agent:a", "r1", 100),
        author("D-MP001", "agent:b", "r2", 200),
        author("fg-MP001", "agent:a", "r1", 300),
    ]
    .concat();
    std::fs::write(out.join("authors.yaml"), authors).unwrap();
}

/// A fresh project with the unowned view `import`, and the export tree beside it.
struct Fixture {
    _dir: tempfile::TempDir,
    project: PathBuf,
    tree: PathBuf,
}

fn fixture(extra: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project).unwrap();
    let mut r = Rdy::init(&project, "rdy-standard").unwrap();
    let by = importer().provenance();
    let base = r.read().canon_head();
    r.store_mut()
        .view_create(VIEW, base, None, false, &by)
        .unwrap();
    r.invalidate();
    assert!(r.read().view_head(VIEW).unwrap().owner.is_none());
    r.close().unwrap();
    let out = dir.path().join("export");
    tree(&out, extra);
    Fixture {
        _dir: dir,
        project,
        tree: out,
    }
}

/// What the project holds durably: the acknowledged log boundary, every ref (views, canon,
/// features), the frame count, the figure's reachability and the view's owner.
#[derive(Debug, PartialEq)]
struct Durable {
    end: Location,
    refs: Vec<(String, Vec<u8>)>,
    frames: usize,
    blob: bool,
    owner: Option<String>,
}

fn durable_of(s: &Store) -> Durable {
    Durable {
        end: s.log_end(),
        refs: s.reader().refs_with_prefix("").unwrap(),
        frames: s.verify().unwrap(),
        blob: s.has_blob(&figure()).unwrap(),
        owner: s
            .reader()
            .view_head(VIEW)
            .unwrap()
            .and_then(|h| h.owner)
            .map(|o| o.actor),
    }
}

/// The project's durable state after reopen and reindex.
fn reopened(project: &Path) -> Durable {
    let mut s = Store::open(&project.join(".rdy").join("store")).unwrap();
    s.reindex().unwrap();
    durable_of(&s)
}

/// The project's durable state now, read through a fresh open (nothing is open on it).
fn before(f: &Fixture) -> Durable {
    let d = reopened(&f.project);
    assert!(!d.blob, "the fixture holds no figure");
    assert_eq!(d.owner, None, "the view is unowned");
    d
}

fn open(f: &Fixture) -> Rdy {
    Rdy::open(&f.project).unwrap()
}

/// The data frames of a successful import's batch (its TRANSITION left out), on a twin project.
fn import_frames(extra: &str) -> usize {
    let f = fixture(extra);
    let mut r = open(&f);
    r.import(&importer(), VIEW, &f.tree).unwrap();
    let n = r
        .store()
        .last_batch()
        .iter()
        .filter(|(k, _)| *k != kinds::TRANSITION)
        .count();
    r.close().unwrap();
    n
}

/// `r` refuses `fault`'s import, leaves no batch open, and closes; the project is as before.
fn refuses_unchanged(f: &Fixture, mut r: Rdy, what: &str, check: impl FnOnce(&RdyError)) {
    let was = before_live(&r);
    let e = r.import(&importer(), VIEW, &f.tree).unwrap_err();
    check(&e);
    assert!(r.store().open_batch().is_none(), "{what}: no batch to seal");
    let _ = r.close();
    assert_eq!(
        reopened(&f.project),
        was,
        "{what}: the project is as before"
    );
}

/// The live handle's durable state before the import.
fn before_live(r: &Rdy) -> Durable {
    assert!(r.store().open_batch().is_none());
    durable_of(r.store())
}

fn internal(at: &str) -> impl FnOnce(&RdyError) {
    let at = at.to_string();
    move |e: &RdyError| {
        assert_eq!(e.code, E_INTERNAL, "{e:?}");
        assert!(e.message.contains(&at), "{e:?}");
    }
}

/// The 5-character Crockford body of `n`.
fn body(mut n: usize) -> String {
    const C: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut out = [b'0'; 5];
    for d in out.iter_mut().rev() {
        *d = C[n % 32];
        n /= 32;
    }
    String::from_utf8(out.to_vec()).unwrap()
}

#[test]
fn m4bc3_bad_last_record_keeps_the_whole_project_unchanged() {
    // a last record that fails validation (a claim with no body): refused before any append
    let bad = row("c-MP001", "claim", Some("s-MP001"), "a3", &[]);
    let f = fixture(&bad);
    let was = before(&f);
    let r = open(&f);
    refuses_unchanged(&f, r, "invalid last record", |e| {
        assert_eq!(e.code, E_VALIDATION, "{e:?}")
    });
    assert_eq!(reopened(&f.project), was);

    // the last record of the import's write is torn after the blob was staged: the write
    // fails, and the whole batch (the blob too) is abandoned
    let n = import_frames("");
    let f = fixture("");
    let was = before(&f);
    let mut r = open(&f);
    r.store_mut()
        .fail_append_at_frame_for_tests(Some((n - 1, 10)));
    refuses_unchanged(&f, r, "torn last record", |_| {});
    assert_eq!(reopened(&f.project), was);
    assert!(!was.blob);
}

#[test]
fn m4bc3_failure_after_blob_and_author_staging_abandons_every_record() {
    for (fault, at) in [
        (ImportFault::StagedBlobs, "StagedBlobs"),
        (ImportFault::StagedAuthors, "StagedAuthors"),
    ] {
        let f = fixture("");
        let was = before(&f);
        let r = open(&f);
        fail_next_import_at_for_tests(fault);
        refuses_unchanged(&f, r, at, internal(at));
        assert_eq!(reopened(&f.project), was, "{at}");
    }

    // C24: over the created-receipt limit with one new blob: E_LIMIT before any append
    let mut big = String::new();
    for i in 0..=brief::MAX_CREATED_BRIEFING {
        big.push_str(&format!(
            "- id: c-{}\n  type: claim\n  parent: s-MP001\n  order: b{i:06}1\n  body: C.\n",
            body(i)
        ));
    }
    let f = fixture(&big);
    let was = before(&f);
    let r = open(&f);
    refuses_unchanged(&f, r, "receipt limit", |e| {
        assert_eq!(
            (e.code, e.message.as_str()),
            (E_LIMIT, "write briefing exceeds 100000 created blocks"),
            "{e:?}"
        )
    });
    assert_eq!(reopened(&f.project), was, "no log change, no blob");
}

#[test]
fn m4bc3_state_build_failure_never_seals_the_import() {
    let f = fixture("");
    let was = before(&f);
    let r = open(&f);
    fail_next_import_at_for_tests(ImportFault::StateBuild);
    refuses_unchanged(&f, r, "state build", internal("StateBuild"));
    assert_eq!(reopened(&f.project), was);
}

#[test]
fn m4bc3_outer_writer_cannot_seal_a_failed_import() {
    let run = |engine: &Local, f: &Fixture| {
        let (s, tree) = (importer(), f.tree.clone());
        engine.write(Box::new(move |r| {
            r.import(&s, VIEW, &tree)
                .map(|w| serde_json::to_value(w).unwrap())
        }))
    };
    for (fault, at) in [
        (ImportFault::StagedBlobs, "StagedBlobs"),
        (ImportFault::StagedAuthors, "StagedAuthors"),
        (ImportFault::StateBuild, "StateBuild"),
    ] {
        let f = fixture("");
        let was = before(&f);
        let engine = Local::new(open(&f));
        fail_next_import_at_for_tests(fault);
        let e = run(&engine, &f).unwrap_err();
        internal(at)(&e);
        let open_batch = engine
            .inspect(|r| Ok(r.store().open_batch().is_some()))
            .unwrap();
        assert!(!open_batch, "{at}: Local::write found no batch to seal");
        // a republish after the failure seals nothing either
        engine.republish().unwrap();
        let _ = engine.close();
        assert_eq!(reopened(&f.project), was, "{at}");
    }

    // a panic before the seal: the import abandons its batch and resumes the panic; the lock's
    // recovery (an invalidate) and the close find nothing to seal
    let f = fixture("");
    let was = before(&f);
    let engine = Local::new(open(&f));
    fail_next_import_at_for_tests(ImportFault::Panic);
    let panicked = catch_unwind(AssertUnwindSafe(|| run(&engine, &f)));
    assert!(panicked.is_err(), "the panic is resumed");
    let _ = engine.read();
    let open_batch = engine
        .inspect(|r| Ok(r.store().open_batch().is_some()))
        .unwrap();
    assert!(!open_batch, "the recovered lock found no batch");
    let _ = engine.close();
    assert_eq!(reopened(&f.project), was, "the panic sealed nothing");
}

#[test]
fn m4bc3_seal_index_or_sync_failure_with_confirmed_rollback_is_unchanged() {
    for fault in ["index", "sync"] {
        let f = fixture("");
        let was = before(&f);
        let mut r = open(&f);
        match fault {
            // the blob's write and the changes' write index first; the seal's index fails
            "index" => r.store_mut().fail_index_apply_after(2),
            _ => r.store_mut().fail_next_sync(),
        }
        let e = r.import(&importer(), VIEW, &f.tree).unwrap_err();
        assert_ne!(
            e.message, INDETERMINATE.1,
            "{fault}: a confirmed rollback is the seal's own error"
        );
        assert!(r.store().open_batch().is_none(), "{fault}");
        assert!(r.store().is_poisoned(), "{fault}: writes wait for reopen");
        let _ = r.close();
        assert_eq!(
            reopened(&f.project),
            was,
            "{fault}: the project is as before"
        );
    }
}

/// An import whose seal fails with `fault` and whose rollback fails: the indeterminate refusal,
/// a poisoned handle, and the complete marked batch after reopen and reindex.
fn indeterminate_seal(fault: &str) {
    let f = fixture("");
    let was = before(&f);
    let mut r = open(&f);
    match fault {
        "index" => r.store_mut().fail_index_apply_after(2),
        _ => r.store_mut().fail_next_sync(),
    }
    r.store_mut().fail_next_rollback();
    let e = r.import(&importer(), VIEW, &f.tree).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        INDETERMINATE,
        "{fault}: {e:?}"
    );
    assert!(r.store().is_poisoned(), "{fault}");
    let again = r.import(&importer(), VIEW, &f.tree).unwrap_err();
    assert_ne!(again.code, E_ALREADY_EXISTS, "{fault}: refused as poisoned");
    let _ = r.close();
    // the batch was marked: reopen and reindex keep it, and nothing removes it
    let after = reopened(&f.project);
    assert!(after.end > was.end, "{fault}: {after:?}");
    assert!(after.blob, "{fault}: the marked batch kept its blob");
    assert_eq!(after.owner.as_deref(), Some("human:imp"), "{fault}");
    let r = open(&f);
    let st = r.read();
    let scope = Scope::View(VIEW.into());
    assert!(
        st.block(&scope, &rdy_model::BlockId::parse("D-MP001").unwrap())
            .is_ok(),
        "{fault}: the view holds the import"
    );
}

#[test]
fn m4bc3_seal_index_failure_and_failed_rollback_is_indeterminate() {
    indeterminate_seal("index");
}

#[test]
fn m4bc3_seal_sync_failure_and_failed_rollback_is_indeterminate() {
    indeterminate_seal("sync");
}

#[test]
fn m4bc3_abandon_with_unproven_rollback_is_indeterminate() {
    let f = fixture("");
    let was = before(&f);
    let mut r = open(&f);
    fail_next_import_at_for_tests(ImportFault::StagedAuthors);
    r.store_mut().fail_next_rollback();
    let e = r.import(&importer(), VIEW, &f.tree).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        INDETERMINATE,
        "{e:?}"
    );
    assert!(
        r.store().open_batch().is_none(),
        "nothing later may seal it"
    );
    assert!(r.store().is_poisoned(), "the handle refuses more writes");
    let _ = r.close();
    // the batch had no marker: the reopen never takes it as acknowledged
    let after = reopened(&f.project);
    assert_eq!(after.refs, was.refs, "{after:?}");
    assert!(!after.blob, "{after:?}");
}

#[test]
fn m4bc3_success_keeps_author_times_importer_and_one_receipt() {
    let f = fixture("");
    let was = before(&f);
    let mut r = open(&f);
    let res = r.import(&importer(), VIEW, &f.tree).unwrap();
    assert!(r.store().open_batch().is_none(), "sealed");
    // one change per author, oldest first, each with its author's run and time
    assert_eq!(res.changes.len(), 2, "{res:?}");
    let st = r.read();
    let change = |h: &str| st.reader().change(&h.parse::<Hash>().unwrap()).unwrap();
    let (one, two) = (change(&res.changes[0]), change(&res.changes[1]));
    let who = |c: &rdy_model::Change| {
        (
            c.provenance.actor.clone(),
            c.provenance.run.clone(),
            c.provenance.at_ms,
        )
    };
    assert_eq!(who(&one), ("agent:b".into(), Some("r2".into()), 200));
    assert_eq!(who(&two), ("agent:a".into(), Some("r1".into()), 300));
    for c in [&one, &two] {
        assert_eq!(
            c.links.imported_by,
            Some(ActorRun {
                actor: "human:imp".into(),
                run: None
            })
        );
    }
    drop(st);
    // one receipt over every created block, registered once
    let briefing = res.briefing.clone().expect("a receipt");
    assert_eq!(briefing.covers, 4);
    assert_eq!(r.briefs().len(), 1);
    assert!(r.briefs().get(&briefing.token).is_some());
    // one batch, sealed once: one marker past the old end
    let mut markers = 0;
    r.store()
        .reader()
        .for_each_frame_from(was.end, r.store().log_end(), |_, k, _, _, _| {
            markers += usize::from(k == kinds::BATCH_COMMIT);
            Ok(())
        })
        .unwrap();
    assert_eq!(markers, 1, "the blob, changes and view ops seal together");
    r.close().unwrap();
    let after = reopened(&f.project);
    assert!(after.blob);
    assert_eq!(
        after.owner.as_deref(),
        Some("human:imp"),
        "the import claimed it"
    );
}

#[test]
fn m4bc3_postcommit_baseline_failure_is_success_with_coverage_error() {
    let f = fixture("");
    let engine = Local::new(open(&f));
    fail_next_import_at_for_tests(ImportFault::Baseline);
    let v = import_api::run_import(&engine, &importer(), VIEW, &f.tree).unwrap();
    assert_eq!(v["changes"].as_array().map(Vec::len), Some(2), "{v}");
    assert!(v.get("coverage").is_none(), "{v}");
    assert!(v.get("onboarding").is_none(), "{v}");
    assert_eq!(
        v["coverage_error"], "E_INTERNAL: internal error: injected import failure at Baseline",
        "{v}"
    );
    engine.close().unwrap();
    let after = reopened(&f.project);
    assert!(after.blob, "the import is committed");
    assert_eq!(after.owner.as_deref(), Some("human:imp"));
}
