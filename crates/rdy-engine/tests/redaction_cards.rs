//! rdx8: a proposal that carries a removal request is an individual approval card bound to the
//! complete report (D-CC90E: a person's approval, "never under a policy"; D-6B1W1: the report
//! before the proposal; D-14SAY, D-EPK1Z). The card shows each request's mandatory lines whole,
//! a small budget refuses rather than hides them, and the card hash binds the request, the report
//! hash and every count. A batch of recommended answers that names a removal card is refused as a
//! whole. These tests bind and inspect cards and refusals; none lands a removal.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::pending::{
    CARD_MAX_TOKENS, Item, ItemKind, PendingQuery, RemovalCard, card_hash, render, render_checked,
};
use rdy_engine::redaction::{RedactInput, RedactResult};
use rdy_engine::{
    DecisionInput, E_PROPOSAL_MOVED, E_REDACTION_BUDGET, E_REDACTION_HUMAN, ProposalInfo, Rdy,
    RdyError, Session, WriteRequest, WriteResult, pin_clock,
};
use rdy_model::redaction::{ReasonClass, encode_into};
use rdy_model::{AnswerAct, BlockId, Hash, Role};
use serde_json::json;
use std::path::Path;

const SECRET: &str = "The launch code is ZEBRA-7731.";
const PERSON: &str = "human:t";

const COPIES: &str = "This removes bytes from this store. It cannot erase exports, renders, dumps, clones, published files, working copies, backups, or filesystem snapshots.";
const INVENTORY: &str = "Dumps, clones, exports, renders, and published versions may hold copies. Their locations are unknown to rdy.";
const QUOTES: &str = "Quote matches use complete source strings. Partial quotes may still hold content. Snapshot issue lists and summaries, review findings, tension advice, and answer notes are not erased.";
const REUSE: &str = "This store will refuse these object and chunk hashes on later writes. This can block an identical restore or a file that shares a removed chunk.";
const QUARANTINE: &str = "Quarantine files may hold the content. Redaction does not remove them. A person can run rdy fsck --clean-quarantine.";
const ROTATE: &str = "Rotate or revoke the secret. Redaction does not undo its exposure.";
const IRREVERSIBLE: &str = "Approved content removal cannot be undone.";

fn writer() -> Session {
    Session::human(PERSON.into(), Role::Writer)
}

fn approver() -> Session {
    Session::human(PERSON.into(), Role::Approver)
}

fn agent() -> Session {
    Session::agent("agent:a".into(), Some("run-1".into()), Role::Writer).unwrap()
}

fn write(r: &mut Rdy, view: &str, ops: serde_json::Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap()
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A project whose view `work` holds a doc with a prose block holding `SECRET` and a term.
struct P {
    dir: tempfile::TempDir,
    r: Rdy,
    prose: BlockId,
    term: BlockId,
}

fn base() -> P {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "design", "title": "Design"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Doc", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": SECRET},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "A ledger."}
        ]),
    );
    P {
        prose: id(&w.created["$p"]),
        term: id(&w.created["$t"]),
        dir,
        r,
    }
}

/// Records a request for `target` in view `work` (the human writer's default view).
fn redact(p: &mut P, target: &BlockId, class: &str, reason: &str) -> RedactResult {
    let res =
        p.r.redact(
            &writer(),
            RedactInput {
                targets: vec![target.to_string()],
                reason_class: class.into(),
                reason: reason.into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(res.redaction.is_some(), "the request is recorded");
    res
}

fn propose(p: &mut P, view: &str, title: &str) -> ProposalInfo {
    p.r.propose(&writer(), view, title, Some("Removes the leaked code."))
        .unwrap()
}

fn queue(r: &Rdy, max_tokens: usize) -> Result<Vec<Item>, RdyError> {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens,
        })
        .map(|q| q.items)
}

fn item_of(r: &Rdy, r#ref: &str) -> Item {
    queue(r, CARD_MAX_TOKENS)
        .unwrap()
        .into_iter()
        .find(|i| i.r#ref == r#ref)
        .unwrap_or_else(|| panic!("{ref} is not in the queue"))
}

/// The card of `res` as its request and report give it.
fn expected_card(res: &RedactResult, quarantine: bool) -> RemovalCard {
    let rep = &res.report;
    RemovalCard {
        request: res.redaction.unwrap(),
        report: rep.report_hash,
        versions: rep.versions.len() as u64,
        blobs: rep.blobs.len() as u64,
        shared_blobs: rep.shared_blobs.len() as u64,
        payloads: rep.drop_frames,
        bytes: rep.drop_bytes,
        reason_class: res.reason_class,
        reason: res.reason.clone(),
        quarantine,
    }
}

/// The mandatory lines of one request, as the card shows them.
fn section(c: &RemovalCard) -> String {
    let class = match c.reason_class {
        ReasonClass::Secret => "secret",
        ReasonClass::Personal => "personal",
        ReasonClass::Legal => "legal",
        ReasonClass::Retraction => "retraction",
    };
    let mut lines = vec![
        "Content removal".to_string(),
        format!("  Request: {}", c.request),
        format!("  Report: {}", c.report),
        format!(
            "  Versions: {}; blobs: {}; shared blobs: {}.",
            c.versions, c.blobs, c.shared_blobs
        ),
        format!("  Payloads: {}; bytes: {}.", c.payloads, c.bytes),
        format!("  Reason class: {class}."),
        format!("  Reason: {}", c.reason),
        format!("  {COPIES}"),
        format!("  {QUOTES}"),
        format!("  {REUSE}"),
    ];
    if c.quarantine {
        lines.push(format!("  {QUARANTINE}"));
    }
    if c.reason_class == ReasonClass::Secret {
        lines.push(format!("  {ROTATE}"));
    }
    lines.join("\n")
}

/// The digest the plan fixes: the domain, the full card's UTF-8 bytes, then the postcard bytes
/// of the removal cards.
fn digest(item: &Item) -> Hash {
    let mut bytes = b"rdy-redaction-card-v1\0".to_vec();
    bytes.extend_from_slice(render(item, CARD_MAX_TOKENS).as_bytes());
    let mut buf = vec![0u8; 1 << 20];
    let removal = item.removal.as_ref().expect("a removal card");
    bytes.extend_from_slice(encode_into(removal, &mut buf).unwrap());
    Hash::of(&bytes)
}

fn answer(
    r: &mut Rdy,
    s: &Session,
    r#ref: &str,
    option: &str,
    card_hash: Option<String>,
    relay: RelayInput,
) -> Result<rdy_engine::answer::AnswerInfo, RdyError> {
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

fn relay() -> RelayInput {
    RelayInput {
        evidence: Some("No, do not remove it yet.".into()),
        ..RelayInput::default()
    }
}

/// A project with one recorded secret request for the prose block, proposed with its content.
fn removal_project() -> (P, RedactResult, ProposalInfo) {
    let mut p = base();
    let prose = p.prose.clone();
    let res = redact(&mut p, &prose, "secret", "  a leaked code  ");
    let prop = propose(&mut p, "work", "Remove the code");
    (p, res, prop)
}

#[test]
fn rdx8_local_and_relayed_cards_bind_the_complete_report() {
    let _clock = pin_clock(1_000_000);
    for relayed in [false, true] {
        let (mut p, res, prop) = removal_project();
        let item = item_of(&p.r, &prop.r#ref);
        // an individual approval item, bound to the request it carries
        assert_eq!(item.kind, ItemKind::Approve);
        assert_eq!(item.batch, None);
        let want = expected_card(&res, false);
        assert_eq!(item.removal, Some(vec![want.clone()]));
        assert_eq!(want.reason, "a leaked code");
        assert!(want.versions >= 1 && want.payloads >= 2, "{want:?}");
        // the exact mandatory lines, then the irreversible consequence on approve
        let text = section(&want);
        assert!(item.card.contains(&text), "{}\n---\n{text}", item.card);
        assert!(!item.card.contains(INVENTORY), "{}", item.card);
        let approve = item.options.iter().find(|o| o.id == "approve").unwrap();
        assert_eq!(
            approve.consequences,
            vec![
                "Lands the proposal in canon.".to_string(),
                IRREVERSIBLE.into()
            ]
        );
        assert_eq!(
            item.options
                .iter()
                .map(|o| o.id.as_str())
                .collect::<Vec<_>>(),
            ["approve", "reject", "later"]
        );
        assert!(
            item.card.contains(&format!(
                "     Lands the proposal in canon.\n     {IRREVERSIBLE}\n"
            )),
            "{}",
            item.card
        );
        assert!(
            item.card.find("Content removal").unwrap() < item.card.find("Options").unwrap(),
            "{}",
            item.card
        );
        // the card hash is the removal digest, never the plain card hash
        assert_eq!(item.card_hash, digest(&item).to_string());
        assert_eq!(card_hash(&item), digest(&item));
        let plain = Hash::of(render(&item, CARD_MAX_TOKENS).as_bytes());
        assert_ne!(card_hash(&item), plain);
        // the digest binds the request, the version, the report and each count
        let moved = |f: &dyn Fn(&mut Item)| {
            let mut other = item.clone();
            f(&mut other);
            card_hash(&other)
        };
        let h = card_hash(&item);
        assert_ne!(
            moved(&|i| i.removal.as_mut().unwrap()[0].request = Hash([7; 32])),
            h
        );
        assert_ne!(
            moved(&|i| i.removal.as_mut().unwrap()[0].report = Hash([7; 32])),
            h
        );
        assert_ne!(moved(&|i| i.removal.as_mut().unwrap()[0].bytes += 1), h);
        assert_ne!(
            moved(&|i| i.removal.as_mut().unwrap()[0].shared_blobs += 1),
            h
        );
        assert_ne!(moved(&|i| i.r#ref = i.r#ref.replace("@1", "@2")), h);
        // the request record holds the manifest whose hash the card binds
        let st = p.r.read();
        let req = st.reader().redaction_request(&want.request).unwrap();
        let mut buf = vec![0u8; rdy_model::redaction::MAX_RECORD_BYTES];
        assert_eq!(req.manifest.report_hash(&mut buf).unwrap(), want.report);
        drop(st);

        let (who, rel) = if relayed {
            (agent(), relay())
        } else {
            (approver(), RelayInput::default())
        };
        let end = p.r.store().log_end();
        // a card shown without the removal digest, or with another report, does not bind
        for stale in [
            plain,
            moved(&|i| i.removal.as_mut().unwrap()[0].report = Hash([7; 32])),
        ] {
            for option in ["approve", "reject"] {
                let e = answer(
                    &mut p.r,
                    &who,
                    &prop.r#ref,
                    option,
                    Some(stale.to_string()),
                    rel.clone(),
                )
                .unwrap_err();
                assert_eq!(e.code, E_PROPOSAL_MOVED, "{e}");
                assert_eq!(p.r.store().log_end(), end, "a refusal writes nothing");
            }
        }
        // the card shown binds: a rejection records the removal digest it answered
        let info = answer(
            &mut p.r,
            &who,
            &prop.r#ref,
            "reject",
            Some(item.card_hash.clone()),
            rel.clone(),
        )
        .unwrap();
        assert_eq!(info.act, "reject_proposal");
        assert_eq!(info.assurance, if relayed { "relayed" } else { "local" });
        let (_, a) = p.r.read().reader().answer_of(&prop.r#ref).unwrap().unwrap();
        assert_eq!(a.act, AnswerAct::RejectProposal);
        assert_eq!(a.card_hash, Some(card_hash(&item)));
        assert_eq!(a.person.actor, PERSON);
        assert_eq!(
            a.relayed_by.map(|b| b.actor),
            relayed.then(|| "agent:a".to_string())
        );
        // no landing: the request stays a request
        assert!(p.r.read().canon_head().is_none());
    }
}

/// A project with three open proposals: `removal` carries a request; `decided` holds a one_way
/// decision whose confirm stands beside it; `plain` is ordinary content.
struct Mixed {
    p: P,
    removal: String,
    confirm: String,
    plain: String,
}

fn mixed() -> Mixed {
    let (mut p, _, prop) = removal_project();
    let w = write(
        &mut p.r,
        "decided",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C holds."}
        ]),
    );
    p.r.decide(
        &writer(),
        "decided",
        DecisionInput {
            parent: w.created["$s"].clone(),
            statement: "Never drop C".into(),
            reversibility: Some("one_way".into()),
            affects: vec![w.created["$c"].clone()],
            ..Default::default()
        },
    )
    .unwrap();
    let decided = propose(&mut p, "decided", "Rules");
    write(
        &mut p.r,
        "plain",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Notes"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "N holds."}
        ]),
    );
    let plain = propose(&mut p, "plain", "Notes");
    let items = queue(&p.r, CARD_MAX_TOKENS).unwrap();
    let confirm = items
        .iter()
        .find(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(decided.r#ref.as_str()))
        .expect("the one_way decision stands beside its batch")
        .r#ref
        .clone();
    for r in [&prop.r#ref, &confirm, &plain.r#ref] {
        let i = items.iter().find(|i| &i.r#ref == r).expect("in the queue");
        assert!(i.options.iter().any(|o| o.recommended.is_some()), "{r}");
        assert_eq!(i.removal.is_some(), r == &prop.r#ref, "{r}");
    }
    Mixed {
        p,
        removal: prop.r#ref,
        confirm,
        plain: plain.r#ref,
    }
}

#[test]
fn rdx8_bulk_removal_refuses_the_whole_call() {
    let _clock = pin_clock(1_000_000);
    let mut m = mixed();
    let orders = [
        vec![m.removal.clone(), m.confirm.clone(), m.plain.clone()],
        vec![m.confirm.clone(), m.removal.clone(), m.plain.clone()],
        vec![m.confirm.clone(), m.plain.clone(), m.removal.clone()],
    ];
    for refs in orders {
        let end = m.p.r.store().log_end();
        let head = m.p.r.read().canon_head();
        for (who, rel) in [(approver(), RelayInput::default()), (agent(), relay())] {
            let e =
                m.p.r
                    .answer_all_recommended(&who, PERSON, &refs, &[], rel)
                    .unwrap_err();
            assert_eq!(e.code, E_REDACTION_HUMAN, "{refs:?}: {e}");
            assert_eq!(e.message, "Content removal needs a person's approval.");
            assert_eq!(
                e.fix,
                "Show the redaction card and relay the person's answer, or approve from the CLI as a human."
            );
            // nothing was answered or landed before the refusal
            let done = e
                .corrected
                .as_ref()
                .and_then(|c| c.get("done"))
                .and_then(|d| d.as_array())
                .map_or(0, Vec::len);
            assert_eq!(done, 0, "{refs:?}");
            assert_eq!(m.p.r.store().log_end(), end, "{refs:?}: nothing written");
            assert_eq!(m.p.r.read().canon_head(), head, "{refs:?}: no landing");
            assert!(
                m.p.r
                    .read()
                    .reader()
                    .answer_of(&m.confirm)
                    .unwrap()
                    .is_none(),
                "{refs:?}"
            );
        }
    }
    // the removal card itself is still waiting for its own answer
    let item = item_of(&m.p.r, &m.removal);
    assert!(item.removal.is_some());
    // a call naming no removal keeps its ordered, stop-on-first-refusal behaviour (p-E975A)
    let done =
        m.p.r
            .answer_all_recommended(
                &approver(),
                PERSON,
                &[m.confirm.clone(), m.plain.clone()],
                &[],
                RelayInput::default(),
            )
            .unwrap();
    assert_eq!(
        done.iter().map(|d| d.act.as_str()).collect::<Vec<_>>(),
        ["confirm", "approve"]
    );
}

#[test]
fn rdx8_a_small_budget_cannot_hide_removal() {
    let _clock = pin_clock(1_000_000);
    let (p, res, prop) = removal_project();
    let item = item_of(&p.r, &prop.r#ref);
    let text = section(&expected_card(&res, false));
    let full = render(&item, CARD_MAX_TOKENS);
    assert!(full.contains(&text));
    // the smallest card keeps every mandatory line and cuts only facts and decisions
    let smallest = render(&item, 0);
    assert!(smallest.contains(&text), "{smallest}");
    assert!(smallest.contains(IRREVERSIBLE), "{smallest}");
    assert!(smallest.contains("facts cut"), "{smallest}");
    let min = smallest.len() / 4;
    let card = render_checked(&item, min).unwrap();
    assert!(card.len() / 4 <= min);
    assert!(card.contains(&text), "{card}");
    assert!(card.contains(IRREVERSIBLE), "{card}");
    for small in [0, 1, min - 1] {
        let e = render_checked(&item, small).unwrap_err();
        assert_eq!(e.code, E_REDACTION_BUDGET, "{small}");
        assert_eq!(
            e.message,
            "The output budget is too small for the redaction warning."
        );
        assert_eq!(
            e.fix,
            "Increase max_tokens, or propose fewer redaction requests at once."
        );
    }
    // a sufficient budget keeps every mandatory line; the whole budget gives the full card
    assert_eq!(render_checked(&item, CARD_MAX_TOKENS).unwrap(), full);
    // the queue refuses rather than hides: a budget below the mandatory lines is an error
    let e = queue(&p.r, min - 1).unwrap_err();
    assert_eq!(e.code, E_REDACTION_BUDGET);
    let shown = queue(&p.r, min).unwrap();
    let shown = shown.iter().find(|i| i.r#ref == prop.r#ref).unwrap();
    assert!(shown.card.contains(&text), "{}", shown.card);
    // the budget changes the card shown, never the hash it binds
    assert_eq!(shown.card_hash, item.card_hash);
}

#[test]
fn rdx8_ordinary_cards_keep_their_bytes() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let prop = propose(&mut p, "work", "Doc");
    let item = item_of(&p.r, &prop.r#ref);
    assert_eq!(item.removal, None);
    // no removal key on the wire, and the plain card hash
    let v = serde_json::to_value(&item).unwrap();
    assert!(v.as_object().unwrap().get("removal").is_none(), "{v}");
    assert_eq!(
        card_hash(&item),
        Hash::of(render(&item, CARD_MAX_TOKENS).as_bytes())
    );
    assert_eq!(item.card_hash, card_hash(&item).to_string());
    assert!(!item.card.contains("Content removal"));
    let approve = item.options.iter().find(|o| o.id == "approve").unwrap();
    assert_eq!(
        approve.consequences,
        vec!["Lands the proposal in canon.".to_string()]
    );
    // an ordinary card may stay over a small budget, as before: render_checked never refuses it
    for max in [0, 1, 50, CARD_MAX_TOKENS] {
        assert_eq!(render_checked(&item, max).unwrap(), render(&item, max));
    }
    // the same item with a removal card gains the section, the key and the digest
    let (q, res, rprop) = removal_project();
    let removal = item_of(&q.r, &rprop.r#ref);
    let mut stripped = removal.clone();
    stripped.removal = None;
    assert!(!render(&stripped, CARD_MAX_TOKENS).contains("Content removal"));
    assert_eq!(
        card_hash(&stripped),
        Hash::of(render(&stripped, CARD_MAX_TOKENS).as_bytes())
    );
    let v = serde_json::to_value(&removal).unwrap();
    let card = &v["removal"][0];
    assert_eq!(card["request"], res.redaction.unwrap().to_string());
    assert_eq!(card["report"], res.report.report_hash.to_string());
    assert_eq!(card["reason_class"], "secret");
    assert_eq!(card["quarantine"], false);
    let keys: Vec<&str> = card
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut want = vec![
        "request",
        "report",
        "versions",
        "blobs",
        "shared_blobs",
        "payloads",
        "bytes",
        "reason_class",
        "reason",
        "quarantine",
    ];
    want.sort();
    let mut got = keys.clone();
    got.sort();
    assert_eq!(got, want);
    // the card round-trips and refuses unknown keys
    let back: RemovalCard = serde_json::from_value(card.clone()).unwrap();
    assert_eq!(Some(vec![back]), removal.removal);
    let mut extra = card.clone();
    extra["deleted_quarantine"] = json!(1);
    assert!(serde_json::from_value::<RemovalCard>(extra).is_err());
}

fn quarantine_file(root: &Path) {
    let store = root.join(".rdy/store");
    std::fs::write(store.join("index.redb.damaged-1"), b"x").unwrap();
}

#[test]
fn rdx8_each_request_has_its_own_sorted_facts() {
    let _clock = pin_clock(1_000_000);
    let mut p = base();
    let (prose, term) = (p.prose.clone(), p.term.clone());
    let a = redact(&mut p, &prose, "secret", "a leaked code");
    quarantine_file(p.dir.path());
    let b = redact(&mut p, &term, "personal", "a name");
    let prop = propose(&mut p, "work", "Remove two things");
    let item = item_of(&p.r, &prop.r#ref);
    let mut want = vec![expected_card(&a, false), expected_card(&b, true)];
    want.sort_by_key(|c| c.request);
    assert_ne!(want[0].request, want[1].request);
    assert_eq!(item.removal.as_ref(), Some(&want));
    // one section per request, in request-hash order, each with its own warnings
    let first = item.card.find(&section(&want[0])).expect("first section");
    let second = item.card.find(&section(&want[1])).expect("second section");
    assert!(first < second, "{}", item.card);
    assert_eq!(item.card.matches("Content removal").count(), 2);
    assert_eq!(item.card.matches(COPIES).count(), 2);
    assert_eq!(item.card.matches(QUOTES).count(), 2);
    assert_eq!(item.card.matches(REUSE).count(), 2);
    assert_eq!(item.card.matches(ROTATE).count(), 1);
    assert_eq!(item.card.matches(QUARANTINE).count(), 1);
    let secret = if want[0].reason_class == ReasonClass::Secret {
        &section(&want[0])
    } else {
        &section(&want[1])
    };
    assert!(secret.contains(ROTATE) && !secret.contains(QUARANTINE));
    // deterministic: the same state gives the same card and digest, and so does a reopen
    let again = item_of(&p.r, &prop.r#ref);
    assert_eq!(again, item);
    assert_eq!(item.card_hash, digest(&item).to_string());
    let P { dir, r, .. } = p;
    r.close().unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    let reopened = item_of(&r, &prop.r#ref);
    assert_eq!(reopened.card, item.card);
    assert_eq!(reopened.card_hash, item.card_hash);
    // swapping the two requests' facts moves the digest
    let mut swapped = item.clone();
    swapped.removal.as_mut().unwrap().swap(0, 1);
    assert_ne!(card_hash(&swapped), card_hash(&item));
}
