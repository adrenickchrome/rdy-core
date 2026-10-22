//! di5 (plan Decision intake, Task 5): the needs-you queue's batches and their exceptions. One
//! ready proposal version is one `approve` card (D-YFDG3); each proposed decision that deserves
//! its own look stands beside it as a `confirm` card (p-FN596 with F3); the version's advised
//! tensions stand beside it as one `resolve` item (D-WMCFW); a version that is not ready is no
//! item (D-3JZP4) and is listed under `not_ready` with the landing's texts; the answers a person
//! recorded remove their items (O23).
use rdy_engine::coverage::confidence_of;
use rdy_engine::pending::{
    APPROVE_REASON_BESIDE, CARD_MAX_TOKENS, ExceptionKind, Item, ItemKind, Pending, Recommended,
    card_hash, render,
};
use rdy_engine::tensions::api::AcceptRequest;
use rdy_engine::{
    DecisionInput, E_BLOCKING_TENSION, E_CONFLICT_UNRESOLVED, E_VALIDATION, Rdy, ReadState, Scope,
};
use rdy_model::{AnswerAct, BlockId, Hash, Op};
use rdy_qa::fixtures::{approver, version_of, writer};
use rdy_qa::intake::*;
use rdy_qa::tensions::{ag, created, put, upd};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

/// The items of `q` whose `batch` is `r#ref`, in queue order.
fn beside<'a>(q: &'a Pending, r#ref: &str) -> Vec<&'a Item> {
    q.items
        .iter()
        .filter(|i| i.batch.as_deref() == Some(r#ref))
        .collect()
}

/// The one item whose ref is `r#ref`.
fn item<'a>(q: &'a Pending, r#ref: &str) -> &'a Item {
    let found: Vec<&Item> = q.items.iter().filter(|i| i.r#ref == r#ref).collect();
    assert_eq!(found.len(), 1, "{ref} in {:#?}", refs(q));
    found[0]
}

/// The `confirm` item of decision `d`.
fn confirm_of<'a>(q: &'a Pending, d: &BlockId) -> &'a Item {
    let at = format!("{d}@");
    let found: Vec<&Item> = q
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.r#ref.starts_with(&at))
        .collect();
    assert_eq!(found.len(), 1, "{d} in {:#?}", refs(q));
    found[0]
}

fn refs(q: &Pending) -> Vec<&str> {
    q.items.iter().map(|i| i.r#ref.as_str()).collect()
}

fn approve_refs(q: &Pending) -> Vec<&str> {
    q.items
        .iter()
        .filter(|i| i.kind == ItemKind::Approve)
        .map(|i| i.r#ref.as_str())
        .collect()
}

fn fact<'a>(i: &'a Item, name: &str) -> &'a str {
    i.facts
        .iter()
        .find(|f| f.name == name)
        .map(|f| f.value.as_str())
        .unwrap_or_else(|| panic!("no fact {name} in {:#?}", i.facts))
}

/// The item ref of decision `d` in view `view`: `<id>@<version short>`.
fn decision_ref(r: &Rdy, view: &str, d: &BlockId) -> String {
    let v = version_of(r, &Scope::View(view.into()), &d.to_string());
    format!("{d}@{}", &v[..12])
}

/// The blocks the version `r#ref` writes.
fn written(st: &ReadState, r#ref: &str) -> BTreeSet<BlockId> {
    let pref = st.open_proposals().unwrap();
    let (_, p) = pref
        .iter()
        .find(|(p, _)| p.to_string() == r#ref)
        .expect("open version");
    p.changes
        .iter()
        .flat_map(|c| st.reader().change(c).unwrap().ops)
        .map(|o: Op| o.block_id().clone())
        .collect()
}

#[test]
fn di5_a_ready_proposal_is_one_batch_card() {
    let (_d, r, b) = batch_project();
    let q = pending(&r);
    assert!(q.not_ready.is_empty(), "{:#?}", q.not_ready);
    assert_eq!(approve_refs(&q), vec![b.r#ref.as_str()]);
    let i = item(&q, &b.r#ref);
    assert_eq!(i.kind, ItemKind::Approve);
    assert_eq!(i.title, BATCH_TITLE);
    assert_eq!(
        i.why,
        format!("ready to land: {} changes", b.report["changes"])
    );
    assert_eq!(
        (i.summary.as_deref(), i.summary_by.as_deref()),
        (Some(BATCH_SUMMARY), Some("agent:batch"))
    );
    assert_eq!(
        i.members,
        vec![format!("{}  Guest readers need no account", b.member)]
    );
    assert_eq!((i.batch.as_deref(), i.exception), (None, None));
    assert!(!i.blocking);
    assert_eq!(i.raised_by, "agent:batch");
    let names: Vec<&str> = i.facts.iter().map(|f| f.name.as_ref()).collect();
    assert_eq!(
        names,
        vec![
            "Changes",
            "Decisions",
            "Supersedes",
            "Re-affirms",
            "Reversibility",
            "Consequences",
            "Change plan",
            "Tensions",
            "Coverage",
            "Confidence",
            "Base",
            "Exceptions",
        ]
    );
    let opts: Vec<(&str, &str)> = i
        .options
        .iter()
        .map(|o| (o.id.as_str(), o.label.as_str()))
        .collect();
    let approve = format!("Approve {} and confirm {}", b.r#ref, b.member);
    assert_eq!(
        opts,
        vec![
            ("approve", approve.as_str()),
            ("reject", "Reject"),
            ("later", "Later")
        ]
    );
    // D-D8FAB: the approve reason is computed (decisions stand beside this batch), never the summary
    assert_eq!(
        i.options[0].recommended.as_ref().map(Recommended::reason),
        Some(APPROVE_REASON_BESIDE)
    );
    assert!(i.options[1..].iter().all(|o| o.recommended.is_none()));
    let said: Vec<String> = i.options.iter().map(|o| o.consequences.join(" ")).collect();
    assert_eq!(
        said,
        vec![
            "Lands the proposal in canon.",
            "Returns the proposal to its view. Canon does not change.",
            "Hides this card until the chat session changes.",
        ]
    );
    // the card is the item as `render` lays it out, and the hash is the card's at full size
    assert_eq!(i.card, render(i, CARD_MAX_TOKENS));
    assert_eq!(i.card_hash, card_hash(i).to_string());
    assert!(
        i.card.starts_with(&format!("{}  {BATCH_TITLE}", b.r#ref)),
        "{}",
        i.card
    );
    assert!(i.card.contains("Summary (agent:batch)"), "{}", i.card);
    // every proposed decision, by id, with its title and the reversibility it declares
    let mut decided = [
        (&b.member, "Guest readers need no account", "two_way"),
        (&b.one_way, "Guest links never expire", "one_way"),
        (&b.against, "Keep A for members only", "two_way"),
        (&b.unapplied, "Guests see a banner", "two_way"),
        (&b.not_recommended, "Guests may edit", "two_way"),
    ];
    decided.sort();
    let titles: Vec<String> = decided
        .iter()
        .map(|(id, t, _)| format!("{id} \"{t}\""))
        .collect();
    assert_eq!(
        fact(i, "Decisions"),
        format!("5 new: {}", titles.join(", "))
    );
    assert_eq!(fact(i, "Supersedes"), b.spec.decision.to_string());
    let each: Vec<String> = decided
        .iter()
        .map(|(id, _, rev)| format!("{id} {rev}"))
        .collect();
    assert_eq!(
        fact(i, "Reversibility"),
        format!("mixed: {}, declared by the proposer", each.join(", "))
    );
    assert_eq!(fact(i, "Exceptions"), "4");
}

#[test]
fn di5_a_version_that_is_not_ready_is_not_an_item() {
    let (_d, mut r, m) = ready_and_not_ready();
    let q = pending(&r);
    assert_eq!(approve_refs(&q), vec![m.ready.r#ref.as_str()]);
    assert_eq!(fact(item(&q, &m.ready.r#ref), "Exceptions"), "none");
    let not_ready: BTreeMap<&str, &Vec<rdy_schema::Issue>> = q
        .not_ready
        .iter()
        .map(|n| (n.r#ref.as_str(), &n.issues))
        .collect();
    let mut want: Vec<&str> = vec![&m.no_summary.r#ref, &m.blocking.r#ref, &m.conflict.r#ref];
    want.sort();
    assert_eq!(not_ready.keys().copied().collect::<Vec<_>>(), want);
    for issues in not_ready.values() {
        assert_eq!(issues.len(), 1, "one issue per failed check: {issues:#?}");
        assert!(!issues[0].fix.is_empty());
    }
    // the summary: D-4PG9Y, the gate requires it
    let s = &not_ready[m.no_summary.r#ref.as_str()][0];
    assert_eq!(
        (s.code, s.message.clone(), s.fix.as_str()),
        (
            E_VALIDATION,
            format!("{} has no summary", m.no_summary.r#ref),
            "propose it again with a summary in plain words for the person who approves it"
        )
    );
    // the landing gives the same texts today
    for (version, code) in [
        (&m.blocking, E_BLOCKING_TENSION),
        (&m.conflict, E_CONFLICT_UNRESOLVED),
    ] {
        let issue = &not_ready[version.r#ref.as_str()][0];
        let e = r.land(&approver(), &version.r#ref).unwrap_err();
        assert_eq!(e.code, code, "{e}");
        assert_eq!(
            (issue.code, issue.message.as_str(), issue.fix.as_str()),
            (e.code, e.message.as_str(), e.fix.as_str())
        );
    }
    assert!(
        q.items
            .iter()
            .all(|i| i.batch.as_deref() != Some(&*m.blocking.r#ref)
                && i.batch.as_deref() != Some(&*m.conflict.r#ref)
                && i.batch.as_deref() != Some(&*m.no_summary.r#ref)),
        "{:#?}",
        refs(&q)
    );
}

#[test]
fn di5_each_exception_kind_stands_beside_the_batch() {
    let (d, r, b) = batch_project();
    let mut r = r;
    // against by a tension: a side of an open semantic tension whose other side is live (F3)
    let tense = decide_in(
        &mut r,
        BATCH_VIEW,
        decision(
            &b.spec,
            "Guests follow the member rules",
            "Guests follow the member rules",
        ),
    );
    verdict_on(
        &mut r,
        &Scope::Inflight,
        &tense,
        &b.spec.decision,
        "agent:r",
        false,
    );
    // a blocking tension that does not refuse the landing: another view edits A, which this
    // decision depends on (an in-flight premise shift)
    let shifted = decide_in(
        &mut r,
        BATCH_VIEW,
        DecisionInput {
            depends_on: vec![b.spec.a.to_string()],
            ..decision(&b.spec, "Guests rely on A", "Guests rely on A")
        },
    );
    put(&mut r, "other", upd(&b.spec.a, "A as other has it."));
    let (r, report) = propose_reported(d.path(), r, BATCH_VIEW, BATCH_TITLE, Some(BATCH_SUMMARY));
    let pref = report["ref"].as_str().unwrap().to_string();
    let q = pending(&r);
    assert!(q.not_ready.is_empty(), "{:#?}", q.not_ready);
    let batch = item(&q, &pref);
    assert_eq!(
        batch.members,
        vec![format!("{}  Guest readers need no account", b.member)]
    );
    assert_eq!(fact(batch, "Exceptions"), "6");
    let want = [
        (&b.against, ExceptionKind::Against),
        (&tense, ExceptionKind::Against),
        (&b.one_way, ExceptionKind::OneWay),
        (&b.unapplied, ExceptionKind::Unapplied),
        (&b.not_recommended, ExceptionKind::NotRecommended),
        (&shifted, ExceptionKind::BlockingTension),
    ];
    let mut seen = BTreeSet::new();
    for (d, kind) in want {
        let i = confirm_of(&q, d);
        assert_eq!(i.r#ref, decision_ref(&r, BATCH_VIEW, d));
        assert_eq!(
            (i.kind, i.batch.as_deref(), i.exception),
            (ItemKind::Confirm, Some(pref.as_str()), Some(kind)),
            "{d}"
        );
        assert!(
            !batch.members.iter().any(|m| m.starts_with(&d.to_string())),
            "{d} is never a member"
        );
        let ids: Vec<&str> = i.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, vec!["confirm", "reject", "later"]);
        let recommended: Vec<&str> = i
            .options
            .iter()
            .filter(|o| o.recommended.is_some())
            .map(|o| o.id.as_str())
            .collect();
        let rec = if d == &b.not_recommended {
            "reject"
        } else {
            "confirm"
        };
        assert_eq!(recommended, vec![rec], "{d}");
        assert!(i.card.starts_with(&format!("{}  ", i.r#ref)), "{}", i.card);
        assert!(
            i.card.contains(&format!("Beside {pref}, on its own: ")),
            "{}",
            i.card
        );
        seen.insert(i.r#ref.clone());
    }
    assert_eq!(seen.len(), 6);
    let rejected = confirm_of(&q, &b.not_recommended);
    assert_eq!(
        rejected.options[1]
            .recommended
            .as_ref()
            .map(Recommended::reason),
        Some("guests should not edit")
    );
    // a one-word yes never sweeps them in: the approve option names only the members
    assert_eq!(
        batch.options[0].label,
        format!("Approve {pref} and confirm {}", b.member)
    );
}

#[test]
fn di5_refines_is_not_against() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let spec = rdy_qa::edit_loop::spec_doc(&mut r);
    let refines = decide_in(
        &mut r,
        "refine",
        DecisionInput {
            refines: vec![spec.decision.to_string()],
            ..decision(&spec, "Keep A, with a note", "Keep A, with a note")
        },
    );
    let (r, report) = propose_reported(dir.path(), r, "refine", "refine", Some("Refines D."));
    let pref = report["ref"].as_str().unwrap();
    let q = pending(&r);
    assert_eq!(approve_refs(&q), vec![pref]);
    assert_eq!(
        item(&q, pref).members,
        vec![format!("{refines}  Keep A, with a note")]
    );
    assert!(
        q.items.iter().all(|i| i.kind != ItemKind::Confirm),
        "{:#?}",
        refs(&q)
    );
    assert_eq!(fact(item(&q, pref), "Exceptions"), "none");
}

#[test]
fn di5_advised_tensions_stand_beside_the_batch_as_one_item() {
    let (_d, mut r, b) = batch_project();
    let q = pending(&r);
    let advice = format!("advice:{}", b.r#ref);
    let i = item(&q, &advice);
    assert_eq!(
        (i.kind, i.batch.as_deref(), i.exception),
        (ItemKind::Resolve, Some(b.r#ref.as_str()), None)
    );
    assert_eq!(i.members.len(), 1, "{:#?}", i.members);
    let ids: Vec<&str> = i.options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, vec!["accept", "dismiss", "later"]);
    assert_eq!(
        fact(i, "Advised digest"),
        b.report["advised_digest"].as_str().unwrap()
    );
    // one item for the version, last of its batch
    let group = beside(&q, &b.r#ref);
    assert_eq!(
        group.iter().filter(|i| i.kind == ItemKind::Resolve).count(),
        1
    );
    assert_eq!(group.last().unwrap().r#ref, advice);
    // an accepted advice blocks: the version is no longer ready, and nothing of it is an item
    r.accept_advice(
        &writer(),
        "human:qa",
        AcceptRequest {
            proposal: b.r#ref.clone(),
            advised_digest: b.report["advised_digest"].as_str().unwrap().into(),
            ..Default::default()
        },
    )
    .unwrap();
    let q = pending(&r);
    assert!(beside(&q, &b.r#ref).is_empty(), "{:#?}", refs(&q));
    assert!(approve_refs(&q).is_empty());
    assert_eq!(q.not_ready.len(), 1);
    assert_eq!(q.not_ready[0].issues[0].code, E_BLOCKING_TENSION);
}

#[test]
fn di5_the_card_facts_come_from_the_store() {
    let (_d, mut r, b) = batch_project();
    let st = r.read();
    let q = pending(&r);
    let i = item(&q, &b.r#ref);
    let version: Hash = b.report["id"].as_str().unwrap().parse().unwrap();
    // D-NJ2SP: computed once, on the version's final state
    let c = st.landing_consequences(&version, None).unwrap();
    assert_eq!(
        fact(i, "Consequences"),
        format!("{} returned", c.items.len())
    );
    assert_eq!(
        c.items.len(),
        b.report["consequences"].as_array().unwrap().len()
    );
    // D-C6HHV: the propose report's coverage
    let cov = &b.report["coverage"];
    assert_eq!(
        fact(i, "Coverage"),
        format!("{}% \u{2192} {}%", cov["before"], cov["after"])
    );
    // D-XZXCE: the confidence of the blocks the version writes; the view holds exactly the
    // version here, so its issues are those of the canon the version would give (a view that
    // moved on: di5_coverage_and_confidence_describe_the_version_not_its_view)
    let touched = written(&st, &b.r#ref);
    let issues = st.coverage_issues(&Scope::View(BATCH_VIEW.into())).unwrap();
    let m = st.materialized(&Scope::View(BATCH_VIEW.into())).unwrap();
    let mut paths = BTreeMap::new();
    for id in &touched {
        if let Some(e) = issues.blocks.get(id)
            && !e.issues.is_empty()
            && let Some(p) = e.doc.as_ref().and_then(|d| m.path_of(d))
        {
            paths.insert(id.to_string(), p);
        }
    }
    let conf = confidence_of(&issues, &touched, None, &paths);
    let level = serde_json::to_value(conf.confidence).unwrap();
    let want = match conf.reasons.first() {
        None => level.as_str().unwrap().to_string(),
        Some(first) => format!(
            "{}: {} on {}",
            level.as_str().unwrap(),
            serde_json::to_value(first.code).unwrap().as_str().unwrap(),
            first.block
        ),
    };
    assert_eq!(fact(i, "Confidence"), want);
    // the base: canon's head when the version was proposed, and whether canon moved since
    let head = st.canon_head().unwrap();
    assert_eq!(
        fact(i, "Base"),
        format!("canon at {}, current", head.short())
    );
    assert!(fact(i, "Tensions").ends_with("advisory, answers not recorded yet"));
    put(
        &mut r,
        "elsewhere",
        json!([{"op": "create", "type": "claim", "parent": b.spec.section.to_string(),
                "body": "Another claim."}]),
    );
    rdy_qa::fixtures::land(&mut r, "elsewhere").unwrap();
    let q = pending(&r);
    assert_eq!(
        fact(item(&q, &b.r#ref), "Base"),
        format!("canon at {}, moved", head.short())
    );
}

/// The coverage score of view `view`, as its summary gives it.
fn view_score(r: &Rdy, view: &str) -> u32 {
    r.read()
        .coverage_summary(&Scope::View(view.into()))
        .unwrap()
        .score
}

/// Twenty docs of a type with required sections, none written: they lower a scope's coverage.
fn twenty_bare_docs() -> serde_json::Value {
    serde_json::Value::Array(
        (0..20)
            .map(|n| {
                json!({"op": "create", "type": "doc", "fields": {
                    "name": format!("extra-{n}"), "title": format!("Extra {n}"),
                    "doc_type": "overview"}})
            })
            .collect(),
    )
}

#[test]
fn di5_coverage_and_confidence_describe_the_version_not_its_view() {
    // fix round 1: the facts are worked out on the canon the version would give, so an edit of
    // its view after the propose moves neither of them
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let spec = rdy_qa::edit_loop::spec_doc(&mut r);
    let w = put(
        &mut r,
        "late",
        json!([{"op": "create", "type": "claim", "id": "$x", "parent": spec.section.to_string(),
                "body": "An untied claim."}]),
    );
    let x = created(&w, "$x");
    let p = r
        .propose_subset(&ag("late"), "late", "late", None, Some("Adds a claim."))
        .unwrap();
    let q = pending(&r);
    let i = item(&q, &p.r#ref);
    let (coverage, confidence) = (
        fact(i, "Coverage").to_string(),
        fact(i, "Confidence").to_string(),
    );
    assert!(
        confidence.starts_with("medium: ") && confidence.ends_with(&format!(" on {x}")),
        "{confidence}"
    );
    let score = view_score(&r, "late");
    assert!(coverage.ends_with(&format!(" {score}%")), "{coverage}");
    // the view drops the claim and gains twenty bare docs; the version still holds the claim
    put(
        &mut r,
        "late",
        json!([{"op": "delete", "id": x.to_string()}]),
    );
    put(&mut r, "late", twenty_bare_docs());
    assert_ne!(view_score(&r, "late"), score, "the view's coverage moved");
    let q = pending(&r);
    let i = item(&q, &p.r#ref);
    assert_eq!(
        (fact(i, "Coverage"), fact(i, "Confidence")),
        (coverage.as_str(), confidence.as_str())
    );
}

#[test]
fn di5_a_subset_version_has_its_own_coverage() {
    // fix round 1: a version of some of a view's changes: its coverage is that of canon with
    // those changes, not of the whole view
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let spec = rdy_qa::edit_loop::spec_doc(&mut r);
    let w = put(
        &mut r,
        "some",
        json!([{"op": "create", "type": "claim", "parent": spec.section.to_string(),
                "body": format!("Builds on {{{{{}}}}}.", spec.a)}]),
    );
    // the same change alone in a view of its own: what the version's canon scores
    r.view_include(&ag("alone"), "alone", std::slice::from_ref(&w.change))
        .unwrap();
    let alone = view_score(&r, "alone");
    put(&mut r, "some", twenty_bare_docs());
    assert_ne!(
        view_score(&r, "some"),
        alone,
        "the docs lower the view's coverage"
    );
    let p = r
        .propose_subset(
            &ag("some"),
            "some",
            "some",
            Some(std::slice::from_ref(&w.change)),
            Some("Adds one claim."),
        )
        .unwrap();
    let canon = r.read().coverage_summary(&Scope::Canon).unwrap().score;
    let q = pending(&r);
    assert_eq!(
        fact(item(&q, &p.r#ref), "Coverage"),
        format!("{canon}% \u{2192} {alone}%")
    );
}

#[test]
fn di5_the_consequences_fact_counts_only_the_entries_returned() {
    // fix round 1: past the default budget, the omitted entries are not returned
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let spec = rdy_qa::edit_loop::spec_doc(&mut r);
    let notes: Vec<serde_json::Value> = (0..300)
        .map(|n| {
            json!({"op": "create", "type": "claim", "parent": spec.section.to_string(),
                   "body": format!("Note {n} builds on {{{{{}}}}}.", spec.a)})
        })
        .collect();
    put(&mut r, "notes", serde_json::Value::Array(notes));
    rdy_qa::fixtures::land(&mut r, "notes").unwrap();
    put(&mut r, "reword", upd(&spec.a, "A holds, reworded."));
    let p = r
        .propose_subset(&ag("reword"), "reword", "reword", None, Some("Rewords A."))
        .unwrap();
    let version: Hash = p.id.parse().unwrap();
    let c = r.read().landing_consequences(&version, None).unwrap();
    let cut: usize = c.omitted.values().sum();
    assert!(cut > 0, "the budget cuts some: {} returned", c.items.len());
    let q = pending(&r);
    assert!(q.not_ready.is_empty(), "{:#?}", q.not_ready);
    assert_eq!(
        fact(item(&q, &p.r#ref), "Consequences"),
        format!("{} returned", c.items.len())
    );
}

#[test]
fn di5_a_conflict_does_not_hide_a_blocking_tension() {
    // fix round 1: a version with a conflict and an independent blocking tension lists both,
    // each with its fix
    let (_d, mut r, m) = ready_and_not_ready();
    let blocking: Vec<String> = r
        .read()
        .included("blocking")
        .iter()
        .map(Hash::to_string)
        .collect();
    r.view_include(&ag("conflict"), "conflict", &blocking)
        .unwrap();
    let v2 = r
        .propose_subset(
            &ag("conflict"),
            "conflict",
            "conflict",
            None,
            Some("Rewrites C and builds on A."),
        )
        .unwrap();
    let st = r.read();
    let pv: Hash = v2.id.parse().unwrap();
    let p = st.proposal_version(&pv).unwrap();
    let conflicted: BTreeSet<BlockId> = BTreeSet::from([m.spec.c.clone()]);
    let (listed, _) = st
        .proposal_tensions("conflict", &p.changes, &conflicted, 100)
        .unwrap();
    let refusing = listed.iter().filter(|t| t.refuses == Some(true)).count();
    assert!(refusing > 0, "the version carries a refusing tension");
    let q = pending(&r);
    let issues = &q
        .not_ready
        .iter()
        .find(|n| n.r#ref == v2.r#ref)
        .unwrap_or_else(|| panic!("{} not ready: {:#?}", v2.r#ref, q.not_ready))
        .issues;
    let codes: Vec<&str> = issues.iter().map(|i| i.code).collect();
    assert_eq!(codes, vec![E_CONFLICT_UNRESOLVED, E_BLOCKING_TENSION]);
    // the conflict as the landing gives it: the landing stops there
    let e = r.land(&approver(), &v2.r#ref).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            issues[0].code,
            issues[0].message.as_str(),
            issues[0].fix.as_str()
        )
    );
    // the blocking tension as the landing words it, with the same fix the blocking view's has
    let alone = &q
        .not_ready
        .iter()
        .find(|n| n.r#ref == m.blocking.r#ref)
        .expect("the blocking view's version")
        .issues[0];
    assert_eq!(
        (issues[1].message.as_str(), issues[1].fix.as_str()),
        (
            format!(
                "landing {} would put {refusing} blocking tension(s) into canon",
                v2.r#ref
            )
            .as_str(),
            alone.fix.as_str()
        )
    );
}

#[test]
fn di5_only_the_latest_version_is_an_item() {
    let (_d, mut r, b) = batch_project();
    put(
        &mut r,
        BATCH_VIEW,
        json!([{"op": "create", "type": "claim", "parent": b.spec.section.to_string(),
                "body": "One more claim."}]),
    );
    let v2 = r
        .propose_subset(
            &ag(BATCH_VIEW),
            BATCH_VIEW,
            BATCH_TITLE,
            None,
            Some(BATCH_SUMMARY),
        )
        .unwrap();
    assert_ne!(v2.r#ref, b.r#ref);
    let q = pending(&r);
    assert_eq!(approve_refs(&q), vec![v2.r#ref.as_str()]);
    assert!(beside(&q, &b.r#ref).is_empty(), "{:#?}", refs(&q));
    assert_eq!(beside(&q, &v2.r#ref).len(), 5, "{:#?}", refs(&q));
    assert!(q.items.iter().all(|i| !i.r#ref.contains(&b.r#ref)));
}

#[test]
fn di5_items_come_in_the_closed_order() {
    let (_d, mut r, b) = batch_project();
    put(
        &mut r,
        "second",
        json!([{"op": "create", "type": "claim", "parent": b.spec.section.to_string(),
                "body": "A second batch."}]),
    );
    let decided = decide_in(
        &mut r,
        "second",
        DecisionInput {
            reversibility: Some("one_way".into()),
            ..decision(
                &b.spec,
                "Second batches are one way",
                "Second batches are one way",
            )
        },
    );
    let second = r
        .propose_subset(&ag("second"), "second", "second", None, Some("A second."))
        .unwrap();
    let q = pending(&r);
    // no blocking item yet: the batches, each with its exceptions directly after it, oldest
    // first, then by ref
    assert!(q.items.iter().all(|i| !i.blocking));
    let heads: Vec<&Item> = q
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Approve)
        .collect();
    assert_eq!(heads.len(), 2);
    let key = |i: &Item| (i.raised_at.clone(), i.r#ref.clone());
    assert!(key(heads[0]) <= key(heads[1]), "{:#?}", refs(&q));
    let mut current: Option<&str> = None;
    for i in &q.items {
        match (i.kind, i.batch.as_deref()) {
            (ItemKind::Approve, None) => current = Some(&i.r#ref),
            (_, Some(batch)) => assert_eq!(Some(batch), current, "{:#?}", refs(&q)),
            _ => panic!("an item outside a batch: {:#?}", refs(&q)),
        }
    }
    // within a batch: the approve card, its exceptions by ref, then the advice
    let group = beside(&q, &b.r#ref);
    let confirms: Vec<&str> = group
        .iter()
        .filter(|i| i.kind == ItemKind::Confirm)
        .map(|i| i.r#ref.as_str())
        .collect();
    let mut sorted = confirms.clone();
    sorted.sort();
    assert_eq!(confirms, sorted);
    assert_eq!(group.last().unwrap().kind, ItemKind::Resolve);
    assert_eq!(
        beside(&q, &second.r#ref)
            .iter()
            .map(|i| i.r#ref.clone())
            .collect::<Vec<_>>(),
        vec![decision_ref(&r, "second", &decided)]
    );
    // the same state and query give the same queue
    assert_eq!(pending(&r), q);
}

#[test]
fn di5_the_queue_is_a_pure_read() {
    let (d, r, _b) = batch_project();
    let end = r.store().log_end();
    let refs_before = r.read().reader().refs_with_prefix("").unwrap();
    let seq = r.read().seq();
    let q = pending(&r);
    assert!(!q.items.is_empty());
    assert_eq!(r.store().log_end(), end);
    assert_eq!(r.read().seq(), seq);
    assert_eq!(r.read().reader().refs_with_prefix("").unwrap(), refs_before);
    r.close().unwrap();
    let r = Rdy::open(d.path()).unwrap();
    assert_eq!(r.store().log_end(), end);
    assert_eq!(r.read().reader().refs_with_prefix("").unwrap(), refs_before);
    assert_eq!(pending(&r), q);
}

#[test]
fn di5_answers_remove_their_items() {
    let (d, r, b) = batch_project();
    let q = pending(&r);
    let batch_hash = item(&q, &b.r#ref).card_hash.clone();
    let one_way = confirm_of(&q, &b.one_way).r#ref.clone();
    // a confirm that names another version of the decision does not count
    let stale = format!("{}@000000000000", b.one_way);
    let r = put_answer(
        d.path(),
        r,
        &answer(&stale, AnswerAct::Confirm, "confirm", "Confirm"),
    );
    assert_eq!(confirm_of(&pending(&r), &b.one_way).r#ref, one_way);
    // a confirm of the version shown removes its item, and the batch card stays as it was
    let r = put_answer(
        d.path(),
        r,
        &answer(&one_way, AnswerAct::Confirm, "confirm", "Confirm"),
    );
    let q = pending(&r);
    assert!(!refs(&q).contains(&one_way.as_str()), "{:#?}", refs(&q));
    assert_eq!(beside(&q, &b.r#ref).len(), 4, "{:#?}", refs(&q));
    assert_eq!(item(&q, &b.r#ref).card_hash, batch_hash);
    // a rejected proposal leaves the queue with everything beside it
    let r = put_answer(
        d.path(),
        r,
        &answer(&b.r#ref, AnswerAct::RejectProposal, "reject", "Reject"),
    );
    let q = pending(&r);
    assert!(q.items.is_empty(), "{:#?}", refs(&q));
    assert!(q.not_ready.is_empty(), "{:#?}", q.not_ready);
}

#[test]
fn di5_a_rejected_decision_makes_the_batch_not_ready() {
    let (d, r, b) = batch_project();
    let q = pending(&r);
    let rejected = confirm_of(&q, &b.not_recommended).r#ref.clone();
    let r = put_answer(
        d.path(),
        r,
        &answer(&rejected, AnswerAct::RejectDecision, "reject", "Reject"),
    );
    let q = pending(&r);
    assert!(q.items.is_empty(), "{:#?}", refs(&q));
    assert_eq!(q.not_ready.len(), 1);
    assert_eq!(q.not_ready[0].r#ref, b.r#ref);
    let messages: Vec<&str> = q.not_ready[0]
        .issues
        .iter()
        .map(|i| i.message.as_str())
        .collect();
    assert_eq!(
        messages,
        vec![format!(
            "{} was rejected; drop or revise it, then propose again",
            b.not_recommended
        )]
    );
}

/// Final review fix round 1 (R5): the queue reads only the open proposals, not every view that
/// ever proposed. A landing leaves its view's `open/` ref empty, and the store keeps it (the
/// dump lists it), so landed views pile up. With the same open work, a queue read and an answer
/// read the same number of refs however many views landed before.
#[test]
fn di5_the_queue_reads_only_the_open_proposals_however_many_views_landed() {
    let mut reads = vec![];
    for landed in [0usize, 4, 8] {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let spec = rdy_qa::edit_loop::spec_doc(&mut r);
        for k in 0..landed {
            let view = format!("done-{k}");
            put(
                &mut r,
                &view,
                json!([{"op": "create", "type": "claim", "parent": spec.section.to_string(),
                        "body": format!("A claim of {view}.")}]),
            );
            rdy_qa::fixtures::land(&mut r, &view).expect("land");
        }
        // the same open work every time: one decision, proposed with a summary
        decide_in(&mut r, "open", decision(&spec, "Keep C", "Keep C"));
        let pref = r
            .propose_subset(&ag("open"), "open", "open", None, Some("Keeps C."))
            .unwrap()
            .r#ref;
        let all = r.read().reader().refs_with_prefix("open/").unwrap();
        // spec_doc's own view lands too
        assert!(all.len() > landed, "{all:?}");
        assert_eq!(
            all.iter().filter(|(_, v)| !v.is_empty()).count(),
            1,
            "the landed views keep empty refs: {all:?}"
        );
        let st = r.read();
        let before = st.reader().refs_read();
        let q = pending(&r);
        assert_eq!(q.items[0].r#ref, pref);
        let queue = st.reader().refs_read() - before;
        drop(st);
        let before = r.read().reader().refs_read();
        r.answer(
            &approver(),
            PERSON,
            rdy_engine::answer::AnswerRequest {
                r#ref: pref.clone(),
                option: "reject".into(),
                ..Default::default()
            },
        )
        .expect("the person rejects the proposal");
        let answer = r.read().reader().refs_read() - before;
        reads.push((landed, queue, answer));
    }
    let (_, queue, answer) = reads[0];
    assert!(
        reads.iter().all(|&(_, q, a)| (q, a) == (queue, answer)),
        "refs read by a queue read and an answer, by landed views: {reads:?}"
    );
}
