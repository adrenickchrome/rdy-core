//! rpl20 (repo links, D-8VJAZ "adopt cards", D-MEMMQ, C14): the answer to an adopt card. The card
//! stands in the queue now, and its answer is the approval of its own isolated landing.
//!
//! - `adopt` writes one change and one landing in the reserved view `adopt-<card hash 12>`, with
//!   the person as approver and the card as evidence, and the person's Adopt answer in the same
//!   batch. The change moves exactly the pins the card bound (an accepted decision's too, under
//!   the adoption's own frozen-decision exception) and the instruction baseline pins of the
//!   repo_link block. Nothing else moves.
//! - `keep` writes one KeepPin answer with the person's reason, and moves no pin.
//! - A refused answer writes nothing: no log byte, ref, view, proposal, answer or landing, and the
//!   card stays open with its hash.
//! - The adoption's change carries no authority of its own: replayed into another view, it is
//!   frozen.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::foreign_delivery::{DeliveryTaken, take_delivery};
use rdy_engine::pending::adopt::{ADOPT_LABEL, KEEP_PIN_LABEL};
use rdy_engine::pending::{Item, ItemKind, PlanCardBinding};
use rdy_engine::{
    DecisionInput, E_BAD_INPUT, E_ITEM_MOVED, Rdy, RdyError, Scope, Session, WriteResult,
};
use rdy_model::foreign::{
    ForeignDerived, ForeignId, ForeignPin, ForeignPublication, ForeignSet, ForeignSource,
    ForeignVersion, selection_identity,
};
use rdy_model::{
    AnswerAct, ApprovalEvidence, Assurance, Block, BlockId, Change, EvidenceKind, FieldValue, Hash,
    LandingRecord, Op, Role,
};
use rdy_qa::fixtures::{agent, approver, land, write, writer};
use rdy_qa::intake::{PERSON, pending};
use rdy_store::codec;
use rdy_store::format::baseline_features;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::segment::Location;
use rdy_store::sync_stream::{DeliveryRecord, StreamHeader, StreamRecord, StreamWriter};
use rdy_store::view::ViewOpKind;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

const PROFILE: &str = "pf-AGNT1";

fn cited() -> RepoId {
    RepoId([7; 16])
}

use rdy_model::RepoId;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn created(w: &WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).unwrap()
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
}

/// A cited block, as the cited side delivers it.
fn block(id_: &str, ty: &str, body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: ty.into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::from([("title".to_string(), FieldValue::Str(format!("Title {id_}")))]),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

/// A link delivery stream of the cited repo holding `selected`, written to `dir/name`.
fn stream(dir: &Path, name: &str, selected: &[&Block]) -> File {
    let mut selected = selected.to_vec();
    selected.sort_by(|a, b| a.id.cmp(&b.id));
    let versions: Vec<ForeignVersion> = selected
        .iter()
        .map(|b| ForeignVersion {
            id: b.id.clone(),
            version: b.version_hash(),
            content: b.content_hash(),
            encoding: 0,
            derived: ForeignDerived::default(),
        })
        .collect();
    let delivery = DeliveryRecord {
        publication: ForeignPublication {
            tag: name.into(),
            profile: PROFILE.into(),
            profile_version: None,
        },
        selection: selection_identity(versions.iter().map(|v| (&v.id, &v.version))).unwrap(),
        versions,
        shells: vec![],
    };
    let header =
        StreamHeader::link_delivery(baseline_features(), cited(), Hash::of(name.as_bytes()));
    let mut w = StreamWriter::new_delivery(Vec::new(), &header, &delivery).unwrap();
    for b in &selected {
        let o = codec::encode_new(kinds::BLOCK, *b);
        w.record(&StreamRecord::Present {
            key: ObjectKey {
                kind: kinds::BLOCK,
                hash: Hash::of(&o.payload),
            },
            encoding: o.version,
            payload: o.payload,
        })
        .unwrap();
    }
    let path = dir.join(name);
    std::fs::write(&path, w.finish().unwrap().0).unwrap();
    File::open(path).unwrap()
}

/// Takes the delivery of `selected` for link `link`, of the set `set`.
fn deliver_in(
    r: &mut Rdy,
    dir: &Path,
    name: &str,
    link: &BlockId,
    set: Option<ForeignSet>,
    selected: &[&Block],
) -> DeliveryTaken {
    let source = ForeignSource::Link {
        link: link.to_string(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
    };
    take_delivery(r, &stream(dir, name, selected), &source, set)
        .unwrap_or_else(|e| panic!("take {name}: {e:?}"))
}

fn deliver(r: &mut Rdy, dir: &Path, name: &str, link: &BlockId, selected: &[&Block]) {
    deliver_in(r, dir, name, link, None, selected);
}

fn claim_c(body: &str) -> Block {
    block("c-FRGNC", "claim", body)
}

fn claim_e() -> Block {
    block("c-FRGNE", "claim", "Queues hold at most ten thousand jobs.")
}

/// A landed draft doc with a section and the repo link `core` to the cited repo; returns the
/// section and the link.
fn linked(r: &mut Rdy) -> (BlockId, BlockId) {
    let w = put(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"title": "Guide", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d",
             "fields": {"title": "Rules"}},
            {"op": "create", "type": "repo_link", "id": "$l", "parent": "$d",
             "fields": {"alias": "core", "repo": cited().to_string(), "profile": PROFILE}}
        ]),
    );
    land(r, "seed").unwrap();
    (created(&w, "$s"), created(&w, "$l"))
}

/// A project after the first delivery, where the landed claim `x` and the accepted decision `d`
/// cite the claim `c-FRGNC`.
struct Citing {
    _d: tempfile::TempDir,
    files: tempfile::TempDir,
    r: Rdy,
    link: BlockId,
    x: BlockId,
    d: BlockId,
}

fn citing() -> Citing {
    let (d, mut r) = rdy_qa::fixtures::project();
    let files = tempfile::tempdir().unwrap();
    let (s, link) = linked(&mut r);
    deliver(
        &mut r,
        files.path(),
        "v1",
        &link,
        &[&claim_c("Every service logs in UTC."), &claim_e()],
    );
    let w = put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$x", "parent": s.to_string(),
                "body": "Our billing jobs follow {{core:c-FRGNC}}."}]),
    );
    land(&mut r, "a").unwrap();
    let decided = r
        .decide(
            &writer(),
            "dec",
            DecisionInput {
                parent: s.to_string(),
                statement: "Every job logs as {{core:c-FRGNC}} says.".into(),
                title: Some("Log time".into()),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("decide: {e:?}"));
    let dec = BlockId::parse(&decided.created["$decision"]).unwrap();
    land(&mut r, "dec").unwrap();
    let st = r.read();
    let b = st.block(&Scope::Canon, &dec).unwrap();
    assert!(
        rdy_model::decision::is_frozen(&b),
        "the decision is accepted"
    );
    drop(st);
    Citing {
        _d: d,
        files,
        r,
        link,
        x: created(&w, "$x"),
        d: dec,
    }
}

/// The delivery that changes the cited claim.
fn change_claim(c: &mut Citing, name: &str, body: &str) -> Block {
    let c2 = claim_c(body);
    deliver(&mut c.r, c.files.path(), name, &c.link, &[&c2, &claim_e()]);
    c2
}

fn target() -> ForeignId {
    ForeignId::new(cited(), id("c-FRGNC"))
}

fn pin_of(b: &Block) -> ForeignPin {
    ForeignPin {
        content: b.content_hash(),
        encoding: 0,
    }
}

/// The foreign pins of block `b` in canon.
fn pins_of(r: &Rdy, b: &BlockId) -> BTreeMap<ForeignId, ForeignPin> {
    r.read().block(&Scope::Canon, b).unwrap().foreign.pins
}

/// The adopt cards of the queue.
fn adopt_items(r: &Rdy) -> Vec<Item> {
    pending(r)
        .items
        .into_iter()
        .filter(|i| i.kind == ItemKind::Adopt)
        .collect()
}

fn the_card(r: &Rdy) -> Item {
    let mut all = adopt_items(r);
    assert_eq!(all.len(), 1, "{all:#?}");
    all.remove(0)
}

fn log_end(r: &Rdy) -> Location {
    r.store().log_end()
}

/// The adopt views of the project.
fn adopt_views(r: &Rdy) -> Vec<String> {
    r.store()
        .refs_with_prefix("view/adopt-")
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

fn answer_as(
    r: &mut Rdy,
    s: &Session,
    item: &str,
    option: &str,
    note: Option<&str>,
    shown: Option<&str>,
    relay: RelayInput,
) -> Result<Value, RdyError> {
    r.answer(
        s,
        PERSON,
        AnswerRequest {
            r#ref: item.into(),
            option: option.into(),
            card_hash: shown.map(str::to_string),
            note: note.map(str::to_string),
            relay,
            ..AnswerRequest::default()
        },
    )
    .map(|i| serde_json::to_value(i).unwrap())
}

/// A local answer by the person.
fn answer(
    r: &mut Rdy,
    item: &str,
    option: &str,
    note: Option<&str>,
    shown: Option<&str>,
) -> Result<Value, RdyError> {
    answer_as(
        r,
        &approver(),
        item,
        option,
        note,
        shown,
        RelayInput::default(),
    )
}

/// A relay by `agent:relay` of the person's words, at the command line.
fn relay(kind: EvidenceKind) -> RelayInput {
    RelayInput {
        relayed_by: Some("agent:relay".into()),
        evidence: Some("Yes, adopt it.".into()),
        question: Some("Adopt the delivery from core?".into()),
        session_ref: Some("chat-1".into()),
        kind: Some(kind),
    }
}

fn relayer() -> Session {
    Session::agent("agent:relay".into(), Some("r1".into()), Role::Writer).unwrap()
}

fn labels(i: &Item) -> Vec<String> {
    i.options.iter().map(|o| o.label.clone()).collect()
}

/// Asserts that nothing was written since `end` and the card stands with hash `hash`.
fn nothing_written(r: &Rdy, end: Location, hash: &str, what: &str) {
    assert_eq!(log_end(r), end, "{what}: nothing written");
    assert!(adopt_views(r).is_empty(), "{what}: no adopt view");
    let c = the_card(r);
    assert_eq!(c.card_hash, hash, "{what}: the same card stays open");
    assert!(
        r.read().reader().answer_of(&c.r#ref).unwrap().is_none(),
        "{what}: no answer"
    );
}

#[test]
fn rpl20_adopt_lands_once_in_reserved_view() {
    let mut c = citing();
    let before_x = pins_of(&c.r, &c.x);
    let v1 = claim_c("Every service logs in UTC.");
    assert_eq!(before_x[&target()], pin_of(&v1));
    assert!(adopt_items(&c.r).is_empty(), "no change, no card");
    let c2 = change_claim(&mut c, "v2", "Every service logs in local time.");
    // the card stands in the queue now
    let card = the_card(&c.r);
    assert!(card.r#ref.starts_with("adopt:"), "{}", card.r#ref);
    let canon_before = c.r.read().materialized(&Scope::Canon).unwrap();
    let v = answer(&mut c.r, &card.r#ref, "adopt", None, Some(&card.card_hash)).unwrap();
    assert_eq!(
        (
            v["act"].as_str(),
            v["assurance"].as_str(),
            v["option"].as_str(),
            v["label"].as_str()
        ),
        (
            Some("adopt"),
            Some("local"),
            Some("adopt"),
            Some(ADOPT_LABEL)
        ),
        "{v}"
    );
    let landing: Hash = v["landing"]["landing"].as_str().unwrap().parse().unwrap();
    let answer_id: Hash = v["answer"].as_str().unwrap().parse().unwrap();
    assert_eq!(c.r.read().canon_head(), Some(landing));
    // one batch: the view's create, one change, the proposal, the landing, the close, the answer
    let batch = c.r.store().last_batch().to_vec();
    let view = format!("adopt-{}", &card.card_hash[..12]);
    let count = |k: u8| batch.iter().filter(|(kind, _)| *kind == k).count();
    assert_eq!(count(kinds::CHANGE), 1, "one change: {batch:?}");
    assert_eq!(count(kinds::LANDING), 1, "one landing");
    assert_eq!(count(kinds::ANSWER), 1, "one answer");
    let op_of = |h: &Hash| c.r.store().view_op(h).unwrap();
    let at = |pred: &dyn Fn(u8, &Hash) -> bool| {
        batch
            .iter()
            .position(|(k, h)| pred(*k, h))
            .unwrap_or_else(|| panic!("{batch:?}"))
    };
    let create = at(&|k, h| {
        k == kinds::VIEW_OP
            && op_of(h).view == view
            && matches!(op_of(h).op, ViewOpKind::Create { .. })
    });
    let change_at = at(&|k, _| k == kinds::CHANGE);
    let landing_at = at(&|k, h| k == kinds::LANDING && *h == landing);
    let close = at(&|k, h| {
        k == kinds::VIEW_OP
            && op_of(h).view == view
            && matches!(op_of(h).op, ViewOpKind::Close { .. })
    });
    let answer_at = at(&|k, h| k == kinds::ANSWER && *h == answer_id);
    assert!(
        create < change_at && change_at < landing_at && landing_at < close && close < answer_at,
        "{batch:?}"
    );
    // the landing: the person, local, the card as evidence, in the reserved view
    let rec: LandingRecord = c.r.store().landing(&landing).unwrap();
    assert_eq!(rec.approved_by.actor, PERSON);
    assert_eq!(rec.assurance, Assurance::Local);
    assert_eq!(
        rec.evidence,
        Some(ApprovalEvidence {
            kind: EvidenceKind::Card,
            options: labels(&card),
            choice: Some(ADOPT_LABEL.into()),
            card_hash: Some(card.card_hash.parse().unwrap()),
        })
    );
    let prop = c.r.store().proposal(&rec.proposal).unwrap();
    assert_eq!(
        (
            prop.view.as_str(),
            prop.title.as_str(),
            prop.summary.as_deref()
        ),
        (view.as_str(), card.title.as_str(), card.summary.as_deref())
    );
    assert_eq!(adopt_views(&c.r), vec![format!("view/{view}")]);
    assert!(c.r.read().view_head(&view).unwrap().closed);
    // the answer: the person's Adopt of this card, naming its landing
    let (h, a) = c.r.read().reader().answer_of(&card.r#ref).unwrap().unwrap();
    assert_eq!(h, answer_id);
    assert_eq!(
        (a.act, a.option.as_deref(), a.label.as_deref()),
        (AnswerAct::Adopt, Some("adopt"), Some(ADOPT_LABEL))
    );
    assert_eq!(a.person.actor, PERSON);
    assert_eq!(a.card_hash, Some(card.card_hash.parse().unwrap()));
    assert_eq!(a.targets, vec![landing]);
    assert_eq!(a.options, labels(&card));
    // the change moves exactly the card's pins: the claim and the decision, nothing else
    let ch: Change = c.r.store().change(&rec.changes[0]).unwrap();
    let mut written: Vec<BlockId> = ch.ops.iter().map(|o| o.block_id().clone()).collect();
    written.sort();
    let mut want = vec![c.x.clone(), c.d.clone()];
    want.sort();
    assert_eq!(written, want);
    for op in &ch.ops {
        let Op::Update { base, block } = op else {
            panic!("{op:?}")
        };
        let old = canon_before.get(&block.id).unwrap();
        assert_eq!(*base, old.version_hash());
        let moves = BTreeMap::from([(target(), (pin_of(&v1), pin_of(&c2)))]);
        assert!(
            rdy_model::decision::only_foreign_pins_adopted(old, block, &moves),
            "{}: only the pin moves",
            block.id
        );
    }
    assert_eq!(pins_of(&c.r, &c.x)[&target()], pin_of(&c2));
    assert_eq!(pins_of(&c.r, &c.d)[&target()], pin_of(&c2));
    // the card is gone, and an answer to it again is refused with no write
    assert!(
        adopt_items(&c.r).is_empty(),
        "the adoption settles the card"
    );
    let end = log_end(&c.r);
    let e = answer(&mut c.r, &card.r#ref, "adopt", None, Some(&card.card_hash)).unwrap_err();
    assert_eq!(e.code, E_ITEM_MOVED, "{e:?}");
    assert_eq!(log_end(&c.r), end, "it lands once");
    c.r.fsck().unwrap();
}

#[test]
fn rpl20_relay_uses_exact_adopt_card_evidence() {
    let mut c = citing();
    change_claim(&mut c, "v2", "Every service logs in local time.");
    let card = the_card(&c.r);
    // a relay names the card it showed
    let end = log_end(&c.r);
    let e = answer_as(
        &mut c.r,
        &relayer(),
        &card.r#ref,
        "adopt",
        None,
        None,
        relay(EvidenceKind::Card),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_BAD_INPUT,
            "a relayed answer names the card the person was shown"
        )
    );
    nothing_written(&c.r, end, &card.card_hash, "no card hash");
    // a relay whose words are its evidence records no card: refused, as a landing needs one
    let e = answer_as(
        &mut c.r,
        &relayer(),
        &card.r#ref,
        "adopt",
        None,
        Some(&card.card_hash),
        relay(EvidenceKind::Text),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_BAD_INPUT,
            "an adopt card's landing records card evidence, not text"
        )
    );
    nothing_written(&c.r, end, &card.card_hash, "text evidence");
    // the exact card: the landing records the relay, the person and the card
    let v = answer_as(
        &mut c.r,
        &relayer(),
        &card.r#ref,
        "adopt",
        None,
        Some(&card.card_hash),
        relay(EvidenceKind::Card),
    )
    .unwrap();
    assert_eq!(v["assurance"], "relayed", "{v}");
    let landing: Hash = v["landing"]["landing"].as_str().unwrap().parse().unwrap();
    let rec: LandingRecord = c.r.store().landing(&landing).unwrap();
    assert_eq!(rec.assurance, Assurance::Relayed);
    assert_eq!(rec.approved_by.actor, PERSON);
    let by = rec.relayed_by.as_ref().unwrap();
    assert_eq!(
        (by.actor.as_str(), by.run.as_deref()),
        ("agent:relay", Some("r1"))
    );
    assert_eq!(
        rec.relay_evidence.as_ref().unwrap().quoted,
        "Yes, adopt it."
    );
    assert_eq!(
        rec.evidence,
        Some(ApprovalEvidence {
            kind: EvidenceKind::Card,
            options: labels(&card),
            choice: Some(ADOPT_LABEL.into()),
            card_hash: Some(card.card_hash.parse().unwrap()),
        })
    );
    let (_, a) = c.r.read().reader().answer_of(&card.r#ref).unwrap().unwrap();
    assert_eq!(a.act, AnswerAct::Adopt);
    assert_eq!(a.person.actor, PERSON);
    assert_eq!(a.relayed_by.as_ref().unwrap().actor, "agent:relay");
    assert_eq!(a.evidence_kind, Some(EvidenceKind::Card));
    assert_eq!(a.card_hash, Some(card.card_hash.parse().unwrap()));
    assert_eq!(a.targets, vec![landing]);
    c.r.fsck().unwrap();
}

#[test]
fn rpl20_failure_writes_nothing_and_keeps_card_hash() {
    let mut c = citing();
    let before = (pins_of(&c.r, &c.x), pins_of(&c.r, &c.d));
    change_claim(&mut c, "v2", "Every service logs in local time.");
    let card = the_card(&c.r);
    let end = log_end(&c.r);
    let canon = c.r.read().canon_head();
    // a card the person did not see: the fresh card comes back, nothing is written
    let stale = Hash::of(b"another card").to_string();
    let e = answer(&mut c.r, &card.r#ref, "adopt", None, Some(&stale)).unwrap_err();
    assert_eq!(e.code, E_ITEM_MOVED, "{e:?}");
    assert_eq!(
        e.corrected.as_ref().unwrap()["card_hash"].as_str(),
        Some(card.card_hash.as_str()),
        "the refusal carries the open card"
    );
    nothing_written(&c.r, end, &card.card_hash, "a stale card");
    // an option the card does not offer
    let e = answer(
        &mut c.r,
        &card.r#ref,
        "approve",
        None,
        Some(&card.card_hash),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e:?}");
    nothing_written(&c.r, end, &card.card_hash, "an unknown option");
    // a keep with no reason
    let e = answer(&mut c.r, &card.r#ref, "keep", None, Some(&card.card_hash)).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_BAD_INPUT,
            "Keep the approved pins needs a note",
            "pass note with a reason of 1 to 2000 characters"
        )
    );
    nothing_written(&c.r, end, &card.card_hash, "a keep with no reason");
    // a newer delivery moves the card: the old one is refused with the fresh card
    let c3 = change_claim(&mut c, "v3", "Every service logs in GMT.");
    let fresh = the_card(&c.r);
    assert_ne!(fresh.r#ref, card.r#ref);
    let end = log_end(&c.r);
    let e = answer(&mut c.r, &card.r#ref, "adopt", None, Some(&card.card_hash)).unwrap_err();
    assert_eq!(e.code, E_ITEM_MOVED, "{e:?}");
    nothing_written(&c.r, end, &fresh.card_hash, "a moved card");
    assert_eq!(c.r.read().canon_head(), canon, "canon never moved");
    assert_eq!((pins_of(&c.r, &c.x), pins_of(&c.r, &c.d)), before);
    // the fresh card still adopts, to the newest delivery
    answer(
        &mut c.r,
        &fresh.r#ref,
        "adopt",
        None,
        Some(&fresh.card_hash),
    )
    .unwrap();
    assert_eq!(pins_of(&c.r, &c.d)[&target()], pin_of(&c3));
    c.r.fsck().unwrap();
}

#[test]
fn rpl20_keep_records_reason_without_pin_change() {
    let mut c = citing();
    let before = (pins_of(&c.r, &c.x), pins_of(&c.r, &c.d));
    change_claim(&mut c, "v2", "Every service logs in local time.");
    let card = the_card(&c.r);
    let canon = c.r.read().canon_head();
    let v = answer(
        &mut c.r,
        &card.r#ref,
        "keep",
        Some("  Billing stays on UTC until the audit.  "),
        Some(&card.card_hash),
    )
    .unwrap();
    assert_eq!(
        (v["act"].as_str(), v["option"].as_str(), v["label"].as_str()),
        (Some("keep_pin"), Some("keep"), Some(KEEP_PIN_LABEL)),
        "{v}"
    );
    assert!(v["landing"].is_null(), "a keep lands nothing: {v}");
    let h: Hash = v["answer"].as_str().unwrap().parse().unwrap();
    let batch = c.r.store().last_batch().to_vec();
    assert_eq!(
        batch.iter().filter(|(k, _)| *k == kinds::ANSWER).count(),
        1,
        "{batch:?}"
    );
    assert!(
        batch
            .iter()
            .all(|(k, _)| ![kinds::CHANGE, kinds::LANDING, kinds::PROPOSAL].contains(k)),
        "{batch:?}"
    );
    let (got, a) = c.r.read().reader().answer_of(&card.r#ref).unwrap().unwrap();
    assert_eq!(got, h);
    assert_eq!(
        (a.act, a.option.as_deref(), a.label.as_deref()),
        (AnswerAct::KeepPin, Some("keep"), Some(KEEP_PIN_LABEL))
    );
    assert_eq!(
        a.note.as_deref(),
        Some("Billing stays on UTC until the audit.")
    );
    assert_eq!(a.person.actor, PERSON);
    assert_eq!(a.card_hash, Some(card.card_hash.parse().unwrap()));
    assert!(a.targets.is_empty());
    assert_eq!(a.options, labels(&card));
    // no pin moved, canon stands, no view was made, and the card is settled
    assert_eq!((pins_of(&c.r, &c.x), pins_of(&c.r, &c.d)), before);
    assert_eq!(c.r.read().canon_head(), canon);
    assert!(adopt_views(&c.r).is_empty());
    assert!(adopt_items(&c.r).is_empty());
    c.r.fsck().unwrap();
}

#[test]
fn rpl20_accepted_decision_foreign_pin_adoption_succeeds() {
    let mut c = citing();
    let v1 = claim_c("Every service logs in UTC.");
    let pre = c.r.read().canon_head().unwrap();
    let old = c.r.read().block(&Scope::Canon, &c.d).unwrap();
    let c2 = change_claim(&mut c, "v2", "Every service logs in local time.");
    let card = the_card(&c.r);
    let Some(PlanCardBinding::Adopt { pins, .. }) = &card.plan_binding else {
        panic!("{:#?}", card.plan_binding)
    };
    assert!(pins[0].citers.contains(&c.d), "{pins:#?}");
    let v = answer(&mut c.r, &card.r#ref, "adopt", None, Some(&card.card_hash)).unwrap();
    let landing: Hash = v["landing"]["landing"].as_str().unwrap().parse().unwrap();
    // the accepted decision moved its pin and nothing else: it stays accepted, same content
    let new = c.r.read().block(&Scope::Canon, &c.d).unwrap();
    assert!(rdy_model::decision::is_frozen(&new));
    assert_eq!(new.foreign.pins[&target()], pin_of(&c2));
    assert_eq!(
        new.content_hash(),
        old.content_hash(),
        "its content is the same"
    );
    assert_eq!(
        new.fields, old.fields,
        "its ruling, tracking and engine fields too"
    );
    let moves = BTreeMap::from([(target(), (pin_of(&v1), pin_of(&c2)))]);
    assert!(rdy_model::decision::only_foreign_pins_adopted(
        &old, &new, &moves
    ));
    // the adoption's change carries no authority of its own: its reserved view name and its
    // shape are no approval. Replayed on the canon before it, with no adoption, the merge
    // refuses the frozen decision's move; with the adoption it was landed under, it applies
    let rec: LandingRecord = c.r.store().landing(&landing).unwrap();
    let ch: Change = c.r.store().change(&rec.changes[0]).unwrap();
    assert!(ch.view.starts_with("adopt-"), "{}", ch.view);
    let before = c.r.store().state_of(Some(pre)).unwrap();
    let e =
        c.r.store()
            .merge_checked(&before, std::slice::from_ref(&ch))
            .unwrap_err();
    assert!(
        matches!(&e, rdy_store::StoreError::Frozen(id) if *id == c.d),
        "{e:?}"
    );
    let mut adoption = rdy_store::PinAdoption::default();
    adoption.insert(c.d.clone(), target(), pin_of(&v1), pin_of(&c2));
    adoption.insert(c.x.clone(), target(), pin_of(&v1), pin_of(&c2));
    c.r.store()
        .merge_checked_adopting(&before, std::slice::from_ref(&ch), &adoption)
        .expect("the adoption's own moves apply");
    // a replay into a view writes the change as the view's own work; its landing would merge it
    // with no adoption, so the move never reaches canon that way
    let replayer = agent("agent:w", Some("r9"));
    c.r.view_create(&replayer, "old", Some(&format!("landing:{pre}")), true)
        .unwrap();
    c.r.view_replay(&replayer, "old", &[rec.changes[0].to_string()])
        .unwrap();
    let old = c.r.read().materialized(&Scope::View("old".into())).unwrap();
    assert_eq!(old.get(&c.d).unwrap().foreign.pins[&target()], pin_of(&c2));
    let head = c.r.read().canon_head();
    assert!(land(&mut c.r, "old").is_err(), "the replay does not land");
    assert_eq!(c.r.read().canon_head(), head);
    c.r.fsck().unwrap();
}

#[test]
fn rpl20_adopt_moves_the_link_instruction_baseline() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let files = tempfile::tempdir().unwrap();
    let (_s, link) = linked(&mut r);
    // the first delivery carries an organization instruction; nothing local cites it
    let rule = block(
        "ai-RGMT1",
        "instruction",
        "Write every claim in the present tense.",
    );
    let org = Some(ForeignSet::Organization);
    deliver_in(&mut r, files.path(), "v1", &link, org.clone(), &[&rule]);
    let card = the_card(&r);
    let Some(PlanCardBinding::Adopt { instructions, .. }) = &card.plan_binding else {
        panic!("{:#?}", card.plan_binding)
    };
    assert_eq!(instructions.len(), 1, "{instructions:#?}");
    assert!(pins_of(&r, &link).is_empty(), "no rule is in force yet");
    answer(&mut r, &card.r#ref, "adopt", None, Some(&card.card_hash)).unwrap();
    // the link pins the delivered rule in the same landing: it is in force now
    let rule_id = ForeignId::new(cited(), rule.id.clone());
    let link_pins = pins_of(&r, &link);
    assert_eq!(link_pins.len(), 1, "{link_pins:#?}");
    assert_eq!(link_pins[&rule_id], pin_of(&rule));
    assert!(adopt_items(&r).is_empty(), "the adoption settles the card");
    // the rule changes: the card's baseline is the adopted version, and the set stays
    let rule2 = block(
        "ai-RGMT1",
        "instruction",
        "Write every claim in the past tense.",
    );
    deliver_in(&mut r, files.path(), "v2", &link, org, &[&rule2]);
    let card = the_card(&r);
    let Some(PlanCardBinding::Adopt {
        instructions, set, ..
    }) = &card.plan_binding
    else {
        panic!("{:#?}", card.plan_binding)
    };
    assert_eq!(instructions.len(), 1, "{instructions:#?}");
    assert_eq!(instructions[0].old, Some(rule.version_hash()));
    assert_eq!(instructions[0].new, Some(rule2.version_hash()));
    assert_eq!(
        (set.from.clone(), set.to.clone()),
        (
            Some(ForeignSet::Organization),
            Some(ForeignSet::Organization)
        ),
        "the adopted set is the baseline"
    );
    answer(&mut r, &card.r#ref, "adopt", None, Some(&card.card_hash)).unwrap();
    assert_eq!(pins_of(&r, &link)[&rule_id], pin_of(&rule2));
    // a delivery that drops the rule: adopting it takes the rule out of force
    let other = block("ai-RGMT2", "instruction", "Name every owner.");
    deliver_in(
        &mut r,
        files.path(),
        "v3",
        &link,
        Some(ForeignSet::Organization),
        &[&other],
    );
    let card = the_card(&r);
    answer(&mut r, &card.r#ref, "adopt", None, Some(&card.card_hash)).unwrap();
    let link_pins = pins_of(&r, &link);
    assert_eq!(
        link_pins.keys().cloned().collect::<Vec<_>>(),
        vec![ForeignId::new(cited(), other.id.clone())],
        "the removed rule leaves the baseline, the added one joins it"
    );
    r.fsck().unwrap();
}
