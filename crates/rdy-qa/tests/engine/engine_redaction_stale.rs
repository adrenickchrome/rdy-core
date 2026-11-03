//! TASK-1233 (M3 gate mg5): a stale removal request refuses only itself. A pending read never
//! fails as a whole because one removal request is stale: every other item stays readable and
//! answerable, and the stale removal item stays in the list, while showing its card or answering
//! it is `E_REDACTION_STALE` (p-3FBMK, p-EYDH3). A write to an affected record stales the
//! request; an unrelated write does not, even one whose change depends on the change that holds
//! the removed content.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::pending::{CARD_MAX_TOKENS, Item, Pending, PendingQuery};
use rdy_engine::redaction::RedactInput;
use rdy_engine::*;
use rdy_model::Role;
use rdy_model::redaction::CONTENT_REMOVED;
use serde_json::json;

const PERSON: &str = "human:t";
const SECRET: &str = "The code is 1234.";
const STALE_MESSAGE: &str = "The redaction report changed after the request was recorded.";
const STALE_FIX: &str = "Record a new request, show its report, and ask for approval again.";

fn writer() -> Session {
    Session::human(PERSON.into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human(PERSON.into(), Role::Approver).unwrap()
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

/// A landed canon with a secret in one doc and a claim in another (one change wrote both), an
/// unrelated open proposal (`work`), and a recorded and proposed removal of the secret
/// (`redact`): the mg5 reproducer's state before its write.
struct P {
    _dir: tempfile::TempDir,
    r: Rdy,
    secret: String,
    other: String,
    work: ProposalInfo,
    redact: ProposalInfo,
}

fn project() -> P {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$kd", "fields": {"name": "keys", "title": "Keys", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ks", "parent": "$kd", "fields": {"title": "Keys"}},
            {"op": "create", "type": "prose", "id": "$secret", "parent": "$ks", "body": SECRET},
            {"op": "create", "type": "doc", "id": "$od", "fields": {"name": "other", "title": "Other", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$os", "parent": "$od", "fields": {"title": "More"}},
            {"op": "create", "type": "claim", "id": "$o", "parent": "$os", "body": "O holds."}
        ]),
    );
    let seed = r
        .propose(&writer(), "seed", "Seed", Some("Seeds."))
        .unwrap();
    r.land(&approver(), &seed.id).unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "folder", "fields": {"name": "notes", "title": "Notes"}}]),
    );
    let work = r
        .propose(&writer(), "work", "Notes", Some("Adds notes."))
        .unwrap();
    let secret = w.created["$secret"].clone();
    let res = r
        .redact_in(
            &writer(),
            "redact",
            RedactInput {
                targets: vec![secret.clone()],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(res.redaction.is_some(), "the request is recorded");
    let redact = r
        .propose(&writer(), "redact", "Remove the code", Some("Removes it."))
        .unwrap();
    let p = P {
        _dir: dir,
        r,
        secret,
        other: w.created["$o"].clone(),
        work,
        redact,
    };
    let q = queue(&p.r).expect("the queue reads");
    assert!(item(&q, &p.work.r#ref).is_some(), "{q:#?}");
    let removal = item(&q, &p.redact.r#ref).expect("the removal item");
    assert!(removal.card.contains("Content removal"), "{}", removal.card);
    p
}

fn queue(r: &Rdy) -> Result<Pending, RdyError> {
    r.read().pending(&PendingQuery {
        person: PERSON.into(),
        session_ref: None,
        max_tokens: CARD_MAX_TOKENS,
    })
}

fn item<'a>(q: &'a Pending, r#ref: &str) -> Option<&'a Item> {
    q.items.iter().find(|i| i.r#ref == r#ref)
}

fn answer(p: &mut P, r#ref: &str, option: &str) -> Result<answer::AnswerInfo, RdyError> {
    p.r.answer(
        &approver(),
        PERSON,
        AnswerRequest {
            r#ref: r#ref.into(),
            option: option.into(),
            card_hash: None,
            note: None,
            relay: RelayInput::default(),
            brief: None,
        },
    )
}

/// A write in another view that updates the secret: a change to an affected record.
fn touch_the_secret(p: &mut P) {
    let secret = p.secret.clone();
    write(
        &mut p.r,
        "touch",
        json!([{"op": "update", "id": secret, "body": "The code is rotated."}]),
    );
}

fn assert_stale(e: &RdyError, what: &str) {
    assert_eq!(e.code, E_REDACTION_STALE, "{what}: {e}");
    assert_eq!(e.message, STALE_MESSAGE, "{what}");
    assert_eq!(e.fix, STALE_FIX, "{what}");
}

#[test]
fn fr1_stale_removal_keeps_the_queue_readable() {
    let mut p = project();
    touch_the_secret(&mut p);
    let q = queue(&p.r).expect("a stale removal never fails the whole queue");
    assert!(item(&q, &p.redact.r#ref).is_some(), "{q:#?}");
    let work = item(&q, &p.work.r#ref).expect("the other item stays");
    assert!(work.card.contains("Notes"), "{}", work.card);
    assert_eq!(work.card_hash.len(), 64, "{work:#?}");
    // the counts and the next item read too
    let counts =
        p.r.read()
            .pending_counts(&PendingQuery {
                person: PERSON.into(),
                session_ref: None,
                max_tokens: CARD_MAX_TOKENS,
            })
            .unwrap();
    assert_eq!(counts.items, q.items.len());
    // the other item stays answerable: its approval lands
    let work_ref = p.work.r#ref.clone();
    answer(&mut p, &work_ref, "approve").expect("the other approval lands");
    let q = queue(&p.r).expect("the queue still reads");
    assert!(item(&q, &p.work.r#ref).is_none(), "{q:#?}");
    assert!(item(&q, &p.redact.r#ref).is_some(), "{q:#?}");
}

#[test]
fn fr2_stale_removal_card_refuses_with_the_fix() {
    let mut p = project();
    touch_the_secret(&mut p);
    let q = queue(&p.r).unwrap();
    // the item stays in the list, but its removal card is never shown
    let stale = item(&q, &p.redact.r#ref).expect("the stale item stays in the list");
    assert!(!stale.card.contains("Content removal"), "{}", stale.card);
    assert!(stale.card.contains(STALE_MESSAGE), "{}", stale.card);
    assert!(stale.card.contains(STALE_FIX), "{}", stale.card);
    // showing its card refuses
    let shown = p.r.read().proposal_show(&p.redact.id).unwrap();
    let row = shown.removal_card_error.expect("the card refuses");
    assert_eq!(row.code, E_REDACTION_STALE);
    assert_eq!(row.message, STALE_MESSAGE);
    assert_eq!(row.fix, STALE_FIX);
    assert!(shown.removal_card.is_none());
    // answering it refuses, whatever the option, and writes nothing
    let end = p.r.store().log_end();
    let redact_ref = p.redact.r#ref.clone();
    for option in ["approve", "reject", "later"] {
        let e = answer(&mut p, &redact_ref, option).unwrap_err();
        assert_stale(&e, option);
    }
    // a direct approval refuses
    let e = p.r.land(&approver(), &p.redact.id).unwrap_err();
    assert_stale(&e, "rdy approve");
    assert_eq!(p.r.store().log_end(), end, "nothing is written");
}

#[test]
fn fr3_unrelated_write_does_not_stale_a_removal() {
    let mut p = project();
    // mg5: a view in another doc updates a claim that the secret's change also wrote; nothing
    // touches the removal's target, and the write proposes nothing
    let other = p.other.clone();
    write(
        &mut p.r,
        "x",
        json!([{"op": "update", "id": other, "body": "O holds, now."}]),
    );
    let q = queue(&p.r).expect("the queue reads");
    let removal = item(&q, &p.redact.r#ref).expect("the removal item");
    assert!(removal.card.contains("Content removal"), "{}", removal.card);
    let redact_ref = p.redact.r#ref.clone();
    answer(&mut p, &redact_ref, "approve").expect("the removal lands");
    let canon = p.r.read().materialized(&Scope::Canon).unwrap();
    let secret = rdy_model::BlockId::parse(&p.secret).unwrap();
    assert_eq!(canon.get(&secret).unwrap().body, CONTENT_REMOVED);
}

#[test]
fn fr4_a_write_naming_the_removed_block_stales_it() {
    let mut p = project();
    touch_the_secret(&mut p);
    let q = queue(&p.r).expect("the queue reads");
    let stale = item(&q, &p.redact.r#ref).expect("the stale item stays in the list");
    assert!(!stale.card.contains("Content removal"), "{}", stale.card);
    let redact_ref = p.redact.r#ref.clone();
    let e = answer(&mut p, &redact_ref, "approve").unwrap_err();
    assert_stale(&e, "the approval");
    let e = p.r.land(&approver(), &p.redact.id).unwrap_err();
    assert_stale(&e, "rdy approve");
}
