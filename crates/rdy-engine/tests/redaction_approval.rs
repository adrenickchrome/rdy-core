//! rdx10: a recorded removal lands only with a fresh approval by a person (D-CC90E: "It lands
//! only with a human approval"; D-6B1W1: the blast radius before the proposal; D-14SAY,
//! D-EPK1Z, D-600CW, D-KQ4CA). The approval recomputes the complete report under the writer and
//! refuses a stale one; an agent alone cannot approve; the batch that authorizes the removal logs
//! its tension and suspect moves; old read states refuse new reads once the epoch moves; a failure
//! after authorization never publishes the old content.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::pending::{CARD_MAX_TOKENS, Item, PendingQuery};
use rdy_engine::redaction::{
    RedactInput, RedactResult, fail_next_redaction_previews_for_tests,
    report_computations_for_tests,
};
use rdy_engine::*;
use rdy_model::redaction::{CONTENT_REMOVED, RedactionApply};
use rdy_model::{BlockId, Hash, Role};
use rdy_store::kinds;
use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint};
use rdy_store::transition::{Cause, Transitions};
use serde_json::json;
use std::path::Path;

const SECRET: &str = "The launch code is ZEBRA-7731.";
const PERSON: &str = "human:t";
const QUARANTINE: &str = "Quarantine files may hold the content. Redaction does not remove them. A person can run rdy fsck --clean-quarantine.";

fn writer() -> Session {
    Session::human(PERSON.into(), Role::Writer)
}

fn reviewer() -> Session {
    Session::human(PERSON.into(), Role::Reviewer)
}

fn approver() -> Session {
    Session::human(PERSON.into(), Role::Approver)
}

fn agent() -> Session {
    Session::agent("agent:a".into(), Some("run-1".into()), Role::Writer).unwrap()
}

/// An agent session that claims the approver role, which `Session::agent` never builds.
fn forged_agent_approver() -> Session {
    Session {
        actor: "agent:a".into(),
        run: Some("run-1".into()),
        role: Role::Approver,
        kind: SessionKind::Agent,
    }
}

fn write(r: &mut Rdy, view: &str, ops: serde_json::Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap()
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A project whose canon holds a doc with a prose block holding `SECRET` and a term, landed as
/// `landing` and tagged v1.0.0.
struct P {
    dir: tempfile::TempDir,
    r: Rdy,
    doc: BlockId,
    prose: BlockId,
    term: BlockId,
    landing: String,
}

fn landed() -> P {
    landed_with(SECRET)
}

/// [`landed`], with `body` as the prose block's body.
fn landed_with(body: &str) -> P {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "design", "title": "Design"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Doc", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": body},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "A ledger."}
        ]),
    );
    let prop = r.propose(&writer(), "work", "Base", None).unwrap();
    let landing = r.land(&approver(), &prop.id).unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    P {
        doc: id(&w.created["$d"]),
        prose: id(&w.created["$p"]),
        term: id(&w.created["$t"]),
        landing,
        dir,
        r,
    }
}

/// `id@version` of the prose block's canon version.
fn version_target(p: &P) -> String {
    let v =
        p.r.read()
            .materialized(&Scope::Canon)
            .unwrap()
            .version_of(&p.prose)
            .unwrap();
    format!("{}@{v}", p.prose)
}

fn input(targets: Vec<String>, dry_run: bool) -> RedactInput {
    RedactInput {
        targets,
        reason_class: "secret".into(),
        reason: "a leaked code".into(),
        dry_run,
    }
}

/// Records a request for `targets` in `view` as `s`.
fn request(p: &mut P, s: &Session, view: &str, targets: Vec<String>) -> RedactResult {
    let res = p.r.redact_in(s, view, input(targets, false)).unwrap();
    assert!(res.redaction.is_some(), "the request is recorded");
    res
}

fn propose(p: &mut P, s: &Session, view: &str) -> ProposalInfo {
    p.r.propose(s, view, "Remove the code", Some("Removes the leaked code."))
        .unwrap()
}

/// Whether any segment file holds `needle`'s bytes.
fn on_disk(dir: &Path, needle: &str) -> bool {
    let segs = dir.join(".rdy/store/segments");
    std::fs::read_dir(segs).unwrap().any(|e| {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
    })
}

/// The REDACTION_APPLY records of the last sealed batch.
fn applies_in_last_batch(r: &Rdy) -> Vec<RedactionApply> {
    r.store()
        .last_batch()
        .iter()
        .filter(|(k, _)| *k == kinds::REDACTION_APPLY)
        .map(|(k, h)| r.store().get_typed(*k, h).unwrap())
        .collect()
}

fn queue(r: &Rdy) -> Vec<Item> {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap()
        .items
}

fn item_of(r: &Rdy, r#ref: &str) -> Item {
    queue(r)
        .into_iter()
        .find(|i| i.r#ref == r#ref)
        .unwrap_or_else(|| panic!("{ref} is not in the queue"))
}

fn answer(
    r: &mut Rdy,
    s: &Session,
    r#ref: &str,
    option: &str,
    card_hash: Option<String>,
    relay: RelayInput,
) -> Result<answer::AnswerInfo, RdyError> {
    r.answer(
        s,
        PERSON,
        AnswerRequest {
            r#ref: r#ref.into(),
            option: option.into(),
            card_hash,
            note: None,
            relay,
            brief: None,
        },
    )
}

fn relay_yes() -> RelayInput {
    RelayInput {
        evidence: Some("Yes, remove it.".into()),
        ..RelayInput::default()
    }
}

/// The prose block as the canon of `st` reads it.
fn canon_prose(st: &ReadState, p: &P) -> rdy_model::Block {
    st.block(&Scope::Canon, &p.prose).unwrap()
}

fn assert_stale(e: &RdyError, what: &str) {
    assert_eq!(e.code, E_REDACTION_STALE, "{what}: {e}");
    assert_eq!(
        e.message, "The redaction report changed after the request was recorded.",
        "{what}"
    );
    assert_eq!(
        e.fix, "Record a new request, show its report, and ask for approval again.",
        "{what}"
    );
}

fn assert_human(e: &RdyError, what: &str) {
    assert_eq!(e.code, E_REDACTION_HUMAN, "{what}: {e}");
    assert_eq!(
        e.message, "Content removal needs a person's approval.",
        "{what}"
    );
    assert_eq!(
        e.fix,
        "Show the redaction card and relay the person's answer, or approve from the CLI as a human.",
        "{what}"
    );
}

/// The content is intact: on disk, in canon, with no epoch move.
fn assert_intact(p: &P, what: &str) {
    assert!(
        on_disk(p.dir.path(), SECRET),
        "{what}: the bytes stay on disk"
    );
    let st = p.r.read();
    assert_eq!(canon_prose(&st, p).body, SECRET, "{what}");
    assert_eq!(st.redaction_epoch(), 0, "{what}: the epoch never moved");
    let v = st
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(&p.prose)
        .unwrap();
    assert_eq!(
        st.reader().redaction_of(kinds::BLOCK, &v).unwrap(),
        None,
        "{what}: no mask is active"
    );
}

#[test]
fn rdx10_old_states_refuse_new_reads_after_epoch_advance() {
    let _clock = pin_clock(1_000_000);
    let mut p = landed();
    // an open view with its own change, so its build holds the content beside canon's
    let term = p.term.clone();
    write(
        &mut p.r,
        "edit",
        json!([{"op": "update", "id": term.to_string(), "body": "A ledger of records."}]),
    );
    let target = vec![p.prose.to_string()];
    request(&mut p, &writer(), "redact", target);
    let prop = propose(&mut p, &writer(), "redact");
    let l0: Hash = p.landing.parse().unwrap();
    let pv: Hash = prop.id.parse().unwrap();
    let old = p.r.read();
    let scopes = [
        Scope::Canon,
        Scope::View("edit".into()),
        Scope::At(l0),
        Scope::Proposal(pv),
        Scope::Inflight,
        old.resolve_scope("tag:v1.0.0").unwrap(),
    ];
    // every named cache populated in the old state
    for s in &scopes {
        assert_eq!(old.block(s, &p.prose).unwrap().body, SECRET, "{s:?}");
        old.graph(s).unwrap();
    }
    old.versions().unwrap();
    old.tensions().unwrap();
    old.inflight_set().unwrap();
    old.history(&Scope::Canon, &p.prose, 10).unwrap();
    old.coverage_issues(&Scope::Canon).unwrap();
    old.ensure_current_epoch().unwrap();
    let epoch = old.redaction_epoch();

    p.r.land(&approver(), &prop.id).unwrap();

    // the old state refuses a new response; its cached builds are a possible outside copy
    let e = old.ensure_current_epoch().unwrap_err();
    assert_eq!(e.code, E_REDACTED_STATE, "{e}");
    assert_eq!(e.message, "This read state predates content removal.");
    assert_eq!(e.fix, "Read the project again.");
    // a fresh state is in the new epoch and reads stubs in every scope
    let fresh = p.r.read();
    fresh.ensure_current_epoch().unwrap();
    assert!(fresh.redaction_epoch() > epoch);
    let scopes = [
        Scope::Canon,
        Scope::View("edit".into()),
        Scope::At(l0),
        Scope::Proposal(pv),
        Scope::Inflight,
        fresh.resolve_scope("tag:v1.0.0").unwrap(),
    ];
    for s in &scopes {
        let b = fresh.block(s, &p.prose).unwrap();
        assert_eq!(b.body, CONTENT_REMOVED, "{s:?}");
        assert!(b.read_identity.is_some(), "{s:?}");
        assert_eq!(fresh.block(s, &p.doc).unwrap().id, p.doc, "{s:?}");
    }
    assert_eq!(fresh.versions().unwrap().len(), 1, "the tag still reads");
    fresh.history(&Scope::Canon, &p.prose, 10).unwrap();
    assert!(!on_disk(p.dir.path(), SECRET), "the payloads are erased");
    // the engine input boundary refuses the erased version
    let v = fresh
        .materialized(&Scope::At(l0))
        .unwrap()
        .version_of(&p.prose)
        .unwrap();
    let e =
        p.r.write(
            &writer(),
            WriteRequest {
                view: "restore".into(),
                ops: serde_json::from_value(json!([
                    {"op": "update", "id": p.prose.to_string(), "body": SECRET}
                ]))
                .unwrap(),
                justification: None,
                message: "restore".into(),
            },
        )
        .unwrap_err();
    assert_eq!(e.code, E_REDACTED, "{e}");
    assert!(!e.message.contains(SECRET));
    drop((old, fresh));
    // after a reopen the content stays removed
    let P { dir, r, prose, .. } = p;
    drop(r);
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    st.ensure_current_epoch().unwrap();
    assert_eq!(
        st.block(&Scope::Canon, &prose).unwrap().body,
        CONTENT_REMOVED
    );
    assert!(
        st.reader()
            .redaction_of(kinds::BLOCK, &v)
            .unwrap()
            .is_some(),
        "the version is removed"
    );
}

#[test]
fn rdx10_an_agent_can_request_but_cannot_approve_alone() {
    let _clock = pin_clock(2_000_000);
    let mut p = landed();
    let target = vec![p.prose.to_string()];
    // a writer agent and a human reviewer may each record a request
    let a = p.r.redact(&agent(), input(target.clone(), false)).unwrap();
    assert!(a.redaction.is_some());
    let rv = request(&mut p, &reviewer(), "review-side", target);
    assert!(rv.redaction.is_some());
    let prop = propose(&mut p, &agent(), "run-1");
    let end = p.r.store().log_end();
    // an agent alone, or an agent that claims the approver role, cannot approve the removal
    for (who, what) in [
        (agent(), "an agent"),
        (
            forged_agent_approver(),
            "an agent with a forged approver role",
        ),
    ] {
        let e = p.r.land(&who, &prop.id).unwrap_err();
        assert_human(&e, what);
        assert_eq!(p.r.store().log_end(), end, "{what}: nothing is written");
        assert_intact(&p, what);
    }
    // the ordinary role checks still apply to people
    for (who, what) in [(writer(), "a writer"), (reviewer(), "a reviewer")] {
        let e = p.r.land(&who, &prop.id).unwrap_err();
        assert_eq!(e.code, E_PERMISSION, "{what}: {e}");
        assert_eq!(p.r.store().log_end(), end, "{what}: nothing is written");
    }
    // a batch of recommended answers never approves it
    let item = item_of(&p.r, &prop.r#ref);
    let e =
        p.r.answer_all_recommended(
            &agent(),
            PERSON,
            std::slice::from_ref(&prop.r#ref),
            std::slice::from_ref(&item.card_hash),
            relay_yes(),
        )
        .unwrap_err();
    assert_human(&e, "a bulk answer");
    assert_eq!(p.r.store().log_end(), end);
    assert!(applies_in_last_batch(&p.r).is_empty());
    assert_intact(&p, "after every refusal");
    // the person approves it
    p.r.land(&approver(), &prop.id).unwrap();
    let applies = applies_in_last_batch(&p.r);
    assert_eq!(applies.len(), 1);
    assert_eq!(applies[0].request, a.redaction.unwrap());
    assert_eq!(canon_prose(&p.r.read(), &p).body, CONTENT_REMOVED);
}

#[test]
fn rdx10_local_and_relayed_humans_bind_the_complete_report() {
    let _clock = pin_clock(3_000_000);
    for relayed in [false, true] {
        let mut p = landed();
        let target = version_target(&p);
        let res = request(&mut p, &writer(), "redact", vec![target.clone()]);
        let prop = propose(&mut p, &writer(), "redact");
        let item = item_of(&p.r, &prop.r#ref);
        let card = item.removal.clone().expect("a removal card");
        // the card binds the request and the hash of its complete report: the manifest the
        // request recorded, which a dry run of the same targets still gives
        assert_eq!(card[0].request, res.redaction.unwrap());
        assert_eq!(card[0].report, res.report.report_hash);
        let recorded =
            p.r.read()
                .reader()
                .redaction_request(&card[0].request)
                .unwrap();
        let mut buf = vec![0u8; rdy_model::redaction::MAX_RECORD_BYTES];
        assert_eq!(
            recorded.manifest.report_hash(&mut buf).unwrap(),
            card[0].report
        );
        let dry =
            p.r.read()
                .redaction_report(&input(vec![target], true))
                .unwrap();
        assert_eq!(dry.report_hash, card[0].report);
        let (who, rel) = if relayed {
            (agent(), relay_yes())
        } else {
            (approver(), RelayInput::default())
        };
        // a quarantine name that appears after the card was shown changes the complete report
        let q = p.dir.path().join(".rdy/store/index.redb.damaged-1");
        std::fs::write(&q, b"x").unwrap();
        let end = p.r.store().log_end();
        let e = answer(
            &mut p.r,
            &who,
            &prop.r#ref,
            "approve",
            Some(item.card_hash.clone()),
            rel.clone(),
        )
        .unwrap_err();
        assert_stale(&e, if relayed { "relayed" } else { "local" });
        assert_eq!(p.r.store().log_end(), end);
        assert_intact(&p, "a stale approval");
        std::fs::remove_file(&q).unwrap();
        // the card the person answered binds the request and its complete report
        let info = answer(
            &mut p.r,
            &who,
            &prop.r#ref,
            "approve",
            Some(item.card_hash.clone()),
            rel,
        )
        .unwrap();
        assert_eq!(info.assurance, if relayed { "relayed" } else { "local" });
        let st = p.r.read();
        st.ensure_current_epoch().unwrap();
        let landing = st.canon_head().unwrap();
        let rec = st.reader().landing(&landing).unwrap();
        if relayed {
            assert_eq!(
                rec.evidence.and_then(|e| e.card_hash),
                Some(item.card_hash.parse::<Hash>().unwrap())
            );
        }
        assert_eq!(
            applies_in_last_batch(&p.r),
            vec![RedactionApply {
                request: res.redaction.unwrap(),
                landing,
                report_hash: res.report.report_hash,
            }]
        );
        assert_eq!(canon_prose(&st, &p).body, CONTENT_REMOVED);
        assert!(!on_disk(p.dir.path(), SECRET));
    }
}

/// Records a request in `redact` (all versions of the prose block when `all`, else its canon
/// version), proposes it, runs `mutate`, and checks that the approval is stale and writes nothing.
fn stale_case(what: &str, all: bool, mutate: impl FnOnce(&mut P)) {
    let mut p = landed();
    let prose = p.prose.clone();
    // a view whose change names the target version as its base, recorded before the request
    write(
        &mut p.r,
        "edit",
        json!([{"op": "update", "id": prose.to_string(), "body": "Rotated."}]),
    );
    let target = if all {
        prose.to_string()
    } else {
        version_target(&p)
    };
    request(&mut p, &writer(), "redact", vec![target]);
    let prop = propose(&mut p, &writer(), "redact");
    mutate(&mut p);
    let end = p.r.store().log_end();
    let e = match p.r.land(&approver(), &prop.id) {
        Ok(l) => panic!("{what}: the approval landed as {l}"),
        Err(e) => e,
    };
    assert_stale(&e, what);
    assert_eq!(p.r.store().log_end(), end, "{what}: nothing is written");
    assert_intact(&p, what);
}

#[test]
fn rdx10_each_affected_change_requires_new_approval() {
    let _clock = pin_clock(4_000_000);
    stale_case("a new target version", true, |p| {
        let prose = p.prose.clone();
        write(
            &mut p.r,
            "more",
            json!([{"op": "update", "id": prose.to_string(), "body": "Another text."}]),
        );
    });
    stale_case("a new quote", false, |p| {
        write(
            &mut p.r,
            "quote",
            json!([
                {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "notes", "title": "Notes"}},
                {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Copy", "doc_type": "page"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Copy"}},
                {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": SECRET}
            ]),
        );
    });
    stale_case("a new tag", false, |p| {
        p.r.tag(&approver(), "v1.1.0", None, None).unwrap();
    });
    stale_case("an independent proposal", false, |p| {
        p.r.propose(&writer(), "edit", "Rotate", None).unwrap();
    });
    stale_case("a new quarantine name", false, |p| {
        std::fs::write(
            p.dir.path().join(".rdy/store/index.redb.damaged-7"),
            b"damaged",
        )
        .unwrap();
    });

    // unrelated writes and the request's own control records leave the approval fresh
    let mut p = landed();
    let prose = p.prose.clone();
    write(
        &mut p.r,
        "edit",
        json!([{"op": "update", "id": prose.to_string(), "body": "Rotated."}]),
    );
    let target = version_target(&p);
    // the request joins the view whose change names the target, so its own proposal (which
    // holds that change and the carrier) references the affected content
    let res = request(&mut p, &writer(), "edit", vec![target]);
    write(
        &mut p.r,
        "side",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "other", "title": "Other"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Other", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Other"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Unrelated text."}
        ]),
    );
    let prop = propose(&mut p, &writer(), "edit");
    let item = item_of(&p.r, &prop.r#ref);
    answer(
        &mut p.r,
        &agent(),
        &prop.r#ref,
        "later",
        Some(item.card_hash.clone()),
        relay_yes(),
    )
    .unwrap();
    std::fs::write(p.dir.path().join("export.yaml"), SECRET).unwrap();
    p.r.land(&approver(), &prop.id).unwrap();
    let applies = applies_in_last_batch(&p.r);
    assert_eq!(applies.len(), 1);
    assert_eq!(applies[0].request, res.redaction.unwrap());
    assert_eq!(applies[0].report_hash, res.report.report_hash);
    let st = p.r.read();
    assert_eq!(
        canon_prose(&st, &p).body,
        "Rotated.",
        "the new version landed"
    );
    assert!(!on_disk(p.dir.path(), SECRET));
}

#[test]
fn rdx10_preparation_withdrawal_and_rejection_remove_no_bytes() {
    let _clock = pin_clock(5_000_000);
    let mut p = landed();
    let target = vec![p.prose.to_string()];
    // a dry run and a recorded request
    p.r.redact_in(&writer(), "redact", input(target.clone(), true))
        .unwrap();
    assert_intact(&p, "a dry run");
    request(&mut p, &writer(), "redact", target.clone());
    assert_intact(&p, "a recorded request");
    // a rejection of its proposal
    let prop = propose(&mut p, &writer(), "redact");
    let item = item_of(&p.r, &prop.r#ref);
    answer(
        &mut p.r,
        &approver(),
        &prop.r#ref,
        "reject",
        Some(item.card_hash.clone()),
        RelayInput::default(),
    )
    .unwrap();
    assert_intact(&p, "a rejection");
    // a withdrawal: the view's close withdraws its proposal
    p.r.view_close(&writer(), &["redact".to_string()], "")
        .unwrap();
    assert_intact(&p, "a withdrawal by close");
    // an include, an exclude and an undo of a carrier in another view
    let res = request(&mut p, &writer(), "second", target);
    let carrier = res.change.unwrap().to_string();
    p.r.view_include(&writer(), "third", std::slice::from_ref(&carrier))
        .unwrap();
    assert_intact(&p, "an include");
    p.r.view_exclude(&writer(), "third", std::slice::from_ref(&carrier), false)
        .unwrap();
    assert_intact(&p, "an exclude");
    p.r.undo(&writer(), "third", None, false).unwrap();
    assert_intact(&p, "an undo");
    p.r.view_close(&writer(), &["second".to_string(), "third".to_string()], "")
        .unwrap();
    assert_intact(&p, "a close");
    // nothing was ever applied
    assert!(applies_in_last_batch(&p.r).is_empty());
}

#[test]
fn rdx10_redaction_moves_are_logged_in_the_authorizing_batch() {
    let _clock = pin_clock(6_000_000);
    // two terms of one name in canon: a duplicate definition the removal of one resolves
    let mut p = landed();
    let w = write(
        &mut p.r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "words", "title": "Words"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Glossary", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "A second ledger."}
        ]),
    );
    let twin = id(&w.created["$t"]);
    let base = p.r.propose(&writer(), "work", "Glossary", None).unwrap();
    p.r.land(&approver(), &base.id).unwrap();
    let v =
        p.r.read()
            .materialized(&Scope::Canon)
            .unwrap()
            .version_of(&twin)
            .unwrap();
    request(&mut p, &writer(), "redact", vec![format!("{twin}@{v}")]);
    let prop = propose(&mut p, &writer(), "redact");

    // an injected preview failure aborts before authorization: nothing is sealed
    let end = p.r.store().log_end();
    fail_next_redaction_previews_for_tests(1);
    let before = p.r.read();
    assert!(p.r.land(&approver(), &prop.id).is_err());
    assert!(
        std::sync::Arc::ptr_eq(&before, &p.r.read()),
        "the failure publishes nothing"
    );
    drop(before);
    let P { dir, r, .. } = p;
    drop(r);
    let mut r = Rdy::open(dir.path()).unwrap();
    assert_eq!(
        r.store().log_end(),
        end,
        "no landing and no apply were sealed"
    );
    let st = r.read();
    assert_eq!(
        st.block(&Scope::Canon, &twin).unwrap().body,
        "A second ledger."
    );
    assert_eq!(st.reader().redaction_of(kinds::BLOCK, &v).unwrap(), None);
    assert_eq!(st.redaction_epoch(), 0);
    drop(st);

    // the approval logs the moves the removal makes in its own batch, last
    let landing = r.land(&approver(), &prop.id).unwrap();
    let batch = r.store().last_batch().to_vec();
    let (last_kind, last_hash) = *batch.last().unwrap();
    assert_eq!(last_kind, kinds::TRANSITION, "{batch:?}");
    let kinds_seen: Vec<u8> = batch.iter().map(|(k, _)| *k).collect();
    assert!(kinds_seen.contains(&kinds::LANDING) && kinds_seen.contains(&kinds::REDACTION_APPLY));
    let rec: Transitions = r.store().get_typed(kinds::TRANSITION, &last_hash).unwrap();
    let landing: Hash = landing.parse().unwrap();
    let resolved: Vec<_> = rec
        .resolved
        .iter()
        .filter(|t| t.kind == "duplicate_definition")
        .collect();
    assert_eq!(resolved.len(), 1, "{rec:?}");
    assert_eq!(resolved[0].why.cause, Cause::Landed);
    assert_eq!(resolved[0].why.id, Some(landing));
    let tensions = r.store().open_tensions().unwrap();
    let suspects = r.store().open_suspects().unwrap();
    // the log folds to the active sets after a reopen and after a reindex
    drop(r);
    let mut r = Rdy::open(dir.path()).unwrap();
    assert_eq!(
        r.reconcile_transitions().unwrap(),
        0,
        "the log folds to the sets"
    );
    assert_eq!(r.store().open_tensions().unwrap(), tensions);
    assert_eq!(r.store().open_suspects().unwrap(), suspects);
    r.reindex().unwrap();
    assert_eq!(r.store().open_tensions().unwrap(), tensions);
    assert_eq!(r.store().open_suspects().unwrap(), suspects);
    assert_eq!(
        r.read().block(&Scope::Canon, &twin).unwrap().body,
        CONTENT_REMOVED
    );
}

#[test]
fn rdx10_a_failed_finish_never_publishes_old_content() {
    let _clock = pin_clock(7_000_000);
    let mut p = landed();
    let target = vec![p.prose.to_string()];
    request(&mut p, &writer(), "redact", target);
    let prop = propose(&mut p, &writer(), "redact");
    let old = p.r.read();
    assert_eq!(canon_prose(&old, &p).body, SECRET);
    arm_failpoint(Some((Failpoint::AfterZeroWrite, FailAction::Fail)));
    let e = p.r.land(&approver(), &prop.id).unwrap_err();
    arm_failpoint(None);
    assert_eq!(e.code, E_REDACTION_PENDING, "{e}");
    assert_eq!(
        e.message,
        "Content removal was authorized but has not finished."
    );
    assert_eq!(
        e.fix,
        "Close the project and reopen it to finish the removal. Do not delete the log."
    );
    // the published state serves no content: neither the old builds nor the store's bytes
    assert!(!std::sync::Arc::ptr_eq(&old, &p.r.read()));
    let st = p.r.read();
    assert!(st.ensure_current_epoch().is_err());
    let e = st.block(&Scope::Canon, &p.prose).unwrap_err();
    assert_eq!(e.code, E_REDACTION_PENDING, "{e}");
    let e = st.materialized(&Scope::View("redact".into())).unwrap_err();
    assert_eq!(e.code, E_REDACTION_PENDING, "{e}");
    assert!(old.ensure_current_epoch().is_err());
    drop((old, st));
    // the authorized landing stays: the reopen finishes the removal
    let P { dir, r, prose, .. } = p;
    drop(r);
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    st.ensure_current_epoch().unwrap();
    assert_eq!(
        st.block(&Scope::Canon, &prose).unwrap().body,
        CONTENT_REMOVED
    );
    let (_, head) = st.landings().unwrap().into_iter().next().unwrap();
    assert_eq!(head.proposal, prop.id.parse::<Hash>().unwrap());
    assert!(!on_disk(dir.path(), SECRET));
}

#[test]
fn rdx10_public_reports_have_one_quarantine_source() {
    let _clock = pin_clock(8_000_000);
    let mut p = landed();
    let name = "index.redb.damaged-3".to_string();
    std::fs::write(p.dir.path().join(".rdy/store").join(&name), b"damaged").unwrap();
    let target = vec![p.prose.to_string()];
    // the dry run, the recorded request and the card name the same quarantine files
    let dry =
        p.r.redact_in(&writer(), "redact", input(target.clone(), true))
            .unwrap();
    assert_eq!(dry.report.quarantine, vec![name.clone()]);
    assert_eq!(
        dry.report,
        p.r.read()
            .redaction_report(&input(target.clone(), true))
            .unwrap()
    );
    let res = request(&mut p, &writer(), "redact", target);
    assert_eq!(res.report.quarantine, vec![name.clone()]);
    assert_eq!(res.report.report_hash, dry.report.report_hash);
    let prop = propose(&mut p, &writer(), "redact");
    // the card display computes the complete report once, through the same wrapper
    let before = report_computations_for_tests();
    let item = item_of(&p.r, &prop.r#ref);
    assert_eq!(report_computations_for_tests(), before + 1);
    assert_eq!(
        item.removal_reports.get(&res.report.report_hash),
        Some(&vec![name.clone()])
    );
    assert!(item.card.contains(QUARANTINE), "{}", item.card);
    // a quarantine name that appears before the card is shown again stales the request: the
    // display refuses the card, and never refreshes the request
    let late = p.dir.path().join(".rdy/store/index.redb.damaged-99");
    std::fs::write(&late, b"damaged").unwrap();
    let before = report_computations_for_tests();
    let e =
        p.r.read()
            .pending(&PendingQuery {
                person: PERSON.into(),
                session_ref: None,
                max_tokens: CARD_MAX_TOKENS,
            })
            .unwrap_err();
    assert_stale(&e, "a card shown after a new quarantine name");
    assert_eq!(report_computations_for_tests(), before + 1);
    std::fs::remove_file(&late).unwrap();
    // output files (an export, a render, a dump) are not report inputs: no file list, no hook
    let root = p.dir.path();
    std::fs::create_dir_all(root.join("site")).unwrap();
    std::fs::write(root.join("site/index.html"), SECRET).unwrap();
    std::fs::write(root.join("export.yaml"), SECRET).unwrap();
    std::fs::write(root.join("project.rdydump"), SECRET).unwrap();
    // the final freshness check computes the complete report once, through the same wrapper
    let before = report_computations_for_tests();
    p.r.land(&approver(), &prop.id).unwrap();
    assert_eq!(report_computations_for_tests(), before + 1);
    assert_eq!(
        applies_in_last_batch(&p.r)[0].report_hash,
        res.report.report_hash
    );
    // the quarantine file is reported and never erased
    assert!(root.join(".rdy/store").join(&name).exists());
    assert_eq!(canon_prose(&p.r.read(), &p).body, CONTENT_REMOVED);
}

#[test]
fn rdx10_an_acceptance_that_copies_a_target_is_stale() {
    let _clock = pin_clock(9_000_000);
    const RULING: &str = "Another secret.";
    let mut p = landed();
    // a proposed decision that holds the content, and prose that applies it, in the view that
    // then records the removal of every version of the decision
    let w = write(
        &mut p.r,
        "redact",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Rulings", "doc_type": "decisions-log"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rulings"}},
            {"op": "create", "type": "decision", "id": "$x", "parent": "$s", "fields": {"statement": RULING, "status": "proposed"}},
            {"op": "create", "type": "claim", "parent": "$s", "body": "Codes rotate each week.", "links": {"decided_by": [{"target": "$x"}]}}
        ]),
    );
    let decision = id(&w.created["$x"]);
    request(&mut p, &writer(), "redact", vec![decision.to_string()]);
    let prop = propose(&mut p, &writer(), "redact");
    assert!(
        queue(&p.r)
            .iter()
            .all(|i| i.batch.as_deref() != Some(prop.r#ref.as_str()) || i.r#ref == prop.r#ref),
        "the version needs no other answer"
    );
    // landing would accept the decision: the acceptance change embeds a new version of it that
    // holds the content, outside the removal the person saw, so the approval is stale
    let end = p.r.store().log_end();
    let e = match p.r.land(&approver(), &prop.id) {
        Ok(l) => panic!("the approval landed as {l}"),
        Err(e) => e,
    };
    assert_stale(&e, "an acceptance of a removed decision");
    assert_eq!(p.r.store().log_end(), end, "nothing is written");
    assert!(applies_in_last_batch(&p.r).is_empty());
    assert!(on_disk(p.dir.path(), RULING), "nothing is removed");
    assert_intact(&p, "an acceptance of a removed decision");
    // after a reopen no version of the decision is accepted, and the log holds what it held
    let P { dir, r, .. } = p;
    drop(r);
    let r = Rdy::open(dir.path()).unwrap();
    assert_eq!(r.store().log_end(), end);
    let st = r.read();
    assert!(st.block(&Scope::Canon, &decision).is_err(), "not in canon");
    let d = st.block(&Scope::View("redact".into()), &decision).unwrap();
    assert_eq!(d.field_str("status"), Some("proposed"));
}

#[test]
fn rdx10_own_ref_updates_never_stale_a_request() {
    let _clock = pin_clock(10_000_000);
    // "proposal/" is in the name of the request's own proposal ref, and "work" in the names of
    // its own view's refs (a person's default view): the batches the request itself wrote
    for body in ["proposal/", "work"] {
        let mut p = landed_with(body);
        let res =
            p.r.redact(&writer(), input(vec![p.prose.to_string()], false))
                .unwrap();
        let prop = propose(&mut p, &writer(), "work");
        p.r.land(&approver(), &prop.id)
            .unwrap_or_else(|e| panic!("{body}: {e}"));
        let applies = applies_in_last_batch(&p.r);
        assert_eq!(applies.len(), 1, "{body}");
        assert_eq!(applies[0].request, res.redaction.unwrap(), "{body}");
        assert_eq!(canon_prose(&p.r.read(), &p).body, CONTENT_REMOVED, "{body}");
    }
    // a ref update of another batch that quotes the same text still stales the request: an
    // unrelated proposal's ref, an unrelated view's ref
    let unrelated = json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "other", "title": "Other"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Other", "doc_type": "page"}}
    ]);
    for (body, view, proposes) in [("proposal/", "side", true), ("work", "workshop", false)] {
        let mut p = landed_with(body);
        p.r.redact(&writer(), input(vec![p.prose.to_string()], false))
            .unwrap();
        let prop = propose(&mut p, &writer(), "work");
        write(&mut p.r, view, unrelated.clone());
        if proposes {
            p.r.propose(&writer(), view, "Other", None).unwrap();
        }
        let end = p.r.store().log_end();
        let e = match p.r.land(&approver(), &prop.id) {
            Ok(l) => panic!("{body}: the approval landed as {l}"),
            Err(e) => e,
        };
        assert_stale(&e, body);
        assert_eq!(p.r.store().log_end(), end, "{body}: nothing is written");
        assert_eq!(canon_prose(&p.r.read(), &p).body, body, "{body}");
    }
}

/// Lands the removal of the prose block's canon version, approved by the person.
fn remove_prose(p: &mut P) {
    let target = vec![version_target(p)];
    request(p, &writer(), "redact", target);
    let prop = propose(p, &writer(), "redact");
    p.r.land(&approver(), &prop.id).unwrap();
}

fn update(r: &mut Rdy, view: &str, ops: serde_json::Value) -> Result<WriteResult, RdyError> {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
}

/// Final review fix round 1, finding 3: a person replaces the notice of a removed mutable block
/// with fresh text. The write, the edit op the CLI's `rdy edit` sends and the working copy's
/// commit each author a normal version (no read identity, a new version hash, the stub's version
/// as its base). A write that leaves the projection unchanged, and one that would bring the
/// erased bytes back, stay refused with `E_REDACTED`.
#[test]
fn rdx10_a_fresh_edit_of_a_removed_block_writes_a_normal_version() {
    let _clock = pin_clock(3_000_000);
    let mut p = landed();
    remove_prose(&mut p);
    let stub = canon_prose(&p.r.read(), &p);
    assert_eq!(stub.body, CONTENT_REMOVED);
    assert!(stub.read_identity.is_some());
    let erased = stub.version_hash();
    let prose = p.prose.to_string();
    let fresh = |r: &Rdy, view: &str, body: &str| {
        let b = r
            .read()
            .block(&Scope::View(view.into()), &BlockId::parse(&prose).unwrap())
            .unwrap();
        assert_eq!(b.body, body, "{view}");
        assert!(b.read_identity.is_none(), "{view}: a normal version");
        assert_ne!(b.version_hash(), erased, "{view}");
        assert_eq!(b.parent, stub.parent, "{view}: the stub's place is kept");
        assert_eq!(b.order, stub.order, "{view}");
        b
    };

    // the write path
    let w = update(
        &mut p.r,
        "fix",
        json!([{"op": "update", "id": prose, "body": "New safe replacement text."}]),
    )
    .unwrap();
    let b = fresh(&p.r, "fix", "New safe replacement text.");
    let change = p.r.store().change(&w.change.parse().unwrap()).unwrap();
    assert!(matches!(
        &change.ops[0],
        rdy_model::Op::Update { base, block } if *base == erased && block.version_hash() == b.version_hash()
    ));
    assert!(!on_disk(p.dir.path(), SECRET));

    // the edit op (`rdy edit`, the daemon's edit_block): the stub's YAML with fresh text
    let mut edited = stub.clone();
    edited.read_identity = None;
    edited.body = "Edited by a person.".into();
    p.r.edit_block_with(&writer(), "edit", edited, None, None)
        .unwrap();
    fresh(&p.r, "edit", "Edited by a person.");

    // the working copy's commit: a checkout writes the notice; the person replaces it
    let pack = rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap();
    let m = p.r.read().materialized(&Scope::Canon).unwrap();
    let proj = rdy_engine::wc::project::project_state(&m, &pack, "wc", "canon:test", None).unwrap();
    let file = proj
        .files
        .iter()
        .find(|f| f.text.contains(CONTENT_REMOVED))
        .unwrap()
        .path
        .clone();
    let wc = tempfile::tempdir().unwrap();
    let place = rdy_engine::wc::state::Place::main(wc.path());
    let mut wst = rdy_engine::wc::state::WcState::default();
    let tree = rdy_engine::wc::layout::read_tree(
        &place.root,
        &rdy_engine::wc::ignore::Ignore::empty(),
        &pack,
    );
    rdy_engine::wc::files::checkout(&place, &mut wst, &tree, &proj, false).unwrap();
    let text = std::fs::read_to_string(place.root.join(&file)).unwrap();
    let text = text.replacen(CONTENT_REMOVED, "Written in the working copy.", 1);
    std::fs::write(place.root.join(&file), &text).unwrap();
    let wst = rdy_engine::wc::state::WcState::load(&place).unwrap();
    rdy_engine::wc::state::stage_put(&place, &file, text.as_bytes()).unwrap();
    let plan = rdy_engine::wc::plan::plan_commit(&place, &wst, &pack)
        .unwrap_or_else(|e| panic!("{:#?}", e.diags));
    assert_eq!(plan.ops.len(), 1, "{:?}", plan.ops);
    update(&mut p.r, "wc", serde_json::Value::Array(plan.ops.clone())).unwrap();
    fresh(&p.r, "wc", "Written in the working copy.");

    // an unchanged projection is refused: the edit op with the notice as it was
    let mut same = stub.clone();
    same.read_identity = None;
    let e =
        p.r.edit_block_with(&writer(), "same", same, None, None)
            .unwrap_err();
    assert_eq!(e.code, E_REDACTED, "{e}");
    // and so is a move that keeps the notice
    let e = update(
        &mut p.r,
        "moved",
        json!([{"op": "move", "id": prose, "parent": stub.parent.as_ref().unwrap().to_string()}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_REDACTED, "{e}");
    // the erased bytes stay refused
    let e = update(
        &mut p.r,
        "restore",
        json!([{"op": "update", "id": prose, "body": SECRET}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_REDACTED, "{e}");
    assert!(!e.message.contains(SECRET));
}

/// Final review fix round 1, finding 3: a fresh edit of a removed accepted decision is still
/// checked against the old projection's frozen fact: it is `E_FROZEN`, never a normal version.
#[test]
fn rdx10_a_fresh_edit_of_a_removed_frozen_decision_is_refused() {
    let _clock = pin_clock(4_000_000);
    let mut p = landed();
    let section = canon_prose(&p.r.read(), &p).parent.unwrap();
    let d =
        p.r.decide(
            &writer(),
            "work",
            DecisionInput {
                parent: section.to_string(),
                statement: "Ship weekly".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let dec = id(&d.created["$decision"]);
    let prop =
        p.r.propose(&writer(), "work", "Decide", Some("Decides the cadence."))
            .unwrap();
    // the person confirms the decision's exception card, then lands
    let refs: Vec<String> = queue(&p.r)
        .into_iter()
        .filter(|i| i.batch.as_deref() == Some(prop.r#ref.as_str()))
        .map(|i| i.r#ref)
        .collect();
    assert!(!refs.is_empty());
    for r in refs {
        answer(
            &mut p.r,
            &approver(),
            &r,
            "confirm",
            None,
            RelayInput::default(),
        )
        .unwrap();
    }
    p.r.land(&approver(), &prop.id).unwrap();
    let v =
        p.r.read()
            .materialized(&Scope::Canon)
            .unwrap()
            .version_of(&dec)
            .unwrap();
    request(&mut p, &writer(), "redact", vec![format!("{dec}@{v}")]);
    let prop = propose(&mut p, &writer(), "redact");
    p.r.land(&approver(), &prop.id).unwrap();
    let stub = p.r.read().block(&Scope::Canon, &dec).unwrap();
    assert!(stub.is_redacted() && rdy_model::decision::is_frozen(&stub));
    let e = update(
        &mut p.r,
        "fix",
        json!([{"op": "update", "id": dec.to_string(), "fields": {"statement": "Ship daily"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_FROZEN, "{e}");
}
