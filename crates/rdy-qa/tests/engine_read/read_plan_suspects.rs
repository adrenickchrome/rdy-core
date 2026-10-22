//! cp13: the suspect rows a read shows. A deferral a person accepted stays on its suspect as
//! `deferred` (D-AAQ9Z, p-TFYXT), and a block whose reflected code drifted is a code row with a
//! null target (D-01QX5). Acceptance rows are written as typed records through
//! `write_guarded_for_tests`, then checked by the production fsck.
use rdy_engine::engine::Local;
use rdy_engine::{Rdy, Scope, SuspectReply};
use rdy_model::plan::{
    Plan, PlanAct, PlanAnswer, PlanKind, PlanOutcome, PlanSeal, SealedItem, item_hash,
};
use rdy_model::{Answer, AnswerAct, BlockId, Change, ChangeLinks, Hash, Op, Provenance, Role};
use rdy_qa::fixtures::*;
use rdy_store::kinds::{self, RefUpdate};
use rdy_store::plan::{AcceptedDeferral, plan_ref};
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

// ---------------------------------------------------------------------------------------------
// people and records

fn prov(actor: &str, role: Role, at_ms: i64) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: None,
        role,
        at_ms,
    }
}

/// The agent that defers.
fn agent_w() -> Provenance {
    prov("agent:w", Role::Writer, 10)
}

/// The person that accepts.
fn alice() -> Provenance {
    prov("human:alice", Role::Approver, 20)
}

fn enc<T: serde::Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).unwrap()
}

fn plan_frames(p: &Plan) -> Vec<(u8, Vec<u8>)> {
    vec![
        (kinds::PLAN, enc(p)),
        (
            kinds::REF_UPDATE,
            enc(&RefUpdate {
                name: plan_ref(&p.view),
                value: p.id().0.to_vec(),
                at: Location {
                    segment: 0,
                    offset: 0,
                },
            }),
        ),
    ]
}

fn answer(
    item: &str,
    act: AnswerAct,
    targets: Vec<Hash>,
    person: Provenance,
    note: &str,
) -> Answer {
    Answer {
        item: item.into(),
        act,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets,
        person,
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: (!note.is_empty()).then(|| note.to_string()),
    }
}

fn version(r: &Rdy, id: &BlockId) -> Hash {
    r.read()
        .materialized(&Scope::Canon)
        .unwrap()
        .get(id)
        .unwrap()
        .version_hash()
}

/// What a deferring landing wrote.
struct Sealed {
    landing: Hash,
}

/// Proposes `view`, answers the (`subject`, `cause`) item with a deferral by `by`, has `accepted`
/// accept it, and lands the view with the seal, through the store as the plan writers do. The
/// project then reads the landing.
fn land_deferring(
    r: &mut Rdy,
    view: &str,
    subject: &BlockId,
    cause: &BlockId,
    reason: &str,
    by: Provenance,
    accepted: Provenance,
) -> Sealed {
    let (subject_v, cause_v) = (version(r, subject), version(r, cause));
    // the engine keeps its store held, so each write joins the open batch until a seal
    let s = r.store_mut();
    let proposal = s.propose(view, "first", agent_w()).unwrap();
    let v1 = Plan {
        stable: None,
        prev: None,
        view: view.into(),
        proposal: Some(proposal),
        answers: vec![],
        heads: vec![],
        provenance: agent_w(),
        seal: None,
    };
    s.write_guarded_for_tests(plan_frames(&v1)).unwrap();
    s.seal(vec![]).unwrap();
    let bound = v1.id();
    let item = item_hash(cause, subject.as_str(), PlanKind::Confirm);
    let v2 = Plan {
        stable: Some(bound),
        prev: Some(bound),
        proposal: None,
        answers: vec![PlanAnswer {
            item,
            act: PlanAct::Defer,
            reason: Some(reason.into()),
            subject: Some(subject_v),
            cause: cause_v,
            by,
        }],
        ..v1.clone()
    };
    let answered = s.put_plan(&v2).unwrap();
    let accept = s
        .put_answer(&answer(
            "defer:P-abcd@1",
            AnswerAct::AcceptDeferrals,
            vec![],
            accepted,
            "",
        ))
        .unwrap();
    s.seal(vec![]).unwrap();
    let landing = s.land(&proposal, alice()).unwrap();
    let landed = s.landing(&landing).unwrap().proposal;
    let v3 = Plan {
        stable: Some(bound),
        prev: Some(answered),
        proposal: Some(landed),
        seal: Some(PlanSeal {
            landing,
            version: landed,
            decisions: vec![],
            items: vec![SealedItem {
                id: item,
                kind: PlanKind::Confirm,
                subject: subject.to_string(),
                cause: cause.clone(),
                edge: "references".into(),
                outcome: PlanOutcome::Deferred,
                answer: Some(accept),
            }],
        }),
        ..v2.clone()
    };
    s.write_guarded_for_tests(plan_frames(&v3)).unwrap();
    // the publish seals the landing and its seal in one batch and logs the landing's suspect
    // moves
    r.invalidate();
    Sealed { landing }
}

/// A person's KeepSuspect of `member` against `cause`; returns the answer's id.
fn keep_suspect(
    r: &mut Rdy,
    member: &BlockId,
    cause: &BlockId,
    person: Provenance,
    note: &str,
) -> Hash {
    let v = version(r, member);
    let item = format!("reaffirm:{cause}@0123456789ab");
    let id = r
        .store_mut()
        .put_answer(&answer(
            &item,
            AnswerAct::KeepSuspect,
            vec![v],
            person,
            note,
        ))
        .unwrap();
    r.invalidate();
    id
}

// ---------------------------------------------------------------------------------------------
// the project

/// Doc `spec`: claims A and X, and B, which references both. Landed in view `base`.
struct Ids {
    a: BlockId,
    x: BlockId,
    b: BlockId,
}

fn built(r: &mut Rdy, b_reflects: Option<&str>) -> Ids {
    let mut b = json!({"op": "create", "type": "claim", "id": "$b", "parent": "$s",
        "body": "B uses {{$a}} and {{$x}}."});
    if let Some(g) = b_reflects {
        b["fields"] = json!({"reflects": [g]});
    }
    let w = write(
        r,
        "base",
        json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d",
             "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds."},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."},
            b
        ]),
    )
    .unwrap();
    land(r, "base").unwrap();
    let id = |k: &str| BlockId::parse(&w.created[k]).unwrap();
    Ids {
        a: id("$a"),
        x: id("$x"),
        b: id("$b"),
    }
}

/// Changes A and X in view `view` (so B is suspect against both).
fn drift(r: &mut Rdy, view: &str, ids: &Ids, text: &str) {
    write(
        r,
        view,
        json!([
            {"op": "update", "id": ids.a.to_string(), "body": format!("A {text}.")},
            {"op": "update", "id": ids.x.to_string(), "body": format!("X {text}.")}
        ]),
    )
    .unwrap();
}

fn repin_all(r: &mut Rdy, view: &str, ids: &Ids) {
    write(
        r,
        view,
        json!([{"op": "repin", "id": ids.b.to_string(),
            "targets": [ids.a.to_string(), ids.x.to_string()]}]),
    )
    .unwrap();
}

fn canon(r: &Rdy) -> Vec<SuspectReply> {
    r.read().suspect_details(&Scope::Canon).unwrap()
}

fn row<'a>(rows: &'a [SuspectReply], block: &BlockId, target: &BlockId) -> &'a SuspectReply {
    rows.iter()
        .find(|s| &s.block == block && s.target.as_ref() == Some(target))
        .unwrap_or_else(|| panic!("no row {block} -> {target}: {rows:?}"))
}

fn fsck_clean(r: &Rdy) {
    let f = r.fsck().unwrap();
    assert_eq!(f.unlogged_moves, 0, "{f:?}");
}

// ---------------------------------------------------------------------------------------------
// the code checkout

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=qa",
            "-c",
            "user.email=qa",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", "change"]);
}

fn checkout(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    commit(d.path(), files);
    d
}

/// Links `checkout` as the code repo `app` of the project at `dir`.
fn link(dir: &Path, checkout: &Path) {
    let yaml = dir.join(".rdy.yaml");
    let mut text = std::fs::read_to_string(&yaml).unwrap();
    text.push_str("\ncode_repos: [app]\n");
    std::fs::write(&yaml, text).unwrap();
    std::fs::create_dir_all(dir.join(".rdy")).unwrap();
    std::fs::write(
        dir.join(".rdy/local.yaml"),
        format!("code_paths:\n  app: {}\n", checkout.display()),
    )
    .unwrap();
}

/// A project whose block B reflects `src/*.rs` of a linked checkout, with B suspect against A and
/// X in view `v1`, and the checkout changed after B recorded it.
fn with_code() -> (tempfile::TempDir, Rdy, tempfile::TempDir, Ids) {
    let app = checkout(&[("src/a.rs", "a\n"), ("src/b.rs", "b\n")]);
    let (dir, mut r) = project();
    link(dir.path(), app.path());
    let ids = built(&mut r, Some("src/*.rs"));
    commit(app.path(), &[("src/a.rs", "a2\n")]);
    (dir, r, app, ids)
}

// ---------------------------------------------------------------------------------------------
// the tests

#[test]
fn cp13_deferred_suspect_keeps_its_reason_and_acceptance() {
    let (dir, mut r) = project();
    let ids = built(&mut r, None);
    assert!(canon(&r).is_empty());
    drift(&mut r, "v1", &ids, "moved");
    // the agent defers, a person accepts: two people
    let sealed = land_deferring(
        &mut r,
        "v1",
        &ids.b,
        &ids.a,
        "the release owns it",
        agent_w(),
        alice(),
    );
    fsck_clean(&r);
    let rows = canon(&r);
    assert_eq!(rows.len(), 2, "{rows:?}");
    let a = row(&rows, &ids.b, &ids.a);
    assert_eq!(a.reason, "changed");
    assert_eq!(a.paths, None);
    let d = a.deferred.as_ref().expect("the seal's deferral");
    assert_eq!(d.by, agent_w());
    assert_eq!(d.reason, "the release owns it");
    assert_eq!(d.landing, Some(sealed.landing));
    assert_eq!(d.accepted_by, "human:alice");
    // the same member against its other cause is not deferred
    assert_eq!(row(&rows, &ids.b, &ids.x).deferred, None);

    // JSON: deferred and paths are absent when empty; the target is a string
    let v = serde_json::to_value(&rows).unwrap();
    let ax = v
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["target"] == ids.a.to_string())
        .unwrap();
    assert_eq!(ax["deferred"]["by"]["actor"], "agent:w");
    assert_eq!(ax["deferred"]["landing"], sealed.landing.to_string());
    assert_eq!(ax["deferred"]["accepted_by"], "human:alice");
    assert!(ax.get("paths").is_none(), "{ax}");
    let other = v
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["target"] == ids.x.to_string())
        .unwrap();
    assert!(other.get("deferred").is_none() && other.get("paths").is_none());

    // a later KeepSuspect of the same pair is the latest acceptance, whatever the clocks say:
    // its time is earlier than the seal's
    let early = prov("human:bob", Role::Approver, 1);
    keep_suspect(&mut r, &ids.b, &ids.a, early.clone(), "it still holds");
    fsck_clean(&r);
    let rows = canon(&r);
    let d = row(&rows, &ids.b, &ids.a).deferred.clone().unwrap();
    assert_eq!(d.by, early);
    assert_eq!(d.reason, "it still holds");
    assert_eq!(d.landing, None);
    assert_eq!(d.accepted_by, "human:bob");
    assert_eq!(row(&rows, &ids.b, &ids.x).deferred, None, "per cause");

    // the exact text, from the real binary
    r.close().unwrap();
    let (_cfg, env) = human_env("human:qa");
    let out = rdy_qa::daemon::human_command(dir.path(), &env, &["--no-daemon", "suspects"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = format!(
        "{} -> {}  changed\n  deferred by human:bob: it still holds",
        ids.b, ids.a
    );
    assert!(text.contains(&line), "{text}");
    let plain = format!("{} -> {}  changed\n{} -> ", ids.b, ids.a, ids.b);
    assert!(
        !text.contains(&plain),
        "the line before the other cause: {text}"
    );
}

#[test]
fn cp13_deferral_is_per_cause_and_expires_after_clear() {
    let (_dir, mut r) = project();
    let ids = built(&mut r, None);
    drift(&mut r, "v1", &ids, "moved");
    land_deferring(&mut r, "v1", &ids.b, &ids.a, "later", agent_w(), alice());
    let rows = canon(&r);
    assert!(row(&rows, &ids.b, &ids.a).deferred.is_some());
    assert!(row(&rows, &ids.b, &ids.x).deferred.is_none());

    // a KeepSuspect names the member and the cause it recorded: another member of the cause, and
    // another cause of the member, show none
    let end = r.store().log_end();
    assert!(matches!(
        r.store()
            .reader()
            .accepted_deferral(&ids.b, &ids.a, end)
            .unwrap(),
        Some(AcceptedDeferral::Plan { .. })
    ));
    assert_eq!(
        r.store()
            .reader()
            .accepted_deferral(&ids.b, &ids.x, end)
            .unwrap(),
        None
    );

    // clear: B is re-pinned in canon, so the pair is gone
    repin_all(&mut r, "v2", &ids);
    land(&mut r, "v2").unwrap();
    assert!(canon(&r).is_empty());
    let end = r.store().log_end();
    assert_eq!(
        r.store()
            .reader()
            .accepted_deferral(&ids.b, &ids.a, end)
            .unwrap(),
        None,
        "a clear expires the acceptance"
    );

    // reopen: the same pair is suspect again and inherits nothing
    drift(&mut r, "v3", &ids, "moved again");
    land(&mut r, "v3").unwrap();
    let rows = canon(&r);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows.iter().all(|s| s.deferred.is_none()), "{rows:?}");
    // not even after a reindex
    r.reindex().unwrap();
    assert!(canon(&r).iter().all(|s| s.deferred.is_none()));
    fsck_clean(&r);
}

#[test]
fn cp13_reason_only_replacement_keeps_the_deferral() {
    let (_dir, mut r) = project();
    let ids = built(&mut r, None);
    drift(&mut r, "v1", &ids, "moved");
    land_deferring(&mut r, "v1", &ids.b, &ids.a, "later", agent_w(), alice());
    assert_eq!(row(&canon(&r), &ids.b, &ids.a).reason, "changed");
    // one batch: the pair (B, A) goes from `changed` to `deleted`, a replacement on one pair.
    // The engine refuses a delete that leaves B's reference dangling, so the change is written
    // to the store as a store that holds no such rule would take it.
    let base = version(&r, &ids.a);
    r.store_mut()
        .commit_to_view(
            "v2",
            Change {
                view: "v2".into(),
                ops: vec![Op::Delete {
                    base,
                    id: ids.a.clone(),
                }],
                deps: vec![],
                provenance: writer().provenance(),
                justification: None,
                message: "delete A".into(),
                links: ChangeLinks::default(),
                read_identity: None,
            },
        )
        .unwrap();
    r.invalidate();
    land_with_a_tie(&mut r, "v2");
    let rows = canon(&r);
    let a = row(&rows, &ids.b, &ids.a);
    assert_eq!(a.reason, "deleted");
    let d = a
        .deferred
        .as_ref()
        .expect("a reason-only replacement keeps the deferral");
    assert_eq!(d.reason, "later");
    assert_eq!(d.accepted_by, "human:alice");
    fsck_clean(&r);
}

#[test]
fn cp13_captured_reader_does_not_see_later_acceptance() {
    let (_dir, mut r) = project();
    let ids = built(&mut r, None);
    drift(&mut r, "v1", &ids, "moved");
    // the state before any acceptance, then the acceptance, then its replacement
    land_with_a_tie(&mut r, "v1");
    let before = r.read();
    assert_eq!(
        row(
            &before.suspect_details(&Scope::Canon).unwrap(),
            &ids.b,
            &ids.a
        )
        .deferred,
        None
    );
    keep_suspect(&mut r, &ids.b, &ids.a, alice(), "first");
    let first = r.read();
    keep_suspect(
        &mut r,
        &ids.b,
        &ids.a,
        prov("human:carol", Role::Approver, 30),
        "second",
    );
    let second = r.read();
    let shows = |st: &rdy_engine::ReadState| -> Option<String> {
        row(&st.suspect_details(&Scope::Canon).unwrap(), &ids.b, &ids.a)
            .deferred
            .as_ref()
            .map(|d| d.reason.clone())
    };
    let check = |when: &str| {
        assert_eq!(shows(&before), None, "{when}: before the acceptance");
        assert_eq!(shows(&first).as_deref(), Some("first"), "{when}: first");
        assert_eq!(shows(&second).as_deref(), Some("second"), "{when}: second");
    };
    check("live");
    // the explicit ends agree: the table row is newer than `before`'s end, so it is folded
    let end = before.seq();
    assert_eq!(
        r.store()
            .reader()
            .accepted_deferral(&ids.b, &ids.a, end)
            .unwrap(),
        None
    );
    // a reindex rebuilds the table; the captured states still read their own ends
    r.reindex().unwrap();
    check("after reindex");
    assert_eq!(shows(&r.read()).as_deref(), Some("second"));
}

#[test]
fn cp13_code_suspect_has_null_target_and_sorted_paths() {
    let (_dir, mut r, app, ids) = with_code();
    // B is suspect against A and X in view v1, and its reflected code moved
    commit(app.path(), &[("src/c.rs", "c\n")]);
    drift(&mut r, "v1", &ids, "moved");
    let scope = Scope::View("v1".into());
    let rows = r.read().suspect_details(&scope).unwrap();
    // A and X rows first, then B's code row: the null target sorts last
    let b: Vec<&SuspectReply> = rows.iter().filter(|s| s.block == ids.b).collect();
    assert_eq!(b.len(), 3, "{rows:?}");
    let mut targets = [ids.a.clone(), ids.x.clone()];
    targets.sort();
    assert_eq!(b[0].target.as_ref(), Some(&targets[0]));
    assert_eq!(b[1].target.as_ref(), Some(&targets[1]));
    let code = b[2];
    assert_eq!(code.target, None);
    assert_eq!(code.reason, "code_changed");
    assert_eq!(code.deferred, None);
    assert_eq!(
        code.paths.as_deref(),
        Some(&["app:src/a.rs".to_string(), "app:src/c.rs".to_string()][..])
    );
    // JSON: null target, paths present, no deferred
    let v = serde_json::to_value(code).unwrap();
    assert_eq!(v["target"], Value::Null);
    assert!(v.get("target").is_some());
    assert_eq!(v["paths"], json!(["app:src/a.rs", "app:src/c.rs"]));
    assert!(v.get("deferred").is_none());
    // the same rows in context
    let ctx = r.read().context(&scope, &ids.b).unwrap();
    assert_eq!(ctx.suspects, b.into_iter().cloned().collect::<Vec<_>>());
    // canon has no structural row but still the code row
    let canon_rows = canon(&r);
    assert_eq!(canon_rows.len(), 1, "{canon_rows:?}");
    assert_eq!(canon_rows[0].target, None);
}

#[test]
fn cp13_all_public_reads_share_the_same_suspect_fields() {
    use rdy_mcp::tools::{McpState, rdy_brief, rdy_context, rdy_read, rdy_session, rdy_suspects};
    use serde_json::from_value;
    let (dir, mut r, _app, ids) = with_code();
    drift(&mut r, "v1", &ids, "moved");
    land_deferring(&mut r, "v1", &ids.b, &ids.a, "later", agent_w(), alice());
    let want = serde_json::to_value(canon(&r)).unwrap();
    assert_eq!(want.as_array().unwrap().len(), 3, "{want}");
    assert!(
        want.as_array()
            .unwrap()
            .iter()
            .any(|s| s["target"].is_null())
    );
    assert!(
        want.as_array()
            .unwrap()
            .iter()
            .any(|s| s["deferred"].is_object())
    );
    let b_rows: Vec<&Value> = want
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["block"] == ids.b.to_string())
        .collect();
    let mut st = McpState::new(std::sync::Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        from_value(json!({"actor": "agent:qa", "role": "writer", "view": "work"})).unwrap(),
    )
    .unwrap();
    // rdy_suspects
    let v = rdy_suspects(&mut st, from_value(json!({"scope": "canon"})).unwrap()).unwrap();
    assert_eq!(v["items"], want);
    // rdy_context
    let v = rdy_context(
        &mut st,
        from_value(json!({"id": ids.b.to_string(), "scope": "canon"})).unwrap(),
    )
    .unwrap();
    assert_eq!(v["suspects"], json!(b_rows));
    // rdy_brief
    let v = rdy_brief(
        &mut st,
        from_value(json!({"ids": [ids.b.to_string()], "scope": "canon"})).unwrap(),
    )
    .unwrap();
    assert_eq!(v["suspects"], json!(b_rows));
    // the read list marks the block
    let v = rdy_read(
        &mut st,
        from_value(json!({"id": ids.b.to_string(), "scope": "canon"})).unwrap(),
    )
    .unwrap();
    assert!(v["text"].as_str().unwrap().contains("[suspect]"), "{v}");
    drop(st);

    // the CLI: the status count and the texts
    let (_cfg, env) = human_env("human:qa");
    let run = |args: &[&str]| -> String {
        let mut all = vec!["--no-daemon"];
        all.extend_from_slice(args);
        let out = rdy_qa::daemon::human_command(dir.path(), &env, &all)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let status: Value = serde_json::from_str(&run(&["--json", "status"])).unwrap();
    assert_eq!(status["suspects"], 3, "{status}");
    assert!(run(&["status"]).contains("3 suspects"));
    let listed: Value = serde_json::from_str(&run(&["--json", "suspects"])).unwrap();
    assert_eq!(listed["items"], want);
    let text = run(&["suspects"]);
    let b = ids.b.to_string();
    assert!(
        text.contains(&format!(
            "{b} -> {}  changed\n  deferred by agent:w: later",
            ids.a
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("{b}: code_changed\n  app:src/a.rs")),
        "{text}"
    );
    let shown = run(&["show", &b]);
    assert!(shown.contains("\nsuspects:\n  "), "{shown}");
    assert!(
        shown.contains("    deferred by agent:w: later")
            || shown.contains("  deferred by agent:w: later"),
        "{shown}"
    );
    assert!(shown.contains(&format!("{b}: code_changed")), "{shown}");
}

#[test]
fn cp13_historical_reads_do_not_consult_current_code() {
    let (_dir, mut r, app, ids) = with_code();
    drift(&mut r, "v1", &ids, "moved");
    let sealed = land_deferring(&mut r, "v1", &ids.b, &ids.a, "later", agent_w(), alice());
    let at = Scope::At(sealed.landing);
    let historical = |r: &Rdy| r.read().suspect_details(&at).unwrap();
    let before = historical(&r);
    assert!(
        before
            .iter()
            .all(|s| s.target.is_some() && s.paths.is_none()),
        "{before:?}"
    );
    let live_before = canon(&r);
    assert!(live_before.iter().any(|s| s.target.is_none()));
    // moving the checkout's HEAD changes the live rows and no historical one
    commit(app.path(), &[("src/b.rs", "b2\n"), ("src/d.rs", "d\n")]);
    let live_after = canon(&r);
    assert_ne!(live_before, live_after, "the live code row moved");
    assert_eq!(historical(&r), before);
    let ctx = r.read().context(&at, &ids.b).unwrap();
    assert!(
        ctx.suspects.iter().all(|s| s.target.is_some()),
        "{:?}",
        ctx.suspects
    );
}

/// The log end, the bytes of each segment file and the refs.
type Snapshot = (Location, Vec<(String, Vec<u8>)>, Vec<(String, Vec<u8>)>);

#[test]
fn cp13_reading_suspects_changes_no_log_or_ref() {
    let (dir, mut r, _app, ids) = with_code();
    drift(&mut r, "v1", &ids, "moved");
    land_deferring(&mut r, "v1", &ids.b, &ids.a, "later", agent_w(), alice());
    let snapshot = |r: &Rdy| -> Snapshot {
        let mut files = vec![];
        let segments = dir.path().join(".rdy/store/segments");
        for e in std::fs::read_dir(segments).unwrap() {
            let p = e.unwrap().path();
            files.push((
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&p).unwrap(),
            ));
        }
        files.sort();
        (
            r.store().log_end(),
            files,
            r.store().refs_with_prefix("").unwrap(),
        )
    };
    let before = snapshot(&r);
    let st = r.read();
    for scope in [Scope::Canon, Scope::View("v1".into()), Scope::Inflight] {
        st.suspect_details(&scope).unwrap();
        st.context(&scope, &ids.b).unwrap();
    }
    let cov = rdy_engine::brief::Coverage {
        ids: vec![ids.b.to_string()],
        path: None,
    };
    st.brief(&Scope::Canon, &cov, None).unwrap();
    assert_eq!(
        snapshot(&r),
        before,
        "the log, its bytes and the refs are unchanged"
    );
}
