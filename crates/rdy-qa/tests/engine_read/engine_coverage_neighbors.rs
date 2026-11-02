//! nb4 (D-YXBXH, D-2593P; O5, O6): coverage structure in Complete (`crowded_level` on every
//! doc of a level that holds too many docs directly, `question_as_prose` on a prose or note block
//! that is one question) and the located text of each `unlinked_mention`. Every carried layer
//! equals a fresh build.
use rdy_engine::coverage::{
    BlockEntry, CoverageIssue, Dimension, IssueCode, carried_entries, issues_rebuilt_for_tests,
    rebuilt_for_tests, rollup, structural_builds, structural_for_tests,
};
use rdy_engine::mention_review::MentionVerdict;
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

/// The position of the Complete dimension in `Dimension::ALL`.
fn complete_index() -> usize {
    Dimension::ALL
        .iter()
        .position(|d| *d == Dimension::Complete)
        .unwrap()
}

/// The entry of `block` in `scope`'s coverage issues.
fn entry(r: &Rdy, scope: &Scope, block: &str) -> BlockEntry {
    r.read().coverage_issues(scope).unwrap().blocks[&id(block)].clone()
}

/// The issues of `block` with `code` in `scope`.
fn issues_of(r: &Rdy, scope: &Scope, block: &str, code: IssueCode) -> Vec<CoverageIssue> {
    entry(r, scope, block)
        .issues
        .into_iter()
        .filter(|i| i.code == code)
        .collect()
}

/// The memoised (possibly carried) structural layer and coverage issues of `scope` equal a full
/// build of each, and so does the scope's rollup.
fn same(r: &Rdy, scope: &Scope, step: &str) {
    let st = r.read();
    let carried = structural_for_tests(&st, scope).unwrap();
    let rebuilt = rebuilt_for_tests(&st, scope).unwrap();
    for (b, e) in rebuilt.blocks.iter() {
        assert_eq!(carried.blocks.get(b), Some(e), "{step}: structural {b}");
    }
    assert_eq!(*carried, rebuilt, "{step}: structural layer");
    let memo = st.coverage_issues(scope).unwrap();
    let fresh = issues_rebuilt_for_tests(&st, scope).unwrap();
    for (b, e) in fresh.blocks.iter() {
        assert_eq!(memo.blocks.get(b), Some(e), "{step}: issues {b}");
    }
    assert_eq!(*memo, fresh, "{step}: issues");
    assert_eq!(
        *st.scope_rollup(scope).unwrap(),
        rollup(st.pack(), fresh.blocks.values()),
        "{step}: rollup"
    );
}

/// `n` docs named `<prefix><i>` under `parent` (the top level when `None`), as create ops.
fn docs(parent: Option<&str>, prefix: &str, n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| {
            let mut op = json!({"op": "create", "type": "doc", "id": format!("${prefix}{i}"),
                "fields": {"name": format!("{prefix}{i}"), "title": format!("Doc {prefix}{i}"), "doc_type": "page"}});
            if let Some(p) = parent {
                op["parent"] = json!(p);
            }
            op
        })
        .collect()
}

/// The crowded_level issue of `doc`, when it has one: (subject, fix).
fn crowded(r: &Rdy, scope: &Scope, doc: &str) -> Option<(Option<String>, String)> {
    let found = issues_of(r, scope, doc, IssueCode::CrowdedLevel);
    assert!(found.len() <= 1, "{found:?}");
    found.into_iter().next().map(|i| {
        assert_eq!(i.dimension, Dimension::Complete);
        assert!(i.text.is_none());
        (i.subject, i.fix)
    })
}

const TOP_FIX: &str = "group the docs of the top level into folders";

#[test]
fn nb4_crowded_level_marks_every_immediate_doc() {
    let (_d, mut r) = project();
    // eight docs and one folder at the top; the folder holds eight docs and a folder of three
    let mut ops = docs(None, "r", 8);
    ops.push(json!({"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area", "title": "Area"}}));
    ops.push(json!({"op": "create", "type": "folder", "id": "$deep", "parent": "$area", "fields": {"name": "deep", "title": "Deep"}}));
    ops.extend(docs(Some("$area"), "a", 8));
    ops.extend(docs(Some("$deep"), "n", 3));
    let c = write(&mut r, VIEW, Value::Array(ops)).unwrap().created;
    let v = view();
    for doc in (0..8).flat_map(|i| [format!("$r{i}"), format!("$a{i}")]) {
        assert_eq!(
            crowded(&r, &v, &c[&doc]),
            None,
            "8 docs is the limit: {doc}"
        );
        assert_eq!(entry(&r, &v, &c[&doc]).checks[complete_index()], 1);
    }

    // a ninth doc at each level: every doc of the level, the new one too
    let more = write(
        &mut r,
        VIEW,
        json!([
            {"op": "create", "type": "doc", "id": "$r8", "fields": {"name": "r8", "title": "Doc r8", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$a8", "parent": c["$area"], "fields": {"name": "a8", "title": "Doc a8", "doc_type": "page"}}
        ]),
    )
    .unwrap()
    .created;
    let all = |p: &str, k: &str| -> Vec<String> {
        (0..8)
            .map(|i| c[&format!("${p}{i}")].clone())
            .chain([more[k].clone()])
            .collect()
    };
    for doc in all("r", "$r8") {
        assert_eq!(
            crowded(&r, &v, &doc),
            Some((Some("/".to_string()), TOP_FIX.to_string())),
            "{doc}"
        );
        assert_eq!(
            entry(&r, &v, &doc).checks[complete_index()],
            1,
            "the doc's one Complete check, no second denominator"
        );
    }
    for doc in all("a", "$a8") {
        assert_eq!(
            crowded(&r, &v, &doc),
            Some((
                Some("area".to_string()),
                "group the docs of area into folders".to_string()
            )),
            "{doc}"
        );
    }
    // nested docs are counted at their own level only, and a folder is no doc
    for i in 0..3 {
        assert_eq!(crowded(&r, &v, &c[&format!("$n{i}")]), None);
    }
    let folder = r.read().coverage_issues(&v).unwrap();
    assert!(
        folder
            .blocks
            .get(&id(&c["$area"]))
            .is_none_or(|e| e.issues.iter().all(|i| i.code != IssueCode::CrowdedLevel)),
        "a folder gets no crowded_level"
    );
    same(&r, &v, "nine docs");
}

#[test]
fn nb4_level_carry_matches_rebuild_across_both_parents() {
    let (dir, mut r) = project();
    let mut ops = docs(None, "r", 8);
    ops.push(json!({"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area", "title": "Area"}}));
    ops.extend(docs(Some("$area"), "a", 8));
    let c = write(&mut r, VIEW, Value::Array(ops)).unwrap().created;
    let v = view();
    let step = |r: &mut Rdy, name: &str, ops: Value| -> BTreeMap<String, String> {
        r.read().coverage_issues(&v).unwrap();
        r.read().scope_rollup(&v).unwrap();
        let w = write(r, VIEW, ops).unwrap();
        r.read().coverage_issues(&v).unwrap();
        same(r, &v, name);
        w.created
    };

    // a ninth top-level doc is created with a stored name: carried, and every sibling moves
    r.read().coverage_issues(&v).unwrap();
    let before = structural_builds();
    let n = write(
        &mut r,
        VIEW,
        json!([{"op": "create", "type": "doc", "id": "$r8", "fields": {"name": "r8", "title": "Doc r8", "doc_type": "page"}}]),
    )
    .unwrap()
    .created;
    r.read().coverage_issues(&v).unwrap();
    assert_eq!(structural_builds(), before, "a named new doc carries");
    same(&r, &v, "create the 9th");
    assert!(crowded(&r, &v, &c["$r0"]).is_some());
    // removed again
    step(
        &mut r,
        "remove the 9th",
        json!([{"op": "delete", "id": n["$r8"]}]),
    );
    assert_eq!(crowded(&r, &v, &c["$r0"]), None);
    // moved down a level: the top drops to 7, area rises to 9
    step(
        &mut r,
        "move between levels",
        json!([{"op": "move", "id": c["$r0"], "parent": c["$area"]}]),
    );
    assert_eq!(crowded(&r, &v, &c["$r1"]), None);
    assert_eq!(
        crowded(&r, &v, &c["$r0"]),
        Some((
            Some("area".to_string()),
            "group the docs of area into folders".to_string()
        ))
    );
    // the folder renamed: every fix names the new path
    step(
        &mut r,
        "rename the folder",
        json!([{"op": "update", "id": c["$area"], "fields": {"name": "zone", "title": "Area"}}]),
    );
    for doc in ["$r0", "$a0", "$a7"] {
        assert_eq!(
            crowded(&r, &v, &c[doc]),
            Some((
                Some("zone".to_string()),
                "group the docs of zone into folders".to_string()
            )),
            "{doc}"
        );
    }
    // the folder moved under another: the subject is the whole path
    let f = step(
        &mut r,
        "add a parent folder",
        json!([{"op": "create", "type": "folder", "id": "$top", "fields": {"name": "top", "title": "Top"}}]),
    );
    step(
        &mut r,
        "move the folder",
        json!([{"op": "move", "id": c["$area"], "parent": f["$top"]}]),
    );
    assert_eq!(
        crowded(&r, &v, &c["$a3"]),
        Some((
            Some("top/zone".to_string()),
            "group the docs of top/zone into folders".to_string()
        ))
    );

    // the pack's limit changes: a fresh state, built in full, under the new limit
    r.close().unwrap();
    rdy_qa::packs::use_pack(
        dir.path(),
        "levels",
        "name: levels\nversion: \"1\"\nextends: rdy-standard\ncoverage:\n  max_docs_per_level: 9\n",
        "",
    );
    let r = Rdy::open(dir.path()).unwrap();
    assert_eq!(r.read().pack().max_docs_per_level(), 9);
    assert_eq!(
        crowded(&r, &v, &c["$a3"]),
        None,
        "9 docs under a limit of 9"
    );
    same(&r, &v, "pack limit 9");
    drop(r);
    rdy_qa::packs::use_pack(
        dir.path(),
        "levels",
        "name: levels\nversion: \"1\"\nextends: rdy-standard\ncoverage:\n  max_docs_per_level: 6\n",
        "",
    );
    let r = Rdy::open(dir.path()).unwrap();
    assert_eq!(
        crowded(&r, &v, &c["$r1"]),
        Some((Some("/".to_string()), TOP_FIX.to_string())),
        "7 docs over a limit of 6"
    );
    same(&r, &v, "pack limit 6");
}

/// A carried doc recomputes its siblings only when its level crosses the limit: an entry reads
/// the level through "over the limit" alone, so a level that stays on one side keeps them.
#[test]
fn nbp1_a_new_doc_recomputes_its_level_only_when_it_crosses_the_limit() {
    let (_dir, mut r) = project();
    let c = write(&mut r, VIEW, Value::Array(docs(None, "r", 8)))
        .unwrap()
        .created;
    let v = view();
    let created = |r: &mut Rdy, name: &str| -> (BTreeMap<String, String>, u64) {
        r.read().coverage_issues(&v).unwrap();
        let (builds, before) = (structural_builds(), carried_entries());
        let w = write(
            r,
            VIEW,
            json!([{"op": "create", "type": "doc", "id": "$n", "fields": {"name": name, "title": name, "doc_type": "page"}}]),
        )
        .unwrap();
        r.read().coverage_issues(&v).unwrap();
        assert_eq!(structural_builds(), builds, "{name} carries");
        same(r, &v, name);
        (w.created, carried_entries() - before)
    };

    // eight to nine: the level crosses the limit, so every doc of it moves
    let (ninth, crossed) = created(&mut r, "n8");
    assert!(crossed >= 9, "crossing recomputed only {crossed} entries");
    assert!(crowded(&r, &v, &c["$r0"]).is_some());
    assert!(crowded(&r, &v, &ninth["$n"]).is_some());

    // nine to ten and on: it stays over, so the siblings keep their entries
    for i in 9..24 {
        let name = format!("n{i}");
        let (doc, stayed) = created(&mut r, &name);
        assert!(stayed <= 3, "{name} recomputed {stayed} entries");
        assert!(crowded(&r, &v, &doc["$n"]).is_some(), "{name}");
    }
    assert!(crowded(&r, &v, &c["$r7"]).is_some());

    // a landing carries the same way into canon, and an edit of a doc moves no count
    land(&mut r, VIEW).unwrap();
    let doc = &c["$r3"];
    r.read().coverage_issues(&v).unwrap();
    let before = carried_entries();
    write(
        &mut r,
        VIEW,
        json!([{"op": "update", "id": doc, "fields": {"name": "r3", "title": "Doc r3 again", "doc_type": "page"}}]),
    )
    .unwrap();
    r.read().coverage_issues(&v).unwrap();
    let edited = carried_entries() - before;
    assert!(edited <= 3, "a title edit recomputed {edited} entries");
    same(&r, &v, "edit a doc");
}

#[test]
fn nb4_question_as_prose_is_one_trimmed_sentence() {
    let (_d, mut r) = project();
    let c = write(&mut r, VIEW, json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "faq", "title": "FAQ", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Asked"}},
        {"op": "create", "type": "prose", "id": "$p1", "parent": "$s", "body": "Who owns refunds?"},
        {"op": "create", "type": "note", "id": "$n1", "parent": "$s", "body": "  \n Who approves a refund over the limit?\n\n"},
        {"op": "create", "type": "prose", "id": "$p2", "parent": "$s", "body": "Is v1.2 ready for release?"},
        {"op": "create", "type": "prose", "id": "$p3", "parent": "$s", "body": "Refunds are slow.\nWho owns them?"},
        {"op": "create", "type": "prose", "id": "$p4", "parent": "$s", "body": "Who owns refunds? Who pays?"},
        {"op": "create", "type": "note", "id": "$n2", "parent": "$s", "body": "Stop! Who pays?"},
        {"op": "create", "type": "prose", "id": "$p5", "parent": "$s", "body": "Refunds take a week."},
        {"op": "create", "type": "prose", "id": "$p6", "parent": "$s", "body": "Who owns refunds?\u{3000}"},
        {"op": "create", "type": "question", "id": "$q", "parent": "$s", "fields": {"prompt": "Who owns refunds?"}},
        {"op": "create", "type": "code", "id": "$cd", "parent": "$s", "body": "why?"},
        {"op": "create", "type": "diagram", "id": "$dg", "parent": "$s", "fields": {"lang": "mermaid"}, "body": "graph TD; A-->B?"},
        {"op": "create", "type": "table", "id": "$tb", "parent": "$s", "fields": {"columns": ["Ask"], "rows": [["Who owns refunds?"]]}},
        {"op": "create", "type": "example", "id": "$e", "parent": "$s", "body": "Who owns refunds?"},
        {"op": "create", "type": "claim", "id": "$cl", "parent": "$s", "body": "Who owns refunds?"}
    ]))
    .unwrap()
    .created;
    let v = view();
    let ci = complete_index();
    for (block, asks) in [
        ("$p1", true),
        ("$n1", true),
        ("$p2", true),
        ("$p6", true),
        ("$p3", false),
        ("$p4", false),
        ("$n2", false),
        ("$p5", false),
    ] {
        let found = issues_of(&r, &v, &c[block], IssueCode::QuestionAsProse);
        let e = entry(&r, &v, &c[block]);
        assert_eq!(e.checks[ci], 1, "{block}: one Complete check");
        if asks {
            assert_eq!(found.len(), 1, "{block}: {:?}", e.issues);
            let i = &found[0];
            assert_eq!(i.dimension, Dimension::Complete);
            assert_eq!(i.subject, None);
            assert_eq!(i.text, None);
            assert_eq!(
                i.fix,
                format!("change the type of {} to question", c[block]),
                "{block}"
            );
            assert_eq!(
                serde_json::to_value(i).unwrap()["code"],
                "question_as_prose"
            );
        } else {
            assert!(found.is_empty(), "{block}: {found:?}");
        }
    }
    let all = r.read().coverage_issues(&v).unwrap();
    for block in ["$q", "$cd", "$dg", "$tb", "$e", "$cl"] {
        let Some(e) = all.blocks.get(&id(&c[block])) else {
            continue;
        };
        assert!(
            e.issues
                .iter()
                .all(|i| i.code != IssueCode::QuestionAsProse),
            "{block}: {:?}",
            e.issues
        );
    }
    // only prose and note blocks gain the Complete check
    for block in ["$cd", "$e", "$cl", "$tb"] {
        assert_eq!(all.blocks[&id(&c[block])].checks[ci], 0, "{block}");
    }
    // the question block keeps its own one Complete check
    assert_eq!(all.blocks[&id(&c["$q"])].checks[ci], 1);
    same(&r, &v, "questions");
}

/// The located text of `block`'s unlinked mention of `entity` in `scope`, as JSON.
fn mention_text(r: &Rdy, scope: &Scope, block: &str, entity: &str) -> Value {
    let found: Vec<CoverageIssue> = issues_of(r, scope, block, IssueCode::UnlinkedMention)
        .into_iter()
        .filter(|i| i.subject.as_deref() == Some(entity))
        .collect();
    assert_eq!(found.len(), 1, "{block}: one issue per entity: {found:?}");
    let text = found[0]
        .text
        .as_ref()
        .unwrap_or_else(|| panic!("{block}: an unlinked mention has text"));
    serde_json::to_value(text).unwrap()
}

#[test]
fn nb4_mentions_report_original_leaf_line_span_and_words() {
    let (_d, mut r) = project();
    let c = write(&mut r, VIEW, json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "words", "title": "Words", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Here"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "component", "id": "$m", "parent": "$s", "fields": {"name": "Merge Conflict"}},
        {"op": "create", "type": "claim", "id": "$uni", "parent": "$s", "body": "Grüße — the Ledger holds."},
        {"op": "create", "type": "claim", "id": "$low", "parent": "$s", "body": "İİ LEDGER rules apply."},
        {"op": "create", "type": "flow", "id": "$flow", "parent": "$s", "fields": {"name": "Refund flow", "steps": ["Open a case", "Check the\nLedger twice"]}},
        {"op": "create", "type": "claim", "id": "$crlf", "parent": "$s", "body": "First line.\r\nThe Ledger keeps it."},
        {"op": "create", "type": "claim", "id": "$multi", "parent": "$s", "body": "A merge\r\nconflict stops it."},
        {"op": "create", "type": "claim", "id": "$ref", "parent": "$s", "body": "See {{$o}} and the Ledger."},
        {"op": "create", "type": "claim", "id": "$again", "parent": "$s", "body": "The ledgers and the Ledger and the LEDGER."}
    ]))
    .unwrap()
    .created;
    let v = view();
    let t = c["$t"].as_str();
    let text = |f: &str, line: usize, span: [usize; 2], words: &str| json!({"field": f, "line": line, "span": span, "words": words});
    // Unicode before the match: the span counts UTF-8 bytes ("Grüße — the " is 16 bytes)
    assert_eq!(
        mention_text(&r, &v, &c["$uni"], t),
        text("body", 1, [16, 22], "Ledger")
    );
    // a word whose lower case is longer: the span is of the stored text, in its case
    assert_eq!(
        mention_text(&r, &v, &c["$low"], t),
        text("body", 1, [5, 11], "LEDGER")
    );
    // a nested leaf, on its second line
    assert_eq!(
        mention_text(&r, &v, &c["$flow"], t),
        text("steps[1]", 2, [0, 6], "Ledger")
    );
    // CRLF: the line break is two bytes, the line holds neither
    assert_eq!(
        mention_text(&r, &v, &c["$crlf"], t),
        text("body", 2, [4, 10], "Ledger")
    );
    // a phrase across lines: its first line, the part on that line, and the whole match
    assert_eq!(
        mention_text(&r, &v, &c["$multi"], &c["$m"]),
        text("body", 1, [2, 7], "merge\r\nconflict")
    );
    // a reference before the match moves no byte
    let st = r.read();
    let stored = st
        .materialized(&v)
        .unwrap()
        .get(&id(&c["$ref"]))
        .unwrap()
        .body
        .clone();
    drop(st);
    let at = stored.find("Ledger").unwrap();
    assert!(stored.starts_with("See {{d-"), "{stored}");
    assert_eq!(
        mention_text(&r, &v, &c["$ref"], t),
        text("body", 1, [at, at + 6], "Ledger")
    );
    // the entity named three times: one issue, at the first match, with its plural form
    assert_eq!(
        mention_text(&r, &v, &c["$again"], t),
        text("body", 1, [4, 11], "ledgers")
    );
    // every other issue code omits the text
    let all = r.read().coverage_issues(&v).unwrap();
    let mut others = 0;
    for e in all.blocks.values() {
        for i in &e.issues {
            if i.code == IssueCode::UnlinkedMention {
                assert!(i.text.is_some(), "{i:?}");
            } else {
                others += 1;
                assert!(i.text.is_none(), "{i:?}");
                assert!(serde_json::to_value(i).unwrap().get("text").is_none());
            }
        }
    }
    assert!(others > 0);
    same(&r, &v, "located mentions");
}

/// A landed doc with term Ledger and claim c1 naming it twice, unlinked; c2 describes it.
fn ledger_world(r: &mut Rdy) -> BTreeMap<String, String> {
    let w = write(r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "The Ledger is append-only, and the Ledger never shrinks."},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Balances are kept in one place.",
         "links": {"describes": [{"target": "$t"}]}},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Our ledger is small."}
    ]))
    .unwrap();
    land(r, "seed").unwrap();
    w.created
}

#[test]
fn nb4_everyday_and_dismissal_carry_preserve_scores() {
    let (_d, mut r) = project();
    let c = ledger_world(&mut r);
    let v = view();
    let mentioned =
        |r: &Rdy, block: &str| !issues_of(r, &v, &c[block], IssueCode::UnlinkedMention).is_empty();
    let step = |r: &mut Rdy, name: &str, ops: Value| {
        r.read().coverage_issues(&v).unwrap();
        r.read().scope_rollup(&v).unwrap();
        write(r, VIEW, ops).unwrap();
        r.read().coverage_issues(&v).unwrap();
        same(r, &v, name);
    };
    r.read().coverage_issues(&v).unwrap();
    assert!(mentioned(&r, "$c1") && mentioned(&r, "$p"));
    step(
        &mut r,
        "everyday true",
        json!([{"op": "update", "id": c["$t"], "fields": {"everyday": true}}]),
    );
    assert!(!mentioned(&r, "$c1") && !mentioned(&r, "$p"));
    step(
        &mut r,
        "everyday false",
        json!([{"op": "update", "id": c["$t"], "fields": {"everyday": false}}]),
    );
    assert!(mentioned(&r, "$c1"));
    step(
        &mut r,
        "everyday true again",
        json!([{"op": "update", "id": c["$t"], "fields": {"everyday": true}}]),
    );
    step(
        &mut r,
        "everyday removed",
        json!([{"op": "update", "id": c["$t"], "unset": ["everyday"]}]),
    );
    assert!(mentioned(&r, "$c1"));
    step(
        &mut r,
        "everyday true for links",
        json!([{"op": "update", "id": c["$t"], "fields": {"everyday": true}}]),
    );
    // a new link to the everyday entity: the mention counts and is linked
    step(
        &mut r,
        "new link",
        json!([{"op": "update", "id": c["$c1"], "links": {"describes": [{"target": c["$t"]}]}}]),
    );
    assert!(!mentioned(&r, "$c1"));
    let linked = entry(&r, &v, &c["$c1"]).checks[0];
    assert_eq!(linked, 2, "the content check and the linked mention");
    step(
        &mut r,
        "unlink",
        json!([{"op": "update", "id": c["$c1"], "links": {"describes": []}}]),
    );
    assert_eq!(
        entry(&r, &v, &c["$c1"]).checks[0],
        1,
        "untied: an everyday mention is not read"
    );
    step(
        &mut r,
        "everyday off before the dismissal",
        json!([{"op": "update", "id": c["$t"], "fields": {"everyday": false}}]),
    );

    // an existing dismissal, then the flag, then the dismissal restored
    r.review_mention(
        &writer(),
        &Scope::Canon,
        &c["$p"],
        &c["$t"],
        MentionVerdict::Dismiss,
        "a passing word",
    )
    .unwrap();
    r.read().coverage_issues(&v).unwrap();
    same(&r, &v, "dismissed");
    assert!(!mentioned(&r, "$p"));
    step(
        &mut r,
        "everyday true over a dismissal",
        json!([{"op": "update", "id": c["$t"], "fields": {"everyday": true}}]),
    );
    step(
        &mut r,
        "everyday false over a dismissal",
        json!([{"op": "update", "id": c["$t"], "fields": {"everyday": false}}]),
    );
    assert!(!mentioned(&r, "$p"), "the dismissal still settles it");
    r.review_mention(
        &writer(),
        &Scope::Canon,
        &c["$p"],
        &c["$t"],
        MentionVerdict::Restore,
        "it needs a link",
    )
    .unwrap();
    r.read().coverage_issues(&v).unwrap();
    same(&r, &v, "restored");
    assert!(mentioned(&r, "$p"));
    step(
        &mut r,
        "a body edit after the restore",
        json!([{"op": "update", "id": c["$p"], "body": "Our Ledger is small and kept."}]),
    );
    assert!(mentioned(&r, "$p"));
}

#[test]
fn nb4_intrinsic_coverage_keeps_m3d_source_rules() {
    let (dir, r) = project();
    drop(r);
    let app = rdy_qa::git_repo::GitRepo::new();
    app.write("a.txt", "a\n");
    app.commit("c");
    app.run(&["tag", "v1"]);
    app.link(dir.path());
    let mut r = Rdy::open(dir.path()).unwrap();
    let mut ops = docs(None, "r", 9);
    ops.extend([
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$r0", "fields": {"title": "Ask"}}),
        json!({"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."}),
        json!({"op": "create", "type": "source", "id": "$x", "parent": "$s",
            "fields": {"title": "Release nine", "kind": "release", "locator": "v9"}}),
        json!({"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Who reads the Ledger?",
            "links": {"reflects": ["$x"]}}),
    ]);
    let c = write(&mut r, VIEW, Value::Array(ops)).unwrap().created;
    let v = view();
    let p = id(&c["$p"]);
    let codes = |e: &BlockEntry| -> Vec<IssueCode> { e.issues.iter().map(|i| i.code).collect() };
    let read = |r: &Rdy| {
        let st = r.read();
        let intrinsic = st.coverage_issues(&v).unwrap();
        let shown = st.display_coverage_issues(&v).unwrap();
        (
            intrinsic.blocks[&p].clone(),
            shown.blocks[&p].clone(),
            intrinsic.blocks[&id(&c["$r0"])].clone(),
            st.scope_rollup(&v).unwrap(),
        )
    };
    let (i1, s1, doc1, roll1) = read(&r);
    // the new Complete issues and the mention are intrinsic
    for code in [IssueCode::QuestionAsProse, IssueCode::UnlinkedMention] {
        assert!(codes(&i1).contains(&code), "{code:?}: {:?}", i1.issues);
        assert!(codes(&s1).contains(&code), "{code:?}: {:?}", s1.issues);
    }
    assert!(codes(&doc1).contains(&IssueCode::CrowdedLevel));
    assert!(!codes(&i1).contains(&IssueCode::SourceNotFound));
    assert!(
        codes(&s1).contains(&IssueCode::SourceNotFound),
        "v9 is missing"
    );

    // the tag appears: only the displayed Connected part moves
    app.run(&["tag", "v9"]);
    let (i2, s2, doc2, roll2) = read(&r);
    assert_eq!(i1, i2, "the intrinsic entry does not depend on the machine");
    assert_eq!(doc1, doc2);
    assert_eq!(roll1, roll2);
    assert!(!codes(&s2).contains(&IssueCode::SourceNotFound));
    let cn = Dimension::ALL
        .iter()
        .position(|d| *d == Dimension::Connected)
        .unwrap();
    for d in 0..6 {
        if d != cn {
            assert_eq!(s1.checks[d], s2.checks[d], "dimension {d}");
        }
    }
    let shown: Vec<&CoverageIssue> = s1
        .issues
        .iter()
        .filter(|i| i.code != IssueCode::SourceNotFound)
        .collect();
    assert_eq!(shown, s2.issues.iter().collect::<Vec<_>>());
    // the mention's text is the same in both
    let text = |e: &BlockEntry| {
        e.issues
            .iter()
            .find(|i| i.code == IssueCode::UnlinkedMention)
            .and_then(|i| i.text.clone())
    };
    assert!(text(&i1).is_some());
    assert_eq!(text(&i1), text(&s1));
}
