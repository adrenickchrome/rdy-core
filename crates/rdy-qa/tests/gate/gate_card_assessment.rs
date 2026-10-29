//! rg13 (D-ME2HD, D-45RG6, D-4PG9Y, D-SDSV2; O9, O10, O16): the approve card is drawn from the
//! version's stored assessment. A later change to a live fact leaves the card as propose showed
//! it, the Tensions fact shows each advisory tension with the answer the bound plan holds, a
//! version that is ready but has no stored assessment is not ready (O10), and the card's hash
//! binds the card as shown.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::pending::{Item, ItemKind, Pending, PendingQuery};
use rdy_engine::plan::api::PlanAnswerInput;
use rdy_engine::{E_VALIDATION, ProposalInfo, Rdy, Session};
use rdy_model::assessment::Assessment;
use rdy_model::plan::PlanAct;
use rdy_model::{BlockId, Hash, Role};
use rdy_qa::fixtures::{land, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY};
use rdy_qa::tensions::{j_typo, put, put_j, upd};
use rdy_schema::{Issue, Severity};
use serde_json::json;

/// Canon: a draft doc with two claims `x` and `y`, and a second draft doc with claim `z`.
struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
    x: BlockId,
    z: BlockId,
    /// The draft section, and two claims of a deprecated doc.
    t: BlockId,
    o1: BlockId,
    o2: BlockId,
}

fn world() -> World {
    let (dir, mut r) = rdy_qa::fixtures::project();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$t", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$t", "body": "X holds."},
            {"op": "create", "type": "doc", "id": "$m", "fields": {"name": "more", "title": "More", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$u", "parent": "$m", "fields": {"title": "Other"}},
            {"op": "create", "type": "claim", "id": "$z", "parent": "$u", "body": "Z holds."},
            {"op": "create", "type": "doc", "id": "$od", "fields": {"name": "old", "title": "Old", "doc_type": "overview", "lifecycle": "deprecated"}},
            {"op": "create", "type": "section", "id": "$o", "parent": "$od", "fields": {"title": "Old rules"}},
            {"op": "create", "type": "claim", "id": "$o1", "parent": "$o", "body": "O1 is an old rule."},
            {"op": "create", "type": "claim", "id": "$o2", "parent": "$o", "body": "O2 is an old rule."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let id = |k: &str| BlockId::parse(&w.created[k]).unwrap();
    World {
        x: id("$x"),
        z: id("$z"),
        t: id("$t"),
        o1: id("$o1"),
        o2: id("$o2"),
        _dir: dir,
        r,
    }
}

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    r.propose(&writer(), view, view, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"))
}

fn pending(r: &Rdy) -> Pending {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: rdy_engine::pending::CARD_MAX_TOKENS,
        })
        .unwrap()
}

fn approve_item(r: &Rdy, pref: &str) -> Item {
    pending(r)
        .items
        .into_iter()
        .find(|i| i.r#ref == pref && i.kind == ItemKind::Approve)
        .unwrap_or_else(|| panic!("an approve item for {pref}"))
}

/// The assessment version `p` stored.
fn stored(r: &Rdy, p: &ProposalInfo) -> Assessment {
    let st = r.read();
    let v: Hash = p.id.parse().unwrap();
    let h = st
        .reader()
        .assessment_of_proposal(&v, st.seq())
        .unwrap()
        .unwrap_or_else(|| panic!("{} stores an assessment", p.r#ref));
    st.reader().assessment(&h).unwrap()
}

fn lines(i: &Item) -> Vec<String> {
    i.facts
        .iter()
        .map(|f| format!("{}: {}", f.name, f.value))
        .collect()
}

fn fact<'a>(i: &'a Item, name: &str) -> &'a str {
    i.facts
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("fact {name}"))
        .value
        .as_str()
}

fn agent() -> Session {
    Session::agent("agent:relay".into(), Some("r1".into()), Role::Writer).expect("agent")
}

/// View `view` writes a draft claim that cites the deprecated claim `old`: one advisory
/// deprecated-reference tension of the version.
fn cite_deprecated(w: &mut World, view: &str, old: &BlockId, body: &str) {
    put(
        &mut w.r,
        view,
        json!([{"op": "create", "type": "claim", "parent": w.t.to_string(),
                "body": format!("{body} {{{{{old}}}}}.")}]),
    );
}

/// Lands an unrelated change (claim `z`), so canon moves on from the base of any open version.
fn land_elsewhere(w: &mut World) {
    put_j(&mut w.r, "elsewhere", upd(&w.z, "Z holds twice."), j_typo());
    land(&mut w.r, "elsewhere").unwrap();
}

#[test]
fn rg13_the_approve_card_facts_are_the_stored_facts() {
    let mut w = world();
    put_j(&mut w.r, "a", upd(&w.x, "X holds in a."), j_typo());
    let p = propose(&mut w.r, "a");
    let a = stored(&w.r, &p);
    let before = approve_item(&w.r, &p.r#ref);
    assert_eq!(lines(&before), a.card.facts);
    assert!(fact(&before, "Base").ends_with(", current"), "{before:#?}");
    // canon moves on: a live Base fact would now say `moved`
    land_elsewhere(&mut w);
    let after = approve_item(&w.r, &p.r#ref);
    assert_eq!(lines(&after), a.card.facts, "the facts stay as stored");
    assert!(fact(&after, "Base").ends_with(", current"), "{after:#?}");
    assert_eq!(after.facts, before.facts);
}

/// The ids of the advisory tensions `p` reports, as `(tn id, kind, summary)` in tension id order.
fn advisory_rows(p: &ProposalInfo) -> Vec<(String, String, String)> {
    let mut rows: Vec<_> = p
        .tensions
        .iter()
        .filter(|t| t.severity == rdy_engine::tensions::TensionSeverity::Advisory)
        .map(|t| {
            (
                t.tension.clone(),
                t.id.clone(),
                t.kind.name().to_string(),
                t.summary.clone(),
            )
        })
        .collect();
    rows.sort();
    rows.into_iter().map(|(_, a, b, c)| (a, b, c)).collect()
}

#[test]
fn rg13_the_tensions_fact_shows_each_answer() {
    let mut w = world();
    // two draft claims that cite deprecated ones: one advisory tension for each
    let (o1, o2) = (w.o1.clone(), w.o2.clone());
    cite_deprecated(&mut w, "a", &o1, "P follows");
    cite_deprecated(&mut w, "a", &o2, "Q follows");
    let reply = w.r.read().view_plan("a").unwrap();
    let ids: Vec<String> = reply
        .items
        .iter()
        .filter_map(|row| match row {
            rdy_engine::plan::api::PlanRow::Live(i)
                if i.kind == rdy_model::plan::PlanKind::Tension =>
            {
                Some(i.id.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 2, "{reply:#?}");
    // the first is confirmed, the second dismissed
    w.r.answer_plan(
        &writer(),
        "a",
        &[
            PlanAnswerInput {
                item: ids[0].clone(),
                act: PlanAct::Confirm,
                reason: Some("it was meant".into()),
            },
            PlanAnswerInput {
                item: ids[1].clone(),
                act: PlanAct::Dismiss,
                reason: Some("a draft".into()),
            },
        ],
    )
    .unwrap();
    let p = propose(&mut w.r, "a");
    let rows = advisory_rows(&p);
    assert_eq!(rows.len(), 2, "{:#?}", p.tensions);
    let act = |id: &str| {
        if id == ids[0] {
            "confirm: it was meant"
        } else {
            assert_eq!(id, ids[1]);
            "dismiss: a draft"
        }
    };
    let want = format!(
        "0 blocking, 2 advisory: {}",
        rows.iter()
            .map(|(id, kind, summary)| format!("{id} {kind}: {summary} ({})", act(id)))
            .collect::<Vec<_>>()
            .join("; ")
    );
    let card = approve_item(&w.r, &p.r#ref);
    assert_eq!(fact(&card, "Tensions"), want);
    assert_eq!(stored(&w.r, &p).card.facts.len(), card.facts.len());
    assert!(
        !fact(&card, "Tensions").contains("answers not recorded yet"),
        "{card:#?}"
    );
}

#[test]
fn rg13_a_ready_version_with_no_assessment_is_not_ready_with_the_o10_issue() {
    let mut w = world();
    put_j(&mut w.r, "a", upd(&w.x, "X holds in a."), j_typo());
    // a version written below the product's propose (as one proposed before this release): ready
    // by every check, and no assessment
    let by = writer().provenance();
    let changes = w.r.store_mut().included("a").unwrap().as_ref().clone();
    let v =
        w.r.store_mut()
            .propose_changes("a", "a", Some(TEST_SUMMARY), by, changes, None)
            .unwrap();
    w.r.invalidate();
    // its first version: the stable id is its own hash
    let pref = format!("P-{}@1", v.short());
    let q = pending(&w.r);
    assert!(
        q.items.iter().all(|i| i.r#ref != pref),
        "no card for a version with no assessment: {q:#?}"
    );
    let n = q
        .not_ready
        .iter()
        .find(|n| n.r#ref == pref)
        .unwrap_or_else(|| panic!("{pref} is not ready: {q:#?}"));
    assert_eq!(
        n.issues,
        vec![Issue {
            severity: Severity::Error,
            code: E_VALIDATION,
            block: None,
            message: format!("{pref} has no stored assessment"),
            fix: "propose it again: rdy_propose / rdy propose".into(),
        }]
    );
}

#[test]
fn rg13_the_card_hash_binds_the_shown_card() {
    let mut w = world();
    put_j(&mut w.r, "a", upd(&w.x, "X holds in a."), j_typo());
    let p = propose(&mut w.r, "a");
    let shown = approve_item(&w.r, &p.r#ref);
    land_elsewhere(&mut w);
    // the card is as stored, so its hash is the one the person was shown
    let now = approve_item(&w.r, &p.r#ref);
    assert_eq!(now.card_hash, shown.card_hash);
    let head = w.r.read().canon_head();
    let done =
        w.r.answer(
            &agent(),
            PERSON,
            AnswerRequest {
                r#ref: p.r#ref.clone(),
                option: "approve".into(),
                card_hash: Some(shown.card_hash.clone()),
                note: None,
                relay: RelayInput {
                    relayed_by: None,
                    evidence: Some("yes".into()),
                    question: Some("Shall I approve it?".into()),
                    session_ref: Some("chat-7".into()),
                    kind: None,
                },
                brief: None,
            },
        )
        .unwrap();
    assert_eq!(done.act, "approve");
    assert_ne!(w.r.read().canon_head(), head, "the approval landed");
}
