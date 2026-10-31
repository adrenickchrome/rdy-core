//! ck5 (D-JQD5X "Considered options", D-8GR43, O11, O12, O17, review I1): the landing that
//! chooses member M of set S writes the unchosen members into `options` of the decision that
//! answers S's question, else the only decision the version creates. The card shows the entries
//! as the fact `Considered options` after `Decisions`, then `Alternative set` binds the set's
//! version. A version that chooses and names no such decision is not ready (`E_ALT_DECISION`).
//! A changed set, question, membership or target refuses the old card as moved before any write.
use rdy_engine::alt::{AltMemberInput, AltPick};
use rdy_engine::*;
use rdy_model::{AltSet, Block, BlockId, FieldValue, Hash, Op};
use rdy_qa::fixtures::*;
use rdy_qa::intake::{TEST_SUMMARY, confirm_before_landing};
use serde_json::{Value, json};

const ALT_FIX: &str = "answer the set's question with one decision, or create exactly one decision: rdy_decide / rdy new decision";

fn m(label: &str, view: &str) -> AltMemberInput {
    AltMemberInput {
        label: label.into(),
        view: view.into(),
    }
}

/// A landed seed doc (section `section`), with two question blocks when `questions`; each member
/// view writes one claim of its own; then the set `Wording` of `members`, on question `q` when
/// there are questions.
struct W {
    dir: tempfile::TempDir,
    r: Rdy,
    section: String,
    q: Option<String>,
    q2: Option<String>,
    set: String,
}

fn world(members: &[(&str, &str)], questions: bool) -> W {
    let (dir, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    let (q, q2) = if questions {
        let w = write(
            &mut r,
            "seed",
            json!([
                {"op": "create", "type": "question", "id": "$q", "parent": section, "fields": {"prompt": "Which wording?"}},
                {"op": "create", "type": "question", "id": "$q2", "parent": section, "fields": {"prompt": "Which tone?"}}
            ]),
        )
        .unwrap();
        (
            Some(w.created["$q"].clone()),
            Some(w.created["$q2"].clone()),
        )
    } else {
        (None, None)
    };
    land(&mut r, "seed").unwrap();
    for (_, view) in members {
        write(
            &mut r,
            view,
            json!([{"op": "create", "type": "claim", "parent": section, "body": format!("alt {view}")}]),
        )
        .unwrap();
    }
    let ms: Vec<AltMemberInput> = members.iter().map(|(l, v)| m(l, v)).collect();
    let set = r
        .alt_create(&writer(), "Wording", q.as_deref(), &ms)
        .unwrap()
        .id;
    W {
        dir,
        r,
        section,
        q,
        q2,
        set,
    }
}

/// A proposed decision `statement` in `view` under `parent`, answering `answers`, with authored
/// `options`; it needs no content, so it carries no exception. Its id.
fn decide(
    r: &mut Rdy,
    view: &str,
    parent: &str,
    statement: &str,
    answers: &[&str],
    options: &[&str],
) -> String {
    r.decide(
        &writer(),
        view,
        DecisionInput {
            parent: parent.into(),
            statement: statement.into(),
            answers: answers.iter().map(|a| a.to_string()).collect(),
            options: options.iter().map(|o| o.to_string()).collect(),
            fields: [("needs_content".to_string(), json!(false))]
                .into_iter()
                .collect(),
            ..Default::default()
        },
    )
    .unwrap()
    .created["$decision"]
        .clone()
}

fn choose(r: &mut Rdy, view: &str, set: &str, label: &str) -> ProposalInfo {
    r.propose_with(
        &writer(),
        view,
        view,
        None,
        Some(TEST_SUMMARY),
        Some(&AltPick {
            set: set.into(),
            chosen: label.into(),
        }),
    )
    .unwrap()
}

fn land_it(r: &mut Rdy, p: &ProposalInfo) -> LandInfo {
    confirm_before_landing(r, &approver(), p);
    r.land_report(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("land {}: {e:#?}", p.r#ref))
}

/// The card facts the stored assessment of `p` keeps, as `<name>: <value>` lines.
fn stored_facts(r: &Rdy, p: &ProposalInfo) -> Vec<String> {
    let st = r.read();
    let h = st
        .reader()
        .assessment_of_proposal(&p.id.parse().unwrap(), st.seq())
        .unwrap()
        .unwrap_or_else(|| panic!("{} stores an assessment", p.r#ref));
    st.reader().assessment(&h).unwrap().card.facts
}

fn names(facts: &[String]) -> Vec<&str> {
    facts
        .iter()
        .map(|f| f.split_once(": ").map_or(f.as_str(), |(n, _)| n))
        .collect()
}

fn value<'a>(facts: &'a [String], name: &str) -> Option<&'a str> {
    facts.iter().find_map(|f| {
        f.split_once(": ")
            .filter(|(n, _)| *n == name)
            .map(|(_, v)| v)
    })
}

fn canon_block(r: &Rdy, id: &str) -> Block {
    let st = r.read();
    st.materialized(&Scope::Canon)
        .unwrap()
        .get(&BlockId::parse(id).unwrap())
        .unwrap_or_else(|| panic!("{id} in canon"))
        .clone()
}

fn options_of(b: &Block) -> Option<Vec<String>> {
    match b.fields.get("options")? {
        FieldValue::List(items) => Some(
            items
                .iter()
                .map(|i| match i {
                    FieldValue::Str(s) => s.clone(),
                    other => panic!("an option that is not text: {other:?}"),
                })
                .collect(),
        ),
        other => panic!("options that are not a list: {other:?}"),
    }
}

fn entry(label: &str, set: &str, view: &str) -> String {
    format!("{label} ({set}/{view}, not chosen)")
}

fn set_version(r: &Rdy, set: &str) -> Hash {
    r.read().resolve_alt(set).unwrap().1
}

fn alt_issue(p: &ProposalInfo) -> Vec<&rdy_schema::Issue> {
    p.issues
        .iter()
        .filter(|i| i.code == E_ALT_DECISION)
        .collect()
}

fn assert_no_target(p: &ProposalInfo, set: &str) {
    assert!(!p.ready, "{:#?}", p.issues);
    let found = alt_issue(p);
    assert_eq!(found.len(), 1, "{:#?}", p.issues);
    assert_eq!(found[0].severity, rdy_schema::Severity::Error);
    assert_eq!(found[0].block, None);
    assert_eq!(
        found[0].message,
        format!(
            "{} chooses in {set} but names no decision for the considered options",
            p.r#ref
        )
    );
    assert_eq!(found[0].fix, ALT_FIX);
    assert_eq!(p.assessment, None);
}

#[test]
fn ck5_unique_answer_wins_else_only_created_decision() {
    let members = [("x", "a"), ("y", "b"), ("z", "c")];
    // one created decision answers the question: it wins over another created decision
    let mut w = world(&members, true);
    let q = w.q.clone().unwrap();
    let d1 = decide(&mut w.r, "a", &w.section, "Answer", &[&q], &[]);
    let d2 = decide(&mut w.r, "a", &w.section, "Aside", &[], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    assert!(p.ready, "{:#?}", p.issues);
    land_it(&mut w.r, &p);
    let want = vec![entry("y", &w.set, "b"), entry("z", &w.set, "c")];
    assert_eq!(options_of(&canon_block(&w.r, &d1)), Some(want));
    assert_eq!(options_of(&canon_block(&w.r, &d2)), None);
    // no answer: the only created decision is the target
    let mut w = world(&members, true);
    let only = decide(&mut w.r, "a", &w.section, "Only", &[], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    assert!(p.ready, "{:#?}", p.issues);
    land_it(&mut w.r, &p);
    assert!(options_of(&canon_block(&w.r, &only)).is_some());
    // two answers: not ready
    let mut w = world(&members, true);
    let q = w.q.clone().unwrap();
    decide(&mut w.r, "a", &w.section, "One", &[&q], &[]);
    decide(&mut w.r, "a", &w.section, "Two", &[&q], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    assert_no_target(&p, &w.set);
    // several decisions, none answers: not ready
    let mut w = world(&members, true);
    decide(&mut w.r, "a", &w.section, "One", &[], &[]);
    decide(&mut w.r, "a", &w.section, "Two", &[], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    assert_no_target(&p, &w.set);
    // a set with no question and two decisions: not ready
    let mut w = world(&members, false);
    decide(&mut w.r, "a", &w.section, "One", &[], &[]);
    decide(&mut w.r, "a", &w.section, "Two", &[], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    assert_no_target(&p, &w.set);
    // no decision at all: not ready, and the landing is refused with the issue
    let mut w = world(&members, true);
    let p = choose(&mut w.r, "a", &w.set, "x");
    assert_no_target(&p, &w.set);
    let e = w.r.land(&approver(), &p.id).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e:#?}");
    assert!(e.issues.iter().any(|i| i.code == E_ALT_DECISION), "{e:#?}");
}

#[test]
fn ck5_options_preserve_authored_and_member_order() {
    // labels with punctuation; the chosen member sits between the others in member order
    let members = [
        ("zeta, \"last\" (z)", "c"),
        ("chosen: x", "a"),
        ("y/odd [b]", "b"),
    ];
    let mut w = world(&members, false);
    let d = decide(
        &mut w.r,
        "a",
        &w.section,
        "Use x",
        &[],
        &["Keep it", "Drop it", "Keep it"],
    );
    let p = choose(&mut w.r, "a", &w.set, "chosen: x");
    assert!(p.ready, "{:#?}", p.issues);
    let want = vec![
        "Keep it".to_string(),
        "Drop it".to_string(),
        "Keep it".to_string(),
        entry("zeta, \"last\" (z)", &w.set, "c"),
        entry("y/odd [b]", &w.set, "b"),
    ];
    let facts = stored_facts(&w.r, &p);
    assert_eq!(
        value(&facts, "Considered options"),
        Some(serde_json::to_string(&want).unwrap().as_str())
    );
    assert!(
        !value(&facts, "Considered options")
            .unwrap()
            .contains("chosen: x ("),
        "the chosen member is absent"
    );
    land_it(&mut w.r, &p);
    assert_eq!(options_of(&canon_block(&w.r, &d)), Some(want));
}

#[test]
fn ck5_considered_options_follow_decisions_and_bind_the_set() {
    let members = [("x", "a"), ("y", "b")];
    let mut w = world(&members, false);
    decide(&mut w.r, "a", &w.section, "Use x", &[], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    assert!(p.ready, "{:#?}", p.issues);
    let facts = stored_facts(&w.r, &p);
    let n = names(&facts);
    let at = n.iter().position(|x| *x == "Decisions").unwrap();
    assert_eq!(
        &n[at..at + 4],
        &[
            "Decisions",
            "Considered options",
            "Alternative set",
            "Supersedes"
        ],
        "{facts:#?}"
    );
    assert_eq!(
        value(&facts, "Considered options"),
        Some(json!([entry("y", &w.set, "b")]).to_string().as_str())
    );
    assert_eq!(
        value(&facts, "Alternative set"),
        Some(format!("{}@{}", w.set, set_version(&w.r, &w.set)).as_str())
    );
    // the shown card is the stored one
    let item =
        w.r.read()
            .pending(&rdy_engine::pending::PendingQuery {
                person: rdy_qa::intake::PERSON.into(),
                session_ref: None,
                max_tokens: rdy_engine::pending::CARD_MAX_TOKENS,
            })
            .unwrap()
            .items
            .into_iter()
            .find(|i| i.r#ref == p.r#ref)
            .unwrap();
    let shown: Vec<(String, String)> = item
        .facts
        .iter()
        .map(|f| (f.name.to_string(), f.value.clone()))
        .collect();
    assert!(
        shown.contains(&(
            "Considered options".into(),
            value(&facts, "Considered options").unwrap().into()
        )),
        "{shown:#?}"
    );
    land_it(&mut w.r, &p);
    // a version that chooses nothing shows neither fact
    write(
        &mut w.r,
        "n",
        json!([{"op": "create", "type": "claim", "parent": w.section, "body": "plain"}]),
    )
    .unwrap();
    let plain =
        w.r.propose(&writer(), "n", "n", Some(TEST_SUMMARY))
            .unwrap();
    let facts = stored_facts(&w.r, &plain);
    assert!(
        !names(&facts)
            .iter()
            .any(|n| *n == "Considered options" || *n == "Alternative set"),
        "{facts:#?}"
    );
    // entries past the 4,096-byte cut: the record keeps the cut fact, the binding is whole, and
    // the landing still lands
    let long = |c: char| std::iter::repeat_n(c, 2_500).collect::<String>();
    let (ly, lz) = (long('y'), long('z'));
    let members = [("x", "a"), (ly.as_str(), "b"), (lz.as_str(), "c")];
    let mut w = world(&members, false);
    let d = decide(&mut w.r, "a", &w.section, "Use x", &[], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    assert!(p.ready, "{:#?}", p.issues);
    let facts = stored_facts(&w.r, &p);
    let line = facts
        .iter()
        .find(|f| f.starts_with("Considered options: "))
        .unwrap();
    assert_eq!(line.len(), 4_096, "the fact is cut to its bound");
    assert!(line.ends_with("..."), "{}", &line[line.len() - 10..]);
    assert_eq!(
        value(&facts, "Alternative set"),
        Some(format!("{}@{}", w.set, set_version(&w.r, &w.set)).as_str())
    );
    land_it(&mut w.r, &p);
    assert_eq!(
        options_of(&canon_block(&w.r, &d)),
        Some(vec![entry(&ly, &w.set, "b"), entry(&lz, &w.set, "c")])
    );
}

/// A rule that reads `options`: it sees the options of the landing projection, not the
/// author's block.
#[test]
fn ck5_rules_validate_the_projected_options() {
    const PACK: &str = "\
name: opts
version: \"1\"
extends: rdy-standard
rules:
  - name: options-recorded
    select: type = decision
    require: fields.options ^= \"y (\"
    message: a decision records its considered options
    fix: choose through an alternative set
  - name: options-absent
    select: type = decision
    require: not fields.options ^= \"y (\"
    message: a decision records no option of y
    fix: drop the option
";
    let (dir, r) = project();
    drop(r);
    rdy_qa::packs::use_pack(dir.path(), "opts", PACK, "");
    let mut r = Rdy::open(dir.path()).unwrap();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    for view in ["a", "b"] {
        write(
            &mut r,
            view,
            json!([{"op": "create", "type": "claim", "parent": section, "body": view}]),
        )
        .unwrap();
    }
    let set = r
        .alt_create(&writer(), "Wording", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    let d = decide(&mut r, "a", &section, "Use x", &[], &[]);
    // the author's block has no options: in the view, the change stage judges it so
    let st = r.read();
    let authored = st
        .materialized(&Scope::View("a".into()))
        .unwrap()
        .get(&BlockId::parse(&d).unwrap())
        .unwrap()
        .clone();
    drop(st);
    assert_eq!(options_of(&authored), None);
    let p = choose(&mut r, "a", &set, "x");
    assert!(p.ready, "{:#?}", p.issues);
    let rows: Vec<(&str, &str)> = p
        .warnings
        .iter()
        .filter(|w| w.code == W_RULE_CHECK)
        .map(|w| (w.message.as_str(), w.fix.as_str()))
        .collect();
    let want = format!("rule options-absent fails on {d}: a decision records no option of y");
    assert_eq!(rows, vec![(want.as_str(), "drop the option")], "{rows:?}");
}

/// Writes a new version of set `set` in the closed project at `dir` through the store, with
/// question `question`, and opens the project again.
fn requestion(dir: &std::path::Path, r: Rdy, set: &str, question: &str) -> Rdy {
    let (stable, version, cur) = r.read().resolve_alt(set).unwrap();
    r.close().unwrap();
    let mut store = rdy_store::Store::open(&dir.join(".rdy/store")).unwrap();
    let next = AltSet {
        stable: Some(stable),
        prev: Some(version),
        question: Some(BlockId::parse(question).unwrap()),
        provenance: writer().provenance(),
        ..cur
    };
    store.put_alt_set(&next).unwrap();
    drop(store);
    Rdy::open(dir).unwrap()
}

fn moved_message(p: &ProposalInfo) -> String {
    format!(
        "proposal {} moved: the considered options changed after its assessment",
        p.r#ref
    )
}

/// Nothing chose: no landing since `head`, the set is open, the decision is still proposed in
/// view `view` and in no canon, with no options.
fn nothing_written(r: &Rdy, head: Option<Hash>, set: &str, view: &str, d: &str) {
    assert_eq!(r.read().canon_head(), head, "no landing");
    assert_eq!(
        r.read().resolve_alt(set).unwrap().2.status,
        rdy_model::AltStatus::Open
    );
    let st = r.read();
    let id = BlockId::parse(d).unwrap();
    assert!(st.materialized(&Scope::Canon).unwrap().get(&id).is_none());
    let b = st
        .materialized(&Scope::View(view.into()))
        .unwrap()
        .get(&id)
        .unwrap()
        .clone();
    assert_eq!(b.field_str("status"), Some("proposed"));
    assert_eq!(options_of(&b), None);
}

fn corrected_facts(e: &RdyError) -> Vec<String> {
    let card = &e.corrected.as_ref().expect("a fresh card")["card"]["card"];
    let text = card.as_str().unwrap_or_default().to_string();
    text.lines().map(str::to_string).collect()
}

#[test]
fn ck5_changed_set_or_target_refuses_before_writing() {
    let members = [("x", "a"), ("y", "b"), ("z", "c")];
    // membership: a member joins after propose
    let mut w = world(&members, false);
    let d = decide(&mut w.r, "a", &w.section, "Use x", &[], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    assert!(p.ready, "{:#?}", p.issues);
    write(
        &mut w.r,
        "e",
        json!([{"op": "create", "type": "claim", "parent": w.section, "body": "alt e"}]),
    )
    .unwrap();
    w.r.alt_add(&writer(), &w.set, &m("w", "e")).unwrap();
    let head = w.r.read().canon_head();
    confirm_before_landing(&mut w.r, &approver(), &p);
    let e = w.r.land(&approver(), &p.id).unwrap_err();
    assert_eq!(
        (e.code, e.message.clone(), e.fix.clone()),
        (
            E_PROPOSAL_MOVED,
            moved_message(&p),
            format!(
                "review the fresh card in corrected and approve {} again",
                p.r#ref
            )
        ),
        "{e:#?}"
    );
    nothing_written(&w.r, head, &w.set, "a", &d);
    // the corrected card binds the new set version, and the replacement assessment lands
    let bound = format!("{}@{}", w.set, set_version(&w.r, &w.set));
    let facts = stored_facts(&w.r, &p);
    assert_eq!(value(&facts, "Alternative set"), Some(bound.as_str()));
    let shown = corrected_facts(&e).join("\n");
    assert!(shown.contains(&bound), "{shown}");
    land_it(&mut w.r, &p);
    assert_eq!(
        options_of(&canon_block(&w.r, &d)),
        Some(vec![
            entry("y", &w.set, "b"),
            entry("z", &w.set, "c"),
            entry("w", &w.set, "e")
        ])
    );
    // label: a member leaves and comes back under another label
    let mut w = world(&members, false);
    let d = decide(&mut w.r, "a", &w.section, "Use x", &[], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    w.r.alt_remove(&writer(), &w.set, "z").unwrap();
    w.r.alt_add(&writer(), &w.set, &m("zed", "c")).unwrap();
    let head = w.r.read().canon_head();
    confirm_before_landing(&mut w.r, &approver(), &p);
    let e = w.r.land(&approver(), &p.id).unwrap_err();
    assert_eq!(
        (e.code, e.message.clone()),
        (E_PROPOSAL_MOVED, moved_message(&p))
    );
    nothing_written(&w.r, head, &w.set, "a", &d);
    // question: the set's question moves to the other decision's; the target moves with it,
    // and the refreshed card binds the new set version
    let mut w = world(&members, true);
    let (q, q2) = (w.q.clone().unwrap(), w.q2.clone().unwrap());
    let d1 = decide(&mut w.r, "a", &w.section, "Answer one", &[&q], &[]);
    let d2 = decide(&mut w.r, "a", &w.section, "Answer two", &[&q2], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    assert!(p.ready, "{:#?}", p.issues);
    let W { dir, r, set, .. } = w;
    let mut r = requestion(dir.path(), r, &set, &q2);
    let head = r.read().canon_head();
    confirm_before_landing(&mut r, &approver(), &p);
    let e = r.land(&approver(), &p.id).unwrap_err();
    assert_eq!(
        (e.code, e.message.clone()),
        (E_PROPOSAL_MOVED, moved_message(&p))
    );
    nothing_written(&r, head, &set, "a", &d1);
    land_it(&mut r, &p);
    assert!(
        options_of(&canon_block(&r, &d2)).is_some(),
        "d2 answers now"
    );
    assert_eq!(options_of(&canon_block(&r, &d1)), None);
    // target: the question moves to one no created decision answers, with two decisions: the
    // refresh is not ready, so the refusal carries its issues and no card
    let mut w = world(&members, true);
    let q = w.q.clone().unwrap();
    let d1 = decide(&mut w.r, "a", &w.section, "Answer one", &[&q], &[]);
    decide(&mut w.r, "a", &w.section, "Aside", &[], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    assert!(p.ready, "{:#?}", p.issues);
    // a third question in canon, which no decision answers
    let q3 = write(
        &mut w.r,
        "q3",
        json!([{"op": "create", "type": "question", "id": "$q", "parent": w.section, "fields": {"prompt": "Which font?"}}]),
    )
    .unwrap()
    .created["$q"]
        .clone();
    land(&mut w.r, "q3").unwrap();
    let W { dir, r, set, .. } = w;
    let mut r = requestion(dir.path(), r, &set, &q3);
    let head = r.read().canon_head();
    confirm_before_landing(&mut r, &approver(), &p);
    let e = r.land(&approver(), &p.id).unwrap_err();
    assert_eq!(
        (e.code, e.message.clone(), e.fix.clone()),
        (
            E_PROPOSAL_MOVED,
            moved_message(&p),
            "fix each issue and propose again: rdy_propose / rdy propose".to_string()
        ),
        "{e:#?}"
    );
    assert!(e.corrected.is_none(), "no approval card: {e:#?}");
    assert!(e.issues.iter().any(|i| i.code == E_ALT_DECISION), "{e:#?}");
    nothing_written(&r, head, &set, "a", &d1);
}

/// The landing record's acceptance change: its updates of `d`.
fn acceptance_updates(r: &Rdy, landing: &str, d: &str) -> Vec<Block> {
    let st = r.read();
    let rec = st.reader().landing(&landing.parse().unwrap()).unwrap();
    let id = BlockId::parse(d).unwrap();
    let mut out = vec![];
    for c in &rec.changes {
        for op in &st.reader().change(c).unwrap().ops {
            if let Op::Update { block, .. } = op
                && block.id == id
                && block.field_str("status") == Some("accepted")
            {
                out.push(block.clone());
            }
        }
    }
    out
}

#[test]
fn ck5_acceptance_writes_options_atomically() {
    let members = [("x", "a"), ("y", "b")];
    // a person's landing: one update accepts the decision and writes its options
    let mut w = world(&members, false);
    let d = decide(&mut w.r, "a", &w.section, "Use x", &[], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    // an injected append failure leaves no partial choice or options
    confirm_before_landing(&mut w.r, &approver(), &p);
    for k in 0..2 {
        let head = w.r.read().canon_head();
        w.r.store_mut().fail_append_at_frame_for_tests(Some((k, 3)));
        let e = w.r.land(&approver(), &p.id).unwrap_err();
        w.r.store_mut().fail_append_at_frame_for_tests(None);
        nothing_written(&w.r, head, &w.set, "a", &d);
        assert!(!w.r.read().view_head("b").unwrap().closed, "frame {k}: {e}");
        w.r.fsck().unwrap();
    }
    let l = land_it(&mut w.r, &p);
    let ups = acceptance_updates(&w.r, &l.landing, &d);
    assert_eq!(ups.len(), 1, "{ups:#?}");
    assert_eq!(options_of(&ups[0]), Some(vec![entry("y", &w.set, "b")]));
    assert!(ups[0].field_str("decided_at").is_some());
    assert_eq!(canon_block(&w.r, &d), ups[0].clone());
    // a policy landing takes the same path
    let mut pw = policy_world();
    let d = pw.decision("a1");
    let p =
        pw.r.propose_with(
            &rdy_qa::tensions::ag("a1"),
            "a1",
            "a1",
            None,
            Some(TEST_SUMMARY),
            Some(&AltPick {
                set: pw.set.clone(),
                chosen: "x".into(),
            }),
        )
        .unwrap();
    assert!(p.ready, "{:#?}", p.issues);
    assert_eq!(
        p.policy.as_ref().map(|v| v.verdict.as_str()),
        Some("accepts"),
        "{:#?}",
        p.policy
    );
    let l =
        pw.r.land_by_policy(&rdy_qa::tensions::ag("a1"), &p.r#ref)
            .unwrap();
    assert_eq!(l.assurance, "policy");
    let ups = acceptance_updates(&pw.r, &l.landing, &d);
    assert_eq!(ups.len(), 1, "{ups:#?}");
    assert_eq!(options_of(&ups[0]), Some(vec![entry("y", &pw.set, "a2")]));
}

/// A policy world: pack `rdy-qa`, a draft doc `wallets` (area payments) with section `ws`, the
/// policy rule `decides` that lets agents `agent:a*` land two_way decisions with basis
/// precedent in area payments, and the set {x: a1, y: a2} of two agent views.
struct PolicyWorld {
    _dir: tempfile::TempDir,
    r: Rdy,
    ws: String,
    set: String,
}

impl PolicyWorld {
    /// Agent `agent:<view>` decides in its view `view` under `ws`: a two_way precedent decision
    /// that needs no content.
    fn decision(&mut self, view: &str) -> String {
        rdy_qa::tensions::put_j(
            &mut self.r,
            view,
            json!([{"op": "create", "type": "decision", "id": "$dec", "parent": self.ws,
                    "fields": {"statement": "Fees round up", "status": "proposed", "needs_content": false,
                               "reversibility": "two_way", "authority_basis": "precedent"}}]),
            rdy_qa::tensions::j_typo(),
        )
        .created["$dec"]
            .clone()
    }
}

fn policy_world() -> PolicyWorld {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(
        dir.path().join(".rdy/packs/rdy-qa.yaml"),
        rdy_qa::packs::QA_PACK_YAML,
    )
    .unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: rdy-qa\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let seeded = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$w", "fields": {"name": "wallets", "title": "Wallets", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ws", "parent": "$w", "fields": {"title": "Fees"}},
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "overview"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    let ws = seeded.created["$ws"].clone();
    let doc = seeded.created["$d"].clone();
    let adopt = BlockId::parse(&seeded.created["$dec"]).unwrap();
    let policy: Value = json!({
        "key": "authority-policy",
        "agent_may_accept": [{
            "name": "decides",
            "actors": ["agent:a*"],
            "lifecycle": "any",
            "justifications": ["implements", "editorial"],
            "decisions": "two_way",
            "two_way_areas": ["payments"],
            "basis": ["precedent"],
            "max_blocks": 20,
            "max_items": 20
        }],
        "roles": {"approvers": [rdy_qa::intake::PERSON]},
        "require": [],
        "owners": []
    });
    r.write(
        &writer(),
        WriteRequest {
            view: "pol".into(),
            ops: parse_test_ops(json!([{"op": "create", "type": "authority_policy", "parent": doc, "fields": policy}])).unwrap(),
            justification: Some(rdy_model::Justification::Implements(vec![adopt])),
            message: String::new(),
        },
    )
    .unwrap();
    land(&mut r, "pol").unwrap();
    for view in ["a1", "a2"] {
        rdy_qa::tensions::put_j(
            &mut r,
            view,
            json!([{"op": "create", "type": "claim", "parent": ws, "body": format!("Fees by {view}.")}]),
            rdy_qa::tensions::j_typo(),
        );
    }
    let set = r
        .alt_create(&writer(), "Fees", None, &[m("x", "a1"), m("y", "a2")])
        .unwrap()
        .id;
    PolicyWorld {
        _dir: dir,
        r,
        ws,
        set,
    }
}

#[test]
fn ck5_frozen_decisions_are_never_option_targets() {
    let members = [("x", "a"), ("y", "b")];
    // a canon decision that answers the question is not a target and does not block: the only
    // new proposed decision is chosen, though it has no answers link
    let mut w = world(&members, true);
    let q = w.q.clone().unwrap();
    let frozen = decide(
        &mut w.r,
        "f",
        &w.section,
        "Earlier answer",
        &[&q],
        &["kept"],
    );
    land(&mut w.r, "f").unwrap();
    let before = canon_block(&w.r, &frozen);
    let d = decide(&mut w.r, "a", &w.section, "New", &[], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    assert!(p.ready, "{:#?}", p.issues);
    land_it(&mut w.r, &p);
    assert_eq!(
        canon_block(&w.r, &frozen),
        before,
        "the canon answer is not rewritten"
    );
    assert_eq!(
        options_of(&canon_block(&w.r, &d)),
        Some(vec![entry("y", &w.set, "b")])
    );
    // a new decision with the outgoing answers link wins over another new one
    let mut w = world(&members, true);
    let q = w.q.clone().unwrap();
    let frozen = decide(&mut w.r, "f", &w.section, "Earlier answer", &[&q], &[]);
    land(&mut w.r, "f").unwrap();
    let answer = decide(&mut w.r, "a", &w.section, "Answer", &[&q], &[]);
    let aside = decide(&mut w.r, "a", &w.section, "Aside", &[], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    assert!(p.ready, "{:#?}", p.issues);
    land_it(&mut w.r, &p);
    assert!(options_of(&canon_block(&w.r, &answer)).is_some());
    assert_eq!(options_of(&canon_block(&w.r, &aside)), None);
    assert_eq!(options_of(&canon_block(&w.r, &frozen)), None);
    // a set with no question takes the only new decision
    let mut w = world(&members, false);
    let d = decide(&mut w.r, "a", &w.section, "Only", &[], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    land_it(&mut w.r, &p);
    assert!(options_of(&canon_block(&w.r, &d)).is_some());
}

#[test]
fn ck5_dump_load_reindex_keep_the_chosen_options() {
    let members = [("x", "a"), ("y", "b")];
    let mut w = world(&members, false);
    let d = decide(&mut w.r, "a", &w.section, "Use x", &[], &[]);
    let p = choose(&mut w.r, "a", &w.set, "x");
    let l = land_it(&mut w.r, &p);
    let landing: Hash = l.landing.parse().unwrap();
    let want = options_of(&canon_block(&w.r, &d));
    assert_eq!(want, Some(vec![entry("y", &w.set, "b")]));
    let rec = w.r.read().reader().landing(&landing).unwrap();
    assert!(rec.checks.is_empty(), "no check result on a landing");
    let chosen = rec.chosen.clone().expect("the landing chose");
    let binds = w.r.read().coord_heads().alt_binds.clone();
    let check = |r: &Rdy, when: &str| {
        assert_eq!(options_of(&canon_block(r, &d)), want, "{when}");
        let rec = r.read().reader().landing(&landing).unwrap();
        assert_eq!(rec.chosen.as_ref(), Some(&chosen), "{when}");
        assert!(rec.checks.is_empty(), "{when}");
        assert_eq!(r.read().coord_heads().alt_binds, binds, "{when}");
        assert_eq!(
            r.read().resolve_alt(&w.set).unwrap().2.status,
            rdy_model::AltStatus::Chosen {
                member: "a".into(),
                landing
            },
            "{when}"
        );
    };
    w.r.reindex().unwrap();
    check(&w.r, "after a reindex");
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("p.rdydump");
    w.r.read().dump(&file).unwrap();
    let to = scratch.path().join("loaded");
    rdy_engine::load::load(&file, &to).unwrap();
    let loaded = Rdy::open(&to).unwrap();
    check(&loaded, "after a dump and a load");
    drop(w.dir);
}
