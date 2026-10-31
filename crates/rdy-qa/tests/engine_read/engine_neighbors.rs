//! nb2 (D-YXBXH): the section and entity neighbor query. A block's section neighbors are the
//! other content blocks with the same direct parent; its entity neighbors are the live content
//! blocks tied to an entity it is tied to. Only content blocks take part. The query is pure, so
//! each test builds its state directly.
use rdy_engine::Materialized;
use rdy_engine::neighbors::{Neighbor, NeighborVia, neighbors_in};
use rdy_graph::Graph;
use rdy_model::{Block, BlockId, FieldValue, Link, StateSnapshot};
use rdy_schema::{Pack, PackSet, Trait, TypeClass};
use std::collections::{BTreeMap, BTreeSet};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// rdy-standard plus three custom supporting types (review, tension, intent) and one
/// presentational type (banner). rdy-standard has none of them.
fn pack() -> Pack {
    let mut p = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    let note = p.block_types["note"].clone();
    for (name, prefix) in [("review", "rv"), ("tension", "tn"), ("intent", "in")] {
        let mut t = note.clone();
        t.prefix = prefix.into();
        t.class = Some(TypeClass::Supporting);
        p.block_types.insert(name.into(), t);
    }
    let mut banner = note;
    banner.prefix = "bn".into();
    banner.traits = BTreeSet::from([Trait::Presentational]);
    p.block_types.insert("banner".into(), banner);
    p
}

fn blk(i: &str, ty: &str, parent: Option<&str>, body: &str) -> Block {
    Block {
        id: id(i),
        block_type: ty.to_string(),
        parent: parent.map(id),
        order: "a1".to_string(),
        fields: BTreeMap::new(),
        body: body.to_string(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn doc(i: &str, name: &str) -> Block {
    field(blk(i, "doc", None, ""), "name", name)
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

fn scope(blocks: Vec<Block>) -> (Materialized, Graph) {
    let g = Graph::build_from(blocks.iter());
    let snap = StateSnapshot {
        blocks: blocks
            .iter()
            .map(|b| (b.id.clone(), b.version_hash()))
            .collect(),
    };
    let m = Materialized::build(
        snap,
        blocks.into_iter().map(|b| (b.id.clone(), b)).collect(),
    );
    (m, g)
}

fn run(blocks: Vec<Block>, covered: &[&str]) -> Vec<Neighbor> {
    let (m, g) = scope(blocks);
    neighbors_in(&m, &g, &pack(), &covered.iter().map(|s| id(s)).collect()).unwrap()
}

/// Each row as (block, via, entity, of).
fn shape(rows: &[Neighbor]) -> Vec<(String, NeighborVia, Option<String>, Vec<String>)> {
    rows.iter()
        .map(|r| (r.block.clone(), r.via, r.entity.clone(), r.of.clone()))
        .collect()
}

fn blocks_of(rows: &[Neighbor]) -> BTreeSet<String> {
    rows.iter().map(|r| r.block.clone()).collect()
}

#[test]
fn nb2_section_neighbors_are_direct_content_siblings() {
    let s = Some("s-00001");
    let blocks = vec![
        doc("d-00001", "ops"),
        blk("s-00001", "section", Some("d-00001"), ""),
        blk("p-00000", "prose", s, "The source."),
        // content siblings: in
        blk("p-00001", "prose", s, "Prose."),
        blk("c-00001", "claim", s, "Claim."),
        blk("n-00001", "note", s, "Note."),
        blk("e-00001", "example", s, "Example."),
        blk("cd-00001", "code", s, "fn main() {}"),
        field(
            blk("c-00002", "claim", s, "Deprecated claim."),
            "lifecycle",
            "deprecated",
        ),
        // structural, its descendants, presentational, defining and asset types: out
        blk("s-00002", "section", s, ""),
        blk("c-00009", "claim", Some("s-00002"), "A section descendant."),
        blk("p-00009", "prose", Some("s-00002"), "Another descendant."),
        blk("bn-00001", "banner", s, "A banner."),
        field(blk("en-00001", "entity", s, ""), "name", "Ledger"),
        field(blk("t-00001", "term", s, "A term."), "name", "Snapshot"),
        field(blk("v-00001", "value", s, ""), "name", "Timeout"),
        blk("dg-00001", "diagram", s, "graph TD; A-->B"),
        blk("fg-00001", "figure", s, ""),
        // supporting types: out
        field(
            blk("D-00001", "decision", s, ""),
            "statement",
            "Ledger first",
        ),
        field(blk("Q-00001", "question", s, ""), "prompt", "Which ledger?"),
        blk("ra-00001", "rationale", s, "Because."),
        blk("F-00001", "finding", s, "A finding."),
        blk("S-00001", "source", s, "A source."),
        blk("rv-00001", "review", s, "A review."),
        blk("tn-00001", "tension", s, "A tension."),
        blk("in-00001", "intent", s, "An intent."),
    ];
    let rows = run(blocks.clone(), &["p-00000"]);
    let want: BTreeSet<String> = [
        "c-00001", "c-00002", "cd-00001", "e-00001", "n-00001", "p-00001",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(blocks_of(&rows), want);
    assert!(rows.iter().all(|r| r.via == NeighborVia::Section
        && r.entity.is_none()
        && r.of == ["p-00000"]
        && r.doc == "d-00001"));
    let deprecated = rows.iter().find(|r| r.block == "c-00002").unwrap();
    assert_eq!(deprecated.summary, "Deprecated claim.");

    // a missing source, a source with no parent and nonparticipant sources give no rows
    for src in [
        "p-0000Z", "bn-00001", "en-00001", "s-00002", "D-00001", "Q-00001", "ra-00001", "F-00001",
        "S-00001", "rv-00001", "tn-00001", "in-00001", "dg-00001",
    ] {
        assert!(
            run(blocks.clone(), &[src]).is_empty(),
            "{src} gives no rows"
        );
    }
    let mut orphaned = blocks.clone();
    orphaned.push(blk("p-00077", "prose", None, "No parent."));
    assert!(run(orphaned, &["p-00077"]).is_empty());
    // a parent outside any doc: no containing doc, no rows
    let loose = vec![
        blk("s-00050", "section", None, ""),
        blk("p-00050", "prose", Some("s-00050"), "Loose."),
        blk("p-00051", "prose", Some("s-00050"), "Loose too."),
    ];
    assert!(run(loose, &["p-00050"]).is_empty());
}

#[test]
fn nb2_entity_neighbors_use_each_tie_and_live_content() {
    let s2 = Some("s-00002");
    let blocks = vec![
        doc("d-00001", "one"),
        blk("s-00001", "section", Some("d-00001"), ""),
        // the source: a reference to en-00001, describes t-00001, affects en-00003; implements
        // en-00004 (no tie); references a dangling entity and a claim (no defines)
        link(
            link(
                link(
                    blk(
                        "c-00000",
                        "claim",
                        Some("s-00001"),
                        "Uses {{en-00001}}, {{en-0000Z}} and {{c-00009}}.",
                    ),
                    "describes",
                    "t-00001",
                ),
                "affects",
                "en-00003",
            ),
            "implements",
            "en-00004",
        ),
        doc("d-00002", "two"),
        blk("s-00002", "section", Some("d-00002"), ""),
        field(blk("en-00001", "entity", s2, ""), "name", "Ledger"),
        field(blk("t-00001", "term", s2, "A term."), "name", "Snapshot"),
        field(blk("en-00003", "entity", s2, ""), "name", "Auth"),
        field(blk("en-00004", "entity", s2, ""), "name", "Queue"),
        blk("c-00009", "claim", s2, "A plain claim."),
        // tied by each tie kind: in
        blk("p-00001", "prose", s2, "About {{en-00001}}."),
        link(
            blk("n-00001", "note", s2, "Describes it."),
            "describes",
            "t-00001",
        ),
        link(
            blk("c-00003", "claim", s2, "Affects it."),
            "affects",
            "en-00003",
        ),
        // tied to the no-tie, dangling and nonentity targets: out
        blk("p-00004", "prose", s2, "About {{en-00004}}."),
        blk("p-00005", "prose", s2, "About {{en-0000Z}}."),
        blk("p-00006", "prose", s2, "About {{c-00009}}."),
        // unlinked text naming the entity: out
        blk("p-00007", "prose", s2, "The Ledger keeps the books."),
        // deprecated endpoints, own or inherited: out
        field(
            blk("p-00008", "prose", s2, "Old {{en-00001}}."),
            "lifecycle",
            "deprecated",
        ),
        field(
            blk("s-00003", "section", Some("d-00002"), ""),
            "lifecycle",
            "deprecated",
        ),
        blk("p-00010", "prose", Some("s-00003"), "Older {{en-00001}}."),
        // supporting and defining blocks tied to the entity: out
        blk("ra-00001", "rationale", s2, "Because {{en-00001}}."),
        link(
            field(
                blk("D-00001", "decision", s2, ""),
                "statement",
                "Ledger first",
            ),
            "affects",
            "en-00001",
        ),
        field(
            blk("t-00002", "term", s2, "See {{en-00001}}."),
            "name",
            "Book",
        ),
    ];
    let rows = run(blocks, &["c-00000"]);
    assert_eq!(
        shape(&rows),
        vec![
            (
                "c-00003".into(),
                NeighborVia::Entity,
                Some("en-00003".into()),
                vec!["c-00000".into()]
            ),
            (
                "n-00001".into(),
                NeighborVia::Entity,
                Some("t-00001".into()),
                vec!["c-00000".into()]
            ),
            (
                "p-00001".into(),
                NeighborVia::Entity,
                Some("en-00001".into()),
                vec!["c-00000".into()]
            ),
        ]
    );
    assert!(rows.iter().all(|r| r.doc == "d-00002"));
    assert_eq!(rows[2].summary, "About Ledger.");
}

/// Docs omega (the sources' doc), beta and zeta. Sources a = c-00001 and b = c-00002 share
/// section s-00001 with c-00003 and x = p-00001; w = c-00005 shares s-00002 with y = p-00002;
/// v = c-00006 is alone in beta's s-00011. a, b, x, y, v, z1 = p-00003 (zeta) and z2 = p-00004
/// (beta) reference en-00001; a, b and y also reference en-00002.
fn routes() -> Vec<Block> {
    let (s1, s2) = (Some("s-00001"), Some("s-00002"));
    vec![
        doc("d-00001", "omega"),
        blk("s-00001", "section", Some("d-00001"), ""),
        blk("s-00002", "section", Some("d-00001"), ""),
        blk("s-00009", "section", Some("d-00001"), ""),
        field(
            blk("en-00001", "entity", Some("s-00009"), ""),
            "name",
            "Ledger",
        ),
        field(
            blk("en-00002", "entity", Some("s-00009"), ""),
            "name",
            "Auth",
        ),
        blk(
            "c-00001",
            "claim",
            s1,
            "A uses {{en-00002}} and {{en-00001}}.",
        ),
        blk(
            "c-00002",
            "claim",
            s1,
            "B uses {{en-00001}} and {{en-00002}}.",
        ),
        blk("c-00003", "claim", s1, "C."),
        blk("p-00001", "prose", s1, "X uses {{en-00001}}."),
        blk("c-00005", "claim", s2, "W."),
        blk(
            "p-00002",
            "prose",
            s2,
            "Y uses {{en-00002}} and {{en-00001}}.",
        ),
        doc("d-00002", "beta"),
        blk("s-00011", "section", Some("d-00002"), ""),
        blk("s-00012", "section", Some("d-00002"), ""),
        blk("c-00006", "claim", Some("s-00011"), "V uses {{en-00001}}."),
        blk(
            "p-00004",
            "prose",
            Some("s-00012"),
            "Beta uses {{en-00001}}.",
        ),
        doc("d-00003", "zeta"),
        blk("s-00013", "section", Some("d-00003"), ""),
        blk(
            "p-00003",
            "prose",
            Some("s-00013"),
            "Zeta uses {{en-00001}}.",
        ),
    ]
}

#[test]
fn nb2_routes_merge_deterministically() {
    let rows = run(routes(), &["c-00006", "c-00005", "c-00002", "c-00001"]);
    let r = |b: &str, via: NeighborVia, e: Option<&str>, of: &[&str]| {
        (
            b.to_string(),
            via,
            e.map(str::to_string),
            of.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    };
    let (sec, ent, en1) = (NeighborVia::Section, NeighborVia::Entity, Some("en-00001"));
    let abv: &[&str] = &["c-00001", "c-00002", "c-00006"];
    assert_eq!(
        shape(&rows),
        vec![
            // same doc as a source in `of`: z2 (beta, shared with v) first by path, then omega
            r("p-00004", ent, en1, abv),
            r("c-00001", sec, None, &["c-00002"]),
            r("c-00002", sec, None, &["c-00001"]),
            r("c-00003", sec, None, &["c-00001", "c-00002"]),
            r("p-00001", sec, None, &["c-00001", "c-00002"]),
            r("p-00002", sec, None, &["c-00005"]),
            r("p-00002", ent, en1, abv),
            // no shared doc: beta, then omega, then zeta; path before id
            r("c-00006", ent, en1, &["c-00001", "c-00002"]),
            r("c-00001", ent, en1, &["c-00006"]),
            r("c-00002", ent, en1, &["c-00006"]),
            r("p-00001", ent, en1, &["c-00006"]),
            r("p-00003", ent, en1, abv),
        ]
    );
    let z1 = rows.last().unwrap();
    assert_eq!(
        (z1.doc.as_str(), z1.summary.as_str()),
        ("d-00003", "Zeta uses Ledger.")
    );
    assert_eq!(rows[0].doc, "d-00002");
    // the covered set's order does not matter, and a repeat gives the same rows
    let again = run(routes(), &["c-00001", "c-00002", "c-00005", "c-00006"]);
    assert_eq!(again, rows);
}

#[test]
fn nb2_everyday_and_declared_edges_do_not_hide_neighbors() {
    let with = |everyday: Option<bool>| {
        let mut blocks = routes();
        // a declares dependencies on its section neighbor c-00003 and its entity neighbor z1
        let a = blocks.iter_mut().find(|b| b.id == id("c-00001")).unwrap();
        a.body.push_str(" Needs {{c-00003}}.");
        *a = link(a.clone(), "implements", "p-00003");
        if let Some(flag) = everyday {
            for b in blocks.iter_mut().filter(|b| b.block_type == "entity") {
                b.fields.insert("everyday".into(), FieldValue::Bool(flag));
            }
        }
        run(blocks, &["c-00001"])
    };
    let plain = with(None);
    assert_eq!(with(Some(true)), plain);
    assert_eq!(with(Some(false)), plain);
    let c3 = plain.iter().find(|r| r.block == "c-00003").unwrap();
    assert_eq!(
        (c3.via, c3.of.as_slice()),
        (NeighborVia::Section, &["c-00001".to_string()][..])
    );
    let z1 = plain.iter().find(|r| r.block == "p-00003").unwrap();
    assert_eq!(
        (z1.via, z1.entity.as_deref()),
        (NeighborVia::Entity, Some("en-00001"))
    );
    assert_eq!(
        blocks_of(&plain),
        [
            "c-00002", "c-00003", "c-00006", "p-00001", "p-00002", "p-00003", "p-00004"
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    );
}
