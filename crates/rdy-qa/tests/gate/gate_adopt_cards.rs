//! rpl19 (repo links, D-8VJAZ "adopt cards", D-MEMMQ, D-ME2C0): the adopt card of a delivery and
//! the reserved names beside it.
//!
//! - A delivery that changes content the project cites, or an instruction of the link, or its set,
//!   makes one adopt card, ref `adopt:<repo>@<entry>`, that binds the repo, the entry and the
//!   diff. The card is internal until its answer path exists: the queue never lists it, and an
//!   early answer is refused with no write.
//! - Keep and later move no pin. A keep hides the card until the next delivery that changes
//!   something; a later hides it for the chat session.
//! - `adopt-` is reserved: no view and no run takes such a name.
use rdy_engine::foreign_delivery::{DeliveryTaken, take_delivery};
use rdy_engine::pending::{ItemKind, PlanCardBinding, card_hash};
use rdy_engine::{E_RESERVED_VIEW, Rdy, Scope, WriteResult};
use rdy_model::foreign::{
    ForeignDerived, ForeignId, ForeignPin, ForeignPublication, ForeignSet, ForeignSource,
    ForeignVersion, selection_identity,
};
use rdy_model::{
    Answer, AnswerAct, Block, BlockId, EvidenceKind, FieldValue, Hash, Provenance, RepoId,
};
use rdy_qa::fixtures::{land, write, writer};
use rdy_qa::intake::{PERSON, pending};
use rdy_store::codec;
use rdy_store::format::baseline_features;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{DeliveryRecord, StreamHeader, StreamRecord, StreamWriter};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

const PROFILE: &str = "pf-AGNT1";

fn cited() -> RepoId {
    RepoId([7; 16])
}

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

fn live() -> ForeignDerived {
    ForeignDerived {
        status: None,
        lifecycle: None,
        superseded_by: vec![],
    }
}

/// One selected block with the facts its delivery derived for it.
struct Sel<'a> {
    block: &'a Block,
    derived: ForeignDerived,
}

fn plain<'a>(blocks: &[&'a Block]) -> Vec<Sel<'a>> {
    blocks
        .iter()
        .map(|b| Sel {
            block: b,
            derived: live(),
        })
        .collect()
}

/// A link delivery stream of the cited repo holding `selected`, written to `dir/name`.
fn stream(dir: &Path, name: &str, mut selected: Vec<Sel<'_>>) -> File {
    selected.sort_by(|a, b| a.block.id.cmp(&b.block.id));
    let versions: Vec<ForeignVersion> = selected
        .iter()
        .map(|s| ForeignVersion {
            id: s.block.id.clone(),
            version: s.block.version_hash(),
            content: s.block.content_hash(),
            encoding: 0,
            derived: s.derived.clone(),
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
    for s in &selected {
        let o = codec::encode_new(kinds::BLOCK, s.block);
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

/// Takes the delivery `selected` for link `link`, of the set `set`.
fn deliver_in(
    r: &mut Rdy,
    dir: &Path,
    name: &str,
    link: &BlockId,
    set: Option<ForeignSet>,
    selected: Vec<Sel<'_>>,
) -> DeliveryTaken {
    let source = ForeignSource::Link {
        link: link.to_string(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
    };
    take_delivery(r, &stream(dir, name, selected), &source, set)
        .unwrap_or_else(|e| panic!("take {name}: {e:?}"))
}

fn deliver(
    r: &mut Rdy,
    dir: &Path,
    name: &str,
    link: &BlockId,
    selected: &[&Block],
) -> DeliveryTaken {
    deliver_in(r, dir, name, link, None, plain(selected))
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

fn claim_c(body: &str) -> Block {
    block("c-FRGNC", "claim", body)
}

fn claim_e(body: &str) -> Block {
    block("c-FRGNE", "claim", body)
}

/// A project that cites the claim `c-FRGNC` from a landed block, after the first delivery.
/// Returns the project, its files dir, the link and the citing block.
fn citing() -> (tempfile::TempDir, tempfile::TempDir, Rdy, BlockId, BlockId) {
    let (d, mut r) = rdy_qa::fixtures::project();
    let files = tempfile::tempdir().unwrap();
    let (s, link) = linked(&mut r);
    deliver(
        &mut r,
        files.path(),
        "v1",
        &link,
        &[
            &claim_c("Every service logs in UTC."),
            &claim_e("Queues hold at most ten thousand jobs."),
        ],
    );
    let w = put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$x", "parent": s.to_string(),
                "body": "Our billing jobs follow {{core:c-FRGNC}}."}]),
    );
    land(&mut r, "a").unwrap();
    let x = created(&w, "$x");
    (d, files, r, link, x)
}

/// The adopt cards of the project now, for the person in chat session `session`.
fn cards(r: &Rdy, session: Option<&str>) -> Vec<rdy_engine::pending::Item> {
    let q = rdy_engine::pending::PendingQuery {
        person: PERSON.into(),
        session_ref: session.map(str::to_string),
        ..Default::default()
    };
    r.read().adopt_cards(&q).expect("the adopt cards read")
}

/// The adopt item ref of the delivery `entry` of the cited repo.
fn adopt_ref(entry: &Hash) -> String {
    format!("adopt:{}@{}", cited(), entry.short())
}

/// The foreign pins of block `b` in canon.
fn pins_of(r: &Rdy, b: &BlockId) -> BTreeMap<ForeignId, ForeignPin> {
    r.read().block(&Scope::Canon, b).unwrap().foreign.pins
}

/// A keep or a later of `item` by the person, written straight to the store.
fn answer_item(r: &mut Rdy, item: &str, act: AnswerAct, card: &str, run: Option<&str>) {
    let mut person: Provenance = writer().provenance();
    person.run = run.map(str::to_string);
    let a = Answer {
        item: item.into(),
        act,
        option: None,
        label: None,
        options: vec![],
        card_hash: Some(card.parse().unwrap()),
        targets: vec![],
        person,
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: Some(EvidenceKind::Elicited),
        note: (act == AnswerAct::KeepPin).then(|| "the old rule stays".to_string()),
    };
    r.store_mut().put_answer(&a).expect("put_answer");
    r.invalidate();
    r.seal_open().unwrap();
}

#[test]
fn rpl19_adopt_card_binds_repo_entry_and_diff() {
    let (_d, files, mut r, link, x) = citing();
    let before = pins_of(&r, &x);
    assert_eq!(before.len(), 1, "the citation holds one pin");
    assert!(cards(&r, None).is_empty(), "no change, no card");
    let canon = r.read().canon_head();
    // a delivery changes the cited claim
    let c2 = claim_c("Every service logs in local time.");
    let v2 = deliver(
        &mut r,
        files.path(),
        "v2",
        &link,
        &[&c2, &claim_e("Queues hold at most ten thousand jobs.")],
    );
    let cards = cards(&r, None);
    assert_eq!(cards.len(), 1, "{cards:#?}");
    let c = &cards[0];
    assert_eq!(c.kind, ItemKind::Adopt);
    assert_eq!(c.kind.word(), "adopt");
    assert_eq!(
        c.r#ref,
        adopt_ref(&v2.entry),
        "repo and entry name the item"
    );
    assert!(!c.blocking);
    let options: Vec<&str> = c.options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(options, ["adopt", "keep", "later"], "{c:#?}");
    assert!(c.options.iter().all(|o| o.recommended.is_none()));
    // the card shows the old and the new version and the word diff of the cited text
    let old_v = claim_c("Every service logs in UTC.").version_hash().short();
    let new_v = c2.version_hash().short();
    assert!(
        c.card.contains(&old_v) && c.card.contains(&new_v),
        "{}",
        c.card
    );
    assert!(c.card.contains("c-FRGNC"), "{}", c.card);
    assert!(
        c.card.contains("[-UTC-]") && c.card.contains("{+local time+}"),
        "the word diff: {}",
        c.card
    );
    // it names what the adoption clears: the citing block's suspect
    assert!(c.card.contains(x.as_str()), "{}", c.card);
    // the hash binds the complete card: its binding names the repo, the entry and every row
    assert_eq!(c.card_hash, card_hash(c).to_string());
    let Some(PlanCardBinding::Adopt {
        repo,
        entry,
        pins,
        instructions,
        ..
    }) = &c.plan_binding
    else {
        panic!("an adopt card binds its delivery: {:#?}", c.plan_binding)
    };
    assert_eq!((*repo, *entry), (cited(), v2.entry));
    assert!(instructions.is_empty());
    assert_eq!(pins.len(), 1, "{pins:#?}");
    assert_eq!(pins[0].target, ForeignId::new(cited(), id("c-FRGNC")));
    assert_eq!(pins[0].new, Some(c2.version_hash()));
    assert_eq!(pins[0].citers, vec![x.clone()]);
    // the same state gives the same card, and a binding change moves the hash
    let again = self::cards(&r, None);
    assert_eq!(again[0].card_hash, c.card_hash);
    let mut moved = c.clone();
    if let Some(PlanCardBinding::Adopt { pins, .. }) = &mut moved.plan_binding {
        pins[0].citers.clear();
    }
    assert_ne!(
        card_hash(&moved).to_string(),
        c.card_hash,
        "a row the text leaves out still moves the hash"
    );
    // a later delivery is another entry: another ref, another hash
    let c3 = claim_c("Every service logs in UTC always.");
    let v3 = deliver(
        &mut r,
        files.path(),
        "v3",
        &link,
        &[&c3, &claim_e("Queues hold at most ten thousand jobs.")],
    );
    let now = self::cards(&r, None);
    assert_eq!(now.len(), 1, "{now:#?}");
    assert_eq!(now[0].r#ref, adopt_ref(&v3.entry));
    assert_ne!(now[0].card_hash, c.card_hash);
    // the card is internal: the queue does not list it, and nothing moved
    let queue = pending(&r);
    assert!(
        queue.items.iter().all(|i| i.kind != ItemKind::Adopt),
        "{queue:#?}"
    );
    assert_eq!(pins_of(&r, &x), before, "a delivery moves no pin");
    assert_eq!(r.read().canon_head(), canon, "nor canon");
}

#[test]
fn rpl19_keep_reappears_only_on_next_delivery() {
    let (_d, files, mut r, link, x) = citing();
    let before = pins_of(&r, &x);
    let e = claim_e("Queues hold at most ten thousand jobs.");
    let v2 = deliver(
        &mut r,
        files.path(),
        "v2",
        &link,
        &[&claim_c("Every service logs in local time."), &e],
    );
    let card = self::cards(&r, None).remove(0);
    assert_eq!(card.r#ref, adopt_ref(&v2.entry));
    answer_item(
        &mut r,
        &card.r#ref,
        AnswerAct::KeepPin,
        &card.card_hash,
        None,
    );
    assert!(
        self::cards(&r, None).is_empty(),
        "a keep takes the card out"
    );
    assert_eq!(pins_of(&r, &x), before, "a keep moves no pin");
    // a delivery that changes nothing the project cites raises nothing
    let e2 = claim_e("Queues hold at most five thousand jobs.");
    deliver(
        &mut r,
        files.path(),
        "v3",
        &link,
        &[&claim_c("Every service logs in local time."), &e2],
    );
    assert!(
        self::cards(&r, None).is_empty(),
        "the kept change stays kept until the cited content changes again"
    );
    // the next delivery that changes the cited claim makes a card of its own
    let v4 = deliver(
        &mut r,
        files.path(),
        "v4",
        &link,
        &[&claim_c("Every service logs in GMT."), &e2],
    );
    let cards = self::cards(&r, None);
    assert_eq!(cards.len(), 1, "{cards:#?}");
    assert_eq!(cards[0].r#ref, adopt_ref(&v4.entry));
    assert_eq!(pins_of(&r, &x), before, "still no pin moved");
}

#[test]
fn rpl19_later_is_session_snooze() {
    let (_d, files, mut r, link, x) = citing();
    let before = pins_of(&r, &x);
    let v2 = deliver(
        &mut r,
        files.path(),
        "v2",
        &link,
        &[
            &claim_c("Every service logs in local time."),
            &claim_e("Queues hold at most ten thousand jobs."),
        ],
    );
    let card = self::cards(&r, Some("s1")).remove(0);
    assert_eq!(card.r#ref, adopt_ref(&v2.entry));
    answer_item(
        &mut r,
        &card.r#ref,
        AnswerAct::Later,
        &card.card_hash,
        Some("s1"),
    );
    // the snooze is the person's `pending/<person>/<item>` ref, never `answer/<item>`
    let st = r.read();
    assert!(
        st.reader()
            .snooze_of(PERSON, &card.r#ref)
            .unwrap()
            .is_some()
    );
    assert!(st.reader().answer_of(&card.r#ref).unwrap().is_none());
    drop(st);
    assert!(
        self::cards(&r, Some("s1")).is_empty(),
        "hidden in its session"
    );
    assert_eq!(
        self::cards(&r, Some("s2")).len(),
        1,
        "back in another session"
    );
    assert_eq!(
        self::cards(&r, None).len(),
        1,
        "a query with no session hides nothing"
    );
    assert_eq!(pins_of(&r, &x), before, "a later moves no pin");
    // a new delivery is a new item: the snooze does not reach it
    let v3 = deliver(
        &mut r,
        files.path(),
        "v3",
        &link,
        &[
            &claim_c("Every service logs in GMT."),
            &claim_e("Queues hold at most ten thousand jobs."),
        ],
    );
    let now = self::cards(&r, Some("s1"));
    assert_eq!(now.len(), 1, "{now:#?}");
    assert_eq!(now[0].r#ref, adopt_ref(&v3.entry));
}

#[test]
fn rpl19_first_instruction_delivery_raises_adopt_without_citation() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let files = tempfile::tempdir().unwrap();
    let (_s, link) = linked(&mut r);
    assert!(self::cards(&r, None).is_empty(), "no delivery, no card");
    // the first delivery carries an organization instruction; nothing local cites it
    let rule = block(
        "ai-RGMT1",
        "instruction",
        "Write every claim in the present tense.",
    );
    let v1 = deliver_in(
        &mut r,
        files.path(),
        "v1",
        &link,
        Some(ForeignSet::Organization),
        plain(&[&rule]),
    );
    let cards = self::cards(&r, None);
    assert_eq!(cards.len(), 1, "{cards:#?}");
    let c = &cards[0];
    assert_eq!(c.kind, ItemKind::Adopt);
    assert_eq!(c.r#ref, adopt_ref(&v1.entry));
    let Some(PlanCardBinding::Adopt {
        pins,
        instructions,
        set,
        ..
    }) = &c.plan_binding
    else {
        panic!("{:#?}", c.plan_binding)
    };
    assert!(pins.is_empty(), "no local citation, no pin row");
    assert_eq!(instructions.len(), 1, "{instructions:#?}");
    assert_eq!(instructions[0].id, rule.id);
    assert_eq!(
        instructions[0].old, None,
        "none of the link's rules is in force yet"
    );
    assert_eq!(instructions[0].new, Some(rule.version_hash()));
    assert_eq!(
        (set.from.clone(), set.to.clone()),
        (None, Some(ForeignSet::Organization))
    );
    assert!(
        c.card.contains("ai-RGMT1") && c.card.contains("organization"),
        "{}",
        c.card
    );
    // keep on a first delivery leaves none of the rules in force and the card gone
    answer_item(&mut r, &c.r#ref, AnswerAct::KeepPin, &c.card_hash, None);
    assert!(self::cards(&r, None).is_empty());
    // an instruction that changes raises a card again, against the empty baseline
    let rule2 = block(
        "ai-RGMT1",
        "instruction",
        "Write every claim in the past tense.",
    );
    let v2 = deliver_in(
        &mut r,
        files.path(),
        "v2",
        &link,
        Some(ForeignSet::Organization),
        plain(&[&rule2]),
    );
    let cards = self::cards(&r, None);
    assert_eq!(cards.len(), 1, "{cards:#?}");
    assert_eq!(cards[0].r#ref, adopt_ref(&v2.entry));
    let Some(PlanCardBinding::Adopt { instructions, .. }) = &cards[0].plan_binding else {
        panic!()
    };
    assert_eq!(
        instructions[0].old, None,
        "the kept delivery never became the baseline"
    );
    assert_eq!(instructions[0].new, Some(rule2.version_hash()));
}

#[test]
fn rpl19_status_only_delivery_offers_only_rebind() {
    let (_d, files, mut r, link, x) = citing();
    let before = pins_of(&r, &x);
    // the cited claim is superseded by another: the same versions, new derived facts
    let c = claim_c("Every service logs in UTC.");
    let e = claim_e("Queues hold at most ten thousand jobs.");
    let status_only = vec![
        Sel {
            block: &c,
            derived: ForeignDerived {
                status: Some("superseded".into()),
                lifecycle: None,
                superseded_by: vec![e.id.clone()],
            },
        },
        Sel {
            block: &e,
            derived: live(),
        },
    ];
    let v2 = deliver_in(&mut r, files.path(), "v2", &link, None, status_only);
    let cards = self::cards(&r, None);
    assert_eq!(cards.len(), 1, "{cards:#?}");
    assert_eq!(cards[0].r#ref, adopt_ref(&v2.entry));
    let Some(PlanCardBinding::Adopt {
        pins,
        rebinds,
        instructions,
        ..
    }) = &cards[0].plan_binding
    else {
        panic!("{:#?}", cards[0].plan_binding)
    };
    assert!(
        pins.is_empty() && instructions.is_empty(),
        "no pin and no rule moves"
    );
    assert_eq!(rebinds.len(), 1, "{rebinds:#?}");
    assert_eq!(rebinds[0].citer, x);
    assert_eq!(rebinds[0].target, ForeignId::new(cited(), c.id.clone()));
    assert_eq!(rebinds[0].successor, e.id);
    assert_eq!(pins_of(&r, &x), before, "a delivery moves no pin");
    // a superseded block with no successor in the selection offers nothing to rebind to
    let lone = vec![Sel {
        block: &c,
        derived: ForeignDerived {
            status: Some("superseded".into()),
            lifecycle: None,
            superseded_by: vec![id("c-NXTCM")],
        },
    }];
    let (_d2, files2, mut r2, link2, _x2) = citing();
    deliver_in(&mut r2, files2.path(), "v2", &link2, None, lone);
    assert!(
        self::cards(&r2, None).is_empty(),
        "a status change names no successor: nothing to offer"
    );
}

#[test]
fn rpl19_adopt_view_and_run_prefix_are_reserved() {
    // the view name: `adopt-` and the first 12 hex digits of the card hash
    let card: Hash = Hash::of(b"an adopt card");
    let view = rdy_store::adopt_view(&card);
    assert_eq!(view, format!("adopt-{}", card.short()));
    assert!(rdy_engine::is_reserved_view(&view));
    assert!(rdy_engine::is_reserved_view("adopt-"));
    assert!(!rdy_engine::is_reserved_view("adopt"));
    let e = rdy_engine::reserved_view_error(&view);
    assert_eq!(e.code, E_RESERVED_VIEW);
    assert_eq!(
        e.message,
        "view names that start with adopt- are reserved for adopt cards"
    );
    assert_eq!(e.fix, "pick a view name that does not start with adopt-");
    // the reaffirm- reservation keeps its words
    let e = rdy_engine::reserved_view_error("reaffirm-0123456789ab");
    assert_eq!(
        e.message,
        "view names that start with reaffirm- are reserved for re-affirmation cards"
    );
    // no write takes such a view
    let (_d, mut r) = rdy_qa::fixtures::project();
    let err = write(
        &mut r,
        "adopt-0123456789ab",
        json!([{"op": "create", "type": "doc", "id": "$d",
                "fields": {"title": "Guide", "doc_type": "overview"}}]),
    )
    .expect_err("a reserved view takes no write");
    assert_eq!(err.code, E_RESERVED_VIEW);
    assert_eq!(
        err.message,
        "view names that start with adopt- are reserved for adopt cards"
    );
    // no run takes such a name, and the refusal names it
    assert!(!rdy_engine::identity::is_run("adopt-x"));
    assert!(!rdy_engine::identity::is_run("reaffirm-x"));
    assert!(rdy_engine::identity::is_run("adopt"));
    assert!(rdy_engine::identity::is_run("adoption-1"));
    let e = rdy_engine::identity::validate_run("adopt-x").unwrap_err();
    assert_eq!(
        e.fix,
        "use 1 to 64 letters, digits, -, _ or ., with a letter or digit at each end; not none, work, canon, inflight, reaffirm-\u{2026} or adopt-\u{2026}"
    );
}
