//! cc6: coverage issues (FW-CH1, FW-CV1, FW-CV4, FW-CH5, FW-CH6, FW-CH7): what each dimension
//! counts on each block, and the fix each issue names.
use rdy_engine::coverage::{CoverageIssues, Dimension, IssueCode};
use rdy_engine::mention_review::MentionVerdict;
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::json;
use std::collections::BTreeMap;

/// Doc `ops` (overview: needs Summary and Goals; has Summary, stable, and Empty) holds term
/// Ledger, claim c1 (names Ledger twice, unlinked, no decision), claim c2 (describes Ledger,
/// decided_by d1), question q (unanswered), decision d1 (live, applied by c2), decision d2
/// (live, nothing applies it), decision d3 (live, needs_content: false). Doc `lone` holds one
/// prose block. Landed.
fn world(r: &mut Rdy) -> BTreeMap<String, String> {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary", "lifecycle": "stable"}},
        {"op": "create", "type": "section", "id": "$empty", "parent": "$o", "fields": {"title": "Empty"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "The Ledger is append-only, and the Ledger never shrinks."},
        {"op": "create", "type": "decision", "id": "$d1", "parent": "$s", "fields": {"statement": "Balances live in one store", "status": "proposed"}},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Balances are kept in one place.",
         "links": {"describes": [{"target": "$t"}], "decided_by": [{"target": "$d1"}]}},
        {"op": "create", "type": "decision", "id": "$d2", "parent": "$s", "fields": {"statement": "Refunds need two approvers", "status": "proposed"}},
        {"op": "create", "type": "decision", "id": "$d3", "parent": "$s", "fields": {"statement": "Reviews happen on Fridays", "status": "proposed", "needs_content": false}},
        {"op": "create", "type": "question", "id": "$q", "parent": "$s", "fields": {"prompt": "Who approves refunds?"}},
        {"op": "create", "type": "doc", "id": "$l", "fields": {"name": "lone", "title": "Lone", "doc_type": "page"}},
        {"op": "create", "type": "prose", "parent": "$l", "body": "Nothing here."}
    ]))
    .unwrap();
    land(r, "work").unwrap(); // the landing accepts d1, d2 and d3: they are live
    w.created
}

fn issues(r: &Rdy) -> std::sync::Arc<CoverageIssues> {
    r.read().coverage_issues(&Scope::Canon).unwrap()
}

fn codes(i: &CoverageIssues, id: &str) -> Vec<IssueCode> {
    i.blocks[&BlockId::parse(id).unwrap()]
        .issues
        .iter()
        .map(|x| x.code)
        .collect()
}

fn id(text: &str) -> BlockId {
    BlockId::parse(text).unwrap()
}

/// Two landed claims with one body in a draft section: a duplicate definition that persists in
/// canon (FW-CH6a), advisory because the content is draft. The two claims and the tension's
/// label.
fn canon_duplicate_pair(r: &mut Rdy) -> (BlockId, BlockId, String) {
    let w = write(r, "dup", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "fees", "title": "Fees", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Rules", "lifecycle": "draft"}},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "Fees are charged every month."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "Fees are charged every month."}
    ]))
    .unwrap();
    land(r, "dup").unwrap();
    let (a, b) = (id(&w.created["$a"]), id(&w.created["$b"]));
    let st = r.read();
    let set = st.tensions().unwrap();
    let t = set
        .iter()
        .find(|t| {
            t.kind == tensions::TensionKind::DuplicateDefinition
                && t.blocks().contains(&a)
                && t.blocks().contains(&b)
        })
        .expect("a canon duplicate of the two claims");
    assert!(set.from_canon(&t.id), "every side is canon");
    (a, b, tensions::tension_label(&t.id))
}

/// A landed claim that cites a target at a pin, then a landed edit of the target. The claim and
/// the target.
fn landed_citation_then_target_changed(r: &mut Rdy) -> (BlockId, BlockId) {
    let w = write(r, "cite", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "pay", "title": "Pay", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Rules"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Wallet"}, "body": "Holds cards."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Payment uses {{$t}}."}
    ]))
    .unwrap();
    land(r, "cite").unwrap();
    let (c, t) = (id(&w.created["$c"]), id(&w.created["$t"]));
    write(
        r,
        "edit",
        json!([{"op": "update", "id": t.to_string(), "body": "Holds cards and vouchers."}]),
    )
    .unwrap();
    land(r, "edit").unwrap();
    (c, t)
}

/// A landed claim, then edited from views `a` and `b`: an edit collision on a landed block. The
/// claim and view `a`.
fn inflight_collision(r: &mut Rdy) -> (BlockId, String) {
    let w = write(r, "base", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ship", "title": "Ship", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Rules"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Parcels ship daily."}
    ]))
    .unwrap();
    land(r, "base").unwrap();
    let c = id(&w.created["$c"]);
    write(
        r,
        "a",
        json!([{"op": "update", "id": c.to_string(), "body": "Parcels ship twice a day."}]),
    )
    .unwrap();
    write(
        r,
        "b",
        json!([{"op": "update", "id": c.to_string(), "body": "Parcels ship weekly."}]),
    )
    .unwrap();
    (c, "a".to_string())
}

#[test]
fn cc6_each_dimension_counts_its_checks() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let i = issues(&r);
    let checks = |id: &str| i.blocks[&BlockId::parse(id).unwrap()].checks;
    // c1: content (1) + one mention of Ledger (1) in Grounded; 1 each in Consistent, Current, Connected
    assert_eq!(checks(&c["$c1"]), [2, 1, 1, 1, 0, 1]);
    assert_eq!(
        codes(&i, &c["$c1"]),
        vec![
            IssueCode::UntiedContent,
            IssueCode::UnlinkedMention,
            IssueCode::Ungrounded
        ]
    );
    assert_eq!(
        codes(&i, &c["$o"]),
        vec![IssueCode::UnlinkedDoc, IssueCode::MissingSection]
    );
    assert_eq!(codes(&i, &c["$empty"]), vec![IssueCode::StubSection]);
    assert_eq!(
        codes(&i, &c["$q"]),
        vec![IssueCode::UntiedContent, IssueCode::OpenQuestion],
        "a question is informative content"
    );
    assert_eq!(codes(&i, &c["$d1"]), vec![]);
    assert_eq!(codes(&i, &c["$d2"]), vec![IssueCode::UnappliedDecision]);
    assert_eq!(
        checks(&c["$d3"])[5],
        0,
        "needs_content: false leaves Governed out"
    );
}

/// guard (D-JV4QB, first sentence; already true at the base): fails before the change only by
/// not compiling; pins that the new code keeps one mention per (block, entity).
#[test]
fn cc6_a_block_names_an_entity_once_however_often() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let i = issues(&r);
    let n = i.blocks[&BlockId::parse(&c["$c1"]).unwrap()]
        .issues
        .iter()
        .filter(|x| x.code == IssueCode::UnlinkedMention)
        .count();
    assert_eq!(n, 1, "c1 names Ledger twice: one check, one issue");
}

#[test]
fn cc6_a_dismissed_mention_is_settled_not_an_issue() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    r.review_mention(
        &writer(),
        &Scope::Canon,
        &c["$c1"],
        &c["$t"],
        MentionVerdict::Dismiss,
        "everyday word",
    )
    .unwrap();
    let i = issues(&r);
    assert!(!codes(&i, &c["$c1"]).contains(&IssueCode::UnlinkedMention));
    assert_eq!(
        i.blocks[&BlockId::parse(&c["$c1"]).unwrap()].checks[0],
        2,
        "the check stays, settled"
    );
}

#[test]
fn cc6_a_dismissal_the_text_no_longer_needs_does_nothing() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    r.review_mention(
        &writer(),
        &Scope::Canon,
        &c["$c1"],
        &c["$t"],
        MentionVerdict::Dismiss,
        "everyday word",
    )
    .unwrap();
    rdy_qa::tensions::put_j(
        &mut r,
        "edit",
        json!([{"op": "update", "id": c["$c1"], "body": "Entries are append-only."}]),
        rdy_qa::tensions::j_typo(),
    );
    let v = r
        .read()
        .coverage_issues(&Scope::View("edit".into()))
        .unwrap();
    let e = &v.blocks[&BlockId::parse(&c["$c1"]).unwrap()];
    assert_eq!(e.checks[0], 1, "no mention left: only the content check");
    assert!(
        !e.issues
            .iter()
            .any(|x| x.code == IssueCode::UnlinkedMention)
    );
}

#[test]
fn cc6_a_dismissed_tension_stays_an_issue_marked_dismissed() {
    let (_d, mut r) = project();
    let (a, b, label) = canon_duplicate_pair(&mut r); // two landed draft claims with the same text: an advisory duplicate
    r.dismiss_tension(&writer(), &label, "both say it on purpose")
        .unwrap();
    let i = issues(&r);
    let x = i.blocks[&a]
        .issues
        .iter()
        .find(|x| x.code == IssueCode::DismissedTension)
        .unwrap();
    assert_eq!(x.dimension, Dimension::Consistent);
    assert_eq!(
        x.dismissed.as_ref().unwrap().reason,
        "both say it on purpose"
    );
    assert!(
        i.blocks[&b]
            .issues
            .iter()
            .any(|x| x.code == IssueCode::DismissedTension)
    );
}

#[test]
fn cc6_fix_texts_are_exact() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let i = issues(&r);
    let fix = |id: &str, code: IssueCode| {
        i.blocks[&BlockId::parse(id).unwrap()]
            .issues
            .iter()
            .find(|x| x.code == code)
            .unwrap()
            .fix
            .clone()
    };
    assert_eq!(
        fix(&c["$c1"], IssueCode::UnlinkedMention),
        format!(
            "link this mention to Ledger ({}), or dismiss it as a passing mention",
            c["$t"]
        )
    );
    assert_eq!(
        fix(&c["$empty"], IssueCode::StubSection),
        "write content under Empty, or remove the section"
    );
    assert_eq!(
        fix(&c["$d2"], IssueCode::UnappliedDecision),
        format!(
            "apply {}: link the content that carries it with decided_by, or set needs_content: false",
            c["$d2"]
        )
    );
    assert_eq!(
        fix(&c["$o"], IssueCode::MissingSection),
        "add the required section Goals"
    );
}

#[test]
fn cc6_a_stale_canon_pin_counts_once_as_a_suspect() {
    let (_d, mut r) = project();
    let (citer, _target) = landed_citation_then_target_changed(&mut r);
    let i = issues(&r);
    let mine: Vec<IssueCode> = i.blocks[&citer].issues.iter().map(|x| x.code).collect();
    assert_eq!(
        mine.iter()
            .filter(|c| **c == IssueCode::OpenSuspect)
            .count(),
        1
    );
    assert!(
        !mine.contains(&IssueCode::OpenTension),
        "no premise-shift tension in canon (D-84QSN)"
    );
}

#[test]
fn cc6_a_view_counts_its_own_tensions_and_canon_does_not() {
    let (_d, mut r) = project();
    let (block, view) = inflight_collision(&mut r); // an edit collision between views a and b on a landed block
    let canon = r.read().coverage_issues(&Scope::Canon).unwrap();
    assert!(
        !canon.blocks[&block]
            .issues
            .iter()
            .any(|x| x.code == IssueCode::OpenTension)
    );
    let v = r.read().coverage_issues(&Scope::View(view)).unwrap();
    assert!(
        v.blocks[&block]
            .issues
            .iter()
            .any(|x| x.code == IssueCode::OpenTension)
    );
}

#[test]
fn cc6_a_reference_to_a_missing_block_is_dangling() {
    let (_d, mut r) = project();
    // a normal write refuses an unknown target, and so does the gate: commit and land raw
    raw_commit(
        &mut r,
        "raw",
        vec![
            raw(
                "d-AAAAA",
                "doc",
                None,
                "V",
                &[("name", "refs"), ("title", "Refs"), ("doc_type", "page")],
                "",
            ),
            raw(
                "s-AAAAA",
                "section",
                Some("d-AAAAA"),
                "V",
                &[("title", "Rules")],
                "",
            ),
            raw(
                "c-AAAAA",
                "claim",
                Some("s-AAAAA"),
                "V",
                &[],
                "See {{c-ZZZZZ}}.",
            ),
        ],
    );
    let p = r
        .store_mut()
        .propose("raw", "raw", writer().provenance())
        .unwrap();
    r.store_mut().land(&p, approver().provenance()).unwrap();
    r.invalidate();
    let i = issues(&r);
    let dangling: Vec<_> = i.blocks[&id("c-AAAAA")]
        .issues
        .iter()
        .filter(|x| x.code == IssueCode::DanglingRef)
        .collect();
    assert_eq!(dangling.len(), 1, "{dangling:?}");
    assert_eq!(dangling[0].dimension, Dimension::Connected);
    assert_eq!(dangling[0].subject.as_deref(), Some("c-ZZZZZ"));
    assert_eq!(
        dangling[0].fix,
        "fix the reference to c-ZZZZZ, which does not exist"
    );
}

/// guard (D-XQVM1): no coverage source says health, and no issue type says finding.
#[test]
fn cc6_names_say_coverage_issue() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../rdy-engine/src/coverage");
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let text = std::fs::read_to_string(&p).unwrap().to_lowercase();
        assert!(!text.contains("health"), "{}", p.display());
        assert!(!text.contains("finding"), "{}", p.display());
    }
}
