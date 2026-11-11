//! rpl21 (repo links, D-8VJAZ "Adopted upstream"): a local decision accepted in its home project
//! and delivered back is superseded by its adopted copy.
//!
//! - `Rdy::supersede_adopted_as` is the `adopted_as` branch of supersede, apart from the
//!   `decision` branch: it writes the store-only engine field `adopted_as` on the accepted old
//!   decision, naming the cited decision and the delivered content it checked. Nothing else of
//!   the old decision changes, and the cited decision is never written.
//! - The old decision's status becomes `superseded`, `by` naming the cited decision. Its
//!   dependents get items through the existing plan path.
//! - No write op names `adopted_as`: a create or an update that sets or unsets it is refused
//!   (`E_FIELD_UNKNOWN`, fix `remove adopted_as; use rdy_supersede with adopted_as`). The content
//!   hash leaves it out.
//! - A version that writes it never lands under the authority policy: a person lands it.
use rdy_engine::foreign_delivery::{DeliveryTaken, take_delivery};
use rdy_engine::plan::api::PlanRow;
use rdy_engine::{
    DecisionInput, E_BAD_INPUT, E_FOREIGN_READ_ONLY, E_POLICY_HUMAN, E_UNKNOWN_REF, E_VALIDATION,
    ProposalInfo, Rdy, RdyError, Scope, WriteResult,
};
use rdy_graph::DerivedStatus;
use rdy_model::assessment::PolicyVerdict;
use rdy_model::engine_fields::{ADOPTED_AS, AdoptedAs, decode_adopted_as, encode_adopted_as};
use rdy_model::foreign::{
    ForeignDerived, ForeignId, ForeignPin, ForeignPublication, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::plan::PlanKind;
use rdy_model::{Block, BlockId, FieldValue, Hash, RepoId};
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY};
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

/// The fix of a write that names `adopted_as`.
const FIX: &str = "remove adopted_as; use rdy_supersede with adopted_as";

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

/// A cited block, as its home project delivers it.
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

/// The cited decision that the home project accepted as the adopted copy of a local one.
fn upstream() -> Block {
    block(
        "D-PSTRM",
        "decision",
        &[
            ("statement", "Every job logs in UTC."),
            ("status", "accepted"),
        ],
        "",
    )
}

/// A cited claim: delivered, but no decision.
fn cited_claim() -> Block {
    block("c-FRGNC", "claim", &[], "Every service logs in UTC.")
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

/// Takes the delivery of `selected` for link `link`.
fn deliver(r: &mut Rdy, dir: &Path, name: &str, link: &BlockId, selected: &[&Block]) {
    let source = ForeignSource::Link {
        link: link.to_string(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
    };
    let _: DeliveryTaken = take_delivery(r, &stream(dir, name, selected), &source, None)
        .unwrap_or_else(|e| panic!("take {name}: {e:?}"));
}

/// The adoption of the upstream decision as delivered.
fn adopted() -> AdoptedAs {
    let up = upstream();
    AdoptedAs {
        target: ForeignId::new(cited(), up.id.clone()),
        pin: ForeignPin {
            content: up.content_hash(),
            encoding: 0,
        },
    }
}

/// A project with a draft doc, its section, the repo link `core` and the delivery of the
/// upstream decision and a cited claim; the landed, accepted local decision `old`, and the
/// landed claim `dep` that depends on it.
struct World {
    _d: tempfile::TempDir,
    _files: tempfile::TempDir,
    r: Rdy,
    section: BlockId,
    old: BlockId,
    dep: BlockId,
}

fn world() -> World {
    let (d, mut r) = rdy_qa::fixtures::project();
    let files = tempfile::tempdir().unwrap();
    let w = put(
        &mut r,
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
    land(&mut r, "seed").unwrap();
    let (section, link) = (created(&w, "$s"), created(&w, "$l"));
    deliver(
        &mut r,
        files.path(),
        "v1",
        &link,
        &[&upstream(), &cited_claim()],
    );
    let decided = r
        .decide(
            &writer(),
            "dec",
            DecisionInput {
                parent: section.to_string(),
                statement: "Every job logs in UTC.".into(),
                title: Some("Log time".into()),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("decide: {e:?}"));
    let old = BlockId::parse(&decided.created["$decision"]).unwrap();
    land(&mut r, "dec").unwrap();
    let w = put(
        &mut r,
        "dep",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": section.to_string(),
                "body": "Our billing jobs log in UTC.",
                "links": {"depends_on": [old.to_string()]}}]),
    );
    land(&mut r, "dep").unwrap();
    World {
        _d: d,
        _files: files,
        r,
        section,
        old,
        dep: created(&w, "$c"),
    }
}

/// A project with the repo link `core`; the landed, accepted local decision `old` that states
/// `rule`, and the landed claim `dep` that depends on it and states `dep_body`; then the delivery
/// of the upstream copy that `upstream` makes from `old`'s id.
fn rule_world(rule: &str, dep_body: &str, upstream: impl FnOnce(&BlockId) -> Block) -> World {
    let (d, mut r) = rdy_qa::fixtures::project();
    let files = tempfile::tempdir().unwrap();
    let w = put(
        &mut r,
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
    land(&mut r, "seed").unwrap();
    let (section, link) = (created(&w, "$s"), created(&w, "$l"));
    let decided = r
        .decide(
            &writer(),
            "dec",
            DecisionInput {
                parent: section.to_string(),
                statement: rule.into(),
                title: Some("Log retention".into()),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("decide: {e:?}"));
    let old = BlockId::parse(&decided.created["$decision"]).unwrap();
    land(&mut r, "dec").unwrap();
    let w = put(
        &mut r,
        "dep",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": section.to_string(),
                "body": dep_body,
                "links": {"depends_on": [old.to_string()]}}]),
    );
    land(&mut r, "dep").unwrap();
    deliver(&mut r, files.path(), "v1", &link, &[&upstream(&old)]);
    World {
        _d: d,
        _files: files,
        r,
        section,
        old,
        dep: created(&w, "$c"),
    }
}

/// The upstream copy, as decision `id_` that states `rule`.
fn upstream_rule(id_: &str, rule: &str) -> Block {
    block(
        id_,
        "decision",
        &[
            ("title", "Log retention"),
            ("statement", rule),
            ("status", "accepted"),
        ],
        "",
    )
}

/// The kinds of the items the plan of view `view` gives `dep` for cause `old`.
fn dep_kinds(r: &Rdy, view: &str, dep: &BlockId, old: &BlockId) -> Vec<PlanKind> {
    let plan = r.read().view_plan(view).unwrap();
    plan.items
        .iter()
        .filter_map(|row| match row {
            PlanRow::Live(i) if i.subject == dep.as_str() && i.cause == old.as_str() => {
                Some(i.kind)
            }
            _ => None,
        })
        .collect()
}

fn canon_block(r: &Rdy, b: &BlockId) -> Block {
    r.read().block(&Scope::Canon, b).unwrap()
}

fn status(r: &Rdy, scope: &Scope, b: &BlockId) -> Option<DerivedStatus> {
    r.read().graph(scope).unwrap().status(b)
}

fn adopt(r: &mut Rdy, view: &str, old: &BlockId, target: &str) -> Result<WriteResult, RdyError> {
    r.supersede_adopted_as(&writer(), view, old.as_str(), target, None, None)
}

fn refused(r: Result<WriteResult, RdyError>, code: &str, message: &str) -> RdyError {
    let e = r.expect_err("refused");
    assert_eq!(e.code, code, "{e:#?}");
    assert_eq!(e.message, message, "{e:#?}");
    e
}

#[test]
fn rpl21_accepted_decision_upstream_adoption_succeeds() {
    let mut w = world();
    let before = canon_block(&w.r, &w.old);
    let cited_before =
        w.r.read()
            .foreign()
            .unwrap()
            .current_version(&adopted().target)
            .cloned();
    let res = adopt(&mut w.r, "up", &w.old, "core:D-PSTRM").unwrap();
    // one update of the old decision, in the session's view; it writes adopted_as and nothing else
    let ch = w.r.store().change(&res.change.parse().unwrap()).unwrap();
    assert_eq!(ch.ops.len(), 1, "{:#?}", ch.ops);
    let view = Scope::View("up".into());
    let written = w.r.read().block(&view, &w.old).unwrap();
    assert_eq!(
        decode_adopted_as(&written.fields[ADOPTED_AS]).unwrap(),
        adopted()
    );
    assert_eq!(
        rdy_model::decision::adopted_as_written(&before, &written),
        Some(adopted())
    );
    // a person lands it: canon records it on the still accepted decision
    land(&mut w.r, "up").unwrap();
    let after = canon_block(&w.r, &w.old);
    assert_eq!(after.fields[ADOPTED_AS], encode_adopted_as(&adopted()));
    assert_eq!(after.field_str("status"), Some("accepted"));
    assert_eq!(
        after.content_hash(),
        before.content_hash(),
        "no pin goes stale"
    );
    assert_eq!(after.foreign, before.foreign);
    assert_eq!(after.pins, before.pins);
    // its status is superseded, by the cited decision
    assert_eq!(
        status(&w.r, &Scope::Canon, &w.old),
        Some(DerivedStatus::Superseded {
            by: vec![id("D-PSTRM")]
        })
    );
    // the cited decision is not written: its current delivered version is the same
    assert_eq!(
        w.r.read()
            .foreign()
            .unwrap()
            .current_version(&adopted().target)
            .cloned(),
        cited_before
    );
    // the plain supersede still takes its decision input and gives its results
    let res =
        w.r.supersede(
            &writer(),
            "plain",
            w.dep.as_str(),
            None,
            DecisionInput {
                parent: w.section.to_string(),
                statement: "x".into(),
                ..Default::default()
            },
        )
        .expect_err("a claim is no decision");
    assert_eq!(res.code, E_BAD_INPUT, "{res:#?}");
}

#[test]
fn rpl21_adopted_as_supersedes_and_offers_rebinds() {
    let mut w = world();
    assert_eq!(
        status(&w.r, &Scope::Canon, &w.old),
        Some(DerivedStatus::Live)
    );
    adopt(&mut w.r, "up", &w.old, "core:D-PSTRM").unwrap();
    // in the view the old decision reads as superseded by the cited one, and its dependent is
    // suspect for it
    let view = Scope::View("up".into());
    assert_eq!(
        status(&w.r, &view, &w.old),
        Some(DerivedStatus::Superseded {
            by: vec![id("D-PSTRM")]
        })
    );
    let suspects = w.r.read().suspects(&view).unwrap();
    assert!(
        suspects
            .iter()
            .any(|s| s.block == w.dep && s.target == w.old && s.reason == "superseded"),
        "{suspects:#?}"
    );
    // the version's plan offers the dependent's item, through the existing plan path
    let plan = w.r.read().view_plan("up").unwrap();
    let items: Vec<_> = plan
        .items
        .iter()
        .filter_map(|row| match row {
            PlanRow::Live(i) => Some(i),
            _ => None,
        })
        .collect();
    assert!(
        items.iter().any(|i| i.subject == w.dep.as_str()
            && i.cause == w.old.as_str()
            && i.kind == PlanKind::Rejustify),
        "the upstream copy states what the old decision stated: {items:#?}"
    );
    // landed, canon keeps the status and the dependent's suspect
    land(&mut w.r, "up").unwrap();
    assert_eq!(
        status(&w.r, &Scope::Canon, &w.old),
        Some(DerivedStatus::Superseded {
            by: vec![id("D-PSTRM")]
        })
    );
    let suspects = w.r.read().suspects(&Scope::Canon).unwrap();
    assert!(
        suspects
            .iter()
            .any(|s| s.block == w.dep && s.target == w.old && s.reason == "superseded"),
        "{suspects:#?}"
    );
    // the refusals: what is not an accepted local decision, or not a delivered decision
    let w2 = put(
        &mut w.r,
        "p",
        json!([{"op": "create", "type": "decision", "id": "$p", "parent": w.section.to_string(),
                "fields": {"statement": "Queues stay small.", "status": "proposed"}}]),
    );
    let proposed = created(&w2, "$p");
    refused(
        adopt(&mut w.r, "p", &proposed, "core:D-PSTRM"),
        E_BAD_INPUT,
        &format!("{proposed} is proposed, not accepted, so it has no upstream copy to adopt"),
    );
    refused(
        adopt(&mut w.r, "x", &w.old, "core:D-PSTRM"),
        E_BAD_INPUT,
        &format!("{} is superseded already", w.old),
    );
    refused(
        adopt(&mut w.r, "x", &w.dep, "core:D-PSTRM"),
        E_BAD_INPUT,
        &format!("{} is a claim, not a decision", w.dep),
    );
    let fresh = world();
    let mut r = fresh.r;
    refused(
        adopt(&mut r, "x", &fresh.old, "edge:D-PSTRM"),
        E_UNKNOWN_REF,
        "no repo link has the alias edge",
    );
    refused(
        adopt(&mut r, "x", &fresh.old, "core:D-NEVER"),
        E_UNKNOWN_REF,
        "core:D-NEVER is not in the content delivered from core",
    );
    refused(
        adopt(&mut r, "x", &fresh.old, "core:c-FRGNC"),
        E_BAD_INPUT,
        "core:c-FRGNC is a claim, not a decision",
    );
    refused(
        adopt(&mut r, "x", &fresh.old, fresh.dep.as_str()),
        E_BAD_INPUT,
        &format!(
            "adopted_as names {}, which is not a cited decision",
            fresh.dep
        ),
    );
    // the cited decision itself is read-only here
    refused(
        r.supersede_adopted_as(&writer(), "x", "core:D-PSTRM", "core:D-PSTRM", None, None),
        E_FOREIGN_READ_ONLY,
        "core:D-PSTRM is cited from core and is read-only here",
    );
    // a refusal writes nothing
    assert!(r.read().view_head("x").is_none());
}

#[test]
fn rpl21_raw_adopted_as_refuses_and_hash_excludes_it() {
    let mut w = world();
    let value = serde_json::to_value(encode_adopted_as(&adopted())).unwrap();
    let raw_value = json!({
        "content": adopted().pin.content.to_string(),
        "encoding": 0,
        "id": "D-PSTRM",
        "repo": cited().to_string()
    });
    let issue = |e: &RdyError| {
        assert_eq!(e.code, E_VALIDATION, "{e:#?}");
        assert_eq!(e.issues.len(), 1, "{e:#?}");
        let i = &e.issues[0];
        assert_eq!(i.code, "E_FIELD_UNKNOWN", "{e:#?}");
        assert_eq!(i.fix, FIX, "{e:#?}");
        i.message.clone()
    };
    for v in [&value, &raw_value] {
        // an update that sets it
        let e = write(
            &mut w.r,
            "raw",
            json!([{"op": "update", "id": w.old.to_string(), "fields": {"adopted_as": v}}]),
        )
        .expect_err("refused");
        assert_eq!(
            issue(&e),
            format!("{} sets \"adopted_as\", and rdy sets it", w.old)
        );
        // a create that sets it
        let e = write(
            &mut w.r,
            "raw",
            json!([{"op": "create", "type": "decision", "parent": w.section.to_string(),
                    "fields": {"statement": "x", "status": "proposed", "adopted_as": v}}]),
        )
        .expect_err("refused");
        issue(&e);
        // rdy_decide's own fields
        let e =
            w.r.decide(
                &writer(),
                "raw",
                DecisionInput {
                    parent: w.section.to_string(),
                    statement: "x".into(),
                    fields: BTreeMap::from([("adopted_as".to_string(), v.clone())]),
                    ..Default::default()
                },
            )
            .expect_err("refused");
        issue(&e);
    }
    // an update that unsets it, after it landed
    adopt(&mut w.r, "up", &w.old, "core:D-PSTRM").unwrap();
    land(&mut w.r, "up").unwrap();
    let e = write(
        &mut w.r,
        "raw",
        json!([{"op": "update", "id": w.old.to_string(), "unset": ["adopted_as"]}]),
    )
    .expect_err("refused");
    issue(&e);
    // no write was made
    assert!(w.r.read().view_head("raw").is_none());
    // the content hash leaves it out, on a local block and on one with a foreign part
    let b = canon_block(&w.r, &w.old);
    let mut without = b.clone();
    without.fields.remove(ADOPTED_AS);
    assert_ne!(b, without);
    assert_eq!(b.content_hash(), without.content_hash());
    let mut foreign = without.clone();
    foreign.foreign.pins.insert(adopted().target, adopted().pin);
    let mut foreign_adopted = foreign.clone();
    foreign_adopted
        .fields
        .insert(ADOPTED_AS.into(), encode_adopted_as(&adopted()));
    assert_eq!(foreign.content_hash(), foreign_adopted.content_hash());
}

// ---- the authority policy never accepts a version that writes adopted_as ----

/// The policy of one rule: agents `agent:a*` may land `two_way` decisions with basis
/// `precedent` in area `payments`.
fn policy_fields() -> Value {
    json!({
        "key": "authority-policy",
        "agent_may_accept": [
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

#[test]
fn rpl21_adopted_as_never_uses_policy() {
    let dir = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
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
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$r", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}},
            {"op": "create", "type": "decision", "id": "$old", "parent": "$ws",
             "fields": {"statement": "Fees round down.", "status": "proposed", "needs_content": false,
                        "reversibility": "two_way", "authority_basis": "precedent"}}
        ]),
    );
    land(&mut r, "seed").unwrap();
    let (rules, dec, link, old) = (
        created(&w, "$r"),
        created(&w, "$dec"),
        created(&w, "$l"),
        created(&w, "$old"),
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
    deliver(&mut r, files.path(), "v1", &link, &[&upstream()]);
    // the policy accepts a version of its own kind: a two_way precedent decision in payments
    put_j(
        &mut r,
        "a1",
        json!([{"op": "create", "type": "decision", "id": "$d", "parent": created(&w, "$ws").to_string(),
                "fields": {"statement": "Fees round up.", "status": "proposed", "needs_content": false,
                           "reversibility": "two_way", "authority_basis": "precedent"}}]),
        j_typo(),
    );
    let p = propose_as(&mut r, "a1");
    assert!(
        matches!(verdict(&r, &p), PolicyVerdict::Accepts { .. }),
        "{:#?}",
        verdict(&r, &p)
    );
    // the adoption by the same agent needs a person, by name
    r.supersede_adopted_as(&ag("a2"), "a2", old.as_str(), "core:D-PSTRM", None, None)
        .unwrap_or_else(|e| panic!("adopt: {e:#?}"));
    let p = propose_as(&mut r, "a2");
    let PolicyVerdict::NeedsPerson { reasons, .. } = verdict(&r, &p) else {
        panic!("{:#?}", verdict(&r, &p))
    };
    assert!(
        reasons.contains(&format!("it writes adopted_as on {old}")),
        "{reasons:#?}"
    );
    let e = r
        .land_by_policy(&ag("a2"), &p.r#ref)
        .expect_err("the policy does not accept it");
    assert_eq!(e.code, E_POLICY_HUMAN, "{e:#?}");
    assert!(
        !canon_block(&r, &old).fields.contains_key(ADOPTED_AS),
        "nothing landed"
    );
    // a person lands it
    rdy_qa::intake::confirm_before_landing(&mut r, &approver(), &p);
    r.land(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("a person lands it: {e:#?}"));
    assert_eq!(
        canon_block(&r, &old).fields[ADOPTED_AS],
        encode_adopted_as(&adopted())
    );
}

/// Agent `agent:<name>` proposes its view `<name>` with a summary; ready.
fn propose_as(r: &mut Rdy, name: &str) -> ProposalInfo {
    rdy_qa::intake::answer_open_items(r, &ag(name), name);
    let p = r
        .propose(&ag(name), name, name, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {name}: {e}"));
    assert!(p.ready, "{name}: {:#?}", p.issues);
    p
}

/// The stored policy verdict of version `p`.
fn verdict(r: &Rdy, p: &ProposalInfo) -> PolicyVerdict {
    let st = r.read();
    let a = st
        .reader()
        .assessment_of_proposal(&h(&p.id), st.seq())
        .unwrap()
        .unwrap_or_else(|| panic!("{} stores an assessment", p.r#ref));
    st.reader()
        .assessment_wire(&a)
        .unwrap()
        .policy
        .expect("a verdict")
        .verdict
}

/// The plan compares the old decision with the delivered successor that the adoption records,
/// read from the foreign log, never with a local block of the same bare id.
#[test]
fn rpl21_adoption_compares_with_the_delivered_successor() {
    const RULE: &str = "Keep logs for 30 days.";
    const DEP: &str = "Billing keeps its logs for 30 days.";
    // (successor id: other or the same as old's, successor rule, the dependent's item)
    let cases = [
        (false, RULE, PlanKind::Rejustify),
        (false, "Keep logs for 60 days.", PlanKind::Update),
        (true, RULE, PlanKind::Rejustify),
        (true, "Keep logs for 60 days.", PlanKind::Update),
    ];
    let mut wrong = vec![];
    for (same_id, rule, want) in cases {
        let mut w = rule_world(RULE, DEP, |old| {
            let id_ = if same_id { old.as_str() } else { "D-PSTRM" };
            upstream_rule(id_, rule)
        });
        let target = if same_id {
            format!("core:{}", w.old)
        } else {
            "core:D-PSTRM".to_string()
        };
        adopt(&mut w.r, "up", &w.old, &target).unwrap_or_else(|e| panic!("adopt {target}: {e:?}"));
        let got = dep_kinds(&w.r, "up", &w.dep, &w.old);
        if got != [want] {
            wrong.push(format!(
                "same id {same_id}, successor states {rule:?}: {got:?}, want {want:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
