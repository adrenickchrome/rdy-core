//! rf6 (D-VPATZ): every engine read that names governing decisions applies the graph's one
//! route rule (`decided_by`, `implements`, `affects`, `describes/affects` through an entity) and
//! the closed status table: context, semantic case and coverage read live decisions; briefing
//! and fact sheets add proposed ones; the search filter `governed_by` takes any status. D-Z8P09:
//! a supporting block never makes a decision applied.
use rdy_engine::coverage::IssueCode;
use rdy_engine::lexicon::NameKind;
use rdy_engine::search::SearchFilters;
use rdy_engine::*;
use rdy_model::*;
use std::collections::{BTreeMap, BTreeSet};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn w() -> Scope {
    Scope::View("w".into())
}

/// A block with no links, under `parent`, at `order`.
fn block(i: &str, ty: &str, parent: Option<&str>, order: &str, body: &str) -> Block {
    Block {
        id: id(i),
        block_type: ty.into(),
        parent: parent.map(id),
        order: order.into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn field(b: &mut Block, k: &str, v: &str) {
    b.fields.insert(k.into(), FieldValue::Str(v.into()));
}

fn link(b: &mut Block, kind: &str, target: &str, part: Option<&str>) {
    b.links.entry(kind.into()).or_default().push(Link {
        target: id(target),
        part: part.map(str::to_string),
    });
}

fn decision(i: &str, order: &str, status: &str) -> Block {
    let mut d = block(i, "decision", Some("s-SEC00"), order, "");
    field(&mut d, "status", status);
    field(&mut d, "statement", &format!("statement of {i}"));
    d
}

fn entity(i: &str, order: &str, name: &str) -> Block {
    let mut t = block(i, "term", Some("s-SEC00"), order, &format!("{name} rule."));
    field(&mut t, "name", name);
    t
}

/// Writes `blocks` straight to view `w` (no engine rule), as one change.
fn commit(r: &mut Rdy, blocks: Vec<Block>) {
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

/// Doc `spec` / stable section with: entity t-TERM0 (affected by live D-AAAA1 and partly
/// superseded D-AAAA6), entity t-TERM1 (affected by proposed D-AAAA2, rejected D-AAAA3,
/// superseded D-AAAA4 and revoked D-AAAA5); claim c-CCCC1 describes both and is decided by live
/// D-BBBB1 and implements proposed D-BBBB2; claim c-CCCC2 describes t-TERM1 only; claim c-CCCC3
/// describes claim c-CCCC4 (no entity), which live D-BBBB3 affects. D-AAAA7, D-AAAA8 and
/// D-AAAA9 supersede, revoke and part-supersede.
fn world(r: &mut Rdy) {
    let mut doc = block("d-D0C00", "doc", None, "B", "");
    field(&mut doc, "name", "spec");
    field(&mut doc, "title", "Spec");
    field(&mut doc, "doc_type", "page");
    let mut sec = block("s-SEC00", "section", Some("d-D0C00"), "B", "");
    field(&mut sec, "title", "Rules");
    field(&mut sec, "lifecycle", "stable");
    let mut blocks = vec![
        doc,
        sec,
        entity("t-TERM0", "C", "Hub"),
        entity("t-TERM1", "D", "Spoke"),
    ];
    let d = |i: &str, order: &str, status: &str, affects: &[&str]| {
        let mut b = decision(i, order, status);
        for a in affects {
            link(&mut b, "affects", a, None);
        }
        b
    };
    let mut ds = vec![
        d("D-AAAA1", "E", "accepted", &["t-TERM0"]),
        d("D-AAAA2", "F", "proposed", &["t-TERM1"]),
        d("D-AAAA3", "G", "rejected", &["t-TERM1"]),
        d("D-AAAA4", "H", "accepted", &["t-TERM1"]),
        d("D-AAAA5", "J", "accepted", &["t-TERM1"]),
        d("D-AAAA6", "K", "accepted", &["t-TERM0"]),
        d("D-AAAA7", "M", "accepted", &[]),
        d("D-AAAA8", "N", "accepted", &[]),
        d("D-AAAA9", "P", "accepted", &[]),
        d("D-BBBB1", "Q", "accepted", &[]),
        d("D-BBBB2", "R", "proposed", &[]),
        d("D-BBBB3", "S", "accepted", &["c-CCCC4"]),
    ];
    link(&mut ds[6], "supersedes", "D-AAAA4", None);
    link(&mut ds[7], "revokes", "D-AAAA5", None);
    link(&mut ds[8], "supersedes", "D-AAAA6", Some("the second half"));
    blocks.extend(ds);
    let mut c1 = block(
        "c-CCCC1",
        "claim",
        Some("s-SEC00"),
        "T",
        "Every hub rule holds.",
    );
    link(&mut c1, "describes", "t-TERM0", None);
    link(&mut c1, "describes", "t-TERM1", None);
    link(&mut c1, "decided_by", "D-BBBB1", None);
    link(&mut c1, "implements", "D-BBBB2", None);
    let mut c2 = block(
        "c-CCCC2",
        "claim",
        Some("s-SEC00"),
        "V",
        "No spoke rule holds.",
    );
    link(&mut c2, "describes", "t-TERM1", None);
    let mut c3 = block(
        "c-CCCC3",
        "claim",
        Some("s-SEC00"),
        "W",
        "A claim rule on a claim.",
    );
    link(&mut c3, "describes", "c-CCCC4", None);
    let c4 = block(
        "c-CCCC4",
        "claim",
        Some("s-SEC00"),
        "X",
        "An affected rule.",
    );
    blocks.extend([c1, c2, c3, c4]);
    commit(r, blocks);
}

/// A governs row: decision, status, and its (block, entity) route pairs.
type Row = (String, String, Vec<(String, String)>);
/// A fact sheet decision: id, via, status and the blocks it governs.
type SheetRow = (String, String, String, Vec<String>);

fn ids<'a>(v: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    v.into_iter().map(str::to_string).collect()
}

fn ungrounded(issues: &rdy_engine::coverage::CoverageIssues, b: &str) -> bool {
    issues.blocks[&id(b)]
        .issues
        .iter()
        .any(|i| i.code == IssueCode::Ungrounded)
}

#[test]
fn rf6_every_engine_consumer_uses_the_four_routes() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    world(&mut r);
    let st = r.read();
    let live = ids(["D-AAAA1", "D-AAAA6", "D-BBBB1"]);

    // context: live routes only, in id order, ids only
    let ctx = st.context(&w(), &id("c-CCCC1")).unwrap();
    assert_eq!(ctx.governing, live, "context");
    assert!(
        st.context(&w(), &id("c-CCCC2"))
            .unwrap()
            .governing
            .is_empty(),
        "no live route to c-CCCC2"
    );
    assert!(
        st.context(&w(), &id("c-CCCC3"))
            .unwrap()
            .governing
            .is_empty(),
        "a describes target that is no entity is no route"
    );

    // briefing: live and proposed, with the entity routes in through
    let b = st
        .brief(
            &w(),
            &brief::Coverage {
                ids: vec!["c-CCCC1".into()],
                path: None,
            },
            None,
        )
        .unwrap();
    let governs: Vec<Row> = b
        .governs
        .iter()
        .map(|g| {
            (
                g.decision.clone(),
                g.status.clone(),
                g.through
                    .iter()
                    .map(|t| (t.block.clone(), t.entity.clone()))
                    .collect(),
            )
        })
        .collect();
    let via = |e: &str| vec![("c-CCCC1".to_string(), e.to_string())];
    assert_eq!(
        governs,
        vec![
            ("D-AAAA1".into(), "live".into(), via("t-TERM0")),
            ("D-AAAA2".into(), "proposed".into(), via("t-TERM1")),
            (
                "D-AAAA6".into(),
                "partially_superseded".into(),
                via("t-TERM0")
            ),
            ("D-BBBB1".into(), "live".into(), vec![]),
            ("D-BBBB2".into(), "proposed".into(), vec![]),
        ],
        "briefing"
    );

    // fact sheet of t-TERM1: its own proposed decision lists what it governs through it; the
    // facts' live and proposed governors are listed with the fact
    let sheet = st.fact_sheet(&w(), &id("t-TERM1"), NameKind::Name).unwrap();
    let decisions: Vec<SheetRow> = sheet
        .decisions
        .iter()
        .map(|d| (d.id.clone(), d.via.clone(), d.status.clone(), d.on.clone()))
        .collect();
    let c1 = ids(["c-CCCC1"]);
    assert_eq!(
        decisions,
        vec![
            (
                "D-AAAA1".into(),
                "governs".into(),
                "live".into(),
                c1.clone()
            ),
            (
                "D-AAAA2".into(),
                "affects".into(),
                "proposed".into(),
                ids(["c-CCCC1", "c-CCCC2"])
            ),
            (
                "D-AAAA6".into(),
                "governs".into(),
                "partially_superseded".into(),
                c1.clone()
            ),
            (
                "D-BBBB1".into(),
                "governs".into(),
                "live".into(),
                c1.clone()
            ),
            ("D-BBBB2".into(), "governs".into(), "proposed".into(), c1),
        ],
        "fact sheet"
    );

    // semantic case: live routes only
    let mut leased = BTreeSet::new();
    let mut found = None;
    for _ in 0..200 {
        let Some(q) = st.next_request("agent:r", &leased).unwrap() else {
            break;
        };
        leased.insert(q.key);
        if q.items.iter().any(|i| i.block == id("c-CCCC1")) {
            found = Some(q);
            break;
        }
    }
    let q = found.expect("a review request names c-CCCC1");
    let case = st.case_of(&q.key, &q.items).unwrap();
    let cb = case.blocks.iter().find(|b| b.id == "c-CCCC1").unwrap();
    assert_eq!(
        cb.governing
            .iter()
            .map(|d| d.id.clone())
            .collect::<Vec<_>>(),
        live,
        "semantic case"
    );

    // coverage: a live route governs; proposed, rejected, superseded and revoked do not; a
    // describes target that is no entity is no route
    let issues = st.coverage_issues(&w()).unwrap();
    assert!(!ungrounded(&issues, "c-CCCC1"), "c-CCCC1 is governed");
    assert!(ungrounded(&issues, "c-CCCC2"), "no live route to c-CCCC2");
    assert!(ungrounded(&issues, "c-CCCC3"), "c-CCCC4 is no entity");
    assert!(!ungrounded(&issues, "c-CCCC4"), "D-BBBB3 affects c-CCCC4");

    // search governed_by: every status, through the entities too
    let governed = |d: &str| -> Vec<String> {
        let res = st
            .search(
                &w(),
                &SearchQuery {
                    text: "rule".into(),
                    regex: false,
                    ignore_case: true,
                    filters: SearchFilters {
                        governed_by: Some(d.into()),
                        ..Default::default()
                    },
                },
                None,
                Some(100_000),
            )
            .unwrap();
        let mut v: Vec<String> = res.matches.iter().map(|m| m.id.clone()).collect();
        v.sort();
        v
    };
    for d in ["D-AAAA2", "D-AAAA3", "D-AAAA4", "D-AAAA5"] {
        assert_eq!(
            governed(d),
            ids(["c-CCCC1", "c-CCCC2", "t-TERM1"]),
            "governed_by {d}"
        );
    }
    assert_eq!(
        governed("D-AAAA1"),
        ids(["c-CCCC1", "t-TERM0"]),
        "governed_by D-AAAA1"
    );
    assert_eq!(
        governed("D-BBBB3"),
        ids(["c-CCCC4"]),
        "the describer of a non-entity is not governed"
    );
}

#[test]
fn rf6_supporting_targets_never_apply_a_decision() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut doc = block("d-D0C00", "doc", None, "B", "");
    field(&mut doc, "name", "log");
    field(&mut doc, "title", "Log");
    field(&mut doc, "doc_type", "page");
    let mut sec = block("s-SEC00", "section", Some("d-D0C00"), "B", "");
    field(&mut sec, "title", "Decisions");
    let finding = |i: &str, order: &str, body: &str| {
        let mut f = block(i, "finding", Some("s-SEC00"), order, body);
        field(&mut f, "statement", "It was found.");
        f
    };
    let source = |i: &str, order: &str| {
        let mut s = block(i, "source", Some("s-SEC00"), order, "");
        field(&mut s, "title", "Release 2");
        field(&mut s, "kind", "release");
        field(&mut s, "locator", "v2");
        s
    };
    // D-SSSS1 affects a finding; D-SSSS2 affects a source; D-SSSS3 is decided by, implemented
    // by, referenced by and depended on by supporting blocks; D-SSSS4 affects a claim (guard)
    let mut d1 = decision("D-SSSS1", "C", "accepted");
    link(&mut d1, "affects", "F-FFFF1", None);
    let mut d2 = decision("D-SSSS2", "D", "accepted");
    link(&mut d2, "affects", "S-SRC01", None);
    let d3 = decision("D-SSSS3", "E", "accepted");
    let mut d4 = decision("D-SSSS4", "F", "accepted");
    link(&mut d4, "affects", "c-CCCC1", None);
    let f1 = finding("F-FFFF1", "G", "");
    let mut f2 = finding("F-FFFF2", "H", "See {{D-SSSS3}}.");
    link(&mut f2, "decided_by", "D-SSSS3", None);
    link(&mut f2, "depends_on", "D-SSSS3", None);
    let s1 = source("S-SRC01", "J");
    let mut s2 = source("S-SRC02", "K");
    link(&mut s2, "implements", "D-SSSS3", None);
    link(&mut s2, "depends_on", "D-SSSS3", None);
    let c1 = block("c-CCCC1", "claim", Some("s-SEC00"), "M", "A claim.");
    commit(&mut r, vec![doc, sec, d1, d2, d3, d4, f1, f2, s1, s2, c1]);
    let issues = r.read().coverage_issues(&w()).unwrap();
    let unapplied = |d: &str| {
        issues.blocks[&id(d)]
            .issues
            .iter()
            .any(|i| i.code == IssueCode::UnappliedDecision)
    };
    for d in ["D-SSSS1", "D-SSSS2", "D-SSSS3"] {
        assert!(
            unapplied(d),
            "{d}: a supporting block alone applies nothing"
        );
    }
    assert!(!unapplied("D-SSSS4"), "guard: affecting a claim applies it");
}

/// An entity that implements live D-CCCC3 and describes entity t-TERME, which live D-CCCC1 and
/// partly superseded D-CCCC2 affect: its fact sheet lists all three through the four routes,
/// as context does.
#[test]
fn rf6_a_fact_sheet_reads_its_own_entity_through_the_four_routes() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut doc = block("d-D0C00", "doc", None, "B", "");
    field(&mut doc, "name", "spec");
    field(&mut doc, "title", "Spec");
    field(&mut doc, "doc_type", "page");
    let mut sec = block("s-SEC00", "section", Some("d-D0C00"), "B", "");
    field(&mut sec, "title", "Rules");
    let mut t = entity("t-TERMT", "C", "Hub");
    link(&mut t, "implements", "D-CCCC3", None);
    link(&mut t, "describes", "t-TERME", None);
    let e = entity("t-TERME", "D", "Spoke");
    let mut d1 = decision("D-CCCC1", "E", "accepted");
    link(&mut d1, "affects", "t-TERME", None);
    let mut d2 = decision("D-CCCC2", "F", "accepted");
    link(&mut d2, "affects", "t-TERME", None);
    let d3 = decision("D-CCCC3", "G", "accepted");
    let mut d4 = decision("D-CCCC4", "H", "accepted");
    link(&mut d4, "supersedes", "D-CCCC2", Some("the second half"));
    commit(&mut r, vec![doc, sec, t, e, d1, d2, d3, d4]);
    let st = r.read();
    let ctx = st.context(&w(), &id("t-TERMT")).unwrap();
    assert_eq!(ctx.governing, ids(["D-CCCC1", "D-CCCC2", "D-CCCC3"]));
    let sheet = st.fact_sheet(&w(), &id("t-TERMT"), NameKind::Name).unwrap();
    let decisions: Vec<SheetRow> = sheet
        .decisions
        .iter()
        .map(|d| (d.id.clone(), d.via.clone(), d.status.clone(), d.on.clone()))
        .collect();
    assert_eq!(
        decisions,
        vec![
            (
                "D-CCCC1".into(),
                "describes/affects".into(),
                "live".into(),
                vec![]
            ),
            (
                "D-CCCC2".into(),
                "describes/affects".into(),
                "partially_superseded".into(),
                vec![]
            ),
            ("D-CCCC3".into(), "implements".into(), "live".into(), vec![]),
        ]
    );
    // the relationship lists stay apart: t-TERMT is no fact or content of itself
    assert!(sheet.facts.is_empty() && sheet.also_described_in.is_empty());
}
