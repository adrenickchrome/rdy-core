//! relc4 (D-B2AC3, TASK-1422): the hidden planned blocks a publication check reports. The
//! metadata comes from the one planned filter, counts hidden blocks per doc, compares ids and
//! never versions, and changes no hash, digest, status or warning.
use rdy_engine::publication::{
    self, CheckStatus, HiddenList, HiddenRow, ReadPublicationSource, Resolved, disclosure_diff,
};
use rdy_engine::{Materialized, Rdy, ReadState};
use rdy_model::redaction::RedactedBlockIdentity;
use rdy_model::{Block, BlockId, FieldValue, Hash, StateSnapshot};
use rdy_schema::PackSet;
use std::collections::BTreeMap;

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn blk(id: &str, ty: &str, parent: Option<&str>, order: &str) -> Block {
    Block {
        id: bid(id),
        block_type: ty.into(),
        parent: parent.map(bid),
        order: order.into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
}

fn with(mut b: Block, k: &str, v: FieldValue) -> Block {
    b.fields.insert(k.into(), v);
    b
}

fn body(mut b: Block, text: &str) -> Block {
    b.body = text.into();
    b
}

fn planned(b: Block) -> Block {
    with(b, "planned", FieldValue::Bool(true))
}

fn since(b: Block, v: &str) -> Block {
    with(b, "since", s(v))
}

fn doc(id: &str, name: &str, order: &str) -> Block {
    with(
        with(
            with(blk(id, "doc", None, order), "name", s(name)),
            "title",
            s(&format!("Doc {name}")),
        ),
        "doc_type",
        s("page"),
    )
}

fn section(id: &str, parent: &str, order: &str) -> Block {
    with(blk(id, "section", Some(parent), order), "title", s("Sec"))
}

fn prose(id: &str, parent: &str, order: &str) -> Block {
    body(blk(id, "prose", Some(parent), order), "Text.")
}

fn note(id: &str, parent: &str, order: &str) -> Block {
    body(blk(id, "note", Some(parent), order), "Note.")
}

fn profile(key: &str, id: &str, audience: &str, select: &[&str], planned_setting: &str) -> Block {
    let mut b = blk(id, "publication_profile", Some("d-ADM1N"), "z");
    b.fields.insert("key".into(), s(key));
    b.fields.insert("title".into(), s("Profile title"));
    b.fields.insert("audience".into(), s(audience));
    b.fields.insert("planned".into(), s(planned_setting));
    b.fields.insert(
        "select".into(),
        FieldValue::List(select.iter().map(|x| s(x)).collect()),
    );
    b
}

fn synth(blocks: Vec<Block>) -> Materialized {
    Materialized::build(
        StateSnapshot {
            blocks: BTreeMap::new(),
        },
        blocks.into_iter().map(|b| (b.id.clone(), b)).collect(),
    )
}

struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
}

fn world() -> World {
    let (dir, r) = rdy_qa::fixtures::project();
    World { _dir: dir, r }
}

fn resolve(st: &ReadState, m: &Materialized, profile: &str) -> Resolved {
    let pack = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    let source = ReadPublicationSource::with_settings(st, &pack, "Project", None);
    publication::resolve_source(&source, m, profile).unwrap()
}

fn row(doc: &str, title: &str, blocks: u64, doc_hidden: bool) -> HiddenRow {
    HiddenRow {
        doc: bid(doc),
        title: title.into(),
        redacted: false,
        hidden_blocks: blocks,
        doc_hidden,
    }
}

fn shown_world() -> Vec<Block> {
    let mut v = vec![
        doc("d-ADM1N", "admin", "a"),
        // a guide with shown content and four hidden descendants
        doc("d-GDE01", "guide", "b"),
        section("s-SEC01", "d-GDE01", "a"),
        prose("p-PBC01", "s-SEC01", "a"),
        planned(note("n-PN001", "s-SEC01", "b")),
        planned(note("n-PN002", "d-GDE01", "b")),
        planned(section("s-SEC02", "d-GDE01", "c")),
        prose("p-G0N01", "s-SEC02", "a"),
        // a planned doc with two hidden descendants
        planned(doc("d-GST01", "ghost", "c")),
        prose("p-G0N02", "d-GST01", "a"),
        section("s-SEC03", "d-GST01", "b"),
        // a planned doc with nothing in it
        planned(doc("d-EMP01", "empty", "d")),
    ];
    v.push(profile(
        "pub",
        "pf-PBH01",
        "public",
        &["d-GDE01", "d-GST01", "d-EMP01"],
        "hide",
    ));
    v
}

#[test]
fn relc4_first_publication_reports_fully_hidden_docs() {
    let w = world();
    let st = w.r.read();
    let m = synth(shown_world());
    let list = resolve(&st, &m, "pub").hidden_planned.full();
    assert_eq!(
        list,
        HiddenList {
            docs: 3,
            blocks: 6,
            rows: vec![
                row("d-EMP01", "Doc empty", 0, true),
                row("d-GDE01", "Doc guide", 4, false),
                row("d-GST01", "Doc ghost", 2, true),
            ],
            omitted_rows: 0,
        }
    );
}

#[test]
fn relc4_mark_shells_and_supporting_closure_do_not_overcount() {
    let w = world();
    let st = w.r.read();
    let mut blocks = shown_world();
    blocks.push(profile(
        "mark",
        "pf-PBM01",
        "public",
        &["d-GDE01", "d-GST01", "d-EMP01"],
        "mark",
    ));
    // a planned doc kept as a shell by one shipped block; its other child is hidden
    blocks.push(planned(doc("d-SHK01", "shell", "e")));
    blocks.push(since(note("n-S1N01", "d-SHK01", "a"), "1.0.0"));
    blocks.push(prose("p-G0N03", "d-SHK01", "b"));
    // a supporting decision outside every doc, reached by a shown block's link
    let mut linked = prose("p-PBC02", "s-SEC01", "c");
    linked
        .links
        .entry("decided_by".into())
        .or_default()
        .push(rdy_model::Link {
            target: bid("D-SPR01"),
            part: None,
        });
    blocks.push(linked);
    blocks.push(with(
        with(
            blk("D-SPR01", "decision", None, "m"),
            "status",
            s("accepted"),
        ),
        "statement",
        s("Support."),
    ));
    blocks.push(profile(
        "hide",
        "pf-PBH02",
        "public",
        &["d-GDE01", "d-GST01", "d-EMP01", "d-SHK01"],
        "hide",
    ));
    let m = synth(blocks);

    let marked = resolve(&st, &m, "mark");
    assert_eq!(marked.hidden_planned.full(), HiddenList::default());
    assert_eq!(marked.hidden_planned.full().rows, vec![]);

    let hidden = resolve(&st, &m, "hide");
    assert!(hidden.shells.contains(&bid("d-SHK01")));
    assert!(
        hidden.m.get(&bid("D-SPR01")).is_some(),
        "closure stays shown"
    );
    let list = hidden.hidden_planned.full();
    // the shell counts only its hidden child; neither the shell nor the closure is hidden
    assert_eq!(list.docs, 4);
    assert_eq!(list.blocks, 7);
    assert_eq!(list.rows[3], row("d-SHK01", "Doc shell", 1, false));
}

fn newly_world(extra: Vec<Block>) -> Vec<Block> {
    let mut v = vec![
        doc("d-ADM1N", "admin", "a"),
        doc("d-GDE01", "guide", "b"),
        prose("p-PBC01", "d-GDE01", "a"),
        profile("pub", "pf-PBH01", "public", &["d-GDE01"], "hide"),
    ];
    v.extend(extra);
    v
}

#[test]
fn relc4_newly_hidden_compares_ids_not_versions() {
    let w = world();
    let st = w.r.read();
    let before = synth(newly_world(vec![
        planned(note("n-PN001", "d-GDE01", "b")),
        note("n-PN002", "d-GDE01", "c"),
    ]));
    let after = synth(newly_world(vec![
        // hidden on both sides, with a new version
        body(planned(note("n-PN001", "d-GDE01", "b")), "A new version."),
        // shown, then hidden
        planned(note("n-PN002", "d-GDE01", "c")),
        // new, and hidden
        planned(note("n-PN003", "d-GDE01", "d")),
    ]));
    let (b, a) = (resolve(&st, &before, "pub"), resolve(&st, &after, "pub"));
    let diff = disclosure_diff(&b, &a).unwrap();
    assert_eq!(
        diff.newly_hidden_planned,
        HiddenList {
            docs: 1,
            blocks: 2,
            rows: vec![row("d-GDE01", "Doc guide", 2, false)],
            omitted_rows: 0,
        }
    );
    // the full list still counts all three
    assert_eq!(a.hidden_planned.full().blocks, 3);
    // nothing changed: nothing is new
    let same = disclosure_diff(&a, &a).unwrap();
    assert_eq!(same.newly_hidden_planned, HiddenList::default());
}

#[test]
fn relc4_newly_hidden_doc_can_have_zero_new_blocks() {
    let w = world();
    let st = w.r.read();
    let base = |shipped: bool| {
        let mut v = vec![
            doc("d-ADM1N", "admin", "a"),
            planned(doc("d-GDE01", "guide", "b")),
            prose("p-G0N01", "d-GDE01", "a"),
            doc("d-TRN01", "other", "c"),
            profile("pub", "pf-PBH01", "public", &["d-GDE01", "d-TRN01"], "hide"),
        ];
        if shipped {
            v.push(since(note("n-S1N01", "d-GDE01", "b"), "1.0.0"));
        }
        v
    };
    // before: a shell with one hidden child; after: the shipped block is gone, so the doc goes
    let before = resolve(&st, &synth(base(true)), "pub");
    let after = resolve(&st, &synth(base(false)), "pub");
    assert!(before.shells.contains(&bid("d-GDE01")));
    let diff = disclosure_diff(&before, &after).unwrap();
    assert_eq!(
        diff.newly_hidden_planned,
        HiddenList {
            docs: 1,
            blocks: 0,
            rows: vec![row("d-GDE01", "Doc guide", 0, true)],
            omitted_rows: 0,
        }
    );
}

fn stub(id: &str, ty: &str, parent: Option<&str>, request: Hash) -> Block {
    Block::redacted(
        bid(id),
        ty.into(),
        parent.map(bid),
        "m".into(),
        RedactedBlockIdentity {
            version: Hash::of(format!("version of {id}").as_bytes()),
            content_hash: Hash::of(format!("content of {id}").as_bytes()),
            frozen: false,
            request,
        },
    )
}

#[test]
fn relc4_candidate_titles_respect_redaction() {
    let w = world();
    let st = w.r.read();
    let request = Hash::of(b"a removal request");
    let make = |title: &str| {
        vec![
            doc("d-ADM1N", "admin", "a"),
            with(doc("d-GDE01", "guide", "b"), "title", s(title)),
            planned(note("n-PN001", "d-GDE01", "a")),
            // a doc whose removal replaced its title
            stub("d-RMV01", "doc", None, request),
            planned(note("n-PN002", "d-RMV01", "a")),
            profile("pub", "pf-PBH01", "public", &["d-GDE01", "d-RMV01"], "hide"),
        ]
    };
    // the baseline titled the doc otherwise: the candidate's title is the row's
    let before = resolve(&st, &synth(make("Old title")), "pub");
    let after = resolve(&st, &synth(make("New title")), "pub");
    let list = after.hidden_planned.full();
    assert_eq!(list.rows.len(), 2);
    assert_eq!(
        list.rows[0],
        HiddenRow {
            doc: bid("d-GDE01"),
            title: "New title".into(),
            redacted: false,
            hidden_blocks: 1,
            doc_hidden: false,
        }
    );
    assert_eq!(
        list.rows[1],
        HiddenRow {
            doc: bid("d-RMV01"),
            title: "d-RMV01".into(),
            redacted: true,
            hidden_blocks: 1,
            doc_hidden: false,
        }
    );
    // a baseline that hides nothing new shows the same candidate titles
    let diff = disclosure_diff(&before, &after).unwrap();
    assert!(diff.newly_hidden_planned.rows.is_empty());
}

#[test]
fn relc4_hidden_metadata_changes_no_hash_digest_or_status() {
    let w = world();
    let st = w.r.read();
    let make = |title: &str, hidden_body: &str| {
        let mut v = shown_world();
        for b in &mut v {
            if b.id.as_str() == "d-GST01" {
                b.fields.insert("title".into(), s(title));
            }
            if b.id.as_str() == "n-PN001" {
                b.body = hidden_body.into();
            }
        }
        v
    };
    let a = resolve(&st, &synth(make("Doc ghost", "One.")), "pub");
    let b = resolve(&st, &synth(make("Renamed", "Two.")), "pub");
    assert_eq!(a.selection_hash, b.selection_hash);
    assert_eq!(a.selection_identity, b.selection_identity);
    assert_eq!(a.docs, b.docs);
    assert_eq!(a.entries, b.entries);
    assert_eq!(a.references.status, CheckStatus::Clear);
    assert_eq!(a.references.status, b.references.status);
    assert!(a.references.warning().is_none() && b.references.warning().is_none());
    // only the report metadata moved
    assert_ne!(a.hidden_planned.full(), b.hidden_planned.full());
    assert_eq!(b.hidden_planned.full().rows[2].title, "Renamed");

    // a real shown-to-hidden change moves the hash through the existing filter
    let shown = |planned_note: bool| {
        let n = note("n-PN001", "d-GDE01", "b");
        newly_world(vec![if planned_note { planned(n) } else { n }])
    };
    let (x, y) = (
        resolve(&st, &synth(shown(false)), "pub"),
        resolve(&st, &synth(shown(true)), "pub"),
    );
    assert_ne!(x.selection_hash, y.selection_hash);
    assert_ne!(x.docs[0].digest, y.docs[0].digest);
    assert_eq!(x.hidden_planned.full(), HiddenList::default());
    assert_eq!(y.hidden_planned.full().blocks, 1);
}

fn many(n: usize, planned_docs: bool) -> Vec<Block> {
    let ids: Vec<String> = (0..n).map(|i| format!("d-H{i:04}")).collect();
    let select: Vec<&str> = ids.iter().map(String::as_str).collect();
    let mut v = vec![doc("d-ADM1N", "admin", "a")];
    for (i, id) in ids.iter().enumerate() {
        let d = doc(id, &format!("h{i}"), "b");
        v.push(if planned_docs { planned(d) } else { d });
        let p = prose(&format!("p-H{i:04}"), id, "a");
        v.push(p);
    }
    v.push(profile("pub", "pf-PBH01", "public", &select, "hide"));
    v
}

#[test]
fn relc4_lists_keep_full_counts_and_at_most_two_hundred_rows() {
    let w = world();
    let st = w.r.read();
    for (n, rows, omitted) in [(199usize, 199usize, 0u64), (200, 200, 0), (201, 200, 1)] {
        let after = resolve(&st, &synth(many(n, true)), "pub");
        let list = after.hidden_planned.full();
        assert_eq!(list.docs, n as u64, "{n}");
        assert_eq!(list.blocks, n as u64, "{n}");
        assert_eq!(list.rows.len(), rows, "{n}");
        assert_eq!(list.omitted_rows, omitted, "{n}");
        let ids: Vec<String> = list.rows.iter().map(|r| r.doc.to_string()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "{n}: rows are in doc id order");
        assert_eq!(ids[0], "d-H0000");
        assert_eq!(ids[rows - 1], format!("d-H{:04}", rows - 1));

        // the comparison list obeys the same cut
        let before = resolve(&st, &synth(many(n, false)), "pub");
        let diff = disclosure_diff(&before, &after).unwrap();
        let new = diff.newly_hidden_planned;
        assert_eq!((new.docs, new.blocks), (n as u64, n as u64), "{n}");
        assert_eq!(new.rows.len(), rows, "{n}");
        assert_eq!(new.omitted_rows, omitted, "{n}");
    }
}
