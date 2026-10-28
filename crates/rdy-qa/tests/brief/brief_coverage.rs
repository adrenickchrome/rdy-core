//! cc13: the briefing's coverage section (FW-CV1, FW-CH3, FW-CV2).
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// `rdy_qa::fixtures::write` with an editorial justification: the fixture writes stable normative content, and an
/// unjustified change to stable content blocks its landing (D-ME2HD).
fn clarified(
    r: &mut rdy_engine::Rdy,
    view: &str,
    ops: serde_json::Value,
) -> Result<rdy_engine::WriteResult, rdy_engine::RdyError> {
    r.write(
        &writer(),
        rdy_engine::WriteRequest {
            view: view.into(),
            ops: parse_test_ops(ops)?,
            justification: Some(rdy_model::Justification::Editorial {
                class: rdy_model::EditorialClass::Clarify,
                reason: "seed the test world".into(),
            }),
            message: String::new(),
        },
    )
}

/// Doc `ops` (overview: needs Summary and Goals; has Summary, stable, and Empty) holds term
/// Ledger, claim c1 (names Ledger twice, unlinked, no decision), claim c2 (describes Ledger,
/// decided_by d1), question q (unanswered), decision d1 (live, applied by c2), decision d2
/// (live, nothing applies it), decision d3 (live, needs_content: false). Doc `lone` holds one
/// prose block. Landed.
fn world(r: &mut Rdy) -> BTreeMap<String, String> {
    let w = clarified(r, "work", json!([
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

fn id(text: &str) -> BlockId {
    BlockId::parse(text).unwrap()
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

/// The canon briefing on blocks `ids`.
fn brief_ids(r: &Rdy, ids: &[&str]) -> Briefing {
    r.read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: ids.iter().map(|s| s.to_string()).collect(),
                path: None,
            },
            None,
        )
        .unwrap()
}

/// Doc `hub` with claim H; doc `many` with `n` claims that each cite H; landed. H's doc path.
fn hub(r: &mut Rdy, n: usize) {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "hub", "title": "Hub", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$h", "parent": "$s", "body": "H holds."}
    ]))
    .unwrap();
    let h = w.created["$h"].clone();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$m", "fields": {"name": "many", "title": "Many", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$ms", "parent": "$m", "fields": {"title": "S"}}),
    ];
    for i in 0..n {
        ops.push(json!({"op": "create", "type": "claim", "parent": "$ms", "body": format!("Claim {i} cites {{{{{h}}}}}.")}));
    }
    write(r, "work", Value::Array(ops)).unwrap();
    land(r, "work").unwrap();
}

/// Every page of the canon briefing of the doc at `path`, cut at `max_tokens`, as JSON.
fn brief_all_pages(r: &Rdy, path: &str, max_tokens: usize) -> Vec<Value> {
    let cov = Coverage {
        ids: vec![],
        path: Some(path.to_string()),
    };
    let first = r
        .read()
        .brief(&Scope::Canon, &cov, Some(max_tokens))
        .unwrap();
    first.register(&r.briefs());
    let mut pages = vec![serde_json::to_value(&first).unwrap()];
    let mut next = first.next.clone();
    while let Some(c) = next {
        let p = r
            .read()
            .brief_page(&Scope::Canon, &r.briefs(), &c, Some(max_tokens))
            .unwrap();
        next = p.next.clone();
        pages.push(serde_json::to_value(&p).unwrap());
        assert!(pages.len() < 200, "paging does not end");
    }
    pages
}

#[test]
fn cc13_a_briefing_carries_score_pages_issues_and_confidence() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let b = brief_ids(&r, &[&c["$c1"]]);
    let v = serde_json::to_value(&b).unwrap();
    let cov = &v["coverage"];
    assert!(cov["score"].as_u64().unwrap() < 100, "{cov}");
    assert!(cov["dimensions"].is_object(), "{cov}");
    assert_eq!(cov["pages"][0]["path"], "ops", "{cov}");
    assert_eq!(cov["confidence"], "medium", "{cov}");
    assert_eq!(cov["reasons"][0]["code"], "untied_content", "{cov}");
    assert!(
        cov["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "unlinked_mention"),
        "{cov}"
    );
    assert!(cov.get("issues_omitted").is_none(), "{cov}");
    assert!(
        cov.get("grounded").is_none(),
        "the graph measures are gone (fix forward)"
    );
}

#[test]
fn cc13_an_open_suspect_on_a_dependent_makes_the_briefing_low() {
    let (_d, mut r) = project();
    let (citer, target) = landed_citation_then_target_changed(&mut r);
    let v = serde_json::to_value(brief_ids(&r, &[&target.to_string()])).unwrap();
    assert_eq!(
        v["coverage"]["confidence"], "low",
        "{citer} depends on the target and is suspect"
    );
    assert_eq!(v["coverage"]["reasons"][0]["code"], "open_suspect");
    assert_eq!(
        v["coverage"]["reasons"][0]["block"],
        citer.to_string(),
        "the issue is on the dependent"
    );
}

/// Final review M8 (task 13, D-EEN7A): a decision that governs a covered block is in the
/// neighbourhood. Claim c (draft, describes a term, decided by d1) has no issue of its own; d1
/// and d2 state the same thing, an open duplicate in canon. The briefing on c is Low, and its
/// first reason is on d1.
#[test]
fn cc13_an_open_issue_on_a_governing_decision_lowers_the_briefing() {
    let (_d, mut r) = project();
    let w = write(&mut r, "gov", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "fees", "title": "Fees", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Rules", "lifecycle": "draft"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Fee"}, "body": "A charge."},
        {"op": "create", "type": "decision", "id": "$d1", "parent": "$s", "fields": {"statement": "Fees are charged every month", "status": "proposed"}},
        {"op": "create", "type": "decision", "id": "$d2", "parent": "$s", "fields": {"statement": "Fees are charged every month", "status": "proposed"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "A fee is billed monthly.",
         "links": {"describes": [{"target": "$t"}], "decided_by": [{"target": "$d1"}]}}
    ]))
    .unwrap()
    .created;
    land(&mut r, "gov").unwrap();
    let v = serde_json::to_value(brief_ids(&r, &[&w["$c"]])).unwrap();
    assert!(
        v["governs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["decision"] == w["$d1"]),
        "{v}"
    );
    let cov = &v["coverage"];
    assert_eq!(cov["confidence"], "low", "{cov}");
    assert_eq!(cov["reasons"][0]["block"], w["$d1"], "{cov}");
    assert_eq!(cov["reasons"][0]["code"], "open_tension", "{cov}");
}

/// Final review M8 (task 13, D-EEN7A): a block that will go stale when a covered block changes is
/// in the neighbourhood. Claim c pins a citation to the term Wallet and names the term Ledger
/// without a link; the briefing on Wallet lists c under will_go_stale, is Medium, and its first
/// reason is c's unlinked mention.
#[test]
fn cc13_an_issue_on_a_will_go_stale_block_reaches_the_briefing() {
    let (_d, mut r) = project();
    let w = write(&mut r, "cite", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "pay", "title": "Pay", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Rules"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Wallet"}, "body": "Holds cards."},
        {"op": "create", "type": "term", "id": "$l", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Payment uses {{$t}} and the Ledger."}
    ]))
    .unwrap()
    .created;
    land(&mut r, "cite").unwrap();
    let v = serde_json::to_value(brief_ids(&r, &[&w["$t"]])).unwrap();
    assert!(
        v["will_go_stale"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["block"] == w["$c"]),
        "{v}"
    );
    let cov = &v["coverage"];
    assert_eq!(cov["confidence"], "medium", "{cov}");
    assert_eq!(cov["reasons"][0]["block"], w["$c"], "{cov}");
    assert_eq!(cov["reasons"][0]["code"], "unlinked_mention", "{cov}");
}

#[test]
fn cc13_issues_past_the_cap_are_counted_not_listed() {
    let (_d, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$o", "fields": {"name": "wide", "title": "Wide", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "S"}}),
        json!({"op": "create", "type": "term", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."}),
    ];
    for i in 0..30 {
        ops.push(json!({"op": "create", "type": "claim", "parent": "$s", "body": format!("Note {i} mentions the Ledger.")}));
    }
    write(&mut r, "work", Value::Array(ops)).unwrap();
    land(&mut r, "work").unwrap();
    let b = r
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![],
                path: Some("wide".into()),
            },
            Some(100_000),
        )
        .unwrap();
    let v = serde_json::to_value(&b).unwrap();
    let cov = &v["coverage"];
    let shown = cov["issues"].as_array().unwrap().len();
    assert_eq!(shown, rdy_engine::brief::BRIEF_ISSUES_CAP, "{cov}");
    assert!(cov["issues_omitted"].as_u64().unwrap() > 0, "{cov}");
}

/// guard (pre-flight): the section is already never paged, so this passes before the change; it
/// pins that the new section stays the same on every page.
#[test]
fn cc13_every_page_carries_the_same_section() {
    let (_d, mut r) = project();
    hub(&mut r, 60);
    let pages = brief_all_pages(&r, "hub", 300); // a doc-wide briefing cut into pages
    assert!(pages.len() > 1);
    assert!(pages[0]["coverage"].is_object(), "{}", pages[0]);
    for p in &pages[1..] {
        assert_eq!(p["coverage"], pages[0]["coverage"]);
    }
}

/// Folder `big` holds `n` docs `d00`.., each with one section and one claim; the doc `zz` (last
/// by path) also has a claim that names the term Ledger without a link, so it scores lowest; the others describe the term.
/// Landed.
fn big_folder(r: &mut Rdy, n: usize) {
    let mut ops = vec![
        json!({"op": "create", "type": "folder", "id": "$f", "fields": {"name": "big"}}),
        json!({"op": "create", "type": "doc", "id": "$ld", "parent": "$f", "fields": {"name": "ledger", "title": "Ledger", "doc_type": "page"}}),
        json!({"op": "create", "type": "term", "id": "$t", "parent": "$ld", "fields": {"name": "Ledger"}, "body": "Holds balances."}),
    ];
    let names: Vec<String> = (0..n)
        .map(|i| format!("d{i:02}"))
        .chain(["zz".to_string()])
        .collect();
    for name in &names {
        let (d, s) = (format!("${name}"), format!("$s{name}"));
        ops.push(json!({"op": "create", "type": "doc", "id": d, "parent": "$f", "fields": {"name": name, "title": name, "doc_type": "page"}}));
        ops.push(json!({"op": "create", "type": "section", "id": s, "parent": d, "fields": {"title": "S"}}));
        if name == "zz" {
            ops.push(json!({"op": "create", "type": "claim", "parent": s, "body": "The Ledger is mentioned here, and the Ledger again."}));
        } else {
            ops.push(json!({"op": "create", "type": "claim", "parent": s, "body": format!("Claim of {name}."), "links": {"describes": [{"target": "$t"}]}}));
        }
    }
    write(r, "work", Value::Array(ops)).unwrap();
    land(r, "work").unwrap();
}

#[test]
fn cc13_a_folder_briefing_caps_its_pages() {
    let (_d, mut r) = project();
    big_folder(&mut r, 30);
    let b = r
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![],
                path: Some("big".into()),
            },
            None,
        )
        .unwrap();
    let v = serde_json::to_value(&b).unwrap();
    let cov = &v["coverage"];
    let pages = cov["pages"].as_array().unwrap();
    let cap = rdy_engine::coverage::PAGES_CAP;
    assert_eq!(pages.len(), cap, "{cov}");
    // 30 docs + zz + the ledger doc
    assert_eq!(
        cov["pages_omitted"].as_u64().unwrap() as usize,
        32 - cap,
        "{cov}"
    );
    // lowest score first, ties by path; the lowest page sorts last by path but comes first
    assert_eq!(pages[0]["path"], "big/zz", "{cov}");
    let key = |p: &Value| {
        (
            p["score"].as_u64().unwrap(),
            p["path"].as_str().unwrap().to_string(),
        )
    };
    assert!(pages.windows(2).all(|w| key(&w[0]) <= key(&w[1])), "{cov}");
    // the briefing fits the default budget
    let tokens = serde_json::to_string(&v).unwrap().len() / 4;
    assert!(
        tokens <= rdy_engine::brief::BRIEF_DEFAULT_TOKENS,
        "{tokens} tokens"
    );
}

#[test]
fn cc13_a_briefing_with_few_pages_omits_the_count() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let v = serde_json::to_value(brief_ids(&r, &[&c["$c1"]])).unwrap();
    assert!(v["coverage"].get("pages_omitted").is_none(), "{v}");
}

/// The briefing on `cov` in `scope`, as JSON: once from a state that memoises no coverage (the
/// briefing works out its own blocks' entries), then from the same state once its coverage is
/// built in full. The two must be equal (perf4).
fn local_then_memo(r: &Rdy, scope: &Scope, cov: &Coverage) -> (Value, Value) {
    use rdy_engine::coverage::{local_entries, state_layer_builds, structural_builds};
    let st = r.read();
    let (layers, states, local) = (structural_builds(), state_layer_builds(), local_entries());
    let first = serde_json::to_value(st.brief(scope, cov, None).unwrap()).unwrap();
    assert_eq!(structural_builds(), layers, "no structural layer built");
    assert_eq!(state_layer_builds(), states, "no state layer built");
    assert!(local_entries() > local, "the entries were worked out");
    st.coverage_issues(scope).unwrap();
    let local = local_entries();
    let second = serde_json::to_value(st.brief(scope, cov, None).unwrap()).unwrap();
    assert_eq!(local_entries(), local, "the memoised issues are read");
    (first, second)
}

fn by_path(p: &str) -> Coverage {
    Coverage {
        ids: vec![],
        path: Some(p.to_string()),
    }
}

/// perf4: a briefing on a state that memoises no coverage builds neither the structural layer
/// nor the state layer of the scope, and its coverage section equals the one read from the full
/// build: unlinked mentions, a dismissed mention, an open question, unapplied decisions, a stub
/// section, a suspect, open and dismissed tensions, in canon and in a view.
#[test]
fn perf4_a_briefing_without_memoised_coverage_equals_one_with_it() {
    let (d, mut r) = project();
    let c = world(&mut r);
    let (citer, target) = landed_citation_then_target_changed(&mut r);
    // an open duplicate in canon on governing decisions, and a duplicate in a view
    let g = write(&mut r, "gov", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "fees", "title": "Fees", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Rules", "lifecycle": "draft"}},
        {"op": "create", "type": "decision", "id": "$d1", "parent": "$s", "fields": {"statement": "Fees are charged every month", "status": "proposed"}},
        {"op": "create", "type": "decision", "id": "$d2", "parent": "$s", "fields": {"statement": "Fees are charged every month", "status": "proposed"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "A fee is billed monthly.",
         "links": {"decided_by": [{"target": "$d1"}]}}
    ]))
    .unwrap()
    .created;
    land(&mut r, "gov").unwrap();
    r.review_mention(
        &writer(),
        &Scope::Canon,
        &c["$c1"],
        &c["$t"],
        rdy_engine::mention_review::MentionVerdict::Dismiss,
        "everyday word",
    )
    .unwrap();
    write(&mut r, "v", json!([
        {"op": "create", "type": "claim", "parent": c["$empty"], "body": "Refunds are paid within a week."},
        {"op": "create", "type": "claim", "parent": c["$empty"], "body": "Refunds are paid within a week."},
        {"op": "create", "type": "claim", "parent": c["$empty"], "body": "The Ledger is audited."}
    ]))
    .unwrap();
    let dup = {
        let st = r.read();
        st.tensions()
            .unwrap()
            .iter()
            .find(|t| {
                t.kind == tensions::TensionKind::DuplicateDefinition
                    && t.blocks().iter().any(|b| b.to_string() == g["$d1"])
            })
            .map(|t| tensions::tension_label(&t.id))
            .expect("the duplicate decisions")
    };
    r.dismiss_tension(&writer(), &dup, "said twice on purpose")
        .unwrap();

    drop(r);
    let view = Scope::View("v".into());
    let mut codes = std::collections::BTreeSet::new();
    for (scope, cov) in [
        (Scope::Canon, by_path("ops")),
        (Scope::Canon, by_path("fees")),
        (Scope::Canon, by_path("pay")),
        (
            Scope::Canon,
            Coverage {
                ids: vec![target.to_string()],
                path: None,
            },
        ),
        (
            Scope::Canon,
            Coverage {
                ids: vec![c["$c1"].clone(), g["$c"].clone()],
                path: None,
            },
        ),
        (view.clone(), by_path("ops")),
        (
            view,
            Coverage {
                ids: vec![citer.to_string()],
                path: None,
            },
        ),
    ] {
        // reopened: a state that memoises no coverage
        let r = Rdy::open(d.path()).unwrap();
        let (local, memo) = local_then_memo(&r, &scope, &cov);
        assert_eq!(local["coverage"], memo["coverage"], "{scope:?} {cov:?}");
        assert_eq!(local, memo, "{scope:?} {cov:?}");
        for key in ["issues", "reasons"] {
            for i in local["coverage"][key].as_array().unwrap() {
                codes.insert(i["code"].as_str().unwrap().to_string());
            }
        }
    }
    for code in [
        "unlinked_mention",
        "open_question",
        "unapplied_decision",
        "open_suspect",
        "open_tension",
        "dismissed_tension",
        "untied_content",
    ] {
        assert!(codes.contains(code), "{code} not exercised: {codes:?}");
    }
}

/// perf4: with no memoised coverage, a briefing on one doc works out the entries of that doc's
/// blocks only (its neighbourhood here is the doc itself): not the project's other docs. The
/// state keeps what it worked out for the next briefing of it.
#[test]
fn perf4_a_cold_briefing_works_out_its_own_blocks_only() {
    let dir = tempfile::tempdir().unwrap();
    let docs = rdy_engine::testkit::generate_tree(dir.path(), 3, 4, 10);
    let mut r = Rdy::open(dir.path()).unwrap();
    land(&mut r, "work").unwrap();
    let total = r.read().materialized(&Scope::Canon).unwrap().len();
    let before = rdy_engine::coverage::local_entries();
    let b = r
        .read()
        .brief(&Scope::Canon, &by_path(&docs[0]), None)
        .unwrap();
    let worked = rdy_engine::coverage::local_entries() - before;
    assert_eq!(
        worked, b.covers as u64,
        "one entry per covered block, each once"
    );
    assert!(worked * 10 < total as u64, "{worked} of {total} blocks");
    // the state keeps them: a second briefing of it works out none again
    let before = rdy_engine::coverage::local_entries();
    let again = r
        .read()
        .brief(&Scope::Canon, &by_path(&docs[0]), None)
        .unwrap();
    assert_eq!(rdy_engine::coverage::local_entries(), before);
    assert_eq!(
        serde_json::to_value(&again.coverage).unwrap(),
        serde_json::to_value(&b.coverage).unwrap()
    );
}

/// perf4: after a write, a briefing in a view whose coverage was read before the write carries
/// it (the write reply's path): it works out no entry and builds nothing in full.
#[test]
fn perf4_a_briefing_after_a_read_and_a_write_carries_the_coverage() {
    use rdy_engine::coverage::{local_entries, state_layer_builds, structural_builds};
    let (_d, mut r) = project();
    let c = world(&mut r);
    let v = Scope::View("next".into());
    write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "prose", "parent": c["$empty"], "body": "One."}]),
    )
    .unwrap();
    r.read().coverage_issues(&v).unwrap();
    write(
        &mut r,
        "next",
        json!([{"op": "create", "type": "prose", "parent": c["$empty"], "body": "Two."}]),
    )
    .unwrap();
    let (layers, states, local) = (structural_builds(), state_layer_builds(), local_entries());
    let b = r.read().brief(&v, &by_path("ops"), None).unwrap();
    assert!(b.coverage.is_some());
    assert_eq!(
        (structural_builds(), state_layer_builds(), local_entries()),
        (layers, states, local)
    );
}

/// perf4 fix: a briefing that works out its own entries reads the dismissed mentions of its
/// blocks only: with dismissals on other pages, it visits none of them.
#[test]
fn perf4_a_briefing_visits_no_dismissed_mention_of_another_page() {
    use rdy_engine::coverage::{local_entries, mention_dismissals_visited};
    let (_d, mut r) = project();
    let c = world(&mut r);
    let n = 5;
    let ops: Vec<Value> = (0..n)
        .map(|i| json!({"op": "create", "type": "claim", "id": format!("$m{i}"), "parent": c["$empty"], "body": format!("The Ledger holds entry {i}.")}))
        .collect();
    let m = write(&mut r, "more", Value::Array(ops)).unwrap().created;
    land(&mut r, "more").unwrap();
    for i in 0..n {
        r.review_mention(
            &writer(),
            &Scope::Canon,
            &m[&format!("$m{i}")],
            &c["$t"],
            rdy_engine::mention_review::MentionVerdict::Dismiss,
            "everyday word",
        )
        .unwrap();
    }
    assert_eq!(r.read().mention_dismissals().len(), n);
    let st = r.read();
    let (visited, local) = (mention_dismissals_visited(), local_entries());
    let b = st.brief(&Scope::Canon, &by_path("lone"), None).unwrap();
    assert!(local_entries() > local, "the entries were worked out");
    assert!(b.covers > 0);
    assert_eq!(
        mention_dismissals_visited(),
        visited,
        "no dismissal of another page visited"
    );
    // a briefing of the page that holds them still settles them, visiting each once per read
    let visited = mention_dismissals_visited();
    let b = st.brief(&Scope::Canon, &by_path("ops"), None).unwrap();
    let cov = serde_json::to_value(&b.coverage).unwrap();
    let issues = cov["issues"].as_array().unwrap();
    assert!(
        issues.iter().any(|i| i["code"] == "unlinked_mention"),
        "c1's mention stays: {cov}"
    );
    for i in 0..n {
        assert!(
            !issues
                .iter()
                .any(|x| x["block"] == m[&format!("$m{i}")].as_str()
                    && x["code"] == "unlinked_mention"),
            "{cov}"
        );
    }
    assert!(mention_dismissals_visited() > visited);
}
