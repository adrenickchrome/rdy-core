//! rpl29 (repo links, D-DAPR8, D-FK58A): a reference or link to a cited block is an excluded
//! publication target. The delivered body lives in the foreign log only, so a selection never
//! holds it and no output shows it.
use rdy_engine::{Rdy, publication};
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignPublication, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::{Block, BlockId, FieldValue, Hash, RepoId};
use rdy_qa::fixtures::{land, write};
use rdy_store::{codec, kinds};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const PROFILE: &str = "pf-AGNT1";
const CITED_BODY: &str = "FOREIGNCANARY statement.";

fn cited() -> RepoId {
    RepoId([7; 16])
}

fn decision(id: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "decision".into(),
        parent: None,
        order: "V".into(),
        fields: [("statement", CITED_BODY), ("status", "accepted")]
            .iter()
            .map(|(k, v)| (k.to_string(), FieldValue::Str((*v).into())))
            .collect(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

/// Delivers `b` as the first delivery of the link block `link`.
fn deliver(r: &mut Rdy, link: &str, b: &Block) {
    let version = Hash::of(&codec::item_new(kinds::BLOCK, b).1);
    let entry = ForeignEntry {
        repo: cited(),
        source: ForeignSource::Link {
            link: link.to_string(),
            site: "hub.example".into(),
            at_ms: 1_760_000_000_000,
        },
        event: ForeignEvent::Delivery {
            prev: None,
            publication: ForeignPublication {
                tag: "v1".into(),
                profile: PROFILE.into(),
                profile_version: None,
            },
            set: None,
            added: vec![ForeignVersion {
                id: b.id.clone(),
                version,
                content: Hash::of(format!("content {}", b.id).as_bytes()),
                encoding: 0,
                derived: ForeignDerived::default(),
            }],
            changed: vec![],
            removed: vec![],
            shells: vec![],
            selection: selection_identity([(&b.id, &version)]).unwrap(),
        },
    };
    r.store_mut()
        .put_foreign(&entry, std::slice::from_ref(b))
        .unwrap();
    r.publish_after_foreign_for_tests().unwrap();
}

struct World {
    _d: tempfile::TempDir,
    r: Rdy,
    claim: String,
    prose: String,
}

/// A public profile selecting `guide`, which holds a prose block that cites the delivered decision
/// by reference and a claim that cites it by link.
fn world() -> World {
    let (d, mut r) = rdy_qa::fixtures::project();
    let w = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$guide",
             "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin",
             "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let (guide, admin) = (w.created["$guide"].clone(), w.created["$admin"].clone());
    let w = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "repo_link", "id": "$link", "parent": admin,
             "fields": {"alias": "core", "repo": cited().to_string(), "profile": PROFILE}},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "pubp", "title": "Public", "audience": "public", "select": [guide]}}
        ]),
    )
    .unwrap();
    let link = w.created["$link"].clone();
    land(&mut r, "w1").unwrap();
    deliver(&mut r, &link, &decision("d-FRGNX"));
    let w = write(
        &mut r,
        "w2",
        json!([
            {"op": "create", "type": "prose", "id": "$p", "parent": guide,
             "body": "See {{core:d-FRGNX}} for the rule."},
            {"op": "create", "type": "claim", "id": "$c", "parent": guide, "body": "Rule applies.",
             "links": {"decided_by": [{"target": "core:d-FRGNX"}]}}
        ]),
    )
    .unwrap();
    land(&mut r, "w2").unwrap();
    World {
        _d: d,
        r,
        claim: w.created["$c"].clone(),
        prose: w.created["$p"].clone(),
    }
}

#[test]
fn rpl29_publication_reports_foreign_target_excluded() {
    let World {
        _d,
        r,
        claim,
        prose,
    } = world();
    let token = format!("{}:d-FRGNX", cited());
    let v = publication::check(&r.read(), "pubp", None, None, None).unwrap();
    let refs = &v["references"];
    // the reference is an excluded target, never a missing one, and it blocks a public profile
    assert_eq!(refs["status"], "blocked", "{v}");
    assert_eq!(refs["excluded"], 1, "{v}");
    assert_eq!(refs["missing"], 0, "{v}");
    assert_eq!(
        refs["rows"],
        json!([{"source": prose, "location": "body", "target": token,
                "classification": "excluded", "occurrences": 1}]),
        "{v}"
    );
    // the link is a link outside the selection: listed apart and never counted
    assert_eq!(refs["links_outside"]["count"], 1, "{v}");
    assert_eq!(
        refs["links_outside"]["rows"],
        json!([{"source": claim, "location": "/links/decided_by/0", "target": token,
                "classification": "excluded", "occurrences": 1}]),
        "{v}"
    );
    // the delivered block is no part of the selection and its text never reaches the report
    let entries = v["selection"]["entries"].as_array().unwrap();
    assert!(!entries.iter().any(|e| e["id"] == json!("d-FRGNX")), "{v}");
    assert!(!Value::to_string(&v).contains("FOREIGNCANARY"), "{v}");
}
