//! ap4 (D-ZWP7Q, D-TF78H, D-A8J01; O10, O11, O12): the live policy is the authority_policy block
//! in canon at its head; owner entries resolve to the most specific one; the first matching
//! `require` entry names an item's policy role, and a role holds its listed humans and, for a
//! team, every person.
mod common;

use rdy_engine::pending::ItemKind;
use rdy_engine::policy::owners::owner_entry;
use rdy_engine::policy::roles::{ItemFacts, RequiredRole, holds, required_role};
use rdy_engine::policy::{LivePolicy, live_policy};
use rdy_engine::*;
use rdy_model::assessment::PolicyRef;
use rdy_model::*;
use rdy_schema::policy::{OwnerEntry, PolicyFields, Require, When};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// A pack with two areas, `payments` and `identity`, extending `rdy-standard`.
const TEAM_PACK: &str = "\
name: team
version: \"1\"
extends: rdy-standard
areas:
  payments:
    paths: [payments/**]
  identity:
    paths: [identity/**]
";

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn work() -> Scope {
    Scope::View("work".into())
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A project whose pack is `team`: initialised with `rdy-standard`, then switched.
fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    use_pack(dir.path(), "team", TEAM_PACK);
    let r = Rdy::open(dir.path()).unwrap();
    (dir, r)
}

fn use_pack(dir: &Path, name: &str, yaml: &str) {
    std::fs::write(dir.join(format!(".rdy/packs/{name}.yaml")), yaml).unwrap();
    std::fs::write(dir.join(".rdy.yaml"), format!("pack: {name}\n")).unwrap();
}

fn write(r: &mut Rdy, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "write to work".into(),
        },
    )
    .unwrap()
}

/// Proposes view `work` under `title` and lands it.
fn land(r: &mut Rdy, title: &str) {
    let p = r
        .propose(&writer(), "work", title, Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
}

/// The policy fields of a block written as JSON: no rule, the role `approvers` of one human,
/// and `require` and `owners` as given.
fn policy_json(require: Value, owners: Value) -> Value {
    json!({
        "key": "authority-policy",
        "agent_may_accept": [],
        "roles": {"approvers": ["human:ann"]},
        "require": require,
        "owners": owners,
    })
}

/// Writes a doc `rules` with a decision and a policy block under it, in `work`; the policy's
/// change implements the decision, as a change to the policy must (ap8, O19). Returns (doc,
/// policy).
fn write_policy(r: &mut Rdy, fields: Value) -> (BlockId, BlockId) {
    let res = write(
        r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
    );
    let doc = id(&res.created["$d"]);
    let decision = id(&res.created["$dec"]);
    let res = r
        .write(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "authority_policy", "id": "$p", "parent": doc.to_string(), "fields": fields}
                ]))
                .unwrap(),
                justification: Some(Justification::Implements(vec![decision])),
                message: "write the policy".into(),
            },
        )
        .unwrap();
    (doc, id(&res.created["$p"]))
}

fn canon_policy(r: &Rdy) -> Option<LivePolicy> {
    let st = r.read();
    let canon = st.materialized(&Scope::Canon).unwrap();
    live_policy(&st, &canon)
}

/// The tree in `work`: payments/ { cards/ { visa (doc: section, claim) }, fees (doc: claim) },
/// identity/login (doc: claim). Returns the created ids by name.
fn tree(r: &mut Rdy) -> BTreeMap<&'static str, BlockId> {
    let res = write(
        r,
        json!([
            {"op": "create", "type": "folder", "id": "$pay", "fields": {"name": "payments"}},
            {"op": "create", "type": "folder", "id": "$cards", "parent": "$pay", "fields": {"name": "cards"}},
            {"op": "create", "type": "doc", "id": "$visa", "parent": "$cards", "fields": {"name": "visa", "title": "Visa", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$vs", "parent": "$visa", "fields": {"title": "S"}},
            {"op": "create", "type": "claim", "id": "$vc", "parent": "$vs", "body": "x"},
            {"op": "create", "type": "doc", "id": "$fees", "parent": "$pay", "fields": {"name": "fees", "title": "Fees", "doc_type": "page"}},
            {"op": "create", "type": "claim", "id": "$fc", "parent": "$fees", "body": "y"},
            {"op": "create", "type": "folder", "id": "$idf", "fields": {"name": "identity"}},
            {"op": "create", "type": "doc", "id": "$login", "parent": "$idf", "fields": {"name": "login", "title": "Login", "doc_type": "page"}},
            {"op": "create", "type": "claim", "id": "$lc", "parent": "$login", "body": "z"}
        ]),
    );
    let mut out = BTreeMap::new();
    for (name, key) in [
        ("payments", "$pay"),
        ("cards", "$cards"),
        ("visa", "$visa"),
        ("visa_section", "$vs"),
        ("visa_claim", "$vc"),
        ("fees", "$fees"),
        ("fees_claim", "$fc"),
        ("identity", "$idf"),
        ("login", "$login"),
        ("login_claim", "$lc"),
    ] {
        out.insert(name, id(&res.created[key]));
    }
    out
}

fn owner(path: &str, owners: &[&str]) -> OwnerEntry {
    OwnerEntry {
        path: path.into(),
        owners: owners.iter().map(|o| o.to_string()).collect(),
    }
}

fn fields(require: Vec<Require>, owners: Vec<OwnerEntry>) -> PolicyFields {
    PolicyFields {
        rules: vec![],
        roles: BTreeMap::from([
            ("approvers".to_string(), vec!["human:ann".to_string()]),
            ("deciders".to_string(), vec!["human:bob".to_string()]),
            ("payers".to_string(), vec!["team:payments".to_string()]),
            ("everyone".to_string(), vec!["human:cat".to_string()]),
        ]),
        require,
        owners,
    }
}

fn req(when: When, role: Option<&str>) -> Require {
    Require {
        when,
        role: role.map(str::to_string),
    }
}

fn item(kind: ItemKind, blocks: &[&BlockId], one_way: bool, decision: bool) -> ItemFacts {
    ItemFacts {
        kind,
        blocks: blocks.iter().map(|b| (*b).clone()).collect(),
        one_way,
        decision,
    }
}

fn role(name: &str, members: &[&str]) -> RequiredRole {
    RequiredRole::Role {
        name: name.into(),
        members: members.iter().map(|m| m.to_string()).collect(),
    }
}

// ---------------------------------------------------------------------------------------------
// the live policy

#[test]
fn ap4_no_policy_block_gives_none() {
    let (_d, mut r) = project();
    assert!(canon_policy(&r).is_none(), "an empty canon holds no policy");
    let t = tree(&mut r);
    land(&mut r, "content");
    assert!(
        canon_policy(&r).is_none(),
        "canon holds content and no policy"
    );
    assert!(
        r.read()
            .materialized(&Scope::Canon)
            .unwrap()
            .get(&t["visa"])
            .is_some()
    );
}

#[test]
fn ap4_live_policy_reads_the_block_and_its_version() {
    let (_d, mut r) = project();
    let (_doc, ap) = write_policy(
        &mut r,
        policy_json(json!([{"when": "any", "role": "approvers"}]), json!([])),
    );
    land(&mut r, "policy");
    let st = r.read();
    let canon = st.materialized(&Scope::Canon).unwrap();
    let live = live_policy(&st, &canon).expect("canon holds the policy");
    let version = canon.version_of(&ap).unwrap();
    assert_eq!(live.block, ap);
    assert_eq!(live.version, version);
    assert_eq!(
        live.reference(),
        PolicyRef {
            block: ap.clone(),
            version,
        }
    );
    assert_eq!(live.label(), format!("{ap}@{}", version.short()));
    let f = live.fields.expect("the policy validates");
    assert_eq!(f.require, vec![req(When::Any, Some("approvers"))]);
    assert_eq!(
        f.roles,
        BTreeMap::from([("approvers".to_string(), vec!["human:ann".to_string()])])
    );
}

#[test]
fn ap4_live_policy_reads_canon_not_the_view() {
    let (_d, mut r) = project();
    let (_doc, ap) = write_policy(&mut r, policy_json(json!([]), json!([])));
    assert!(
        canon_policy(&r).is_none(),
        "a policy written in a view is not the live policy"
    );
    land(&mut r, "policy");
    let before = canon_policy(&r).expect("landed");
    write(
        &mut r,
        json!([{"op": "update", "id": ap.to_string(), "fields": {"require": [{"when": "any", "role": "approvers"}]}}]),
    );
    let st = r.read();
    let view = st.materialized(&work()).unwrap();
    assert_ne!(
        view.version_of(&ap),
        Some(before.version),
        "the view holds a newer version"
    );
    let after = canon_policy(&r).expect("still landed");
    assert_eq!(
        after.version, before.version,
        "canon at its head is unchanged"
    );
    assert_eq!(after.fields, before.fields);
    assert_eq!(after.fields.unwrap().require, vec![]);
}

#[test]
fn ap4_a_policy_that_no_longer_validates_reads_with_no_fields() {
    let (dir, mut r) = project();
    let t = tree(&mut r);
    let (_doc, ap) = write_policy(
        &mut r,
        policy_json(
            json!([{"when": "area:payments", "role": "approvers"}]),
            json!([]),
        ),
    );
    land(&mut r, "policy");
    assert!(
        canon_policy(&r).unwrap().fields.is_some(),
        "valid under team"
    );
    drop(r);
    // the pack change removes the area `payments`, which the policy names
    use_pack(
        dir.path(),
        "team",
        "name: team\nversion: \"2\"\nextends: rdy-standard\nareas:\n  identity:\n    paths: [identity/**]\n",
    );
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    let canon = st.materialized(&Scope::Canon).unwrap();
    let live = live_policy(&st, &canon).expect("the block is still in canon");
    assert_eq!(live.block, ap);
    assert_eq!(live.version, canon.version_of(&ap).unwrap());
    assert!(live.fields.is_none(), "the block no longer validates");
    let kinds = [
        ItemKind::Approve,
        ItemKind::Confirm,
        ItemKind::Answer,
        ItemKind::Resolve,
        ItemKind::Defer,
        ItemKind::Reaffirm,
    ];
    for kind in kinds {
        let facts = item(kind, &[&t["fees_claim"]], true, true);
        assert_eq!(
            live.fields
                .as_ref()
                .and_then(|p| required_role(&st, &canon, p, &facts)),
            None,
            "{kind:?}: no role applies under a policy that does not validate"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// owners (O12)

#[test]
fn ap4_a_block_entry_wins_over_its_doc_and_folder() {
    let (_d, mut r) = project();
    let t = tree(&mut r);
    let st = r.read();
    let m = st.materialized(&work()).unwrap();
    let claim = t["visa_claim"].to_string();
    let p = fields(
        vec![],
        vec![
            owner("payments", &["human:folder"]),
            owner("payments/cards/visa", &["human:doc"]),
            owner(&claim, &["human:block"]),
        ],
    );
    let e = owner_entry(&m, &p, &t["visa_claim"]).expect("owned");
    assert_eq!(e.path, claim);
    assert_eq!(e.owners, vec!["human:block".to_string()]);
    let e = owner_entry(&m, &p, &t["visa_section"]).expect("its doc's entry");
    assert_eq!(e.path, "payments/cards/visa");
    let e = owner_entry(&m, &p, &t["visa"]).expect("the doc's own entry");
    assert_eq!(e.path, "payments/cards/visa");
    let e = owner_entry(&m, &p, &t["fees_claim"]).expect("the folder above it");
    assert_eq!(e.path, "payments");
    assert!(
        owner_entry(&m, &p, &t["login_claim"]).is_none(),
        "no entry covers identity"
    );
}

#[test]
fn ap4_a_deeper_folder_wins_over_a_shallower_one() {
    let (_d, mut r) = project();
    let t = tree(&mut r);
    let st = r.read();
    let m = st.materialized(&work()).unwrap();
    let p = fields(
        vec![],
        vec![
            owner("payments", &["human:shallow"]),
            owner("payments/cards", &["human:deep"]),
        ],
    );
    assert_eq!(
        owner_entry(&m, &p, &t["visa_claim"]).unwrap().path,
        "payments/cards"
    );
    assert_eq!(
        owner_entry(&m, &p, &t["visa"]).unwrap().path,
        "payments/cards"
    );
    assert_eq!(
        owner_entry(&m, &p, &t["cards"]).unwrap().path,
        "payments/cards",
        "a folder's own entry"
    );
    assert_eq!(
        owner_entry(&m, &p, &t["fees_claim"]).unwrap().path,
        "payments"
    );
    // the list order does not change which folder is deeper
    let p = fields(
        vec![],
        vec![
            owner("payments/cards", &["human:deep"]),
            owner("payments", &["human:shallow"]),
        ],
    );
    assert_eq!(
        owner_entry(&m, &p, &t["visa_claim"]).unwrap().path,
        "payments/cards"
    );
    // two texts for one folder: the first in the list wins
    let p = fields(
        vec![],
        vec![
            owner("Payments", &["human:first"]),
            owner("payments", &["human:second"]),
        ],
    );
    assert_eq!(
        owner_entry(&m, &p, &t["fees_claim"]).unwrap().owners,
        vec!["human:first".to_string()]
    );
}

#[test]
fn ap4_an_entry_that_names_nothing_is_skipped() {
    let (_d, mut r) = project();
    let t = tree(&mut r);
    let st = r.read();
    let m = st.materialized(&work()).unwrap();
    let p = fields(
        vec![],
        vec![
            owner("payments/nowhere", &["human:ghost"]),
            owner("payments/cards/visa/S", &["human:section"]),
            owner("payments", &["human:folder"]),
        ],
    );
    let e = owner_entry(&m, &p, &t["visa_claim"]).expect("the folder's entry");
    assert_eq!(
        e.path, "payments",
        "a missing path and a section title name nothing"
    );
    let p = fields(vec![], vec![owner("payments/nowhere", &["human:ghost"])]);
    assert!(owner_entry(&m, &p, &t["visa_claim"]).is_none());
}

#[test]
fn ap4_a_block_id_entry_that_names_no_block_is_skipped() {
    let (_d, mut r) = project();
    let t = tree(&mut r);
    let st = r.read();
    let m = st.materialized(&work()).unwrap();
    let absent = "c-ZZZZZ";
    assert!(m.get(&id(absent)).is_none());
    let p = fields(
        vec![],
        vec![
            owner(absent, &["human:ghost"]),
            owner("payments/cards/visa", &["human:doc"]),
        ],
    );
    assert_eq!(
        owner_entry(&m, &p, &t["visa_claim"]).unwrap().path,
        "payments/cards/visa"
    );
    assert!(
        owner_entry(&m, &p, &id(absent)).is_none(),
        "an entry for a block the scope does not hold names nothing"
    );
}

// ---------------------------------------------------------------------------------------------
// required roles (O10)

#[test]
fn ap4_the_first_matching_require_entry_names_the_role() {
    let (_d, mut r) = project();
    let t = tree(&mut r);
    let st = r.read();
    let m = st.materialized(&work()).unwrap();
    let p = fields(
        vec![
            req(When::Decision, Some("deciders")),
            req(When::Any, Some("everyone")),
        ],
        vec![],
    );
    let decides = item(ItemKind::Approve, &[&t["visa_claim"]], false, true);
    assert_eq!(
        required_role(&st, &m, &p, &decides),
        Some(role("deciders", &["human:bob"]))
    );
    let edits = item(ItemKind::Approve, &[&t["visa_claim"]], false, false);
    assert_eq!(
        required_role(&st, &m, &p, &edits),
        Some(role("everyone", &["human:cat"]))
    );
    // listed the other way round, `any` matches first
    let p = fields(
        vec![
            req(When::Any, Some("everyone")),
            req(When::Decision, Some("deciders")),
        ],
        vec![],
    );
    assert_eq!(
        required_role(&st, &m, &p, &decides),
        Some(role("everyone", &["human:cat"]))
    );
    // no entry matches
    let p = fields(vec![req(When::Deferral, Some("approvers"))], vec![]);
    assert_eq!(required_role(&st, &m, &p, &decides), None);
    let p = fields(vec![], vec![]);
    assert_eq!(required_role(&st, &m, &p, &decides), None);
    // deferral and reaffirm match their item kinds only
    let p = fields(
        vec![
            req(When::Deferral, Some("approvers")),
            req(When::Reaffirm, Some("deciders")),
        ],
        vec![],
    );
    assert_eq!(
        required_role(
            &st,
            &m,
            &p,
            &item(ItemKind::Defer, &[&t["fees_claim"]], false, false)
        ),
        Some(role("approvers", &["human:ann"]))
    );
    assert_eq!(
        required_role(
            &st,
            &m,
            &p,
            &item(ItemKind::Reaffirm, &[&t["fees_claim"]], false, false)
        ),
        Some(role("deciders", &["human:bob"]))
    );
    assert_eq!(
        required_role(
            &st,
            &m,
            &p,
            &item(ItemKind::Approve, &[&t["fees_claim"]], false, false)
        ),
        None
    );
}

#[test]
fn ap4_one_way_matches_a_reaffirmed_one_way_decision() {
    let (_d, mut r) = project();
    let t = tree(&mut r);
    let st = r.read();
    let m = st.materialized(&work()).unwrap();
    let p = fields(
        vec![
            req(When::OneWay, Some("deciders")),
            req(When::Any, Some("everyone")),
        ],
        vec![],
    );
    // a reaffirm item whose member decision is one_way: it writes no decision
    let reaffirm = item(ItemKind::Reaffirm, &[&t["fees_claim"]], true, false);
    assert_eq!(
        required_role(&st, &m, &p, &reaffirm),
        Some(role("deciders", &["human:bob"]))
    );
    let approve = item(ItemKind::Approve, &[&t["fees_claim"]], true, true);
    assert_eq!(
        required_role(&st, &m, &p, &approve),
        Some(role("deciders", &["human:bob"]))
    );
    let two_way = item(ItemKind::Reaffirm, &[&t["fees_claim"]], false, false);
    assert_eq!(
        required_role(&st, &m, &p, &two_way),
        Some(role("everyone", &["human:cat"]))
    );
}

#[test]
fn ap4_area_matches_a_covered_block_in_the_area() {
    let (_d, mut r) = project();
    let t = tree(&mut r);
    let st = r.read();
    let m = st.materialized(&work()).unwrap();
    let p = fields(
        vec![
            req(When::Area("payments".into()), Some("payers")),
            req(When::Area("identity".into()), Some("approvers")),
        ],
        vec![],
    );
    assert_eq!(
        required_role(
            &st,
            &m,
            &p,
            &item(ItemKind::Approve, &[&t["login_claim"]], false, false)
        ),
        Some(role("approvers", &["human:ann"]))
    );
    // one covered block in payments is enough, whatever the other blocks are
    assert_eq!(
        required_role(
            &st,
            &m,
            &p,
            &item(
                ItemKind::Approve,
                &[&t["login_claim"], &t["visa_claim"]],
                false,
                false
            )
        ),
        Some(role("payers", &["team:payments"]))
    );
    // a block in no area, and a block the scope does not hold, match no area entry
    let (doc, _ap) = {
        drop(st);
        drop(m);
        write_policy(&mut r, policy_json(json!([]), json!([])))
    };
    let st = r.read();
    let m = st.materialized(&work()).unwrap();
    assert_eq!(
        required_role(
            &st,
            &m,
            &p,
            &item(ItemKind::Approve, &[&doc, &id("c-ZZZZZ")], false, false)
        ),
        None
    );
}

#[test]
fn ap4_owner_names_the_owners_of_the_first_owned_block() {
    let (_d, mut r) = project();
    let t = tree(&mut r);
    let st = r.read();
    let m = st.materialized(&work()).unwrap();
    let p = fields(
        vec![req(When::Owner, None), req(When::Any, Some("everyone"))],
        vec![
            owner("identity", &["human:ida"]),
            owner("payments/cards", &["human:carl", "team:cards"]),
        ],
    );
    let mut blocks = [&t["visa_claim"], &t["login_claim"]];
    blocks.sort();
    // the first covered block in block id order that has an entry names the owners
    let first = blocks[0];
    let expected = if first == &t["visa_claim"] {
        RequiredRole::Owners {
            path: "payments/cards".into(),
            members: vec!["human:carl".into(), "team:cards".into()],
        }
    } else {
        RequiredRole::Owners {
            path: "identity".into(),
            members: vec!["human:ida".into()],
        }
    };
    for order in [[blocks[1], blocks[0]], [blocks[0], blocks[1]]] {
        let both = item(ItemKind::Approve, &order, false, false);
        assert_eq!(
            required_role(&st, &m, &p, &both),
            Some(expected.clone()),
            "the item's block order does not matter"
        );
    }
    assert_eq!(
        expected.label(),
        match &expected {
            RequiredRole::Owners { path, .. } => format!("owners of {path}"),
            _ => unreachable!(),
        }
    );
    // a block with no entry is passed over for the next one that has one
    let one = item(
        ItemKind::Approve,
        &[&t["fees_claim"], &t["visa_claim"]],
        false,
        false,
    );
    let got = required_role(&st, &m, &p, &one).unwrap();
    assert_eq!(
        got,
        RequiredRole::Owners {
            path: "payments/cards".into(),
            members: vec!["human:carl".into(), "team:cards".into()],
        }
    );
    assert_eq!(got.label(), "owners of payments/cards");
    // no covered block has an entry: `owner` does not match, and `any` does
    let none = item(ItemKind::Approve, &[&t["fees_claim"]], false, false);
    assert_eq!(
        required_role(&st, &m, &p, &none),
        Some(role("everyone", &["human:cat"]))
    );
    assert_eq!(role("everyone", &["human:cat"]).label(), "everyone");
}

// ---------------------------------------------------------------------------------------------
// holds (O11)

#[test]
fn ap4_holds_a_listed_human_and_any_person_for_a_team() {
    let r = role("approvers", &["human:ann", "human:bob"]);
    assert!(holds(&r, "human:ann"));
    assert!(holds(&r, "human:bob"));
    let t = role("payers", &["team:payments"]);
    assert!(holds(&t, "human:ann"));
    assert!(holds(&t, "human:zed"));
    let o = RequiredRole::Owners {
        path: "payments".into(),
        members: vec!["human:carl".into(), "team:cards".into()],
    };
    assert!(holds(&o, "human:carl"));
    assert!(
        holds(&o, "human:anyone"),
        "the team holds every person locally"
    );
}

#[test]
fn ap4_holds_refuses_an_unlisted_human() {
    let r = role("approvers", &["human:ann"]);
    assert!(!holds(&r, "human:bob"));
    assert!(!holds(&r, "human:an"), "a prefix is not the actor");
    assert!(!holds(&r, "human:ann2"));
    assert!(!holds(&r, "ann"), "the bare name is not the actor");
    let o = RequiredRole::Owners {
        path: "payments".into(),
        members: vec!["human:carl".into()],
    };
    assert!(!holds(&o, "human:ann"));
}
