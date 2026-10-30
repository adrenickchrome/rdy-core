//! ck4: the fixed catalogue of agent checks and the six document case recipes (D-JQD5X). A case
//! is read data: it writes no record, and nothing here dispatches an agent.
mod common;

use rdy_engine::check::{
    AGENT_CHECKS, AgentCheck, BuiltCase, CaseInputs, CheckSpec, build_case, contradiction_cases,
};
use rdy_engine::semantic::case::{CASE_QUESTION, case_hash};
use rdy_engine::{E_BAD_INPUT, E_LIMIT, Materialized, Rdy, RdyError, ReadState, Scope, Session};
use rdy_engine::{WriteRequest, parse_ops};
use rdy_model::check_request::CheckSubject;
use rdy_model::{
    Block, BlockId, Change, ChangeLinks, EditorialClass, FieldValue, Hash, Justification, Link, Op,
    PairItem, Provenance, Role, pair_key,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// The `n`th id with prefix `p`.
fn nth(p: &str, n: usize) -> String {
    const A: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut tail = String::new();
    let mut k = n;
    for _ in 0..5 {
        tail.insert(0, A[k % 32] as char);
        k /= 32;
    }
    format!("{p}-{tail}")
}

fn block(i: &str, ty: &str, parent: Option<&str>, body: &str) -> Block {
    Block {
        id: id(i),
        block_type: ty.into(),
        parent: parent.map(id),
        order: "a0".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn field(mut b: Block, k: &str, v: &str) -> Block {
    b.fields.insert(k.into(), FieldValue::Str(v.into()));
    b
}

fn link(mut b: Block, kind: &str, target: &str) -> Block {
    b.links.entry(kind.into()).or_default().push(Link {
        target: id(target),
        part: None,
    });
    b
}

fn decision(i: &str, parent: Option<&str>, status: &str) -> Block {
    let b = field(block(i, "decision", parent, ""), "status", status);
    field(b, "statement", &format!("statement of {i}"))
}

fn state(blocks: Vec<Block>) -> Materialized {
    let ops: Vec<Op> = blocks
        .into_iter()
        .map(|block| Op::Create { block })
        .collect();
    Materialized::default().with(&ops)
}

fn doc_and_section() -> Vec<Block> {
    vec![
        field(
            field(block("d-D0C00", "doc", None, ""), "name", "spec"),
            "title",
            "Spec",
        ),
        field(
            block("s-SEC00", "section", Some("d-D0C00"), ""),
            "title",
            "Rules",
        ),
    ]
}

fn change(ops: Vec<Op>, justification: Option<Justification>) -> Change {
    Change {
        view: "w".into(),
        ops,
        deps: vec![],
        provenance: Provenance {
            actor: "agent:a".into(),
            run: None,
            role: Role::Writer,
            at_ms: 1_760_000_000_000,
        },
        justification,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn editorial() -> Option<Justification> {
    Some(Justification::Editorial {
        class: EditorialClass::Typo,
        reason: "typo".into(),
    })
}

fn implements(d: &str) -> Option<Justification> {
    Some(Justification::Implements(vec![id(d)]))
}

fn update(m: &Materialized, b: Block) -> Op {
    Op::Update {
        base: m.version_of(&b.id).unwrap_or(Hash([0; 32])),
        block: b,
    }
}

fn subject_blocks(ids: &[&str]) -> CheckSubject {
    CheckSubject {
        blocks: ids.iter().map(|i| id(i)).collect(),
        change: None,
        version: None,
        code: None,
    }
}

fn subject_change() -> CheckSubject {
    CheckSubject {
        blocks: vec![],
        change: Some(Hash::of(b"change")),
        version: None,
        code: None,
    }
}

fn subject_version() -> CheckSubject {
    CheckSubject {
        blocks: vec![],
        change: None,
        version: Some(Hash::of(b"version")),
        code: None,
    }
}

/// A project that only supplies the pack: every input of the recipe is built by hand.
fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn build(
    st: &ReadState,
    check: AgentCheck,
    subject: &CheckSubject,
    before: &Materialized,
    after: &Materialized,
    changes: &[Change],
) -> Result<BuiltCase, RdyError> {
    build_case(
        check,
        subject,
        CaseInputs {
            st,
            before,
            after,
            changes,
        },
    )
}

fn rows_of(case: &Value) -> Vec<(String, String)> {
    case["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["role"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn row<'a>(case: &'a Value, i: &str) -> &'a Value {
    case["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == i)
        .unwrap_or_else(|| panic!("no row {i} in {case:#}"))
}

fn text_of(side: &Value, f: &str) -> String {
    side["text"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["field"] == f)
        .unwrap_or_else(|| panic!("no field {f} in {side}"))["text"]
        .as_str()
        .unwrap()
        .to_string()
}

fn assert_wrong_subject(e: &RdyError, check: &str, kind: &str, who: &str, ty: &str) {
    assert_eq!(e.code, E_BAD_INPUT, "{e:?}");
    assert_eq!(e.message, format!("{check} takes {kind}; {who} is a {ty}"));
    assert_eq!(
        e.fix,
        format!("pass {kind}: rdy_review {{op: \"checks\"}} / rdy review checks")
    );
}

#[test]
fn ck4_catalogue_has_eight_checks_and_no_stage() {
    let want: [(&str, &str, &str); 8] = [
        ("contradiction", "two blocks or a decision", CASE_QUESTION),
        (
            "meaning_preservation",
            "a change or proposal version with editorial blocks",
            "Does each edit keep the meaning of the earlier text? Cite each changed meaning.",
        ),
        (
            "implementation_fidelity",
            "a change or proposal version with implements",
            "Does each changed block follow the decision it implements? Cite each difference.",
        ),
        (
            "decision_review",
            "a proposed decision",
            "Do the decision, its options, basis and premises support one another? Cite each concern.",
        ),
        (
            "entity_divergence",
            "an entity",
            "Do these blocks describe the same entity consistently? Cite each difference.",
        ),
        (
            "prose_drift",
            "a decision or proposal version that supersedes, revokes or refines one",
            "Does the unchanged prose still follow the decision? Cite each stale statement.",
        ),
        (
            "diff_contradicts",
            "a decision and a code range",
            "Does this code diff contradict the decision? Cite the decision and name each file and line in the note.",
        ),
        (
            "confirmation",
            "a decision with confirmation text",
            "Do the shown implementation and files meet the decision's confirmation text? Cite each unmet part.",
        ),
    ];
    assert_eq!(AGENT_CHECKS.len(), 8);
    for (spec, (name, subject, question)) in AGENT_CHECKS.iter().zip(want) {
        // an exhaustive pattern: a stage or a callback field would stop this compiling
        let CheckSpec {
            name: n,
            subject: s,
            question: q,
            verdicts,
        } = spec;
        assert_eq!((*n, *s, *q), (name, subject, question));
        let expected: &[&str] = if name == "contradiction" {
            &["tension", "no_tension"]
        } else {
            &["pass", "concern", "fail"]
        };
        assert_eq!(*verdicts, expected, "{name}");
    }
    // names parse by exact equality: no display name, no case folding
    for (i, c) in AgentCheck::ALL.iter().enumerate() {
        assert_eq!(c.spec().name, AGENT_CHECKS[i].name);
        assert_eq!(AgentCheck::parse(AGENT_CHECKS[i].name), Some(*c));
    }
    for alias in [
        "meaning preservation",
        "Meaning_Preservation",
        "tension",
        "any",
        "",
    ] {
        assert_eq!(AgentCheck::parse(alias), None, "{alias:?}");
    }
}

/// A person's session.
fn person() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}

fn agent(v: &str) -> Session {
    Session::agent(format!("agent:{v}"), Some(v.into()), Role::Writer).unwrap()
}

fn put(r: &mut Rdy, v: &str, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &agent(v),
        WriteRequest {
            view: v.into(),
            ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("write to {v}: {e}"))
}

fn raw_commit(r: &mut Rdy, blocks: Vec<Block>) {
    let ops = blocks
        .into_iter()
        .map(|block| Op::Create { block })
        .collect();
    let s = Session::human("human:t".into(), Role::Writer).unwrap();
    r.store_mut()
        .commit_to_view(
            "w",
            Change {
                view: "w".into(),
                ops,
                deps: vec![],
                provenance: s.provenance(),
                justification: None,
                message: "raw".into(),
                links: ChangeLinks::default(),
                read_identity: None,
            },
        )
        .expect("raw commit");
    r.invalidate();
}

#[test]
fn ck4_contradiction_reuses_the_pair_case() {
    let (_d, mut r) = project();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "after": "$a", "body": "B stands."}
        ]),
    );
    let p = r
        .propose(&agent("seed"), "seed", "seed", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(&mut r, &person(), &p);
    r.land(&person(), &p.id).unwrap();
    let (a, b) = (w.created["$a"].clone(), w.created["$b"].clone());

    // a pair held entirely in canon builds, with its canon items
    let st = r.read();
    let canon = st.materialized(&Scope::Inflight).unwrap();
    let got = build(
        &st,
        AgentCheck::Contradiction,
        &subject_blocks(&[&a, &b]),
        &canon,
        &canon,
        &[],
    )
    .unwrap();
    assert_eq!(got.case["kind"], "tension");
    assert_eq!(got.case["blocks"].as_array().unwrap().len(), 2);
    let want_blocks = {
        let mut v = vec![id(&a), id(&b)];
        v.sort();
        v
    };
    assert_eq!(got.subject_blocks, want_blocks);
    let tracking = st.reader().tracking();
    let items = st
        .pair_now(&[
            (
                id(&a),
                canon.get(&id(&a)).unwrap().content_hash_with(tracking),
            ),
            (
                id(&b),
                canon.get(&id(&b)).unwrap().content_hash_with(tracking),
            ),
        ])
        .unwrap()
        .unwrap();
    let key = pair_key(
        &items
            .iter()
            .map(|i| PairItem {
                block: i.block.clone(),
                content: i.content,
            })
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        got.hash,
        case_hash(&st.case_of(&key, &items).unwrap()),
        "the canon pair's case is the existing builder's"
    );
    drop(st);

    // an in-flight pair: byte for byte the case and hash the review queue serves
    put(
        &mut r,
        "w",
        json!([{"op": "update", "id": a, "body": "A holds, mostly."}]),
    );
    let st = r.read();
    let q = st
        .next_request("agent:r", &BTreeSet::new())
        .unwrap()
        .unwrap();
    let queued = st.case_of(&q.key, &q.items).unwrap();
    let inflight = st.materialized(&Scope::Inflight).unwrap();
    let pair: Vec<String> = q.items.iter().map(|i| i.block.to_string()).collect();
    let got = build(
        &st,
        AgentCheck::Contradiction,
        &subject_blocks(&[&pair[0], &pair[1]]),
        &canon,
        &inflight,
        &[],
    )
    .unwrap();
    assert_eq!(
        serde_json::to_string(&got.case).unwrap(),
        serde_json::to_string(&serde_json::to_value(&queued).unwrap()).unwrap(),
    );
    assert_eq!(got.hash, case_hash(&queued));
    drop(st);

    // one block that is no decision, or the same block twice, is a wrong subject
    let st = r.read();
    let inflight = st.materialized(&Scope::Inflight).unwrap();
    let e = build(
        &st,
        AgentCheck::Contradiction,
        &subject_blocks(&[&a]),
        &inflight,
        &inflight,
        &[],
    )
    .unwrap_err();
    assert_wrong_subject(&e, "contradiction", "two blocks or a decision", &a, "claim");
}

#[test]
fn ck4_contradiction_related_pairs_keep_order_and_cap() {
    let (_d, mut r) = project();
    // the subject sorts between its neighbours, so a pair is sorted by id, not by role
    let subject = "D-MMMM0";
    let mut d = decision(subject, Some("s-SEC00"), "proposed");
    let mut others = vec![];
    for n in 0..22 {
        let other = nth("D", n + 1);
        d = link(d, "refines", &other);
        others.push(decision(&other, Some("s-SEC00"), "accepted"));
    }
    let mut blocks = doc_and_section();
    blocks.push(d);
    blocks.extend(others);
    raw_commit(&mut r, blocks);
    let st = r.read();
    let m = st.materialized(&Scope::Inflight).unwrap();
    let inputs = || CaseInputs {
        st: &st,
        before: &m,
        after: &m,
        changes: &[],
    };
    let (cases, omitted) = contradiction_cases(&subject_blocks(&[subject]), inputs()).unwrap();
    assert_eq!(cases.len(), 20, "at most 20 pairs");
    assert_eq!(omitted, 2, "the rows the related query's cap dropped");
    let (rows, dropped) = st
        .related_decisions(&Scope::Inflight, &[id(subject)])
        .unwrap();
    assert_eq!(dropped, 2);
    for (case, row) in cases.iter().zip(&rows) {
        let mut want = vec![id(subject), id(&row.decision)];
        want.sort();
        assert_eq!(case.subject_blocks, want, "the related rows' order");
        assert_eq!(case.case["kind"], "tension");
    }
    // the single-case form is the first pair
    let top = build_case(
        AgentCheck::Contradiction,
        &subject_blocks(&[subject]),
        inputs(),
    )
    .unwrap();
    assert_eq!(top.hash, cases[0].hash);
    drop(st);

    // a decision related twice to one other makes one pair
    let (_d, mut r) = project();
    let x = decision("D-XXXX0", Some("s-SEC00"), "accepted");
    let y = decision("D-YYYY0", Some("s-SEC00"), "accepted");
    let d = link(
        link(
            link(
                decision("D-AAAA0", Some("s-SEC00"), "proposed"),
                "refines",
                "D-XXXX0",
            ),
            "refines",
            "D-YYYY0",
        ),
        "supersedes",
        "D-XXXX0",
    );
    let mut blocks = doc_and_section();
    blocks.extend([d, x, y]);
    raw_commit(&mut r, blocks);
    let st = r.read();
    let m = st.materialized(&Scope::Inflight).unwrap();
    let (cases, omitted) = contradiction_cases(
        &subject_blocks(&["D-AAAA0"]),
        CaseInputs {
            st: &st,
            before: &m,
            after: &m,
            changes: &[],
        },
    )
    .unwrap();
    let pairs: Vec<&Vec<BlockId>> = cases.iter().map(|c| &c.subject_blocks).collect();
    assert_eq!(
        pairs,
        [
            &vec![id("D-AAAA0"), id("D-XXXX0")],
            &vec![id("D-AAAA0"), id("D-YYYY0")]
        ]
    );
    assert_eq!(omitted, 0);
}

/// Commits `ops` straight to `view`.
fn raw_ops(r: &mut Rdy, view: &str, ops: Vec<Op>) {
    let s = Session::human("human:t".into(), Role::Writer).unwrap();
    r.store_mut()
        .commit_to_view(
            view,
            Change {
                view: view.into(),
                ops,
                deps: vec![],
                provenance: s.provenance(),
                justification: None,
                message: "raw".into(),
                links: ChangeLinks::default(),
                read_identity: None,
            },
        )
        .expect("raw commit");
    r.invalidate();
}

/// The decision review and contradiction cases of D-REVW0 in `view`: the hash and rows of the
/// review, and each pair's blocks and hash with the count left out.
#[allow(clippy::type_complexity)]
fn scoped_cases(
    r: &Rdy,
    view: &str,
) -> (
    Hash,
    Vec<(String, String)>,
    Vec<(Vec<BlockId>, Hash)>,
    usize,
) {
    let st = r.read();
    let m = st.materialized(&Scope::View(view.into())).unwrap();
    let subject = subject_blocks(&["D-REVW0"]);
    let review = build(&st, AgentCheck::DecisionReview, &subject, &m, &m, &[]).unwrap();
    let (cases, omitted) = contradiction_cases(
        &subject,
        CaseInputs {
            st: &st,
            before: &m,
            after: &m,
            changes: &[],
        },
    )
    .unwrap();
    (
        review.hash,
        rows_of(&review.case),
        cases
            .into_iter()
            .map(|c| (c.subject_blocks, c.hash))
            .collect(),
        omitted,
    )
}

#[test]
fn ck4_related_rows_read_the_supplied_state() {
    let (_d, mut r) = project();
    let mut blocks = doc_and_section();
    blocks.extend([
        link(
            reviewed("proposed", "The body.", "Founder"),
            "refines",
            "D-REKA0",
        ),
        finding(),
        decision("D-REKA0", Some("s-SEC00"), "accepted"),
        decision("D-0THER", Some("s-SEC00"), "proposed"),
    ]);
    raw_commit(&mut r, blocks);
    let scoped = scoped_cases(&r, "w");
    let related = |rows: &[(String, String)]| -> Vec<String> {
        rows.iter()
            .filter(|(_, role)| role == "related")
            .map(|(i, _)| i.clone())
            .collect()
    };
    assert_eq!(related(&scoped.1), ["D-REKA0"]);
    assert_eq!(scoped.2.len(), 1);

    // another view relates D-0THER to the subject: an incoming link, written on D-0THER
    r.view_fork(&agent("f"), "w", "f").unwrap();
    let st = r.read();
    let m = st.materialized(&Scope::View("f".into())).unwrap();
    let incoming = link(m.get(&id("D-0THER")).unwrap().clone(), "refines", "D-REVW0");
    let op = update(&m, incoming);
    drop(st);
    raw_ops(&mut r, "f", vec![op]);
    let st = r.read();
    let (rows, _) = st
        .related_decisions(&Scope::Inflight, &[id("D-REVW0")])
        .unwrap();
    assert!(
        rows.iter().any(|x| x.decision == "D-0THER"),
        "the in-flight scope holds the fork's link"
    );
    drop(st);
    assert_eq!(
        scoped_cases(&r, "w"),
        scoped,
        "another view's incoming link leaves the scoped cases unchanged"
    );
    let forked = scoped_cases(&r, "f");
    assert_eq!(
        related(&forked.1),
        ["D-REKA0", "D-0THER"],
        "the view that holds the link sees it"
    );
    assert_ne!(forked.0, scoped.0);
    assert_eq!(forked.2.len(), 2);

    // another view relates the subject itself: an outgoing link, written on D-REVW0
    r.view_fork(&agent("g"), "w", "g").unwrap();
    let st = r.read();
    let m = st.materialized(&Scope::View("g".into())).unwrap();
    let outgoing = link(
        m.get(&id("D-REVW0")).unwrap().clone(),
        "supersedes",
        "D-0THER",
    );
    let op = update(&m, outgoing);
    drop(st);
    raw_ops(&mut r, "g", vec![op]);
    assert_eq!(
        scoped_cases(&r, "w"),
        scoped,
        "another view's outgoing link leaves the scoped cases unchanged"
    );

    // a state no view holds, such as a proposal's candidate, reads its own relations
    let st = r.read();
    let w = st.materialized(&Scope::View("w".into())).unwrap();
    let extra = decision("D-CAND0", Some("s-SEC00"), "accepted");
    let tied = link(w.get(&id("D-REVW0")).unwrap().clone(), "refines", "D-CAND0");
    let candidate = w.with(&[Op::Create { block: extra }, update(&w, tied)]);
    let got = build(
        &st,
        AgentCheck::DecisionReview,
        &subject_blocks(&["D-REVW0"]),
        &w,
        &candidate,
        &[],
    )
    .unwrap();
    assert_eq!(related(&rows_of(&got.case)), ["D-CAND0", "D-REKA0"]);
}

/// A pair held in canon stays one case when another view edits a block of the pair.
#[test]
fn ck4_canon_pair_survives_another_views_edit() {
    let (_d, mut r) = project();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "after": "$a", "body": "B stands."}
        ]),
    );
    let p = r
        .propose(&agent("seed"), "seed", "seed", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(&mut r, &person(), &p);
    r.land(&person(), &p.id).unwrap();
    let (a, b) = (w.created["$a"].clone(), w.created["$b"].clone());
    r.view_create(&agent("w2"), "w2", None, false).unwrap();
    let subject = subject_blocks(&[&a, &b]);
    let scoped = |r: &Rdy| {
        let st = r.read();
        let m = st.materialized(&Scope::View("w2".into())).unwrap();
        build(&st, AgentCheck::Contradiction, &subject, &m, &m, &[]).map(|c| c.hash)
    };
    let before = scoped(&r).unwrap();
    put(
        &mut r,
        "f",
        json!([{"op": "update", "id": b, "body": "B stands, mostly."}]),
    );
    assert_eq!(scoped(&r).unwrap(), before);
}

/// Canon: a section; claims a, b, c, g (decided by the live D-GVRN0), x (to delete); and z, which
/// no selected change writes.
fn fold_world() -> Materialized {
    let mut blocks = doc_and_section();
    blocks.push(decision("D-GVRN0", Some("s-SEC00"), "accepted"));
    blocks.push(decision("D-1MP00", Some("s-SEC00"), "accepted"));
    blocks.push(link(
        block("c-AAAA0", "claim", Some("s-SEC00"), "A one."),
        "decided_by",
        "D-GVRN0",
    ));
    for (i, body) in [
        ("c-BBBB0", "B one."),
        ("c-CCCC0", "C one."),
        ("c-GGGG0", "G one."),
        ("c-XXXX0", "X one."),
        ("c-ZZZZ0", "Z one."),
    ] {
        blocks.push(block(i, "claim", Some("s-SEC00"), body));
    }
    state(blocks)
}

fn edit(m: &Materialized, i: &str, body: &str) -> Op {
    let mut b = m.get(&id(i)).unwrap().clone();
    b.body = body.into();
    update(m, b)
}

#[test]
fn ck4_meaning_and_fidelity_use_net_selected_changes() {
    let (_d, r) = project();
    let st = r.read();
    let before = fold_world();
    let ops1 = vec![
        edit(&before, "c-AAAA0", "A two."),
        Op::Delete {
            base: before.version_of(&id("c-XXXX0")).unwrap(),
            id: id("c-XXXX0"),
        },
        Op::Create {
            block: block("c-NEWW0", "claim", Some("s-SEC00"), "New."),
        },
    ];
    // b: implements, then editorial: its last justification is editorial
    let ops2 = vec![edit(&before, "c-BBBB0", "B two.")];
    let mid = before.with(&ops1).with(&ops2);
    let ops3 = vec![edit(&mid, "c-BBBB0", "B three.")];
    // c: editorial, then implements: its last justification is implements
    let ops4 = vec![edit(&before, "c-CCCC0", "C two.")];
    let mid = mid.with(&ops3).with(&ops4);
    let ops5 = vec![edit(&mid, "c-CCCC0", "C three.")];
    // g: editorial, then a change with no justification, which does not end it
    let ops6 = vec![edit(&before, "c-GGGG0", "G two.")];
    let mid = mid.with(&ops5).with(&ops6);
    let ops7 = vec![edit(&mid, "c-GGGG0", "G three.")];
    let changes = vec![
        change(ops1.clone(), editorial()),
        change(ops2.clone(), implements("D-1MP00")),
        change(ops3.clone(), editorial()),
        change(ops4.clone(), editorial()),
        change(ops5.clone(), implements("D-1MP00")),
        change(ops6.clone(), editorial()),
        change(ops7.clone(), None),
    ];
    let selected = mid.with(&ops7);
    // an edit the view holds and the subject does not select
    let after = selected.with(&[edit(&selected, "c-ZZZZ0", "Z edited elsewhere.")]);

    let got = build(
        &st,
        AgentCheck::MeaningPreservation,
        &subject_version(),
        &before,
        &after,
        &changes,
    )
    .unwrap();
    let c = &got.case;
    assert_eq!(c["kind"], "meaning_preservation");
    assert_eq!(
        c["question"],
        "Does each edit keep the meaning of the earlier text? Cite each changed meaning."
    );
    assert_eq!(
        rows_of(c),
        [
            ("c-AAAA0".to_string(), "subject".to_string()),
            ("c-XXXX0".into(), "subject".into()),
            ("c-NEWW0".into(), "subject".into()),
            ("c-BBBB0".into(), "subject".into()),
            ("c-GGGG0".into(), "subject".into()),
            ("D-GVRN0".into(), "decision".into()),
        ],
        "editorial blocks in landing order, then their live governing decisions; c and z stay out"
    );
    let a = row(c, "c-AAAA0");
    assert_eq!(text_of(&a["before"], "body"), "A one.");
    assert_eq!(text_of(&a["after"], "body"), "A two.");
    assert_eq!(a["doc"], "spec");
    assert_eq!(a["headings"], json!(["Spec", "Rules"]));
    assert_eq!(
        a["before"]["version"],
        before.version_of(&id("c-AAAA0")).unwrap().to_string()
    );
    assert!(row(c, "c-XXXX0")["after"].is_null(), "a delete");
    assert!(row(c, "c-NEWW0")["before"].is_null(), "a create");
    assert_eq!(text_of(&row(c, "c-BBBB0")["after"], "body"), "B three.");
    assert_eq!(text_of(&row(c, "c-BBBB0")["before"], "body"), "B one.");
    assert_eq!(text_of(&row(c, "c-GGGG0")["after"], "body"), "G three.");
    assert_eq!(c["code"], json!([]));
    assert_eq!(
        c["omitted"],
        json!({"blocks": 0, "text_bytes": 0, "code_files": 0, "code_bytes": 0})
    );
    assert_eq!(
        c["schema"],
        json!({
            "verdict": ["pass", "concern", "fail"],
            "citations": "0 to 20 {block, note} entries; concern and fail need at least one; each note has 1 to 2000 characters",
            "reason": "concern and fail need 1 to 2000 characters; pass may omit it",
        })
    );
    let mut keys: Vec<&String> = c.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "blocks",
            "code",
            "inputs_hash",
            "kind",
            "omitted",
            "question",
            "schema"
        ]
    );
    let mut row_keys: Vec<&String> = a.as_object().unwrap().keys().collect();
    row_keys.sort();
    assert_eq!(
        row_keys,
        ["after", "before", "doc", "headings", "id", "role", "type"]
    );
    let mut wanted = vec![
        id("c-AAAA0"),
        id("c-BBBB0"),
        id("c-GGGG0"),
        id("c-NEWW0"),
        id("c-XXXX0"),
    ];
    wanted.sort();
    assert_eq!(got.subject_blocks, wanted, "sorted, primary blocks only");

    let got = build(
        &st,
        AgentCheck::ImplementationFidelity,
        &subject_version(),
        &before,
        &after,
        &changes,
    )
    .unwrap();
    assert_eq!(
        rows_of(&got.case),
        [
            ("D-1MP00".to_string(), "decision".to_string()),
            ("c-CCCC0".into(), "subject".into()),
        ],
        "only c's last justification is implements"
    );
    let d = row(&got.case, "D-1MP00");
    assert_eq!(text_of(&d["after"], "statement"), "statement of D-1MP00");
    assert_eq!(got.subject_blocks, [id("c-CCCC0")]);

    // a decision two blocks implement shows once
    let two = vec![
        change(vec![edit(&before, "c-AAAA0", "A2.")], implements("D-1MP00")),
        change(vec![edit(&before, "c-BBBB0", "B2.")], implements("D-1MP00")),
    ];
    let after2 = before.with(&two[0].ops).with(&two[1].ops);
    let got = build(
        &st,
        AgentCheck::ImplementationFidelity,
        &subject_change(),
        &before,
        &after2,
        &two,
    )
    .unwrap();
    assert_eq!(
        rows_of(&got.case),
        [
            ("D-1MP00".to_string(), "decision".to_string()),
            ("c-AAAA0".into(), "subject".into()),
            ("c-BBBB0".into(), "subject".into()),
        ]
    );

    // the empty sets are wrong subjects, each in its exact words
    let none = vec![change(
        vec![edit(&before, "c-AAAA0", "A2.")],
        Some(Justification::Reflects {
            sources: vec![],
            code: true,
        }),
    )];
    let after3 = before.with(&none[0].ops);
    let change_id = Hash::of(b"change").to_string();
    let version_id = Hash::of(b"version").to_string();
    let meaning = "a change or proposal version with editorial blocks";
    let fidelity = "a change or proposal version with implements";
    for (check, name, kind, subject, who, ty) in [
        (
            AgentCheck::MeaningPreservation,
            "meaning_preservation",
            meaning,
            subject_change(),
            &change_id,
            "change without editorial blocks",
        ),
        (
            AgentCheck::MeaningPreservation,
            "meaning_preservation",
            meaning,
            subject_version(),
            &version_id,
            "proposal version without editorial blocks",
        ),
        (
            AgentCheck::ImplementationFidelity,
            "implementation_fidelity",
            fidelity,
            subject_change(),
            &change_id,
            "change without implements",
        ),
        (
            AgentCheck::ImplementationFidelity,
            "implementation_fidelity",
            fidelity,
            subject_version(),
            &version_id,
            "proposal version without implements",
        ),
    ] {
        let e = build(&st, check, &subject, &before, &after3, &none).unwrap_err();
        assert_wrong_subject(&e, name, kind, who, ty);
    }
    // a block is no change
    let e = build(
        &st,
        AgentCheck::MeaningPreservation,
        &subject_blocks(&["c-AAAA0"]),
        &before,
        &after3,
        &none,
    )
    .unwrap_err();
    assert_wrong_subject(&e, "meaning_preservation", meaning, "c-AAAA0", "claim");
}

/// The decision of the decision-review pin, with its six selected fields.
fn reviewed(status: &str, body: &str, attested: &str) -> Block {
    let mut d = decision("D-REVW0", Some("s-SEC00"), status);
    d.body = body.into();
    d = field(d, "statement", "We keep it.");
    d = field(d, "reversibility", "one_way");
    d = field(d, "authority_basis", "recorded_ruling");
    d = field(d, "attested_by", attested);
    d.fields.insert(
        "options".into(),
        FieldValue::List(vec![
            FieldValue::Str("zebra".into()),
            FieldValue::Str("apple".into()),
        ]),
    );
    link(d, "depends_on", "F-F1ND0")
}

fn finding() -> Block {
    let entry = |q: &str| {
        FieldValue::Map(BTreeMap::from([(
            "quote".to_string(),
            FieldValue::Str(q.into()),
        )]))
    };
    let mut f = block("F-F1ND0", "finding", Some("s-SEC00"), "It holds.");
    f.fields.insert(
        "evidence".into(),
        FieldValue::List(vec![entry("second first"), entry("first second")]),
    );
    f
}

#[test]
fn ck4_decision_review_includes_findings_and_options() {
    let (_d, mut r) = project();
    let mut blocks = doc_and_section();
    let rel = decision("D-REKA0", Some("s-SEC00"), "accepted");
    blocks.extend([
        link(
            reviewed("proposed", "The body.", "Founder, mandate 7"),
            "refines",
            "D-REKA0",
        ),
        finding(),
        rel,
        decision("D-ACCP0", Some("s-SEC00"), "accepted"),
    ]);
    raw_commit(&mut r, blocks);
    let st = r.read();
    let m = st.materialized(&Scope::Inflight).unwrap();
    let got = build(
        &st,
        AgentCheck::DecisionReview,
        &subject_blocks(&["D-REVW0"]),
        &m,
        &m,
        &[],
    )
    .unwrap();
    let c = &got.case;
    assert_eq!(
        rows_of(c),
        [
            ("D-REVW0".to_string(), "subject".to_string()),
            ("F-F1ND0".into(), "finding".into()),
            ("D-REKA0".into(), "related".into()),
        ]
    );
    let after = &row(c, "D-REVW0")["after"];
    let fields: Vec<(&str, &str)> = after["text"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["field"].as_str().unwrap(), t["text"].as_str().unwrap()))
        .collect();
    assert_eq!(
        fields,
        [
            ("statement", "We keep it."),
            ("body", "The body."),
            ("options[0]", "zebra"),
            ("options[1]", "apple"),
            ("reversibility", "one_way"),
            ("authority_basis", "recorded_ruling"),
            ("attested_by", "Founder, mandate 7"),
        ],
        "the six selected fields, in order, options in list order"
    );
    let f = &row(c, "F-F1ND0")["after"];
    assert_eq!(text_of(f, "evidence[0].quote"), "second first");
    assert_eq!(text_of(f, "evidence[1].quote"), "first second");
    assert_eq!(got.subject_blocks, [id("D-REVW0")]);

    let e = build(
        &st,
        AgentCheck::DecisionReview,
        &subject_blocks(&["D-ACCP0"]),
        &m,
        &m,
        &[],
    )
    .unwrap_err();
    assert_wrong_subject(
        &e,
        "decision_review",
        "a proposed decision",
        "D-ACCP0",
        "decision with status accepted",
    );
    let e = build(
        &st,
        AgentCheck::DecisionReview,
        &subject_blocks(&["F-F1ND0"]),
        &m,
        &m,
        &[],
    )
    .unwrap_err();
    assert_wrong_subject(
        &e,
        "decision_review",
        "a proposed decision",
        "F-F1ND0",
        "finding",
    );
    drop(st);

    // a metadata value or a field name moves the digest and counts toward the byte caps
    let (_d, r) = project();
    let st = r.read();
    let world = |d: Block| {
        let mut blocks = doc_and_section();
        blocks.extend([d, finding()]);
        state(blocks)
    };
    let hash_of = |d: Block| {
        let m = world(d);
        build(
            &st,
            AgentCheck::DecisionReview,
            &subject_blocks(&["D-REVW0"]),
            &m,
            &m,
            &[],
        )
        .unwrap()
        .case["inputs_hash"]
            .clone()
    };
    let base = reviewed("proposed", "B.", "Founder");
    assert_ne!(
        hash_of(base.clone()),
        hash_of(reviewed("proposed", "B.", "Another"))
    );
    let mut moved = base.clone();
    let v = moved.fields.remove("attested_by").unwrap();
    moved.fields.insert("attested_byx".into(), v);
    assert_ne!(hash_of(base.clone()), hash_of(moved));

    // the row's budget: statement, options, each metadata name and value, and the body
    let meta: usize = ["reversibility", "one_way"].concat().len()
        + ["authority_basis", "recorded_ruling"].concat().len()
        + ["attested_by", "F"].concat().len();
    let fixed = "We keep it.".len() + "zebra".len() + "apple".len() + meta;
    let omitted = |body_len: usize| {
        let m = world(reviewed("proposed", &"b".repeat(body_len), "F"));
        build(
            &st,
            AgentCheck::DecisionReview,
            &subject_blocks(&["D-REVW0"]),
            &m,
            &m,
            &[],
        )
        .unwrap()
        .case["omitted"]["text_bytes"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(omitted(16_384 - fixed), 0, "exactly the row's cap");
    assert!(omitted(16_384 - fixed + 1) > 0, "one past the row's cap");
}

#[test]
fn ck4_entity_and_prose_cases_use_existing_graph_rules() {
    let (_d, r) = project();
    let st = r.read();

    // entity: same doc first, then doc path with no doc last, then id; three tie kinds
    let term = field(
        block("t-TERM0", "term", Some("s-SEC00"), "Hub rule."),
        "name",
        "Hub",
    );
    let other_doc = field(
        field(block("d-D0C01", "doc", None, ""), "name", "other"),
        "title",
        "Other",
    );
    let mut blocks = doc_and_section();
    blocks.extend([
        term.clone(),
        other_doc,
        link(
            block("c-ZZ000", "claim", Some("s-SEC00"), "same doc, by a link"),
            "describes",
            "t-TERM0",
        ),
        block(
            "c-YY000",
            "claim",
            Some("s-SEC00"),
            "same doc, by a reference {{t-TERM0}}",
        ),
        link(
            field(
                block("D-AA000", "decision", Some("d-D0C01"), ""),
                "status",
                "proposed",
            ),
            "affects",
            "t-TERM0",
        ),
        link(
            block("c-AA000", "claim", None, "no doc"),
            "describes",
            "t-TERM0",
        ),
        block("c-NN000", "claim", Some("s-SEC00"), "Not tied."),
    ]);
    let m = state(blocks.clone());
    let got = build(
        &st,
        AgentCheck::EntityDivergence,
        &subject_blocks(&["t-TERM0"]),
        &m,
        &m,
        &[],
    )
    .unwrap();
    assert_eq!(
        rows_of(&got.case),
        [
            ("t-TERM0".to_string(), "subject".to_string()),
            ("c-YY000".into(), "entity".into()),
            ("c-ZZ000".into(), "entity".into()),
            ("D-AA000".into(), "entity".into()),
            ("c-AA000".into(), "entity".into()),
        ]
    );
    assert!(row(&got.case, "c-AA000")["doc"].is_null(), "null doc");
    assert_eq!(row(&got.case, "c-AA000")["headings"], json!([]));
    assert_eq!(got.subject_blocks, [id("t-TERM0")]);
    let e = build(
        &st,
        AgentCheck::EntityDivergence,
        &subject_blocks(&["c-NN000"]),
        &m,
        &m,
        &[],
    )
    .unwrap_err();
    assert_wrong_subject(&e, "entity_divergence", "an entity", "c-NN000", "claim");

    // an unknown id keeps the resolver's refusal
    let e = build(
        &st,
        AgentCheck::EntityDivergence,
        &subject_blocks(&["t-N0NE0"]),
        &m,
        &m,
        &[],
    )
    .unwrap_err();
    assert_eq!(e.code, rdy_engine::E_UNKNOWN_BLOCK);
    assert_eq!(e.message, "no block t-N0NE0");

    // entity: a cap of 20 contextual rows
    let mut many = doc_and_section();
    many.push(term.clone());
    for n in 0..21 {
        many.push(link(
            block(&nth("c", n), "claim", Some("s-SEC00"), "x"),
            "describes",
            "t-TERM0",
        ));
    }
    let m21 = state(many);
    let got = build(
        &st,
        AgentCheck::EntityDivergence,
        &subject_blocks(&["t-TERM0"]),
        &m21,
        &m21,
        &[],
    )
    .unwrap();
    assert_eq!(rows_of(&got.case).len(), 21, "the entity and 20 blocks");
    assert_eq!(got.case["omitted"]["blocks"], 1);
    assert_eq!(row(&got.case, &nth("c", 19))["role"], "entity");

    // prose: all four routes reach informative blocks; a normative block never takes part
    let prose = |i: &str, doc: &str| block(i, "prose", Some(doc), "Prose.");
    let world = || {
        let mut blocks = doc_and_section();
        blocks.extend([
            term.clone(),
            decision("D-PRE00", Some("s-SEC00"), "accepted"),
            link(
                link(
                    decision("D-NEW00", Some("s-SEC00"), "proposed"),
                    "supersedes",
                    "D-PRE00",
                ),
                "affects",
                "t-TERM0",
            ),
            link(prose("p-DEC1D", "s-SEC00"), "decided_by", "D-PRE00"),
            link(prose("p-1MP00", "s-SEC00"), "implements", "D-NEW00"),
            prose("p-AFFCT", "s-SEC00"),
            link(prose("p-ENT1T", "s-SEC00"), "describes", "t-TERM0"),
            prose("p-N0NE0", "s-SEC00"),
            link(
                block("c-N0RM0", "claim", Some("s-SEC00"), "Normative."),
                "decided_by",
                "D-PRE00",
            ),
        ]);
        blocks
    };
    let mut blocks = world();
    let i = blocks.iter().position(|b| b.id == id("D-PRE00")).unwrap();
    blocks[i] = link(blocks[i].clone(), "affects", "p-AFFCT");
    let m = state(blocks);
    let got = build(
        &st,
        AgentCheck::ProseDrift,
        &subject_blocks(&["D-NEW00"]),
        &m,
        &m,
        &[],
    )
    .unwrap();
    let rows = rows_of(&got.case);
    assert_eq!(rows[0], ("D-NEW00".to_string(), "subject".to_string()));
    assert_eq!(rows[1], ("D-PRE00".to_string(), "decision".to_string()));
    let mut governed: Vec<String> = rows[2..].iter().map(|r| r.0.clone()).collect();
    governed.sort();
    assert_eq!(
        governed,
        ["p-1MP00", "p-AFFCT", "p-DEC1D", "p-ENT1T"],
        "decided_by, implements, affects and describes/affects"
    );
    assert!(rows[2..].iter().all(|r| r.1 == "governed"));

    // a version: only what it left unchanged
    let selected = vec![
        change(
            vec![Op::Create {
                block: link(
                    decision("D-NEW01", Some("s-SEC00"), "proposed"),
                    "revokes",
                    "D-PRE00",
                ),
            }],
            None,
        ),
        change(vec![edit(&m, "p-DEC1D", "Rewritten.")], editorial()),
    ];
    let after = m.with(&selected[0].ops).with(&selected[1].ops);
    let got = build(
        &st,
        AgentCheck::ProseDrift,
        &subject_version(),
        &m,
        &after,
        &selected,
    )
    .unwrap();
    let rows = rows_of(&got.case);
    assert_eq!(rows[0], ("D-NEW01".to_string(), "subject".to_string()));
    assert_eq!(rows[1], ("D-PRE00".to_string(), "decision".to_string()));
    let prose_rows: Vec<&str> = rows[2..].iter().map(|r| r.0.as_str()).collect();
    assert!(
        !prose_rows.contains(&"p-DEC1D"),
        "the version wrote it: {prose_rows:?}"
    );
    assert!(prose_rows.contains(&"p-AFFCT"), "{prose_rows:?}");
    assert_eq!(got.subject_blocks, [id("D-NEW01")]);

    // a decision with no predecessor compares to itself
    let m2 = state({
        let mut b = doc_and_section();
        b.extend([
            decision("D-S0K00", Some("s-SEC00"), "accepted"),
            link(prose("p-G0V00", "s-SEC00"), "decided_by", "D-S0K00"),
        ]);
        b
    });
    let got = build(
        &st,
        AgentCheck::ProseDrift,
        &subject_blocks(&["D-S0K00"]),
        &m2,
        &m2,
        &[],
    )
    .unwrap();
    assert_eq!(
        rows_of(&got.case),
        [
            ("D-S0K00".to_string(), "subject".to_string()),
            ("p-G0V00".into(), "governed".into()),
        ]
    );

    // the empty sets are wrong subjects
    let kind = "a decision or proposal version that supersedes, revokes or refines one";
    let plain = vec![change(vec![edit(&m, "p-DEC1D", "Rewritten.")], editorial())];
    let e = build(
        &st,
        AgentCheck::ProseDrift,
        &subject_version(),
        &m,
        &m.with(&plain[0].ops),
        &plain,
    )
    .unwrap_err();
    assert_wrong_subject(
        &e,
        "prose_drift",
        kind,
        &Hash::of(b"version").to_string(),
        "proposal version without supersedes, revokes or refines",
    );
    let e = build(
        &st,
        AgentCheck::ProseDrift,
        &subject_change(),
        &m,
        &m,
        &plain,
    )
    .unwrap_err();
    assert_wrong_subject(
        &e,
        "prose_drift",
        kind,
        &Hash::of(b"change").to_string(),
        "change",
    );

    // prose: same doc first, then path; 20 kept of 21
    let mut blocks = doc_and_section();
    blocks.push(decision("D-CAP00", Some("s-SEC00"), "accepted"));
    blocks.push(field(
        field(block("d-AAAAA", "doc", None, ""), "name", "aaa"),
        "title",
        "A",
    ));
    // sorts first by id and by path, yet sits in another doc
    blocks.push(link(
        block("p-00000", "prose", Some("d-AAAAA"), "Far."),
        "decided_by",
        "D-CAP00",
    ));
    for n in 1..=20 {
        blocks.push(link(
            block(&nth("p", n), "prose", Some("s-SEC00"), "Near."),
            "decided_by",
            "D-CAP00",
        ));
    }
    let m3 = state(blocks);
    let got = build(
        &st,
        AgentCheck::ProseDrift,
        &subject_blocks(&["D-CAP00"]),
        &m3,
        &m3,
        &[],
    )
    .unwrap();
    let rows = rows_of(&got.case);
    assert_eq!(rows.len(), 21, "the decision and 20 prose blocks");
    assert!(
        rows.iter().all(|r| r.0 != "p-00000"),
        "the 21st, in another doc, is cut"
    );
    assert_eq!(got.case["omitted"]["blocks"], 1);
}

/// `n` claims, each with body `body`, all edited by one editorial change; the case and its inputs.
fn edited_claims(st: &ReadState, bodies: &[String]) -> BuiltCase {
    let before = state(doc_and_section());
    let ops: Vec<Op> = bodies
        .iter()
        .enumerate()
        .map(|(n, b)| Op::Create {
            block: block(&nth("c", n), "claim", Some("s-SEC00"), b),
        })
        .collect();
    let changes = vec![change(ops, editorial())];
    let after = before.with(&changes[0].ops);
    build(
        st,
        AgentCheck::MeaningPreservation,
        &subject_change(),
        &before,
        &after,
        &changes,
    )
    .unwrap()
}

#[test]
fn ck4_case_bounds_and_omissions_are_exact() {
    let (_d, r) = project();
    let st = r.read();

    // 64 rows and one past
    let got = edited_claims(&st, &vec!["x".to_string(); 64]);
    assert_eq!(rows_of(&got.case).len(), 64);
    assert_eq!(got.case["omitted"]["blocks"], 0);
    let got = edited_claims(&st, &vec!["x".to_string(); 65]);
    assert_eq!(rows_of(&got.case).len(), 64);
    assert_eq!(got.case["omitted"]["blocks"], 1);
    assert_eq!(
        got.case["omitted"]["text_bytes"], 1,
        "the dropped row's text"
    );
    assert_eq!(
        got.subject_blocks.len(),
        65,
        "a dropped row is still a subject"
    );

    // 16,384 bytes in a row and one past, cut at a Unicode boundary
    let body = |n: usize| "é".repeat(n / 2) + &"x".repeat(n % 2);
    let got = edited_claims(&st, &[body(16_384)]);
    assert_eq!(
        text_of(&row(&got.case, &nth("c", 0))["after"], "body").len(),
        16_384
    );
    assert_eq!(got.case["omitted"]["text_bytes"], 0);
    let got = edited_claims(&st, &[body(16_385)]);
    let kept = text_of(&row(&got.case, &nth("c", 0))["after"], "body");
    assert_eq!(kept.len(), 16_384, "the whole characters that fit");
    assert_eq!(got.case["omitted"]["text_bytes"], 1);
    // a character the cut would split is left out whole
    let got = edited_claims(&st, &[format!("x{}", "é".repeat(8_192))]);
    let kept = text_of(&row(&got.case, &nth("c", 0))["after"], "body");
    assert_eq!(kept.len(), 16_383);
    assert_eq!(got.case["omitted"]["text_bytes"], 2);

    // 262,144 bytes in a case and one past
    let got = edited_claims(&st, &vec!["x".repeat(16_384); 16]);
    assert_eq!(got.case["omitted"]["text_bytes"], 0);
    let mut bodies = vec!["x".repeat(16_384); 16];
    bodies.push("y".to_string());
    let got = edited_claims(&st, &bodies);
    assert_eq!(got.case["omitted"]["text_bytes"], 1);
    assert_eq!(
        got.case["omitted"]["blocks"], 0,
        "the row stays, cut to nothing"
    );
    assert_eq!(text_of(&row(&got.case, &nth("c", 16))["after"], "body"), "");

    // a row with both sides shares its budget: neither side starves the other
    let before = state({
        let mut b = doc_and_section();
        b.push(block(
            "c-AAAA0",
            "claim",
            Some("s-SEC00"),
            &"o".repeat(16_384),
        ));
        b
    });
    let mut edited = before.get(&id("c-AAAA0")).unwrap().clone();
    edited.body = "n".repeat(16_384);
    let changes = vec![change(vec![update(&before, edited)], editorial())];
    let after = before.with(&changes[0].ops);
    let got = build(
        &st,
        AgentCheck::MeaningPreservation,
        &subject_change(),
        &before,
        &after,
        &changes,
    )
    .unwrap();
    let r0 = row(&got.case, "c-AAAA0");
    assert_eq!(text_of(&r0["before"], "body").len(), 8_192);
    assert_eq!(text_of(&r0["after"], "body").len(), 8_192);
    assert_eq!(got.case["omitted"]["text_bytes"], 16_384);

    // headings: 16 of 17, each 256 characters; what is cut counts as text bytes
    let mut blocks = vec![field(
        field(block("d-D0C00", "doc", None, ""), "name", "spec"),
        "title",
        &"h".repeat(257),
    )];
    let mut parent = "d-D0C00".to_string();
    for n in 0..16 {
        let s = nth("s", n);
        blocks.push(field(block(&s, "section", Some(&parent), ""), "title", "t"));
        parent = s;
    }
    let before = state(blocks);
    let changes = vec![change(
        vec![Op::Create {
            block: block("c-AAAA0", "claim", Some(&parent), "x"),
        }],
        editorial(),
    )];
    let after = before.with(&changes[0].ops);
    let got = build(
        &st,
        AgentCheck::MeaningPreservation,
        &subject_change(),
        &before,
        &after,
        &changes,
    )
    .unwrap();
    let h = row(&got.case, "c-AAAA0")["headings"].as_array().unwrap();
    assert_eq!(h.len(), 16);
    assert_eq!(h[0].as_str().unwrap().chars().count(), 256);
    assert_eq!(
        got.case["omitted"]["text_bytes"], 2,
        "one byte of the long title and the one-byte 17th heading"
    );

    // 100,000 visited blocks and one past: the only traversal refusal
    let mut blocks = doc_and_section();
    blocks.push(field(
        block("t-TERM0", "term", Some("s-SEC00"), "Hub."),
        "name",
        "Hub",
    ));
    for n in 0..99_999 {
        blocks.push(link(
            block(&nth("c", n), "claim", None, "x"),
            "describes",
            "t-TERM0",
        ));
    }
    let m = state(blocks);
    let got = build(
        &st,
        AgentCheck::EntityDivergence,
        &subject_blocks(&["t-TERM0"]),
        &m,
        &m,
        &[],
    )
    .unwrap();
    assert_eq!(rows_of(&got.case).len(), 21, "the entity and 20 blocks");
    assert_eq!(got.case["omitted"]["blocks"], 99_979);
    let one_more = m.with(&[Op::Create {
        block: link(
            block(&nth("c", 99_999), "claim", None, "x"),
            "describes",
            "t-TERM0",
        ),
    }]);
    let e = build(
        &st,
        AgentCheck::EntityDivergence,
        &subject_blocks(&["t-TERM0"]),
        &one_more,
        &one_more,
        &[],
    )
    .unwrap_err();
    assert_eq!(e.code, E_LIMIT);
    assert_eq!(e.message, "check case exceeds 100000 blocks");
    assert_eq!(e.fix, "run the check on a smaller subject");
}

#[test]
fn ck4_case_hash_ignores_request_identity_but_covers_omitted_input() {
    let (_d, r) = project();
    let st = r.read();

    // no request id, requester, time, mode or label reaches a case: the same inputs under
    // another change and another version hash the same
    let bodies = vec!["x".to_string(); 65];
    let a = edited_claims(&st, &bodies);
    let before = state(doc_and_section());
    let ops: Vec<Op> = bodies
        .iter()
        .enumerate()
        .map(|(n, b)| Op::Create {
            block: block(&nth("c", n), "claim", Some("s-SEC00"), b),
        })
        .collect();
    let mut other = change(ops, editorial());
    other.provenance.actor = "agent:someone-else".into();
    other.provenance.at_ms = 1_900_000_000_000;
    other.message = "another message".into();
    let after = before.with(&other.ops);
    let b = build(
        &st,
        AgentCheck::MeaningPreservation,
        &subject_version(),
        &before,
        &after,
        std::slice::from_ref(&other),
    )
    .unwrap();
    assert_eq!(
        a.hash, b.hash,
        "the requester, the time and the label stay out"
    );

    // the case hash has its own domain over the sorted-key JSON
    let mut bytes = b"rdy-case-2\0".to_vec();
    bytes.extend(serde_json::to_vec(&a.case).unwrap());
    assert!(a.case.to_string().starts_with("{\"blocks\""), "keys sorted");
    assert_eq!(a.hash, Hash::of(&bytes));

    // a change in an omitted row moves it, though the shown rows are the same
    let mut moved = bodies.clone();
    moved[64] = "y".to_string();
    let c = edited_claims(&st, &moved);
    assert_eq!(a.case["blocks"], c.case["blocks"]);
    assert_ne!(a.case["inputs_hash"], c.case["inputs_hash"]);
    assert_ne!(a.hash, c.hash);

    // and so does a change past a clip, with the same omission count
    let long = |tail: char| vec![format!("{}{tail}", "x".repeat(16_384))];
    let (c1, c2) = (
        edited_claims(&st, &long('a')),
        edited_claims(&st, &long('b')),
    );
    assert_eq!(
        c1.case["blocks"][0]["after"]["text"],
        c2.case["blocks"][0]["after"]["text"]
    );
    assert_eq!(c1.case["omitted"], c2.case["omitted"]);
    assert_ne!(c1.hash, c2.hash);

    // a selected field too: the decision-review digest covers it
    let world = |attested: &str| {
        let mut blocks = doc_and_section();
        blocks.extend([reviewed("proposed", "B.", attested), finding()]);
        state(blocks)
    };
    let (m1, m2) = (world("One"), world("Two"));
    let review = |m: &Materialized| {
        build(
            &st,
            AgentCheck::DecisionReview,
            &subject_blocks(&["D-REVW0"]),
            m,
            m,
            &[],
        )
        .unwrap()
        .hash
    };
    assert_eq!(review(&m1), review(&m1));
    assert_ne!(review(&m1), review(&m2));
}
