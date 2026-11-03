//! mi1 (D-KVQ7F): a mention check belongs to a normative block. Informative content keeps its one
//! content check, and its unlinked mentions stay as suggestions, search facts and neighbors.
use rdy_engine::coverage::{
    BlockEntry, CoverageIssue, Dimension, IssueCode, Level, issues_rebuilt_for_tests,
    rebuilt_for_tests, rollup, structural_for_tests,
};
use rdy_engine::mention_review::MentionVerdict;
use rdy_engine::search::{SearchFilters, SearchResult};
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const VIEW: &str = "work";

fn view() -> Scope {
    Scope::View(VIEW.into())
}

fn id(text: &str) -> BlockId {
    BlockId::parse(text).unwrap()
}

/// rdy-standard plus `rule` (normative, describes) and `aside` (informative, describes): custom
/// types, so a rule that listed type names could not pass.
fn custom_project() -> (tempfile::TempDir, Rdy) {
    let (dir, r) = project();
    drop(r);
    let pack = "name: g\nversion: '1'\nextends: rdy-standard\nblock_types:\n  rule: { prefix: ru, traits: [normative, describes], body: required }\n  aside: { prefix: as, traits: [informative, describes], body: required }\n  section:\n    prefix: s\n    traits: [structural]\n    fields:\n      title: { kind: str, required: true }\n    children: [section, prose, claim, requirement, constraint, term, flow, example, note, table, code, decision, question, rule, aside]\n";
    std::fs::write(dir.path().join(".rdy/packs/g.yaml"), pack).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: g\n").unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    (dir, r)
}

fn entry(r: &Rdy, scope: &Scope, block: &str) -> BlockEntry {
    r.read().coverage_issues(scope).unwrap().blocks[&id(block)].clone()
}

fn grounded() -> usize {
    Dimension::ALL
        .iter()
        .position(|d| *d == Dimension::Grounded)
        .unwrap()
}

/// The Grounded issues of `e`: its unlinked mentions and its untied content.
fn gaps(e: &BlockEntry) -> Vec<(IssueCode, bool)> {
    e.issues
        .iter()
        .filter(|i| {
            matches!(
                i.code,
                IssueCode::UnlinkedMention | IssueCode::UntiedContent
            )
        })
        .map(|i| (i.code, i.text.is_some()))
        .collect()
}

/// Every carried layer equals a full build, and so does the rollup.
fn same(r: &Rdy, scope: &Scope, step: &str) {
    let st = r.read();
    let carried = structural_for_tests(&st, scope).unwrap();
    let rebuilt = rebuilt_for_tests(&st, scope).unwrap();
    assert_eq!(*carried, rebuilt, "{step}: structural layer");
    let memo = st.coverage_issues(scope).unwrap();
    let fresh = issues_rebuilt_for_tests(&st, scope).unwrap();
    assert_eq!(*memo, fresh, "{step}: issues");
    assert_eq!(
        *st.scope_rollup(scope).unwrap(),
        rollup(st.pack(), fresh.blocks.values()),
        "{step}: rollup"
    );
}

/// The create op of a block of `kind` under `$s` whose text names the Ledger once.
fn op(kind: &str, key: &str, tie: Option<&str>) -> Value {
    let mut o = match kind {
        "question" => json!({"op": "create", "type": "question", "id": key, "parent": "$s",
            "fields": {"prompt": "Does the Ledger close nightly?"}}),
        "flow" => json!({"op": "create", "type": "flow", "id": key, "parent": "$s",
            "fields": {"name": "Nightly close", "steps": ["Close the Ledger"]}}),
        "table" | "table_n" => json!({"op": "create", "type": "table", "id": key, "parent": "$s",
            "fields": {"columns": ["Step"], "rows": ["The Ledger closes"], "normative": kind == "table_n"}}),
        k => {
            json!({"op": "create", "type": k, "id": key, "parent": "$s", "body": "The Ledger closes nightly."})
        }
    };
    if let Some(t) = tie {
        o["links"] = json!({"describes": [{"target": t}]});
    }
    o
}

const INFORMATIVE: [&str; 7] = [
    "prose", "note", "example", "code", "question", "table", "aside",
];
const NORMATIVE: [&str; 6] = [
    "claim",
    "requirement",
    "constraint",
    "flow",
    "table_n",
    "rule",
];

/// Entities Ledger (`$t`) and Archive (`$a`) under `$s`, then one block of each kind tied to
/// Ledger (`<kind>_l`) and one tied to Archive that names the Ledger (`<kind>_o`).
fn kinds_world(r: &mut Rdy) -> BTreeMap<String, String> {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary"}}),
        json!({"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."}),
        json!({"op": "create", "type": "term", "id": "$a", "parent": "$s", "fields": {"name": "Archive"}, "body": "Keeps old entries."}),
    ];
    for k in INFORMATIVE.iter().chain(NORMATIVE.iter()) {
        ops.push(op(k, &format!("${k}_l"), Some("$t")));
        ops.push(op(k, &format!("${k}_o"), Some("$a")));
    }
    let w = write(r, VIEW, Value::Array(ops)).unwrap();
    w.created
}

#[test]
fn mi1_only_normative_mentions_add_checks() {
    let (_d, mut r) = custom_project();
    let c = kinds_world(&mut r);
    let v = view();
    let g = grounded();
    for k in INFORMATIVE {
        for suffix in ["l", "o"] {
            let e = entry(&r, &v, &c[&format!("${k}_{suffix}")]);
            assert_eq!(e.checks[g], 1, "{k}_{suffix}: the content check only");
            assert_eq!(
                gaps(&e),
                vec![],
                "{k}_{suffix}: tied content, no mention issue"
            );
        }
    }
    for k in NORMATIVE {
        let linked = entry(&r, &v, &c[&format!("${k}_l")]);
        assert_eq!(
            linked.checks[g], 2,
            "{k}_l: content plus the linked mention"
        );
        assert_eq!(gaps(&linked), vec![], "{k}_l");
        let other = entry(&r, &v, &c[&format!("${k}_o")]);
        assert_eq!(
            other.checks[g], 2,
            "{k}_o: content plus the unlinked mention"
        );
        assert_eq!(
            gaps(&other),
            vec![(IssueCode::UnlinkedMention, true)],
            "{k}_o"
        );
        let issue: &CoverageIssue = other
            .issues
            .iter()
            .find(|i| i.code == IssueCode::UnlinkedMention)
            .unwrap();
        assert_eq!(issue.subject.as_deref(), Some(c["$t"].as_str()), "{k}_o");
    }
    same(&r, &v, "world");

    // the rollup: Grounded counts 1 check per informative block, 2 per normative block
    let total = (INFORMATIVE.len() * 2) + (NORMATIVE.len() * 2 * 2);
    let sum: usize = r
        .read()
        .coverage_issues(&v)
        .unwrap()
        .blocks
        .values()
        .map(|e| e.checks[g] as usize)
        .sum();
    assert_eq!(sum, total, "term, section and doc add no Grounded check");
    let unlinked = NORMATIVE.len();
    let roll = r.read().scope_rollup(&v).unwrap();
    let want = (100 * (total - unlinked) + total / 2) / total;
    assert_eq!(
        roll.dimensions[&Dimension::Grounded] as usize,
        want,
        "{unlinked} gaps over {total} checks"
    );

    // untied informative content: the content check and UntiedContent, no mention
    let w = write(
        &mut r,
        VIEW,
        json!([
            {"op": "create", "type": "prose", "id": "$u", "parent": c["$s"], "body": "The Ledger is small."},
            {"op": "create", "type": "claim", "id": "$m", "parent": c["$s"], "body": "The Ledger is append-only. It is kept nightly."}
        ]),
    )
    .unwrap();
    let u = entry(&r, &v, &w.created["$u"]);
    assert_eq!(u.checks[g], 1);
    assert_eq!(gaps(&u), vec![(IssueCode::UntiedContent, false)]);
    // a claim that is untied and mentions: the content check, the mention check, both issues
    let m = entry(&r, &v, &w.created["$m"]);
    assert_eq!(m.checks[g], 2);
    assert_eq!(
        gaps(&m),
        vec![
            (IssueCode::UntiedContent, false),
            (IssueCode::UnlinkedMention, true)
        ]
    );
    same(&r, &v, "untied");
}

#[test]
fn mi1_decisions_follow_normative_and_link_rules() {
    let (_d, mut r) = custom_project();
    let c = write(&mut r, VIEW, json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "decision", "id": "$d", "parent": "$s",
            "fields": {"statement": "The Ledger closes nightly", "status": "proposed"}}
    ]))
    .unwrap()
    .created;
    let v = view();
    let g = grounded();
    // a proposed decision can link (affects): the mention is checked
    let open = entry(&r, &v, &c["$d"]);
    assert_eq!(open.checks[g], 1, "{open:?}");
    assert_eq!(gaps(&open), vec![(IssueCode::UnlinkedMention, true)]);
    land(&mut r, VIEW).unwrap();
    // an accepted (frozen) decision cannot link: no check, however normative it is
    let live = entry(&r, &Scope::Canon, &c["$d"]);
    assert_eq!(live.checks[g], 0, "{live:?}");
    assert_eq!(gaps(&live), vec![]);
}

fn find(r: &Rdy, text: &str, mentions: Option<&str>) -> SearchResult {
    r.read()
        .search(
            &Scope::Canon,
            &SearchQuery {
                text: text.into(),
                filters: SearchFilters {
                    mentions: mentions.map(String::from),
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
            Some(100_000),
        )
        .unwrap()
}

fn brief_facts(r: &Rdy, ids: &[&str]) -> Value {
    let b = r
        .read()
        .brief(
            &Scope::Canon,
            &rdy_engine::brief::Coverage {
                ids: ids.iter().map(|s| s.to_string()).collect(),
                path: None,
            },
            None,
        )
        .unwrap();
    let v = serde_json::to_value(&b).unwrap();
    let cov = &v["coverage"];
    let codes: Vec<(String, String)> = cov["issues"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|i| {
            (
                i["block"].as_str().unwrap_or_default().to_string(),
                i["code"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    json!({"score": cov["score"], "confidence": cov["confidence"], "issues": codes, "reasons": cov["reasons"]})
}

#[test]
fn mi1_mentions_survive_without_scoring() {
    let (_d, mut r) = custom_project();
    let w = write(&mut r, VIEW, json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "term", "id": "$a", "parent": "$s", "fields": {"name": "Archive"}, "body": "Keeps old entries."},
        {"op": "create", "type": "prose", "id": "$unl", "parent": "$s", "body": "The Ledger closes nightly.",
            "links": {"describes": [{"target": "$a"}]}},
        {"op": "create", "type": "prose", "id": "$tied", "parent": "$s", "body": "The Ledger is read nightly.",
            "links": {"describes": [{"target": "$t"}]}},
        {"op": "create", "type": "prose", "id": "$untied", "parent": "$s", "body": "The Ledger is copied nightly."}
    ]))
    .unwrap();
    land(&mut r, VIEW).unwrap();
    let c = w.created;
    let v = Scope::Canon;
    let g = grounded();

    // scoring: no mention issue or check on any of the three
    for k in ["$unl", "$tied", "$untied"] {
        assert_eq!(entry(&r, &v, &c[k]).checks[g], 1, "{k}");
        assert!(
            entry(&r, &v, &c[k])
                .issues
                .iter()
                .all(|i| i.code != IssueCode::UnlinkedMention),
            "{k}"
        );
    }
    assert_eq!(
        gaps(&entry(&r, &v, &c["$untied"])),
        vec![(IssueCode::UntiedContent, false)]
    );

    // the reverse mention map and the fact-sheet neighborhood keep every mentioner
    let n = r
        .read()
        .fact_sheet_neighbourhood(&v, &id(&c["$t"]))
        .unwrap();
    for k in ["$unl", "$tied", "$untied"] {
        assert!(n.contains(&id(&c[k])), "{k} names the Ledger: {n:?}");
    }

    // search: the mentions filter and the hit facts
    let unlinked = find(&r, "nightly", Some("unlinked"));
    let hits: Vec<String> = unlinked
        .matches
        .iter()
        .flat_map(|m| std::iter::once(m.id.clone()).chain(m.also.iter().map(|a| a.id.clone())))
        .collect();
    for k in ["$unl", "$untied"] {
        assert!(hits.contains(&c[k]), "{k} is an unlinked mention: {hits:?}");
    }
    assert!(!hits.contains(&c["$tied"]));
    // a hit carries its block's score facts: only the untied block loses confidence
    let facts = |word: &str, k: &str| {
        let res = find(&r, word, None);
        let m = res.matches.iter().find(|m| m.id == c[k]).unwrap().clone();
        (m.coverage, m.issues, m.confidence)
    };
    assert_eq!(
        facts("closes", "$unl"),
        (Some(100), Some(0), Some(Level::High))
    );
    assert_eq!(
        facts("read", "$tied"),
        (Some(100), Some(0), Some(Level::High))
    );
    let (cov, issues, conf) = facts("copied", "$untied");
    assert_eq!(issues, Some(1));
    assert!(cov < Some(100), "{cov:?}");
    assert_eq!(conf, Some(Level::Low));

    // link suggestions stay for informative content
    let w2 = write(
        &mut r,
        VIEW,
        json!([{"op": "create", "type": "prose", "id": "$n", "parent": c["$s"], "body": "Then the Ledger runs."}]),
    )
    .unwrap();
    let (sug, _) = r
        .read()
        .suggestions(&view(), &w2.change.parse().unwrap())
        .unwrap();
    assert_eq!(sug.len(), 1, "{sug:?}");

    // a cold briefing, then the same after the coverage memo is warm
    let ids = [
        c["$unl"].as_str(),
        c["$tied"].as_str(),
        c["$untied"].as_str(),
    ];
    let cold = brief_facts(&r, &ids);
    r.read().coverage_issues(&v).unwrap();
    let warm = brief_facts(&r, &ids);
    assert_eq!(cold, warm);
    assert_eq!(
        cold["issues"],
        json!([[c["$untied"], "untied_content"]]),
        "only the untied content scores: {cold}"
    );
    assert_eq!(cold["confidence"], json!("medium"), "{cold}");
    let reasons = cold["reasons"].as_array().unwrap();
    assert_eq!(reasons.len(), 1, "no mention reason: {cold}");
    assert_eq!(reasons[0]["code"], json!("untied_content"));
}

#[test]
fn mi1_normative_edits_and_reviews_carry_exactly() {
    let (d, mut r) = custom_project();
    let c = write(&mut r, VIEW, json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "term", "id": "$a", "parent": "$s", "fields": {"name": "Archive"}, "body": "Keeps old entries."},
        {"op": "create", "type": "table", "id": "$tb", "parent": "$s",
            "fields": {"columns": ["Step"], "rows": ["The Ledger closes"]}, "links": {"describes": [{"target": "$a"}]}},
        {"op": "create", "type": "prose", "id": "$pr", "parent": "$s", "body": "The Ledger is small.",
            "links": {"describes": [{"target": "$a"}]}},
        {"op": "create", "type": "claim", "id": "$cl", "parent": "$s", "body": "The Ledger never shrinks.",
            "links": {"describes": [{"target": "$a"}]}}
    ]))
    .unwrap()
    .created;
    let v = view();
    let g = grounded();
    let step = |r: &mut Rdy, name: &str, ops: Value| {
        r.read().coverage_issues(&v).unwrap();
        r.read().scope_rollup(&v).unwrap();
        write(r, VIEW, ops).unwrap();
        r.read().coverage_issues(&v).unwrap();
        same(r, &v, name);
    };
    let state = |r: &Rdy, k: &str| {
        let e = entry(r, &v, &c[k]);
        (
            e.checks[g],
            gaps(&e).into_iter().map(|x| x.0).collect::<Vec<_>>(),
        )
    };
    const GAP: IssueCode = IssueCode::UnlinkedMention;
    r.read().coverage_issues(&v).unwrap();
    assert_eq!(state(&r, "$tb"), (1, vec![]));
    assert_eq!(state(&r, "$pr"), (1, vec![]));
    assert_eq!(state(&r, "$cl"), (2, vec![GAP]));

    // a table's normative field
    step(
        &mut r,
        "table normative",
        json!([{"op": "update", "id": c["$tb"], "fields": {"normative": true}}]),
    );
    assert_eq!(state(&r, "$tb"), (2, vec![GAP]));
    step(
        &mut r,
        "table informative",
        json!([{"op": "update", "id": c["$tb"], "fields": {"normative": false}}]),
    );
    assert_eq!(state(&r, "$tb"), (1, vec![]));

    // a content type, both directions, to a pack type and back
    step(
        &mut r,
        "prose to claim",
        json!([{"op": "update", "id": c["$pr"], "type": "claim"}]),
    );
    assert_eq!(state(&r, "$pr"), (2, vec![GAP]));
    step(
        &mut r,
        "claim to prose",
        json!([{"op": "update", "id": c["$pr"], "type": "prose"}]),
    );
    assert_eq!(state(&r, "$pr"), (1, vec![]));
    step(
        &mut r,
        "prose to rule",
        json!([{"op": "update", "id": c["$pr"], "type": "rule"}]),
    );
    assert_eq!(state(&r, "$pr"), (2, vec![GAP]));
    step(
        &mut r,
        "rule to aside",
        json!([{"op": "update", "id": c["$pr"], "type": "aside"}]),
    );
    assert_eq!(state(&r, "$pr"), (1, vec![]));

    // text and ties
    step(
        &mut r,
        "text drops the name",
        json!([{"op": "update", "id": c["$cl"], "body": "It never shrinks."}]),
    );
    assert_eq!(state(&r, "$cl"), (1, vec![]));
    step(
        &mut r,
        "text names it again",
        json!([{"op": "update", "id": c["$cl"], "body": "The Ledger never shrinks."}]),
    );
    assert_eq!(state(&r, "$cl"), (2, vec![GAP]));
    step(
        &mut r,
        "a tie",
        json!([{"op": "update", "id": c["$cl"], "links": {"describes": [{"target": c["$a"]}, {"target": c["$t"]}]}}]),
    );
    assert_eq!(state(&r, "$cl"), (2, vec![]));
    step(
        &mut r,
        "the tie removed",
        json!([{"op": "update", "id": c["$cl"], "links": {"describes": [{"target": c["$a"]}]}}]),
    );
    assert_eq!(state(&r, "$cl"), (2, vec![GAP]));

    // everyday
    step(
        &mut r,
        "everyday",
        json!([{"op": "update", "id": c["$t"], "fields": {"everyday": true}}]),
    );
    assert_eq!(
        state(&r, "$cl"),
        (1, vec![]),
        "an everyday mention is not read"
    );
    step(
        &mut r,
        "everyday off",
        json!([{"op": "update", "id": c["$t"], "fields": {"everyday": false}}]),
    );
    assert_eq!(state(&r, "$cl"), (2, vec![GAP]));

    // a normative dismissal settles the check, keeps it, and keeps its actor and reason
    let rev = r
        .review_mention(
            &writer(),
            &v,
            &c["$cl"],
            &c["$t"],
            MentionVerdict::Dismiss,
            "a passing word",
        )
        .unwrap();
    r.read().coverage_issues(&v).unwrap();
    same(&r, &v, "dismissed");
    assert_eq!(state(&r, "$cl"), (2, vec![]), "settled, not removed");
    let held = |r: &Rdy| r.read().mention_dismissed(&id(&c["$cl"]), &id(&c["$t"]));
    assert_eq!(held(&r).map(|h| h.to_string()), Some(rev.review.clone()));
    let record = r.read().reader().review(&held(&r).unwrap()).unwrap();
    assert_eq!(record.reason, "a passing word");
    assert_eq!(record.provenance.actor, "human:qa");
    step(
        &mut r,
        "text edit over a dismissal",
        json!([{"op": "update", "id": c["$cl"], "body": "The Ledger never, ever shrinks."}]),
    );
    assert_eq!(state(&r, "$cl"), (2, vec![]));
    step(
        &mut r,
        "table normative over a dismissal",
        json!([{"op": "update", "id": c["$tb"], "fields": {"normative": true}}]),
    );
    land(&mut r, VIEW).unwrap();
    assert_eq!(entry(&r, &Scope::Canon, &c["$cl"]).checks[g], 2);
    assert!(gaps(&entry(&r, &Scope::Canon, &c["$cl"])).is_empty());
    drop(r);
    let mut r = Rdy::open(d.path()).unwrap();
    assert_eq!(
        held(&r).map(|h| h.to_string()),
        Some(rev.review.clone()),
        "kept at reopen"
    );
    let record = r.read().reader().review(&held(&r).unwrap()).unwrap();
    assert_eq!(
        (record.reason.as_str(), record.provenance.actor.as_str()),
        ("a passing word", "human:qa")
    );
    assert_eq!(entry(&r, &Scope::Canon, &c["$cl"]).checks[g], 2);
    assert!(gaps(&entry(&r, &Scope::Canon, &c["$cl"])).is_empty());
    r.review_mention(
        &writer(),
        &Scope::Canon,
        &c["$cl"],
        &c["$t"],
        MentionVerdict::Restore,
        "it needs a link",
    )
    .unwrap();
    r.read().coverage_issues(&Scope::Canon).unwrap();
    same(&r, &Scope::Canon, "restored");
    assert_eq!(entry(&r, &Scope::Canon, &c["$cl"]).checks[g], 2);
    assert_eq!(gaps(&entry(&r, &Scope::Canon, &c["$cl"])).len(), 1);

    // a stored dismissal on prose changes no score; suggestion suppression and restore still work
    let w = write(&mut r, VIEW, json!([{"op": "create", "type": "prose", "id": "$p2", "parent": c["$s"], "body": "The Ledger is read."}])).unwrap();
    let p2 = w.created["$p2"].clone();
    let suggestions = |r: &Rdy, w: &WriteResult| {
        r.read()
            .suggestions(&v, &w.change.parse().unwrap())
            .unwrap()
            .0
            .len()
    };
    let before = entry(&r, &v, &p2);
    assert_eq!(suggestions(&r, &w), 1);
    r.review_mention(
        &writer(),
        &v,
        &p2,
        &c["$t"],
        MentionVerdict::Dismiss,
        "passing",
    )
    .unwrap();
    assert_eq!(entry(&r, &v, &p2), before, "no score change");
    assert_eq!(
        suggestions(&r, &w),
        0,
        "a dismissed pair gets no suggestion"
    );
    r.review_mention(
        &writer(),
        &v,
        &p2,
        &c["$t"],
        MentionVerdict::Restore,
        "needs it",
    )
    .unwrap();
    assert_eq!(entry(&r, &v, &p2), before);
    assert_eq!(suggestions(&r, &w), 1, "restored");
    same(&r, &v, "prose review");
}
