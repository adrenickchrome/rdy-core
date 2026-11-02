//! ui2 (D-Y9DNZ, D-ZWP7Q, D-DDSBJ, D-JQD5X, D-HAG73; TASK-982 O1 and C1): a version whose
//! selected changes carry an imported accepted or rejected decision status needs a person. The
//! approve card computes an `Imported` fact from the selected changes, after `Policy` and before
//! the live check verdicts; the stored assessment keeps its propose-time card. The policy's
//! never-list holds `it carries an imported decision status`, and `rdy_land` refuses such a
//! version, whatever its stored verdict says.
use rdy_engine::pending::{CARD_MAX_TOKENS, Item, ItemKind, PendingQuery, card_hash};
use rdy_engine::policy::live_policy;
use rdy_engine::policy_verdict::IMPORTED_STATUS;
use rdy_engine::{
    E_LIMIT, E_POLICY_HUMAN, E_PROPOSAL_MOVED, POLICY_HUMAN_FIX, ProposalInfo, Rdy, RdyError,
    Scope, Session, WriteRequest, WriteResult,
};
use rdy_model::assessment::{AssessedPolicy, Assessment, PolicyVerdict};
use rdy_model::{BlockId, Hash, Justification};
use rdy_qa::fixtures::{approver, as_agent, land, parse_test_ops, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, answer_open_items, confirm_before_landing};
use rdy_qa::packs::QA_PACK_YAML;
use rdy_qa::tensions::ag;
use rdy_schema::{Issue, Severity};
use serde_json::{Value, json};
use std::path::Path;

// ---------------------------------------------------------------- shared helpers

/// One block row of an export tree's doc file.
fn block_yaml(
    id: &str,
    ty: &str,
    parent: Option<&str>,
    order: &str,
    fields: &[(&str, &str)],
) -> String {
    let mut s = format!("- id: {id}\n  type: {ty}\n");
    if let Some(p) = parent {
        s.push_str(&format!("  parent: {p}\n"));
    }
    s.push_str(&format!("  order: {order}\n  fields:\n"));
    for (k, v) in fields {
        s.push_str(&format!("    {k}: {v}\n"));
    }
    s
}

/// An export tree at `out`: doc `name` (id `doc`, key `name`) with section `section` and one
/// decision per `(id, status)` under it, each `two_way` with basis `precedent`. Every block is by
/// agent:src (run s1) at 100, so the import writes one author change.
pub(crate) fn tree(out: &Path, name: &str, doc: &str, section: &str, decisions: &[(&str, &str)]) {
    std::fs::create_dir_all(out.join("docs")).unwrap();
    std::fs::write(
        out.join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: work\n",
    )
    .unwrap();
    let mut docs = block_yaml(
        doc,
        "doc",
        None,
        "a1",
        &[
            ("doc_type", "overview"),
            ("key", name),
            ("title", "Imported"),
        ],
    );
    docs.push_str(&block_yaml(
        section,
        "section",
        Some(doc),
        "a1",
        &[("title", "Rulings")],
    ));
    for (n, (id, status)) in decisions.iter().enumerate() {
        let statement = format!("Decision {id} holds");
        docs.push_str(&block_yaml(
            id,
            "decision",
            Some(section),
            &format!("a{}", n + 1),
            &[
                ("authority_basis", "precedent"),
                ("needs_content", "false"),
                ("reversibility", "two_way"),
                ("statement", &statement),
                ("status", status),
            ],
        ));
    }
    std::fs::write(out.join("docs").join(format!("{name}.yaml")), docs).unwrap();
    let mut authors = String::new();
    for id in [doc, section]
        .into_iter()
        .chain(decisions.iter().map(|(id, _)| *id))
    {
        authors.push_str(&format!(
            "- id: {id}\n  actor: agent:src\n  run: s1\n  role: writer\n  at_ms: 100\n"
        ));
    }
    std::fs::write(out.join("authors.yaml"), authors).unwrap();
}

/// `s` imports the tree at `out` into `view`.
pub(crate) fn import(r: &mut Rdy, s: &Session, view: &str, out: &Path) -> WriteResult {
    r.import(s, view, out)
        .unwrap_or_else(|e| panic!("import into {view}: {e:?}"))
}

/// A fresh tree under `t` named `name`, imported by `s` into `view`.
pub(crate) fn import_tree(
    r: &mut Rdy,
    s: &Session,
    view: &str,
    t: &Path,
    name: &str,
    ids: (&str, &str),
    decisions: &[(&str, &str)],
) -> WriteResult {
    let out = t.join(name);
    tree(&out, name, ids.0, ids.1, decisions);
    import(r, s, view, &out)
}

/// The card line of an Imported fact over `n` decisions by `by`.
pub(crate) fn imported(n: usize, by: &str) -> String {
    let what = if n == 1 {
        "1 decision".to_string()
    } else {
        format!("{n} decisions")
    };
    format!("Imported: {what} recorded as accepted or rejected elsewhere, by {by}")
}

/// `s` answers the open items of `view` and proposes it with a summary; it is ready.
pub(crate) fn propose_as(r: &mut Rdy, s: &Session, view: &str) -> ProposalInfo {
    answer_open_items(r, s, view);
    let p = r
        .propose(s, view, view, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e:?}"));
    assert!(p.ready, "{view}: {:#?}", p.issues);
    p
}

fn approve_item(r: &Rdy, pref: &str) -> Item {
    let q = r
        .read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap();
    let not_ready = q.not_ready.clone();
    q.items
        .into_iter()
        .find(|i| i.r#ref == pref && i.kind == ItemKind::Approve)
        .unwrap_or_else(|| panic!("an approve item for {pref}: {not_ready:#?}"))
}

fn lines(i: &Item) -> Vec<String> {
    i.facts
        .iter()
        .map(|f| format!("{}: {}", f.name, f.value))
        .collect()
}

/// The card's Imported line, if it has one.
fn imported_line(r: &Rdy, pref: &str) -> Option<String> {
    lines(&approve_item(r, pref))
        .into_iter()
        .find(|l| l.starts_with("Imported: "))
}

/// The assessment version `p` stores, with its hash.
fn stored(r: &Rdy, p: &ProposalInfo) -> (Hash, Assessment) {
    let st = r.read();
    let v: Hash = p.id.parse().unwrap();
    let h = st
        .reader()
        .assessment_of_proposal(&v, st.seq())
        .unwrap()
        .unwrap_or_else(|| panic!("{} stores an assessment", p.r#ref));
    (h, st.reader().assessment(&h).unwrap())
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A pack whose decisions have one tracking field, `disposition` (trk1): an accepted or
/// rejected decision is frozen, but a tracking edit with an editorial reason keeps its status.
const TRACKING_PACK: &str = "\
name: trk
version: \"1\"
extends: rdy-standard
block_types:
  decision:
    prefix: D
    traits: [normative]
    fields:
      disposition: { kind: str }
    tracking: [disposition]
";

/// A project of pack [`TRACKING_PACK`].
fn tracking_project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), TRACKING_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: trk\n").unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    (dir, r)
}

fn write_as(r: &mut Rdy, s: &Session, view: &str, ops: Value) -> WriteResult {
    r.write(
        s,
        WriteRequest {
            view: view.into(),
            ops: parse_test_ops(ops).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("write in {view}: {e:?}"))
}

/// Replays change `src` into a new view `view` as the human writer: the replay.
fn replay(r: &mut Rdy, view: &str, src: &str) -> String {
    r.view_create(&writer(), view, None, false).unwrap();
    let res = r.view_replay(&writer(), view, &[src.to_string()]).unwrap();
    assert_eq!(res.replayed.len(), 1, "{res:?}");
    res.replayed[0].change.clone()
}

// ---------------------------------------------------------------- membership

#[test]
fn ui2_imported_status_membership_is_selected_and_net() {
    let (_d, mut r) = tracking_project();
    let t = tempfile::tempdir().unwrap();
    // accepted and rejected count; proposed does not
    let i1 = ag("i1");
    import_tree(
        &mut r,
        &i1,
        "i1",
        t.path(),
        "one",
        ("d-AA001", "s-AA001"),
        &[
            ("D-AC001", "accepted"),
            ("D-RJ001", "rejected"),
            ("D-PR001", "proposed"),
        ],
    );
    let p1 = propose_as(&mut r, &i1, "i1");
    assert_eq!(imported_line(&r, &p1.r#ref), Some(imported(2, "agent:i1")));

    // a later edit that keeps the status keeps the decision counted (an accepted or rejected
    // decision is frozen: no product writer deletes it or changes its status in a view; the walk's
    // own pins cover those)
    let i2 = ag("i2");
    import_tree(
        &mut r,
        &i2,
        "i2",
        t.path(),
        "two",
        ("d-BB001", "s-BB001"),
        &[("D-AC002", "accepted"), ("D-AC004", "accepted")],
    );
    r.write(
        &i2,
        WriteRequest {
            view: "i2".into(),
            ops: parse_test_ops(
                json!([{"op": "update", "id": "D-AC004", "fields": {"disposition": "later"}}]),
            )
            .unwrap(),
            justification: Some(Justification::Editorial {
                class: rdy_model::EditorialClass::Clarify,
                reason: "track it".into(),
            }),
            message: String::new(),
        },
    )
    .unwrap();
    let p2 = propose_as(&mut r, &i2, "i2");
    assert_eq!(imported_line(&r, &p2.r#ref), Some(imported(2, "agent:i2")));

    // an import the version does not select does not count
    let i3 = ag("i3");
    let res = import_tree(
        &mut r,
        &i3,
        "i3",
        t.path(),
        "three",
        ("d-CC001", "s-CC001"),
        &[("D-AC005", "accepted")],
    );
    write_as(
        &mut r,
        &i3,
        "i3",
        json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plain", "title": "Plain", "doc_type": "overview"}}]),
    );
    r.view_exclude(&i3, "i3", &res.changes, false).unwrap();
    let p3 = propose_as(&mut r, &i3, "i3");
    assert_eq!(imported_line(&r, &p3.r#ref), None);

    // an imported decision canon already holds, which the version does not touch, does not count
    confirm_before_landing(&mut r, &approver(), &p1);
    r.land(&approver(), &p1.id).unwrap();
    assert_eq!(
        r.read()
            .block(&Scope::Canon, &id("D-AC001"))
            .unwrap()
            .field_str("status"),
        Some("accepted")
    );
    let i4 = ag("i4");
    write_as(
        &mut r,
        &i4,
        "i4",
        json!([{"op": "create", "type": "section", "parent": "d-AA001", "fields": {"title": "More"}}]),
    );
    let p4 = propose_as(&mut r, &i4, "i4");
    assert_eq!(imported_line(&r, &p4.r#ref), None);

    // a repeated import counts each decision once, and names both importers
    let (i5, i6) = (ag("i5"), ag("i6"));
    let decisions = [("D-AC006", "accepted"), ("D-RJ006", "rejected")];
    let first = import_tree(
        &mut r,
        &i5,
        "i5",
        t.path(),
        "six",
        ("d-DD001", "s-DD001"),
        &decisions,
    );
    import(&mut r, &i6, "i6", &t.path().join("six"));
    let rep = r.view_replay(&i6, "i6", &first.changes).unwrap();
    assert_eq!(rep.replayed.len(), 1, "{rep:?}");
    let p6 = propose_as(&mut r, &i6, "i6");
    assert_eq!(
        imported_line(&r, &p6.r#ref),
        Some(imported(2, "agent:i5, agent:i6"))
    );
}

// ---------------------------------------------------------------- replay chains

#[test]
fn ui2_replayed_import_keeps_its_importer() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let t = tempfile::tempdir().unwrap();
    let i1 = ag("i1");
    let res = import_tree(
        &mut r,
        &i1,
        "i1",
        t.path(),
        "chain",
        ("d-AA001", "s-AA001"),
        &[("D-AC001", "accepted")],
    );
    assert_eq!(res.changes.len(), 1, "{res:?}");
    r.view_exclude(&i1, "i1", &res.changes, false).unwrap();
    // r64's source is 64 replay steps away, r65's 65; each view but the last two gives its replay
    // up, so the in-flight set stays small
    let mut src = res.changes[0].clone();
    for k in 1..=65 {
        let next = replay(&mut r, &format!("r{k}"), &src);
        if (2..=64).contains(&k) {
            r.view_exclude(&writer(), &format!("r{}", k - 1), &[src.clone()], false)
                .unwrap();
        }
        src = next;
    }
    let p64 = propose_as(&mut r, &writer(), "r64");
    assert_eq!(imported_line(&r, &p64.r#ref), Some(imported(1, "agent:i1")));
    // r65 is refused: propose stores it not ready, with the limit as its one issue
    answer_open_items(&mut r, &writer(), "r65");
    let p65 = r
        .propose(&writer(), "r65", "r65", Some(TEST_SUMMARY))
        .unwrap();
    assert!(!p65.ready);
    assert_eq!(
        p65.issues,
        vec![Issue {
            severity: Severity::Error,
            code: E_LIMIT,
            block: None,
            message: "the import source chain exceeds 64 replay steps".into(),
            fix: "exclude the replayed change and import the source tree into a new view".into(),
        }]
    );

    // a cycle of replay sources cannot be written (a change's id hashes its source); the walk's
    // own pin covers it

    // several importers: actors in byte order, each once, runs never printed
    let (_d, mut r) = rdy_qa::fixtures::project();
    let owner = as_agent("agent:b", "r2");
    import_tree(
        &mut r,
        &owner,
        "r2",
        t.path(),
        "own",
        ("d-BB001", "s-BB001"),
        &[("D-AC010", "accepted")],
    );
    let mut sources = vec![];
    for (s, view, name, ids, dec) in [
        (
            as_agent("agent:a", "r9"),
            "r9",
            "from-a",
            ("d-BB002", "s-BB002"),
            "D-AC011",
        ),
        (
            as_agent("agent:b", "r1"),
            "r1",
            "from-b",
            ("d-BB003", "s-BB003"),
            "D-RJ012",
        ),
        (
            approver(),
            "h",
            "from-human",
            ("d-BB004", "s-BB004"),
            "D-AC013",
        ),
    ] {
        let status = if dec.starts_with("D-RJ") {
            "rejected"
        } else {
            "accepted"
        };
        let res = import_tree(&mut r, &s, view, t.path(), name, ids, &[(dec, status)]);
        sources.extend(res.changes);
    }
    let rep = r.view_replay(&owner, "r2", &sources).unwrap();
    assert_eq!(rep.replayed.len(), 3, "{rep:?}");
    let p = propose_as(&mut r, &owner, "r2");
    assert_eq!(
        imported_line(&r, &p.r#ref),
        Some(imported(4, "agent:a, agent:b, human:qa"))
    );
}

// ---------------------------------------------------------------- policy

/// The policy of one rule: agents `agent:*` may land `two_way` decisions in area `payments`
/// with basis `precedent`; a decision otherwise needs a person from `approvers`.
fn policy_fields() -> Value {
    json!({
        "key": "authority-policy",
        "agent_may_accept": [
            {
                "name": "decides",
                "actors": ["agent:*"],
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

/// A project of pack `rdy-qa` whose canon holds the policy of [`policy_fields`].
fn policy_project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/rdy-qa.yaml"), QA_PACK_YAML).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: rdy-qa\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let w = write_as(
        &mut r,
        &writer(),
        "pol",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "overview"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
    );
    let (doc, d) = (w.created["$d"].clone(), w.created["$dec"].clone());
    r.write(
        &writer(),
        WriteRequest {
            view: "pol".into(),
            ops: parse_test_ops(json!([{"op": "create", "type": "authority_policy", "id": "$p", "parent": doc, "fields": policy_fields()}])).unwrap(),
            justification: Some(Justification::Implements(vec![id(&d)])),
            message: String::new(),
        },
    )
    .unwrap();
    land(&mut r, "pol").unwrap();
    (dir, r)
}

fn policy_label(r: &Rdy) -> String {
    let st = r.read();
    let canon = st.materialized(&Scope::Canon).unwrap();
    live_policy(&st, &canon).expect("a policy").label()
}

#[test]
fn ui2_imported_status_is_never_under_policy() {
    let (_d, mut r) = policy_project();
    let t = tempfile::tempdir().unwrap();
    // agent:a1 imports a two_way decision with basis precedent in area payments (doc `wallets`)
    let a1 = ag("a1");
    import_tree(
        &mut r,
        &a1,
        "a1",
        t.path(),
        "wallets",
        ("d-WA001", "s-WA001"),
        &[("D-AC001", "accepted")],
    );
    let p = propose_as(&mut r, &a1, "a1");
    let (_, a) = stored(&r, &p);
    let st = r.read();
    let live = live_policy(&st, &st.materialized(&Scope::Canon).unwrap()).unwrap();
    drop(st);
    assert_eq!(
        a.policy,
        Some(AssessedPolicy {
            policy: Some(live.reference()),
            verdict: PolicyVerdict::NeedsPerson {
                role: Some("approvers".into()),
                reasons: vec![IMPORTED_STATUS.to_string()],
            },
        })
    );
    assert_eq!(IMPORTED_STATUS, "it carries an imported decision status");
    let item = approve_item(&r, &p.r#ref);
    let n = item.facts.len();
    assert_eq!(
        lines(&item)[n - 2..],
        [
            format!(
                "Policy: {}: needs a person from approvers: it carries an imported decision status",
                policy_label(&r)
            ),
            imported(1, "agent:a1"),
        ]
    );
    // rdy_land refuses it with the stored reasons
    let refused = |e: RdyError| {
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_POLICY_HUMAN,
                format!(
                    "the authority policy needs a person for {}: it carries an imported decision status",
                    p.r#ref
                )
                .as_str(),
                POLICY_HUMAN_FIX
            ),
            "{e:?}"
        );
    };
    let head = r.read().canon_head();
    refused(r.land_by_policy(&a1, &p.r#ref).unwrap_err());
    // a stored assessment from before this rule that accepts it is refused the same way
    let pref = r.resolve_proposal(&p.id).unwrap();
    let st = r.read();
    let record = st.reader().proposal(&pref.version).unwrap();
    let c = rdy_engine::readiness::Candidate::stored(&st, pref, record).unwrap();
    let mut old = rdy_engine::readiness::assess(&st, &c, &st.gate_config())
        .unwrap()
        .assessment
        .expect("ready");
    drop(st);
    old.policy = Some(AssessedPolicy {
        policy: Some(live.reference()),
        verdict: PolicyVerdict::Accepts {
            rule: "decides".into(),
        },
    });
    r.store_mut().put_assessment(&old).unwrap();
    r.invalidate();
    assert_eq!(
        stored(&r, &p).1.policy,
        old.policy,
        "the old verdict is stored"
    );
    refused(r.land_by_policy(&a1, &p.r#ref).unwrap_err());
    assert_eq!(r.read().canon_head(), head, "nothing landed");
    // a person approves it after the ordinary holds
    confirm_before_landing(&mut r, &approver(), &p);
    let l = r.land(&approver(), &p.id).expect("a person lands it");
    assert_eq!(l, r.read().canon_head().unwrap().to_string());
    assert_eq!(
        r.read()
            .block(&Scope::Canon, &id("D-AC001"))
            .unwrap()
            .field_str("status"),
        Some("accepted")
    );
}

// ---------------------------------------------------------------- the card

#[test]
fn ui2_imported_is_between_policy_and_live_verdicts() {
    let (d, mut r) = rdy_qa::fixtures::project();
    let t = tempfile::tempdir().unwrap();
    let i1 = ag("i1");
    import_tree(
        &mut r,
        &i1,
        "i1",
        t.path(),
        "one",
        ("d-AA001", "s-AA001"),
        &[("D-AC001", "accepted"), ("D-RJ001", "rejected")],
    );
    let p = propose_as(&mut r, &i1, "i1");
    let (h, a) = stored(&r, &p);
    // the stored card ends with Policy; the card shown appends Imported after it
    assert!(
        a.card.facts.last().unwrap().starts_with("Policy: "),
        "{a:#?}"
    );
    assert!(a.card.facts.iter().all(|l| !l.starts_with("Imported")));
    let item = approve_item(&r, &p.r#ref);
    let mut want = a.card.facts.clone();
    want.push(imported(2, "agent:i1"));
    assert_eq!(lines(&item), want);
    // the hash binds the Imported line: without it, or with another count, the hash moves
    assert_eq!(item.card_hash, card_hash(&item).to_string());
    let mut without = item.clone();
    without.facts.pop();
    assert_ne!(card_hash(&without), card_hash(&item));
    let mut other = item.clone();
    other.facts.last_mut().unwrap().value =
        "1 decision recorded as accepted or rejected elsewhere, by agent:i1".into();
    assert_ne!(card_hash(&other), card_hash(&item));
    // a changed membership is a changed card: the next version imports one more decision
    import_tree(
        &mut r,
        &i1,
        "i1",
        t.path(),
        "two",
        ("d-BB001", "s-BB001"),
        &[("D-AC002", "accepted")],
    );
    let p2 = propose_as(&mut r, &i1, "i1");
    let item2 = approve_item(&r, &p2.r#ref);
    assert_eq!(lines(&item2).last().unwrap(), &imported(3, "agent:i1"));
    assert_ne!(item2.card_hash, item.card_hash);
    // a reindex keeps each change's importer and the same computed card
    let before = approve_item(&r, &p2.r#ref);
    r.reindex().unwrap();
    let after = approve_item(&r, &p2.r#ref);
    assert_eq!(lines(&after), lines(&before));
    assert_eq!(after.card_hash, before.card_hash);
    let st = r.read();
    let record = st.reader().proposal(&p2.id.parse().unwrap()).unwrap();
    for c in &record.changes {
        assert_eq!(
            st.reader().change(c).unwrap().links.imported_by,
            Some(rdy_model::ActorRun {
                actor: "agent:i1".into(),
                run: Some("i1".into()),
            })
        );
    }
    drop(st);
    // displaying the card wrote nothing: the stored record is the one propose wrote
    assert_eq!(stored(&r, &p).0, h);
    assert_eq!(stored(&r, &p).1, a);
    // every surface gets the same facts and hash: a refused approval's corrected card (a fresh
    // assessment after the gate config changed) is the queue's card, with the Imported line
    let (h2, a2) = stored(&r, &p2);
    r.close().unwrap();
    std::fs::write(
        d.path().join(".rdy.yaml"),
        "pack: rdy-standard\nreviews:\n  require_up_to_date: true\n",
    )
    .unwrap();
    let mut r = Rdy::open(d.path()).unwrap();
    confirm_before_landing(&mut r, &approver(), &p2);
    let e = r.land(&approver(), &p2.id).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:?}");
    let fresh = approve_item(&r, &p2.r#ref);
    let card = &e.corrected.as_ref().expect("a fresh card")["card"];
    assert_eq!(card["card_hash"], json!(fresh.card_hash), "{e:#?}");
    assert_eq!(
        lines(&fresh).last().unwrap(),
        &imported(3, "agent:i1"),
        "{fresh:#?}"
    );
    // the rendered card shows the line after Policy
    let text = card["card"].as_str().unwrap();
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} in {text}"))
    };
    assert!(
        at("  Policy ")
            < at(
                "  Imported        3 decisions recorded as accepted or rejected elsewhere, by agent:i1\n"
            ),
        "{text}"
    );
    // the fresh record's card ends with Policy too; the earlier record is unchanged
    let (h3, a3) = stored(&r, &p2);
    assert_ne!(h3, h2);
    assert!(
        a3.card.facts.last().unwrap().starts_with("Policy: "),
        "{a3:#?}"
    );
    assert_eq!(r.read().reader().assessment(&h2).unwrap(), a2);
    // an approval of the fresh card lands
    let l = r.land(&approver(), &p2.id).expect("the fresh card lands");
    assert_eq!(Some(l), r.read().canon_head().map(|c| c.to_string()));
}
