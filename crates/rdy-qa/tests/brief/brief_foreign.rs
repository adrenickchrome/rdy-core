//! rpl28 (repo links, D-DAPR8): a briefing over blocks that cite a linked project adds a `links`
//! row for each cited repo, and a `governs` row for each cited decision that governs a covered
//! block. The row shows the version in force (the pin), the delivered version, `newer`, the tag
//! and time of the delivery, and the live deviations from it with their reasons. A delivery moves
//! no pin, so it never stales a briefing: the token, the digest and the covered blocks stay.
use rdy_engine::brief::Coverage;
use rdy_engine::{DecisionInput, Rdy, Scope, rfc3339};
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignPublication, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::{Block, BlockId, FieldValue, Hash, RepoId};
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::intake::{TEST_SUMMARY, answer_open_items, confirm_before_landing};
use rdy_store::{codec, kinds};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const PROFILE: &str = "pf-AGNT1";
const AT_MS: i64 = 1_760_000_000_000;

fn cited() -> RepoId {
    RepoId([7; 16])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> rdy_engine::WriteResult {
    write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
}

fn decision(id_: &str, statement: &str) -> Block {
    Block {
        id: id(id_),
        block_type: "decision".into(),
        parent: None,
        order: "V".into(),
        fields: [("statement", statement), ("status", "accepted")]
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

fn version_of(b: &Block) -> Hash {
    Hash::of(&codec::item_new(kinds::BLOCK, b).1)
}

fn fv(b: &Block) -> ForeignVersion {
    ForeignVersion {
        id: b.id.clone(),
        version: version_of(b),
        content: Hash::of(format!("content {} {:?}", b.id, b.fields).as_bytes()),
        encoding: 0,
        derived: ForeignDerived::default(),
    }
}

/// The chain of the one source of the link block `link`.
struct Source {
    link: String,
    head: Option<Hash>,
    sel: BTreeMap<BlockId, Hash>,
}

impl Source {
    /// Writes a delivery of tag `tag` straight to the store and publishes the state after it.
    fn deliver(&mut self, r: &mut Rdy, tag: &str, added: &[&Block], changed: &[&Block]) {
        for b in added.iter().chain(changed) {
            self.sel.insert(b.id.clone(), version_of(b));
        }
        let entry = ForeignEntry {
            repo: cited(),
            source: ForeignSource::Link {
                link: self.link.clone(),
                site: "hub.example".into(),
                at_ms: AT_MS,
            },
            event: ForeignEvent::Delivery {
                prev: self.head,
                publication: ForeignPublication {
                    tag: tag.into(),
                    profile: PROFILE.into(),
                    profile_version: None,
                },
                set: None,
                added: added.iter().map(|b| fv(b)).collect(),
                changed: changed.iter().map(|b| fv(b)).collect(),
                removed: vec![],
                shells: vec![],
                selection: selection_identity(self.sel.iter()).unwrap(),
            },
        };
        let blocks: Vec<Block> = added.iter().chain(changed).map(|b| (*b).clone()).collect();
        self.head = Some(r.store_mut().put_foreign(&entry, &blocks).unwrap());
        r.publish_after_foreign_for_tests().unwrap();
    }
}

struct World {
    _d: tempfile::TempDir,
    r: Rdy,
    source: Source,
    sec: String,
    claim: String,
    local: String,
    x: Block,
}

/// A landed doc with a section, a local claim that cites nothing, the link `core`, the first
/// delivery of the cited decision X, and a claim decided by X.
fn world() -> World {
    let (d, mut r) = rdy_qa::fixtures::project();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"title": "Guide", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$l", "parent": "$s", "body": "Local claim."},
            {"op": "create", "type": "repo_link", "id": "$link", "parent": "$d",
             "fields": {"alias": "core", "repo": cited().to_string(), "profile": PROFILE}}
        ]),
    );
    land(&mut r, "seed").unwrap();
    let x = decision("d-FRGNX", "Use X.");
    let mut source = Source {
        link: w.created["$link"].clone(),
        head: None,
        sel: BTreeMap::new(),
    };
    source.deliver(&mut r, "v1", &[&x], &[]);
    let c = put(
        &mut r,
        "cite",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": w.created["$s"],
                "body": "Logs follow the cited rule.",
                "links": {"decided_by": [{"target": "core:d-FRGNX"}]}}]),
    );
    land(&mut r, "cite").unwrap();
    World {
        _d: d,
        r,
        source,
        sec: w.created["$s"].clone(),
        claim: c.created["$c"].clone(),
        local: w.created["$l"].clone(),
        x,
    }
}

fn brief_json(r: &Rdy, ids: &[&str]) -> Value {
    let b = r
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: ids.iter().map(|s| s.to_string()).collect(),
                path: None,
            },
            None,
        )
        .unwrap();
    serde_json::to_value(&b).unwrap()
}

fn cited_row(b: &Value) -> &Value {
    b["governs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["decision"] == "core:d-FRGNX")
        .unwrap_or_else(|| panic!("a governs row for the cited decision: {b}"))
}

#[test]
fn rpl28_brief_shows_in_force_delivered_and_deviations() {
    let World {
        _d,
        mut r,
        mut source,
        sec,
        claim,
        local,
        x,
    } = world();
    let at = rfc3339(AT_MS);

    // the delivered version is the one in force
    let b = brief_json(&r, &[&claim]);
    let row = cited_row(&b);
    assert_eq!(row["statement"], "Use X.");
    assert_eq!(row["status"], "accepted");
    assert_eq!(row["blocks"], json!([claim]));
    assert_eq!(
        row["cited"],
        json!({"alias": "core", "repo": cited().to_string(),
               "in_force": version_of(&x).to_string(),
               "delivered": version_of(&x).to_string(),
               "newer": false, "tag": "v1", "delivered_at": at})
    );
    assert!(row.get("deviations").is_none(), "{row}");
    assert_eq!(
        b["links"],
        json!([{"alias": "core", "repo": cited().to_string(), "state": "delivered",
                "tag": "v1", "delivered_at": at, "pins_behind": 0}])
    );

    // a delivery changes X: the pin keeps the version in force, and the row shows both
    let x2 = decision("d-FRGNX", "Use X, now.");
    source.deliver(&mut r, "v2", &[], &[&x2]);
    let b = brief_json(&r, &[&claim]);
    let row = cited_row(&b);
    assert_eq!(row["statement"], "Use X.", "the in-force statement shows");
    assert_eq!(row["cited"]["in_force"], version_of(&x).to_string());
    assert_eq!(row["cited"]["delivered"], version_of(&x2).to_string());
    assert_eq!(row["cited"]["newer"], true);
    assert_eq!(row["cited"]["tag"], "v2");
    assert_eq!(row["cited"]["delivered_statement"], "Use X, now.");
    assert_eq!(b["links"][0]["tag"], "v2");
    assert_eq!(b["links"][0]["pins_behind"], 1);

    // a live deviation from X shows with its reason
    let w = r
        .decide(
            &writer(),
            "dv",
            DecisionInput {
                parent: sec.clone(),
                statement: "Billing keeps the old rule.".into(),
                deviates_from: vec!["core:d-FRGNX".into()],
                ..Default::default()
            },
        )
        .unwrap();
    let dec = w.created["$decision"].clone();
    // it is proposed, not in force: the row lists no deviation yet
    assert!(
        cited_row(&brief_json(&r, &[&claim]))
            .get("deviations")
            .is_none()
    );
    answer_open_items(&mut r, &writer(), "dv");
    let p = r
        .propose(&writer(), "dv", "dv", Some(TEST_SUMMARY))
        .unwrap();
    confirm_before_landing(&mut r, &approver(), &p);
    r.land_report(&approver(), &p.id).unwrap();
    let b = brief_json(&r, &[&claim]);
    assert_eq!(
        cited_row(&b)["deviations"],
        json!([{"id": dec, "reason": "Billing keeps the old rule."}])
    );

    // a briefing over blocks that cite nothing is as it was: no links row, no cited row
    let b = brief_json(&r, &[&local]);
    assert!(b.get("links").is_none(), "{b}");
    for g in b["governs"].as_array().unwrap() {
        assert!(
            g.get("cited").is_none() && g.get("deviations").is_none(),
            "{g}"
        );
    }
}

#[test]
fn rpl28_delivery_does_not_stale_brief() {
    let World {
        _d,
        mut r,
        mut source,
        claim,
        ..
    } = world();
    let before = r
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![claim.clone()],
                path: None,
            },
            None,
        )
        .unwrap();

    // a delivery that changes the cited decision
    source.deliver(&mut r, "v2", &[], &[&decision("d-FRGNX", "Use X, now.")]);
    let st = r.read();
    let after = st
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![claim.clone()],
                path: None,
            },
            None,
        )
        .unwrap();
    assert_eq!(after.record.token, before.record.token, "the token stays");
    assert_eq!(
        after.digest, before.digest,
        "so does the digest a cursor vouches for"
    );
    // the old record is not stale: no recorded block moved and no governing route changed
    let m = st.materialized(&Scope::Canon).unwrap();
    let g = st.graph(&Scope::Canon).unwrap();
    assert!(before.record.stale(st.pack(), &m, &g).is_empty());
    assert!(before.record.moved(&m).is_empty());
    // what the delivery changed shows in the rows, not in the token
    let (b, a) = (
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(&after).unwrap(),
    );
    assert_eq!(cited_row(&b)["cited"]["newer"], false);
    assert_eq!(cited_row(&a)["cited"]["newer"], true);
}
