//! cp10 (D-4JMPH, p-RW856, O5, O6, O16, O19): every explicit propose writes one plan version bound
//! to the version it writes, in the version's own batch: the chain's first HEADs stay the
//! baseline, answers are cumulative, the candidate is the version's (a subset's own changes),
//! a refused plan answer writes nothing (no key change either), and open non-code items warn
//! without refusing.
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::plan::{PLAN_OPEN_FIX, PlanAnswerInput, ProposePlanRequest, W_PLAN_OPEN};
use rdy_engine::{E_PLAN_ITEM, ProposalInfo, Rdy, RdyError, Session};
use rdy_model::plan::{Plan, PlanAct, PlanHead, PlanKind, item_display, item_hash};
use rdy_model::{BlockId, Hash, Role};
use rdy_qa::fixtures::{approver, project, write, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY};
use rdy_store::kinds;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

// ---- the project ----

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Canon: doc `spec`, section `Rules`, claims a ("A holds 30 days."), c ("C stands."), b, which
/// references a and c, and p, which states a's literal. Returns (section, a, b, c, p).
fn spec(r: &mut Rdy) -> (String, String, String, String, String) {
    let w = write(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}} and {{$c}}."},
            {"op": "create", "type": "claim", "id": "$p", "parent": "$s", "body": "Wait 30 days per {{$a}}."}
        ]),
    )
    .unwrap();
    rdy_qa::fixtures::land(r, "seed").unwrap();
    let get = |k: &str| w.created[k].clone();
    (get("$s"), get("$a"), get("$b"), get("$c"), get("$p"))
}

fn edit(r: &mut Rdy, view: &str, target: &str, body: &str) -> String {
    write(
        r,
        view,
        json!([{"op": "update", "id": target, "body": body}]),
    )
    .unwrap_or_else(|e| panic!("edit {target} in {view}: {e}"))
    .change
}

fn propose(r: &mut Rdy, view: &str, answers: &[PlanAnswerInput]) -> Result<ProposalInfo, RdyError> {
    r.propose_plan(
        &writer(),
        ProposePlanRequest {
            plan_answers: answers,
            summary: Some(TEST_SUMMARY),
            ..ProposePlanRequest::new(view, view)
        },
    )
}

fn pi(cause: &str, subject: &str, kind: PlanKind) -> String {
    item_display(&item_hash(&id(cause), subject, kind))
}

fn ans(item: &str, act: PlanAct, reason: Option<&str>) -> PlanAnswerInput {
    PlanAnswerInput {
        item: item.into(),
        act,
        reason: reason.map(str::to_string),
    }
}

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

/// The plan bound to proposal version `version`, as the log holds it now.
fn bound(r: &Rdy, version: &str) -> Option<(Hash, Plan)> {
    let st = r.read();
    let reader = st.reader();
    let b = reader
        .plan_of_proposal(&h(version), r.store().log_end())
        .unwrap()?;
    Some((b, reader.plan(&b).unwrap()))
}

fn plan_ref(r: &Rdy, view: &str) -> Option<Hash> {
    r.read()
        .reader()
        .get_ref(&rdy_store::plan::plan_ref(view))
        .unwrap()
        .map(|v| Hash(v.as_slice().try_into().unwrap()))
}

/// Every ref and the log end: what a refused propose must leave as it was.
fn log_state(r: &Rdy) -> (Location, Vec<(String, Vec<u8>)>) {
    (
        r.store().log_end(),
        r.read().reader().refs_with_prefix("").unwrap(),
    )
}

/// The logical kinds of the frames from `from` to the log end, markers included.
fn frames_from(r: &Rdy, from: Location) -> Vec<u8> {
    let mut out = vec![];
    r.store()
        .reader()
        .for_each_frame_from(from, r.store().log_end(), |_, kind, _, _, _| {
            out.push(rdy_store::codec::logical_kind(kind));
            Ok(())
        })
        .unwrap();
    out
}

fn warning_codes(p: &ProposalInfo) -> Vec<&str> {
    p.warnings.iter().map(|w| w.code).collect()
}

// ---- git ----

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

fn commit(dir: &Path, path: &str, message: &str) -> String {
    std::fs::write(dir.join(path), format!("{message}\n")).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A fresh checkout linked to the project at `project` as code repo `app`.
fn linked_repo(project: &Path) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "--initial-branch=main"]);
    let cfg = project.join(".rdy.yaml");
    let mut text = std::fs::read_to_string(&cfg).unwrap_or_default();
    text.push_str("code_repos: [app]\n");
    std::fs::write(&cfg, text).unwrap();
    std::fs::write(
        project.join(".rdy/local.yaml"),
        format!(
            "code_paths:\n  app: {}\n",
            serde_json::to_string(&tmp.path().display().to_string()).unwrap()
        ),
    )
    .unwrap();
    tmp
}

fn head(repo: &Path) -> Vec<PlanHead> {
    vec![PlanHead {
        repo: "app".into(),
        head: Some(git(repo, &["rev-parse", "HEAD"])),
    }]
}

// ---- tests ----

/// O19, p-AEYTC: a still-open version proposed before store 17 binds no plan; the view's next
/// propose writes a new version whose plan starts a chain with no inherited answer, and the
/// plan warnings apply to it; the old version stays unbound. An answer-only chain the view
/// recorded after the old version is not continued: the new plan names no earlier version,
/// takes none of its answers, and its own HEADs are its code baseline, not the chain's first.
#[test]
fn cp10_next_propose_binds_first_plan_version() {
    let (d, mut r) = project();
    let repo = linked_repo(d.path());
    let (_s, a, b, _c, p) = spec(&mut r);
    commit(repo.path(), "f.rs", &format!("start\n\nImplements: {a}"));
    edit(&mut r, "w", &a, "A holds 60 days.");
    // the version a store-16 engine wrote: the raw proposal writer, no plan
    let by = writer().provenance();
    let changes = r.store().included("w").unwrap().as_ref().clone();
    let old = r
        .store_mut()
        .propose_changes("w", "w", None, by, changes, None)
        .unwrap();
    r.invalidate();
    assert!(bound(&r, &old.to_string()).is_none());
    assert_eq!(plan_ref(&r, "w"), None);
    // an answer-only chain after it, with its first HEADs; then a commit touches f.rs
    let confirm = pi(&a, &b, PlanKind::Confirm);
    let chain = r
        .answer_plan(
            &writer(),
            "w",
            &[ans(&confirm, PlanAct::Defer, Some("later"))],
        )
        .unwrap();
    assert_eq!(plan_ref(&r, "w"), Some(chain));
    commit(repo.path(), "f.rs", &format!("follow\n\nImplements: {a}"));

    let next = propose(&mut r, "w", &[]).unwrap();
    assert_eq!(next.version, 2, "a new version of the open proposal");
    let (bh, plan) = bound(&r, &next.id).expect("the next version binds a plan");
    assert_eq!(
        (plan.stable, plan.prev),
        (None, None),
        "a first chain version, not the answer-only chain's next"
    );
    assert_eq!(plan.proposal, Some(h(&next.id)));
    assert!(plan.answers.is_empty(), "no inherited answer");
    assert_eq!(plan.heads, head(repo.path()), "this propose's HEADs");
    assert_eq!(plan.view, "w");
    assert_eq!(plan_ref(&r, "w"), Some(bh));
    assert!(
        bound(&r, &old.to_string()).is_none(),
        "the old version stays unbound"
    );
    // the normal plan warnings: b's confirm and p's update are open (the chain's defer is not
    // inherited); the code item counts and does not warn
    let open = next
        .warnings
        .iter()
        .find(|w| w.code == W_PLAN_OPEN)
        .expect("W_PLAN_OPEN");
    assert_eq!(open.message, "2 change-plan items are open");
    assert_eq!(open.fix, PLAN_OPEN_FIX);
    assert_eq!(
        (next.plan.open, next.plan.closed, next.plan.deferred),
        (3, 0, 0)
    );
    // the baseline is this propose's HEAD: the later commit is not after it
    assert_eq!(next.plan.by_kind["update_implementation"].open, 1);
    let listed: Vec<&str> = next.plan.items.iter().map(|i| i.id.as_str()).collect();
    let mut full = [
        item_hash(&id(&a), &b, PlanKind::Confirm),
        item_hash(&id(&a), &p, PlanKind::Update),
        item_hash(&id(&a), "app:f.rs", PlanKind::UpdateImplementation),
    ];
    full.sort();
    let want: Vec<String> = full.iter().map(item_display).collect();
    assert_eq!(listed, want, "the open items, in full-hash order");
    r.fsck().unwrap();
}

/// D-4JMPH: each explicit propose writes its own bound plan in its batch, with one marker: a
/// first version, a next version after answers (cumulative, keeping their authors and the
/// chain's first HEADs), an empty-answer and a zero-item version, and a relayed version whose
/// plan is the person's beside the relay's ANSWER.
#[test]
fn cp10_every_proposal_version_binds_one_plan() {
    let (d, mut r) = project();
    let repo = linked_repo(d.path());
    let (s, a, b, c, _p) = spec(&mut r);
    commit(repo.path(), "f.rs", &format!("start\n\nImplements: {a}"));
    let first_heads = head(repo.path());
    edit(&mut r, "w", &a, "A holds 60 days.");
    let start = r.store().log_end();
    let p1 = propose(&mut r, "w", &[]).unwrap();
    let kinds_of = frames_from(&r, start);
    assert_eq!(
        kinds_of
            .iter()
            .filter(|k| **k == kinds::BATCH_COMMIT)
            .count(),
        1,
        "one batch: {kinds_of:?}"
    );
    for k in [kinds::PROPOSAL, kinds::PLAN] {
        assert_eq!(
            kinds_of.iter().filter(|x| **x == k).count(),
            1,
            "{kinds_of:?}"
        );
    }
    let (b1, plan1) = bound(&r, &p1.id).unwrap();
    assert_eq!((plan1.stable, plan1.prev), (None, None));
    assert_eq!(plan1.heads, first_heads);
    assert!(plan1.answers.is_empty());
    assert_eq!(plan1.provenance.actor, "human:qa");
    // the code item counts, and does not warn
    assert_eq!(p1.plan.code.files, 1);
    assert_eq!(p1.plan.by_kind["update_implementation"].open, 1);

    // an answer, then a later commit and a next version: cumulative, the first HEADs kept
    let confirm = pi(&a, &b, PlanKind::Confirm);
    let a1 = r
        .answer_plan(
            &writer(),
            "w",
            &[ans(&confirm, PlanAct::Confirm, Some("B still holds"))],
        )
        .unwrap();
    commit(repo.path(), "g.rs", "unrelated");
    edit(&mut r, "w", &c, "C stands firm.");
    let p2 = propose(&mut r, "w", &[]).unwrap();
    assert_eq!(p2.version, 2);
    let (b2, plan2) = bound(&r, &p2.id).unwrap();
    assert_ne!(b2, b1);
    assert_eq!((plan2.stable, plan2.prev), (Some(b1), Some(a1)));
    assert_eq!(
        plan2.heads,
        head(repo.path()),
        "this propose's captured HEADs"
    );
    let held = plan2
        .answers
        .iter()
        .find(|x| x.item == item_hash(&id(&a), &b, PlanKind::Confirm))
        .expect("the earlier answer is kept");
    assert_eq!(held.by.actor, "human:qa");
    assert_eq!(held.reason.as_deref(), Some("B still holds"));

    // a zero-item version binds its plan too
    write(
        &mut r,
        "z",
        json!([{"op": "create", "type": "claim", "parent": s, "body": "Z stands alone."}]),
    )
    .unwrap();
    let pz = propose(&mut r, "z", &[]).unwrap();
    let (_, planz) = bound(&r, &pz.id).unwrap();
    assert_eq!((planz.stable, planz.prev), (None, None));
    assert_eq!((pz.plan.open, pz.plan.closed, pz.plan.deferred), (0, 0, 0));
    assert!(!warning_codes(&pz).contains(&W_PLAN_OPEN));

    // a relayed propose: the person's version, plan and answer in one batch
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": c, "body": "C stands, relayed."}]),
    )
    .unwrap();
    let relay = Session::agent("agent:relay".into(), Some("r1".into()), Role::Writer).unwrap();
    let words = RelayInput {
        relayed_by: None,
        evidence: Some("Yes, propose it.".into()),
        question: Some("Shall I propose it?".into()),
        session_ref: Some("chat-7".into()),
        kind: None,
    };
    let start = r.store().log_end();
    let pr = r
        .propose_plan_as(
            &relay,
            PERSON,
            &words,
            true,
            ProposePlanRequest::new("work", "Relayed"),
        )
        .unwrap();
    let kinds_of = frames_from(&r, start);
    assert_eq!(
        kinds_of
            .iter()
            .filter(|k| **k == kinds::BATCH_COMMIT)
            .count(),
        1,
        "{kinds_of:?}"
    );
    for k in [kinds::PROPOSAL, kinds::ANSWER, kinds::PLAN] {
        assert_eq!(
            kinds_of.iter().filter(|x| **x == k).count(),
            1,
            "{kinds_of:?}"
        );
    }
    assert!(pr.answer.is_some());
    let (_, planr) = bound(&r, &pr.id).unwrap();
    assert_eq!(planr.provenance.actor, PERSON, "the person proposes");
    assert_eq!(planr.proposal, Some(h(&pr.id)));
    r.fsck().unwrap();
}

/// A refused plan answer writes nothing: no proposal, no plan and no key change rdy would add
/// (K1), every ref and the log end as they were.
#[test]
fn cp10_invalid_plan_answer_does_not_commit_rekeys() {
    let (_d, mut r) = project();
    let _a = r
        .new_doc(
            &writer(),
            "a",
            "engineering-handbook/onboarding/overview",
            None,
            Some("O"),
        )
        .unwrap();
    r.new_doc(
        &writer(),
        "b",
        "product-handbook/onboarding/overview",
        None,
        Some("O"),
    )
    .unwrap();
    let pa = r.propose(&writer(), "a", "a", Some(TEST_SUMMARY)).unwrap();
    r.land(&approver(), &pa.id).unwrap();
    let before = log_state(&r);
    let in_view = r.store().included("b").unwrap().len();
    let e = propose(
        &mut r,
        "b",
        &[ans("pi-0123456789abcdef", PlanAct::Defer, Some("later"))],
    )
    .unwrap_err();
    assert_eq!(e.code, E_PLAN_ITEM, "{e}");
    assert_eq!(
        e.message,
        "pi-0123456789abcdef is not an item of the change plan for view:b"
    );
    assert_eq!(
        log_state(&r),
        before,
        "nothing written, the key change neither"
    );
    assert_eq!(r.store().included("b").unwrap().len(), in_view);
    assert_eq!(plan_ref(&r, "b"), None);
    // the same propose with no answer writes the key change, the version and its plan at once
    let start = r.store().log_end();
    let pb = propose(&mut r, "b", &[]).unwrap();
    assert_eq!(pb.keys.len(), 1, "{:?}", pb.keys);
    assert_eq!(r.store().included("b").unwrap().len(), in_view + 1);
    let kinds_of = frames_from(&r, start);
    assert_eq!(
        kinds_of
            .iter()
            .filter(|k| **k == kinds::BATCH_COMMIT)
            .count(),
        1,
        "the key change is in the version's batch: {kinds_of:?}"
    );
    for k in [kinds::CHANGE, kinds::PROPOSAL, kinds::PLAN] {
        assert_eq!(
            kinds_of.iter().filter(|x| **x == k).count(),
            1,
            "{kinds_of:?}"
        );
    }
    let (_, plan) = bound(&r, &pb.id).unwrap();
    assert_eq!(plan.proposal, Some(h(&pb.id)));
    let stored = r.read().reader().proposal(&h(&pb.id)).unwrap();
    assert_eq!(
        stored.changes.len(),
        in_view + 1,
        "the key change is proposed"
    );
    r.fsck().unwrap();
}

/// A subset proposal's candidate is its own changes: an item that only an excluded change
/// would close stays open, and an answer to an item only the excluded work makes is refused.
#[test]
fn cp10_subset_answers_use_the_subset_candidate() {
    let (_d, mut r) = project();
    let (_s, a, b, c, p) = spec(&mut r);
    let edit_a = edit(&mut r, "w", &a, "A holds 60 days.");
    // the excluded change updates p, which closes p's update item in the whole view, and
    // changes c, which makes b's confirm on c an item of the whole view only
    write(
        &mut r,
        "w",
        json!([
            {"op": "update", "id": p, "body": format!("Wait 60 days per {{{{{a}}}}}.")},
            {"op": "update", "id": c, "body": "C stands firm."}
        ]),
    )
    .unwrap();
    let update = pi(&a, &p, PlanKind::Update);
    let confirm = pi(&a, &b, PlanKind::Confirm);
    let only_whole = pi(&c, &b, PlanKind::Confirm);
    let row = |v: &Value, item: &str| {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == item)
            .cloned()
            .unwrap_or_else(|| panic!("no item {item}: {v:#}"))
    };
    let read = |r: &Rdy, q: rdy_engine::plan::PlanQuery| {
        let q = rdy_engine::plan::PlanQuery {
            max_tokens: Some(100_000),
            ..q
        };
        serde_json::to_value(r.read().change_plan(&q, "work").unwrap()).unwrap()
    };
    let whole = read(
        &r,
        rdy_engine::plan::PlanQuery {
            view: Some("w".into()),
            ..Default::default()
        },
    );
    assert_eq!(
        row(&whole, &update)["state"],
        "closed",
        "the whole view closes it"
    );
    assert_eq!(row(&whole, &only_whole)["state"], "open");
    let subset = [edit_a.clone()];
    // an item of the whole view only is no item of the subset: refused, nothing written
    let before = log_state(&r);
    let refused = [ans(&only_whole, PlanAct::Confirm, Some("B still holds"))];
    let e = r
        .propose_plan(
            &writer(),
            ProposePlanRequest {
                changes: Some(&subset),
                plan_answers: &refused,
                ..ProposePlanRequest::new("w", "only a")
            },
        )
        .unwrap_err();
    assert_eq!(e.code, E_PLAN_ITEM, "{e}");
    assert_eq!(log_state(&r), before);
    // p's update is open on the subset, so a defer answers it
    let answers = [
        ans(&update, PlanAct::Defer, Some("p follows later")),
        ans(&confirm, PlanAct::Confirm, Some("B still holds")),
    ];
    let info = r
        .propose_plan(
            &writer(),
            ProposePlanRequest {
                changes: Some(&subset),
                plan_answers: &answers,
                ..ProposePlanRequest::new("w", "only a")
            },
        )
        .unwrap();
    assert_eq!(info.changes, 1);
    assert_eq!(
        (info.plan.open, info.plan.closed, info.plan.deferred),
        (0, 1, 1),
        "the excluded update closes nothing"
    );
    let (_, plan) = bound(&r, &info.id).unwrap();
    let deferred = plan
        .answers
        .iter()
        .find(|x| x.item == item_hash(&id(&a), &p, PlanKind::Update))
        .expect("the defer is recorded");
    assert_eq!(deferred.act, PlanAct::Defer);
    // the version read agrees: p's update is deferred, not closed
    let version = read(
        &r,
        rdy_engine::plan::PlanQuery {
            proposal: Some(info.r#ref.clone()),
            ..Default::default()
        },
    );
    assert_eq!(row(&version, &update)["state"], "deferred");
    assert!(
        version["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["id"] != only_whole),
        "{version:#}"
    );
    r.fsck().unwrap();
}

/// p-S3NTW: open non-code items warn with W_PLAN_OPEN (code items count but never warn), lapsed
/// answers warn too, and none of them refuses the propose or the landing before M3.
#[test]
fn cp10_open_noncode_items_warn_and_code_items_never_block() {
    let (d, mut r) = project();
    let repo = linked_repo(d.path());
    let (_s, a, b, c, p) = spec(&mut r);
    commit(repo.path(), "f.rs", &format!("start\n\nImplements: {c}"));
    // a code-only change: c's text changes; b references c (a confirm), p does not
    edit(&mut r, "k", &c, "C stands firm.");
    let confirm_cb = pi(&c, &b, PlanKind::Confirm);
    let pk = propose(
        &mut r,
        "k",
        &[ans(&confirm_cb, PlanAct::Confirm, Some("B still holds"))],
    )
    .unwrap();
    assert_eq!(
        pk.plan.by_kind["update_implementation"].open, 1,
        "{:?}",
        pk.plan
    );
    assert_eq!(pk.plan.open, 1, "only the code item is open");
    assert!(
        !warning_codes(&pk).contains(&W_PLAN_OPEN),
        "a code item never warns: {:?}",
        pk.warnings
    );
    rdy_qa::intake::confirm_before_landing(&mut r, &approver(), &pk);
    r.land(&approver(), &pk.id)
        .expect("an open code item never blocks");

    // two open document items warn with their exact count, and the version still lands
    edit(&mut r, "w", &a, "A holds 60 days.");
    let pw = propose(&mut r, "w", &[]).unwrap();
    let open: Vec<_> = pw
        .warnings
        .iter()
        .filter(|w| w.code == W_PLAN_OPEN)
        .collect();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].message, "2 change-plan items are open");
    assert_eq!(open[0].fix, PLAN_OPEN_FIX);
    assert_eq!(pw.plan.open, 2);
    // a lapsed answer: b changes after its confirm, and the next version warns of it
    let confirm_ab = pi(&a, &b, PlanKind::Confirm);
    r.answer_plan(
        &writer(),
        "w",
        &[ans(&confirm_ab, PlanAct::Confirm, Some("B still holds"))],
    )
    .unwrap();
    edit(
        &mut r,
        "w",
        &b,
        &format!("B builds on {{{{{a}}}}} and {{{{{c}}}}}, again."),
    );
    // b still cites c as it stood before k landed: its pin follows, so no premise shift blocks
    write(
        &mut r,
        "w",
        json!([{"op": "repin", "id": b, "targets": [c]}]),
    )
    .unwrap();
    let pw2 = propose(&mut r, "w", &[]).unwrap();
    let lapsed: Vec<_> = pw2
        .warnings
        .iter()
        .filter(|w| w.code == rdy_engine::plan::W_PLAN_ANSWER_LAPSED)
        .collect();
    assert_eq!(lapsed.len(), 1, "{:?}", pw2.warnings);
    assert_eq!(
        lapsed[0].message,
        format!("the answer to {confirm_ab} lapsed: {b} changed")
    );
    let _ = p;
    let l = r
        .land_report(&approver(), &pw2.id)
        .expect("open items never refuse");
    assert!(!l.landing.is_empty());
    r.fsck().unwrap();
}

/// The carried records no product path writes yet, in the log of `r`: kinds 25 to 28, Check
/// subjects and the Pass, Concern and Fail verdicts, the Dispute, ResolveDispute and
/// AcceptVerdict acts, and the new Change and LandingRecord fields. Returns how many frames of
/// the guarded kinds it read.
fn future_records(r: &Rdy) -> (Vec<String>, usize) {
    use rdy_model::{Answer, AnswerAct, Change, LandingRecord, Review, ReviewSubject, Verdict};
    use rdy_store::codec::{decode_record, logical_kind};
    let mut found = vec![];
    let mut read = 0;
    r.store()
        .reader()
        .for_each_frame_from(
            Location {
                segment: 0,
                offset: 0,
            },
            r.store().log_end(),
            |_, raw, hash, payload, _| {
                let what = format!("kind {} {hash}", logical_kind(raw));
                match logical_kind(raw) {
                    kinds::ASSESSMENT
                    | kinds::DISPUTE
                    | kinds::DISPUTE_RESOLUTION
                    | kinds::CHECK_REQUEST => found.push(what),
                    kinds::REVIEW => {
                        read += 1;
                        let v: Review = decode_record(raw, payload)?;
                        if matches!(v.subject, ReviewSubject::Check { .. })
                            || matches!(v.verdict, Verdict::Pass | Verdict::Concern | Verdict::Fail)
                        {
                            found.push(what);
                        }
                    }
                    kinds::ANSWER => {
                        read += 1;
                        let a: Answer = decode_record(raw, payload)?;
                        if matches!(
                            a.act,
                            AnswerAct::Dispute
                                | AnswerAct::ResolveDispute
                                | AnswerAct::AcceptVerdict
                        ) {
                            found.push(what);
                        }
                    }
                    kinds::CHANGE => {
                        read += 1;
                        let c: Change = decode_record(raw, payload)?;
                        if c.links.replayed_from.is_some() || c.links.briefing.is_some() {
                            found.push(what);
                        }
                    }
                    kinds::LANDING => {
                        read += 1;
                        let l: LandingRecord = decode_record(raw, payload)?;
                        if l.assessment.is_some() || l.policy.is_some() {
                            found.push(what);
                        }
                    }
                    _ => {}
                }
                Ok(())
            },
        )
        .unwrap();
    (found, read)
}

/// ok4: the product paths keep producing their existing acts. A propose with its bound plan, a
/// plan answer, a person's approval, an ordinary write and an import write no record of kinds
/// 25 to 28, no Check subject or new verdict, no Dispute, ResolveDispute or AcceptVerdict act,
/// and leave every new carrier field None.
#[test]
fn ok4_product_paths_do_not_write_future_records() {
    let (d, mut r) = project();
    let repo = linked_repo(d.path());
    let (_s, a, b, _c, _p) = spec(&mut r);
    commit(repo.path(), "f.rs", &format!("start\n\nImplements: {a}"));
    edit(&mut r, "w", &a, "A holds 60 days.");
    let confirm = pi(&a, &b, PlanKind::Confirm);
    r.answer_plan(
        &writer(),
        "w",
        &[ans(&confirm, PlanAct::Confirm, Some("B still holds"))],
    )
    .unwrap();
    let p = propose(&mut r, "w", &[]).unwrap();
    r.land(&approver(), &p.id).unwrap();
    // an import of canon into a new project, proposed and landed
    let out = tempfile::tempdir().unwrap();
    let out = out.path().join("export");
    r.read().export(&rdy_engine::Scope::Canon, &out).unwrap();
    let other = tempfile::tempdir().unwrap();
    let mut imp = Rdy::init(other.path(), "rdy-standard").unwrap();
    imp.import(&approver(), "import", &out).unwrap();
    let pi = imp
        .propose(&writer(), "import", "import", Some(TEST_SUMMARY))
        .unwrap();
    imp.land(&approver(), &pi.id).unwrap();
    for (what, r) in [("project", &r), ("import", &imp)] {
        let (found, read) = future_records(r);
        assert!(found.is_empty(), "{what}: {found:?}");
        assert!(read > 0, "{what}: the guard read the product records");
    }
}
