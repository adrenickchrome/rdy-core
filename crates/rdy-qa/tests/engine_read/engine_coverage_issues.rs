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

/// A draft duplicate pair landed at L1, then another doc landed at L2: the pair and L1, L2.
fn duplicate_then_another_landing(r: &mut Rdy) -> (BlockId, rdy_model::Hash, rdy_model::Hash) {
    let (a, _b, _label) = canon_duplicate_pair(r);
    let l1 = r.read().canon_head().unwrap();
    write(r, "more", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "later", "title": "Later", "doc_type": "page"}},
        {"op": "create", "type": "prose", "parent": "$o", "body": "Added later."}
    ]))
    .unwrap();
    land(r, "more").unwrap();
    let l2 = r.read().canon_head().unwrap();
    assert_ne!(l1, l2);
    (a, l1, l2)
}

#[test]
fn cc6_coverage_at_a_past_landing_is_refused() {
    let (_d, mut r) = project();
    let (a, l1, _l2) = duplicate_then_another_landing(&mut r);
    let canon = issues(&r);
    assert!(
        canon.blocks[&a]
            .issues
            .iter()
            .any(|x| x.code == IssueCode::OpenTension),
        "canon counts the duplicate"
    );
    let e = r.read().coverage_issues(&Scope::At(l1)).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(
        e.message,
        format!("coverage is computed for the current state only, not at landing:{l1}")
    );
    assert_eq!(
        e.fix,
        "ask for canon or a view; coverage over time is not part of rdy core"
    );
}

#[test]
fn cc6_coverage_at_canons_head_is_canon() {
    let (_d, mut r) = project();
    let (_a, _l1, l2) = duplicate_then_another_landing(&mut r);
    let canon = issues(&r);
    let head = r.read().coverage_issues(&Scope::At(l2)).unwrap();
    assert_eq!(*head, *canon);
    assert!(
        std::sync::Arc::ptr_eq(&head, &canon),
        "canon's head is canon: one memoised result, not a second one"
    );
}

#[test]
fn cc6_a_briefing_at_a_past_landing_has_no_coverage_key() {
    let (_d, mut r) = project();
    let (a, l1, l2) = duplicate_then_another_landing(&mut r);
    let cov = brief::Coverage {
        ids: vec![a.to_string()],
        path: None,
    };
    let at =
        |scope: Scope| serde_json::to_value(r.read().brief(&scope, &cov, None).unwrap()).unwrap();
    let past = at(Scope::At(l1));
    assert!(
        past.get("coverage").is_none(),
        "no coverage at a past landing: {past}"
    );
    assert!(
        past["base"].is_object(),
        "the briefing itself still works: {past}"
    );
    assert!(
        at(Scope::At(l2)).get("coverage").is_some(),
        "canon's head is canon"
    );
    assert!(at(Scope::Canon).get("coverage").is_some());
}

/// Final review I4, orchestrator ruling (D-S46DE, D-JV4QB): a mention of an entity that is
/// deprecated in the scope is the deprecated reference tension's job, not a linking gap. Doc
/// `jobs` holds term Retry limit (alias "retries", in a deprecated section), and in section
/// Rules term Ledger (live), claim a
/// (names the retries only), claim b (names the retries and the Ledger) and claim c (names the
/// retries and describes Retry limit). Landed. Then a and b carry the deprecated reference
/// tension and no unlinked mention of the retired term, and no write or search suggestion links
/// it; the live Ledger mention still counts; c, which links the retired term, keeps its check.
#[test]
fn cc6_a_deprecated_entity_mention_is_no_linking_gap() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "jobs", "title": "Jobs", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Rules"}},
        {"op": "create", "type": "section", "id": "$retired", "parent": "$o", "fields": {"title": "Retired", "lifecycle": "deprecated"}},
        {"op": "create", "type": "term", "id": "$old", "parent": "$retired", "fields": {"name": "Retry limit", "aliases": ["retries"]}, "body": "How often a job runs again."},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "The retries happen twice a day."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "The Ledger keeps the retries."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "The retries stop at noon.",
         "links": {"describes": [{"target": "$old"}]}}
    ]))
    .unwrap()
    .created;
    land(&mut r, "work").unwrap();
    let (old, ledger) = (w["$old"].clone(), w["$t"].clone());
    let i = issues(&r);
    let mentions_of = |id: &str| -> Vec<Option<String>> {
        i.blocks[&BlockId::parse(id).unwrap()]
            .issues
            .iter()
            .filter(|x| x.code == IssueCode::UnlinkedMention)
            .map(|x| x.subject.clone())
            .collect()
    };
    assert_eq!(mentions_of(&w["$a"]), Vec::<Option<String>>::new());
    assert_eq!(
        mentions_of(&w["$b"]),
        vec![Some(ledger.clone())],
        "a live entity's mention still counts"
    );
    let checks = |id: &str| i.blocks[&BlockId::parse(id).unwrap()].checks[0];
    assert_eq!(checks(&w["$a"]), 1, "the content check only");
    assert_eq!(
        checks(&w["$b"]),
        2,
        "the content check and the Ledger mention"
    );
    assert_eq!(
        checks(&w["$c"]),
        2,
        "a block that links the retired term keeps its check"
    );
    assert!(
        i.mentions[&BlockId::parse(&old).unwrap()].contains(&BlockId::parse(&w["$a"]).unwrap()),
        "the block still names the retired term"
    );
    // onboarding reads the same issues: the entities phase counts a's and b's untied content
    // and b's Ledger mention, and nothing for the retired term
    let entities = r
        .read()
        .onboarding(&Scope::Canon)
        .unwrap()
        .phases
        .into_iter()
        .find(|p| p.phase == rdy_engine::coverage::Phase::Entities)
        .unwrap();
    assert_eq!(entities.issues, 3, "{entities:?}");
    // the deprecated reference tension carries the signal
    let st = r.read();
    let tensions = st.tensions().unwrap();
    for x in [&w["$a"], &w["$b"]] {
        let x = BlockId::parse(x).unwrap();
        assert!(
            tensions.iter().any(|t| t.kind
                == rdy_engine::tensions::TensionKind::DeprecatedReference
                && t.blocks().contains(&x)),
            "{x} carries the deprecated reference tension"
        );
    }
    // a write's suggestions link the live term only
    let wr = write(
        &mut r,
        "more",
        json!([{"op": "create", "type": "claim", "parent": w["$s"], "body": "The Ledger counts the retries."}]),
    )
    .unwrap();
    let change: rdy_model::Hash = wr.change.parse().unwrap();
    let (sugg, _) = r
        .read()
        .suggestions(&Scope::View("more".into()), &change)
        .unwrap();
    let targets: Vec<String> = sugg
        .iter()
        .filter_map(|s| match s {
            rdy_engine::suggest::Suggestion::Link { target, .. } => Some(target.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(targets, vec![ledger.clone()], "{sugg:?}");
    // a search hit gets no link suggestion to the retired term either
    let res = r
        .read()
        .search(
            &Scope::Canon,
            &SearchQuery {
                text: "retries".into(),
                ..Default::default()
            },
            None,
            Some(100_000),
        )
        .unwrap();
    assert!(
        res.matches.iter().any(|m| m.id == w["$a"]),
        "guard: a is a hit"
    );
    assert!(
        res.matches.iter().all(|m| m.suggestions.is_empty()),
        "{:?}",
        res.matches
    );
}

/// Final review M1: a pinned citation whose target a view deletes is one Connected issue, the
/// dangling reference; no open suspect asks to repin it to a block that does not exist. (A
/// landing refuses this state, so only a view holds it.) In canon the target exists and the
/// suspect of the changed target stays.
#[test]
fn cc6_a_deleted_pinned_target_is_a_dangling_ref_only() {
    let (_d, mut r) = project();
    let w = write(&mut r, "cite", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "pay", "title": "Pay", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Rules"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Wallet"}, "body": "Holds cards."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Payment uses {{$t}}."}
    ]))
    .unwrap()
    .created;
    land(&mut r, "cite").unwrap();
    let (c, t) = (w["$c"].clone(), w["$t"].clone());
    write(
        &mut r,
        "edit",
        json!([{"op": "update", "id": t, "body": "Holds cards and vouchers."}]),
    )
    .unwrap();
    land(&mut r, "edit").unwrap();
    assert!(
        codes(&issues(&r), &c).contains(&IssueCode::OpenSuspect),
        "guard: canon holds the suspect of the changed target"
    );
    write(&mut r, "gone", json!([{"op": "delete", "id": t}])).unwrap();
    let v = r
        .read()
        .coverage_issues(&Scope::View("gone".into()))
        .unwrap();
    let mine = &v.blocks[&BlockId::parse(&c).unwrap()].issues;
    let codes: Vec<IssueCode> = mine.iter().map(|x| x.code).collect();
    assert!(codes.contains(&IssueCode::DanglingRef), "{mine:?}");
    assert!(!codes.contains(&IssueCode::OpenSuspect), "{mine:?}");
}
