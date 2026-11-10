//! rpl18 (repo links, D-Y49PX "Freshness", D-8VJAZ "Deviation", D-ZWP7Q never-list, D-ME2HD):
//! freshness and human-only changes across a repo link.
//!
//! - A version's stored assessment records its foreign reach (ASSESSMENT encoding 2). A delivery
//!   after the assessment that changes a cited block in that reach, or a foreign side of a tension
//!   in it, fails freshness as a landing that touches the reach does: the approval refuses with
//!   `E_PROPOSAL_MOVED`, even when canon stayed still. A delivery that changes no block of the
//!   reach leaves the approval as it was.
//! - A deviation is an exception on the card, kind `deviation`: a `confirm` item beside its
//!   batch, never a queue item kind of its own.
//! - A version that records a deviation, changes a repo link or writes `adopted_as` never lands
//!   under the authority policy.
use rdy_engine::foreign_delivery::{DeliveryTaken, take_delivery};
use rdy_engine::pending::{ExceptionKind, Item, ItemKind};
use rdy_engine::{
    E_EXCEPTION_OPEN, E_POLICY_HUMAN, E_PROPOSAL_MOVED, ProposalInfo, Rdy, RdyError, WriteResult,
};
use rdy_model::assessment::{AssessmentExceptionKind, PolicyVerdict};
use rdy_model::foreign::{
    ForeignDerived, ForeignId, ForeignPublication, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::{Block, BlockId, FieldValue, Hash, RepoId};
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, answer_open_items, pending};
use rdy_qa::packs::QA_PACK_YAML;
use rdy_qa::tensions::{ag, j_typo, put_j};
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

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

fn created(w: &WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).unwrap()
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
}

/// A cited block, as the cited side delivers it.
fn block(id_: &str, ty: &str, fields: &[(&str, &str)], body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: ty.into(),
        parent: None,
        order: "V".into(),
        fields: fields
            .iter()
            .map(|(k, v)| (k.to_string(), FieldValue::Str((*v).into())))
            .collect(),
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

/// A link delivery stream of `repo` holding `selected` (any order), written to `dir/name`.
fn stream(dir: &Path, name: &str, repo: RepoId, selected: &[&Block]) -> File {
    let mut selected = selected.to_vec();
    selected.sort_by(|a, b| a.id.cmp(&b.id));
    let versions: Vec<ForeignVersion> = selected
        .iter()
        .map(|b| ForeignVersion {
            id: b.id.clone(),
            version: b.version_hash(),
            content: b.content_hash(),
            encoding: 0,
            derived: live(),
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
    let header = StreamHeader::link_delivery(baseline_features(), repo, Hash::of(name.as_bytes()));
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

/// Takes the delivery of `selected` from the cited repo for link `link`.
fn deliver(
    r: &mut Rdy,
    dir: &Path,
    name: &str,
    link: &BlockId,
    selected: &[&Block],
) -> DeliveryTaken {
    let source = ForeignSource::Link {
        link: link.to_string(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
    };
    take_delivery(r, &stream(dir, name, cited(), selected), &source, None)
        .unwrap_or_else(|e| panic!("take {name}: {e:?}"))
}

/// A landed draft doc with a section and the repo link `core` to the cited repo; returns the
/// doc, the section and the link.
fn linked(r: &mut Rdy) -> (BlockId, BlockId, BlockId) {
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
    (created(&w, "$d"), created(&w, "$s"), created(&w, "$l"))
}

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    answer_open_items(r, &writer(), view);
    let p = r
        .propose(&writer(), view, view, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"));
    assert!(p.ready, "{view}: {:#?}", p.issues);
    assert!(p.assessment.is_some(), "{p:#?}");
    p
}

/// The stored assessment of version `p` now, in the layout that carries its foreign part.
fn stored(r: &Rdy, p: &ProposalInfo) -> (Hash, rdy_model::assessment::AssessmentV2) {
    let st = r.read();
    let a = st
        .reader()
        .assessment_of_proposal(&h(&p.id), st.seq())
        .unwrap()
        .unwrap_or_else(|| panic!("{} stores an assessment", p.r#ref));
    (a, st.reader().assessment_wire(&a).unwrap())
}

/// The encoding of the stored ASSESSMENT `a`.
fn encoding_of(r: &Rdy, a: &Hash) -> u16 {
    let (raw, payload) = r.read().reader().get_raw(a).expect("the assessment frame");
    assert_eq!(codec::logical_kind(raw), kinds::ASSESSMENT);
    codec::split(raw, &payload).unwrap().version
}

/// The two cited claims of the first delivery.
struct Cited {
    c: Block,
    e: Block,
}

fn cited_claims() -> Cited {
    Cited {
        c: block("c-FRGNC", "claim", &[], "Every service logs in UTC."),
        e: block(
            "c-FRGNE",
            "claim",
            &[],
            "Queues hold at most ten thousand jobs.",
        ),
    }
}

/// A project with the link `core` and the first delivery of [`cited_claims`], then view `a`
/// writes a claim that cites `c` and is proposed, ready. Returns the project, its files dir,
/// the link, the claims and the proposal.
fn citing_proposal() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    Rdy,
    BlockId,
    Cited,
    ProposalInfo,
) {
    let (d, mut r) = rdy_qa::fixtures::project();
    let files = tempfile::tempdir().unwrap();
    let (_doc, s, link) = linked(&mut r);
    let k = cited_claims();
    deliver(&mut r, files.path(), "v1", &link, &[&k.c, &k.e]);
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$x", "parent": s.to_string(),
                "body": "Our billing jobs follow {{core:c-FRGNC}}."}]),
    );
    let p = propose(&mut r, "a");
    (d, files, r, link, k, p)
}

#[test]
fn rpl18_delivery_in_reach_moves_proposal() {
    let (_d, files, mut r, link, k, p) = citing_proposal();
    // the stored assessment records the held version of the cited claim in its foreign reach
    let (a, wire) = stored(&r, &p);
    let target = ForeignId::new(cited(), k.c.id.clone());
    assert!(
        wire.foreign
            .reach
            .iter()
            .any(|row| row.target == target && row.version == k.c.version_hash()),
        "{:#?}",
        wire.foreign
    );
    assert_eq!(encoding_of(&r, &a), 2, "a foreign part takes encoding 2");
    let in_force = wire
        .foreign
        .delivered
        .iter()
        .find(|d| d.target == target)
        .map(|d| d.entry)
        .expect("the delivery in force for the cited claim");
    // a delivery changes the cited claim; canon stays still
    let head = r.read().canon_head();
    let c2 = block("c-FRGNC", "claim", &[], "Every service logs in local time.");
    let v2 = deliver(&mut r, files.path(), "v2", &link, &[&c2, &k.e]);
    assert_ne!(v2.entry, in_force);
    assert_eq!(
        r.read().canon_head(),
        head,
        "a delivery moves no canon head"
    );
    let e = r
        .land_report(&approver(), &p.id)
        .expect_err("the delivery moved the version");
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:#?}");
    assert_eq!(
        e.message,
        format!(
            "proposal {} moved: delivery {} changed core:c-FRGNC after its assessment",
            p.r#ref,
            v2.entry.short()
        ),
        "{e:#?}"
    );
    assert_eq!(r.read().canon_head(), head, "nothing landed");
    // the version sees the new signals: the delivery's premise shift is a tension item it has
    // not answered, so its fresh readiness fails (O11) and no fresh card comes
    assert_eq!(e.corrected, None, "{e:#?}");
    assert!(e.issues.iter().any(|i| i.code == "E_PLAN_OPEN"), "{e:#?}");
    let (now, _) = stored(&r, &p);
    assert_eq!(now, a, "no fresh record for a version that is not ready");
}

#[test]
fn rpl18_unrelated_delivery_keeps_approval() {
    let (_d, files, mut r, link, k, p) = citing_proposal();
    let (a, _) = stored(&r, &p);
    // a delivery changes only the other cited claim, which no block of the reach pins
    let e2 = block(
        "c-FRGNE",
        "claim",
        &[],
        "Queues hold at most five thousand jobs.",
    );
    deliver(&mut r, files.path(), "v2", &link, &[&k.c, &e2]);
    let l = r
        .land_report(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("the approval holds: {e:#?}"));
    assert_eq!(l.approved, p.r#ref);
    let st = r.read();
    let rec = st.reader().landing(&st.canon_head().unwrap()).unwrap();
    assert_eq!(rec.assessment, Some(a), "the assessment of propose holds");
}

#[test]
fn rpl18_delivery_changing_a_tension_side_moves_proposal() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let files = tempfile::tempdir().unwrap();
    let (_doc, s, link) = linked(&mut r);
    let ledger = block(
        "t-FRGNT",
        "term",
        &[("name", "Ledger")],
        "The book of accounts.",
    );
    deliver(&mut r, files.path(), "v1", &link, &[&ledger]);
    // the version defines the same entity: a duplicate definition with a foreign side, with no
    // citation of the cited term
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "term", "id": "$t", "parent": s.to_string(),
                "fields": {"name": "ledger"}, "body": "Our ledger."}]),
    );
    let p = propose(&mut r, "a");
    let (_, wire) = stored(&r, &p);
    assert!(
        wire.foreign.tensions.iter().any(|t| t.repo == cited()),
        "{:#?}",
        wire.foreign
    );
    let ledger2 = block(
        "t-FRGNT",
        "term",
        &[("name", "Ledger")],
        "The book of all accounts.",
    );
    let v2 = deliver(&mut r, files.path(), "v2", &link, &[&ledger2]);
    let e = r
        .land_report(&approver(), &p.id)
        .expect_err("the delivery moved the version");
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:#?}");
    assert_eq!(
        e.message,
        format!(
            "proposal {} moved: delivery {} changed core:t-FRGNT after its assessment",
            p.r#ref,
            v2.entry.short()
        ),
        "{e:#?}"
    );
    // the next version sees the new signal: the changed side is a new tension item
    assert!(e.issues.iter().any(|i| i.code == "E_PLAN_OPEN"), "{e:#?}");
}

#[test]
fn rpl18_a_fresh_record_after_a_delivery_holds() {
    let (_d, files, mut r, link, k, p) = citing_proposal();
    let (a, _) = stored(&r, &p);
    // a delivery drops the cited claim from the selection: the held pin still reads it
    let v2 = deliver(&mut r, files.path(), "v2", &link, &[&k.e]);
    let e = r
        .land_report(&approver(), &p.id)
        .expect_err("the delivery moved the version");
    assert_eq!(
        (e.code, e.message.clone()),
        (
            E_PROPOSAL_MOVED,
            format!(
                "proposal {} moved: delivery {} changed core:c-FRGNC after its assessment",
                p.r#ref,
                v2.entry.short()
            )
        ),
        "{e:#?}"
    );
    // the version is still ready: the refusal carries a fresh card, and its fresh record,
    // written after the delivery, holds for the next approval
    assert!(e.corrected.is_some(), "{e:#?}");
    // canon stayed still, but the fresh record names the deliveries in force now
    let (fresh, wire) = stored(&r, &p);
    assert_ne!(fresh, a, "a fresh record");
    let target = ForeignId::new(cited(), k.c.id.clone());
    assert!(
        !wire.foreign.delivered.iter().any(|d| d.target == target),
        "no live source selects the claim now: {:#?}",
        wire.foreign
    );
    let l = r
        .land_report(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("the fresh record holds: {e:#?}"));
    let st = r.read();
    let rec = st.reader().landing(&st.canon_head().unwrap()).unwrap();
    assert_eq!((l.approved, rec.assessment), (p.r#ref.clone(), Some(fresh)));
}

/// The confirm items beside batch `pref`, as the person sees them.
fn exceptions_of(r: &Rdy, pref: &str) -> Vec<Item> {
    pending(r)
        .items
        .into_iter()
        .filter(|i| i.batch.as_deref() == Some(pref))
        .collect()
}

#[test]
fn rpl18_deviation_is_confirm_exception_not_queue_kind() {
    // the closed list: the name, the parser and the wire
    assert_eq!(ExceptionKind::Deviation.name(), "deviation");
    assert_eq!(
        "deviation".parse::<ExceptionKind>().unwrap(),
        ExceptionKind::Deviation
    );
    assert!(ExceptionKind::ALL.contains(&ExceptionKind::Deviation));
    assert_eq!(
        serde_json::to_value(ExceptionKind::Deviation).unwrap(),
        json!("deviation")
    );

    let (_d, mut r) = rdy_qa::fixtures::project();
    let files = tempfile::tempdir().unwrap();
    let (_doc, s, link) = linked(&mut r);
    let rule = block(
        "d-FRGNX",
        "decision",
        &[
            ("statement", "Logs keep ninety days."),
            ("status", "accepted"),
        ],
        "",
    );
    deliver(&mut r, files.path(), "v1", &link, &[&rule]);
    let w = put(
        &mut r,
        "dv",
        json!([{"op": "create", "type": "decision", "id": "$dec", "parent": s.to_string(),
                "fields": {"statement": "Our logs keep thirty days.", "status": "proposed",
                           "needs_content": false},
                "links": {"deviates_from": ["core:d-FRGNX"]}}]),
    );
    let dec = created(&w, "$dec");
    let p = propose(&mut r, "dv");
    let items = exceptions_of(&r, &p.r#ref);
    assert_eq!(items.len(), 1, "{items:#?}");
    let it = &items[0];
    // a confirm item beside its batch, with the exception kind deviation
    assert_eq!(it.kind, ItemKind::Confirm);
    assert!(it.r#ref.starts_with(&format!("{dec}@")), "{it:#?}");
    assert_eq!(it.exception, Some(ExceptionKind::Deviation), "{it:#?}");
    assert!(
        it.facts
            .iter()
            .any(|f| f.name == "Deviates from" && f.value == "core:d-FRGNX"),
        "{it:#?}"
    );
    assert!(
        it.card.contains(&format!(
            "Beside {}, on its own: it departs from cited or external content.",
            p.r#ref
        )),
        "{}",
        it.card
    );
    // no item of the queue is a kind of its own for it: only the approve and the confirm items
    for i in pending(&r).items {
        assert!(
            matches!(i.kind, ItemKind::Approve | ItemKind::Confirm),
            "{i:#?}"
        );
    }
    // the stored assessment names the exception, in the foreign encoding
    let (a, wire) = stored(&r, &p);
    assert!(
        wire.facts
            .exceptions
            .iter()
            .any(|e| e.kind == AssessmentExceptionKind::Deviation && e.reference == it.r#ref),
        "{:#?}",
        wire.facts
    );
    assert_eq!(encoding_of(&r, &a), 2);
    // the approval waits for the person's confirm answer
    let e = r
        .land_report(&approver(), &p.id)
        .expect_err("the exception is open");
    assert_eq!(e.code, E_EXCEPTION_OPEN, "{e:#?}");
    rdy_qa::intake::confirm_before_landing(&mut r, &approver(), &p);
    r.land_report(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("lands once confirmed: {e:#?}"));
}

// ---- the authority policy never accepts a repo link change or a deviation ----

/// The policy of two rules: `drafts` lets agents `agent:a*` land draft edits with no decision;
/// `decides` lets them land `two_way` decisions with basis `precedent` in area `payments`.
fn policy_fields() -> Value {
    json!({
        "key": "authority-policy",
        "agent_may_accept": [
            {
                "name": "drafts",
                "actors": ["agent:a*"],
                "lifecycle": "draft",
                "justifications": ["implements", "editorial"],
                "decisions": "none",
                "max_blocks": 20,
                "max_items": 20
            },
            {
                "name": "decides",
                "actors": ["agent:a*"],
                "lifecycle": "any",
                "justifications": ["implements", "editorial"],
                "decisions": "two_way",
                "two_way_areas": ["payments"],
                "basis": ["precedent"],
                "max_blocks": 20,
                "max_items": 20
            }
        ],
        "roles": {"approvers": [PERSON]},
        "require": [{"when": "decision", "role": "approvers"}],
        "owners": []
    })
}

/// An `rdy-qa` pack project with: a draft doc `wallets` (area `payments`) with a section, the
/// repo link `core`, the policy landed with the decision it implements, and the first delivery
/// of a cited decision `d-FRGNX`. Returns the project, the doc, the section and the delivery's
/// files dir.
fn policy_world() -> (tempfile::TempDir, Rdy, BlockId, BlockId, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").expect("init"));
    std::fs::write(dir.path().join(".rdy/packs/rdy-qa.yaml"), QA_PACK_YAML).expect("pack");
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: rdy-qa\n").expect("config");
    let mut r = Rdy::open(dir.path()).unwrap();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$w", "fields": {"name": "wallets", "title": "Wallets", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ws", "parent": "$w", "fields": {"title": "Fees"}},
            {"op": "create", "type": "repo_link", "id": "$l", "parent": "$w",
             "fields": {"alias": "core", "repo": cited().to_string(), "profile": PROFILE}},
            {"op": "create", "type": "doc", "id": "$r", "fields": {"name": "rules", "title": "Rules", "doc_type": "overview"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$r", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
    );
    land(&mut r, "seed").unwrap();
    let (doc, ws, link, rules, dec) = (
        created(&w, "$w"),
        created(&w, "$ws"),
        created(&w, "$l"),
        created(&w, "$r"),
        created(&w, "$dec"),
    );
    r.write(
        &writer(),
        rdy_engine::WriteRequest {
            view: "pol".into(),
            ops: rdy_qa::fixtures::parse_test_ops(json!([
                {"op": "create", "type": "authority_policy", "id": "$p", "parent": rules.to_string(), "fields": policy_fields()}
            ]))
            .unwrap(),
            justification: Some(rdy_model::Justification::Implements(vec![dec])),
            message: String::new(),
        },
    )
    .unwrap();
    land(&mut r, "pol").unwrap();
    let files = tempfile::tempdir().unwrap();
    let rule = block(
        "d-FRGNX",
        "decision",
        &[("statement", "Fees round down."), ("status", "accepted")],
        "",
    );
    deliver(&mut r, files.path(), "v1", &link, &[&rule]);
    (dir, r, doc, ws, files)
}

/// Agent `agent:<name>` proposes its view `<name>` with a summary; ready.
fn propose_as(r: &mut Rdy, name: &str) -> ProposalInfo {
    let p = r
        .propose(&ag(name), name, name, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {name}: {e}"));
    assert!(p.ready, "{name}: {:#?}", p.issues);
    p
}

/// The stored verdict's reasons of version `p`: it needs a person.
fn needs_person(r: &Rdy, p: &ProposalInfo) -> Vec<String> {
    match stored(r, p).1.policy.expect("a verdict").verdict {
        PolicyVerdict::NeedsPerson { reasons, .. } => reasons,
        PolicyVerdict::Accepts { rule } => panic!("{}: accepted under {rule}", p.r#ref),
    }
}

fn policy_refused(r: &mut Rdy, name: &str, p: &ProposalInfo) -> RdyError {
    r.land_by_policy(&ag(name), &p.r#ref)
        .expect_err("the policy does not accept it")
}

#[test]
fn rpl18_repo_link_and_deviation_never_policy_land() {
    let (_d, mut r, doc, ws, _files) = policy_world();
    // a repo link change: a draft edit the rule drafts would otherwise accept
    let w = put_j(
        &mut r,
        "a1",
        json!([{"op": "create", "type": "repo_link", "id": "$l", "parent": doc.to_string(),
                "fields": {"alias": "edge", "repo": RepoId([9; 16]).to_string(), "profile": PROFILE}}]),
        j_typo(),
    );
    let rl = created(&w, "$l");
    let p = propose_as(&mut r, "a1");
    let reasons = needs_person(&r, &p);
    assert!(
        reasons.contains(&format!("it changes the repo link {rl}")),
        "{reasons:#?}"
    );
    let e = policy_refused(&mut r, "a1", &p);
    assert_eq!(e.code, E_POLICY_HUMAN, "{e:#?}");

    // a deviation: a two_way decision in payments with basis precedent, which the rule decides
    // would otherwise accept
    let w = put_j(
        &mut r,
        "a2",
        json!([{"op": "create", "type": "decision", "id": "$dec", "parent": ws.to_string(),
                "fields": {"statement": "Fees round up here.", "status": "proposed", "needs_content": false,
                           "reversibility": "two_way", "authority_basis": "precedent"},
                "links": {"deviates_from": ["core:d-FRGNX"]}}]),
        j_typo(),
    );
    let dec = created(&w, "$dec");
    let p = propose_as(&mut r, "a2");
    // a person confirms the exception; the policy still never lands the version
    rdy_qa::intake::confirm_before_landing(&mut r, &approver(), &p);
    let reasons = needs_person(&r, &p);
    assert!(
        reasons
            .iter()
            .any(|x| x.starts_with(&format!("it has the exception deviation on {dec}@"))),
        "{reasons:#?}"
    );
    let e = policy_refused(&mut r, "a2", &p);
    assert_eq!(e.code, E_POLICY_HUMAN, "{e:#?}");
}
