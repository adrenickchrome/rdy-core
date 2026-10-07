//! brefs1: a briefing lists the refines, supersedes and revokes lineage of the blocks it covers,
//! in both directions; the incoming refines and supersedes are dependents too, as `rdy impact`
//! shows them at depth 1.
use rdy_engine::brief::{Briefing, Coverage};
use rdy_engine::*;
use rdy_model::*;
use serde_json::json;
use std::collections::BTreeSet;

fn agent() -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    }
}

fn work() -> Scope {
    Scope::View("work".into())
}

/// One decision of the fixture, with its links.
fn dec(id: &str, statement: &str, links: serde_json::Value) -> serde_json::Value {
    json!({"op": "create", "type": "decision", "id": id, "parent": "$s",
           "fields": {"statement": statement, "status": "proposed"}, "links": links})
}

/// A project in view `work` holding a doc and a section, then `ops` under the section. Returns
/// the project and the created ids by name.
fn project_with(
    ops: Vec<serde_json::Value>,
) -> (
    tempfile::TempDir,
    Rdy,
    std::collections::BTreeMap<String, String>,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut all = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Refunds", "doc_type": "decisions-log"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}),
    ];
    all.extend(ops);
    let res = r
        .write(
            &agent(),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!(all)).unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    let created = res.created.into_iter().collect();
    (dir, r, created)
}

fn brief(r: &Rdy, ids: &[&str]) -> Briefing {
    r.read()
        .brief(
            &work(),
            &Coverage {
                ids: ids.iter().map(ToString::to_string).collect(),
                path: None,
            },
            None,
        )
        .unwrap()
}

fn dependents(b: &Briefing) -> BTreeSet<(String, String, String)> {
    b.dependents
        .iter()
        .map(|d| (d.block.clone(), d.kind.clone(), d.on.clone()))
        .collect()
}

fn lineage(b: &Briefing) -> BTreeSet<(String, String, String)> {
    b.lineage
        .iter()
        .map(|l| (l.block.clone(), l.kind.clone(), l.direction.clone()))
        .collect()
}

fn pair(a: &str, k: &str, c: &str) -> (String, String, String) {
    (a.to_string(), k.to_string(), c.to_string())
}

#[test]
fn brefs1_incoming_refines_is_a_dependent() {
    let (_d, r, id) = project_with(vec![
        dec("$a", "Approvers sign refunds", json!({})),
        dec(
            "$b",
            "Approvers sign refunds over 100",
            json!({"refines": [{"target": "$a"}]}),
        ),
    ]);
    let b = brief(&r, &[&id["$a"]]);
    assert!(
        dependents(&b).contains(&pair(&id["$b"], "refines", &id["$a"])),
        "{:?}",
        b.dependents
    );
    let d = b.dependents.iter().find(|d| d.block == id["$b"]).unwrap();
    assert!(
        d.summary.contains("over 100"),
        "the summary names the refiner: {}",
        d.summary
    );
}

#[test]
fn brefs1_incoming_supersedes_is_a_dependent() {
    let (_d, r, id) = project_with(vec![
        dec("$a", "Approvers sign refunds", json!({})),
        dec(
            "$b",
            "Two approvers sign refunds",
            json!({"supersedes": [{"target": "$a"}]}),
        ),
    ]);
    let b = brief(&r, &[&id["$a"]]);
    assert!(
        dependents(&b).contains(&pair(&id["$b"], "supersedes", &id["$a"])),
        "{:?}",
        b.dependents
    );
}

#[test]
fn brefs1_outgoing_refines_is_listed() {
    let (_d, r, id) = project_with(vec![
        dec("$a", "Approvers sign refunds", json!({})),
        dec(
            "$b",
            "Approvers sign refunds over 100",
            json!({"refines": [{"target": "$a"}]}),
        ),
    ]);
    let b = brief(&r, &[&id["$b"]]);
    assert!(
        lineage(&b).contains(&pair(&id["$a"], "refines", "out")),
        "{:?}",
        b.lineage
    );
    let l = b.lineage.iter().find(|l| l.block == id["$a"]).unwrap();
    assert_eq!(l.on, id["$b"]);
    assert!(
        l.summary.contains("Approvers sign refunds"),
        "{}",
        l.summary
    );
    assert!(
        b.dependents.iter().all(|d| d.block != id["$a"]),
        "dependents hold incoming edges only: {:?}",
        b.dependents
    );
    // the other end lists the same edge as incoming, and as a dependent
    let a = brief(&r, &[&id["$a"]]);
    assert!(lineage(&a).contains(&pair(&id["$b"], "refines", "in")));
    assert!(dependents(&a).contains(&pair(&id["$b"], "refines", &id["$a"])));
}

#[test]
fn brefs1_not_suspect_through_refines() {
    let (_d, r, id) = project_with(vec![
        dec("$a", "Approvers sign refunds", json!({})),
        dec(
            "$b",
            "Approvers sign refunds over 100",
            json!({"refines": [{"target": "$a"}]}),
        ),
        dec(
            "$c",
            "Two approvers sign refunds",
            json!({"supersedes": [{"target": "$a"}]}),
        ),
    ]);
    let b = brief(&r, &[&id["$a"]]);
    assert!(
        b.will_go_stale.is_empty(),
        "refines and supersedes are not suspect links: {:?}",
        b.will_go_stale
    );
}

#[test]
fn brefs1_brief_matches_impact_depth1() {
    let (_d, r, id) = project_with(vec![
        dec("$x", "Approvers sign refunds", json!({})),
        dec(
            "$dep",
            "Limits follow from X",
            json!({"depends_on": [{"target": "$x"}]}),
        ),
        dec("$ref", "X over 100", json!({"refines": [{"target": "$x"}]})),
        dec(
            "$sup",
            "Two approvers",
            json!({"supersedes": [{"target": "$x"}]}),
        ),
        json!({"op": "create", "type": "note", "id": "$desc", "parent": "$s",
               "body": "Describes X.", "links": {"describes": [{"target": "$x"}]}}),
        json!({"op": "create", "type": "note", "id": "$n", "parent": "$s", "body": "See {{$x}}."}),
    ]);
    let x = BlockId::parse(&id["$x"]).unwrap();
    let imp: BTreeSet<(String, String)> = r
        .read()
        .impact(&work(), &x, 1)
        .unwrap()
        .into_iter()
        .filter(|i| i.depth == 1)
        .map(|i| (i.id, i.via.kind))
        .collect();
    let b = brief(&r, &[&id["$x"]]);
    let have: BTreeSet<(String, String)> = b
        .dependents
        .iter()
        .map(|d| (d.block.clone(), d.kind.clone()))
        .collect();
    assert_eq!(have, imp, "briefing dependents vs impact depth 1");
    for k in [
        "depends_on",
        "describes",
        "refines",
        "supersedes",
        "references",
    ] {
        assert!(
            imp.iter().any(|(_, kind)| kind == k),
            "{k} is in the fixture: {imp:?}"
        );
    }
}

#[test]
fn brefs1_revokes_in_lineage_both_ways() {
    let (_d, r, id) = project_with(vec![
        dec("$a", "Approvers sign refunds", json!({})),
        dec(
            "$r",
            "Revoke A: no longer needed",
            json!({"revokes": [{"target": "$a"}]}),
        ),
    ]);
    let a = brief(&r, &[&id["$a"]]);
    assert!(
        lineage(&a).contains(&pair(&id["$r"], "revokes", "in")),
        "{:?}",
        a.lineage
    );
    assert!(
        a.dependents.iter().all(|d| d.block != id["$r"]),
        "revokes stays out of dependents, as in rdy impact: {:?}",
        a.dependents
    );
    let r_brief = brief(&r, &[&id["$r"]]);
    assert!(
        lineage(&r_brief).contains(&pair(&id["$a"], "revokes", "out")),
        "{:?}",
        r_brief.lineage
    );
}

#[test]
fn brefs1_lineage_serializes_as_block_kind_direction_summary() {
    let (_d, r, id) = project_with(vec![
        dec("$a", "Approvers sign refunds", json!({})),
        dec("$b", "Over 100", json!({"refines": [{"target": "$a"}]})),
    ]);
    let v = serde_json::to_value(brief(&r, &[&id["$a"]])).unwrap();
    let l = &v["lineage"][0];
    assert_eq!(l["block"], json!(id["$b"]));
    assert_eq!(l["kind"], "refines");
    assert_eq!(l["direction"], "in");
    assert!(l["summary"].as_str().unwrap().contains("Over 100"));
}

#[test]
fn brefs1_two_covered_blocks_in_one_lineage_are_not_listed() {
    // both ends covered: the edge is inside the briefing, as dependents skip it
    let (_d, r, id) = project_with(vec![
        dec("$a", "Approvers sign refunds", json!({})),
        dec("$b", "Over 100", json!({"refines": [{"target": "$a"}]})),
    ]);
    let b = brief(&r, &[&id["$a"], &id["$b"]]);
    assert!(b.lineage.is_empty(), "{:?}", b.lineage);
}

#[test]
fn brefs1_lineage_counts_in_the_page_budget_and_pages_like_dependents() {
    let mut ops = vec![dec("$a", "Approvers sign refunds", json!({}))];
    for i in 0..40 {
        ops.push(dec(
            &format!("$r{i}"),
            &format!("Refinement {i} of the approval rule"),
            json!({"refines": [{"target": "$a"}]}),
        ));
    }
    let (_d, r, id) = project_with(ops);
    let cov = |ids: &[&str]| Coverage {
        ids: ids.iter().map(ToString::to_string).collect(),
        path: None,
    };
    let full = r
        .read()
        .brief(&work(), &cov(&[&id["$a"]]), Some(100_000))
        .unwrap();
    assert_eq!(full.lineage.len(), 40);
    assert!(!full.truncated && full.next.is_none());
    let first = r
        .read()
        .brief(&work(), &cov(&[&id["$a"]]), Some(400))
        .unwrap();
    assert!(
        first.truncated && first.omitted.get("lineage").copied().unwrap_or(0) > 0,
        "{:?}",
        first.omitted
    );
    first.register(&r.briefs());
    let mut seen: Vec<_> = first.lineage.iter().map(|l| l.block.clone()).collect();
    let mut next = first.next.clone();
    let mut pages = 1;
    while let Some(cursor) = next {
        let p = r
            .read()
            .brief_page(&work(), &r.briefs(), &cursor, Some(400))
            .unwrap();
        seen.extend(p.lineage.iter().map(|l| l.block.clone()));
        next = p.next.clone();
        pages += 1;
        assert!(pages < 200, "paging ends");
    }
    let want: Vec<_> = full.lineage.iter().map(|l| l.block.clone()).collect();
    assert_eq!(seen, want, "every lineage entry once, in order");
}
