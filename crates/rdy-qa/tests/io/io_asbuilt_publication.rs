//! ab4 (D-WJ0SJ, D-69YCN): a profile filters planned content in its own resolution only. The
//! source stays whole: the hidden block is in the canon read, its history, the source export,
//! the tag's history stream and a dump. A profiled Markdown artifact holds only the view. A
//! reopen, a reindex and a load give the same profile outcome, and no filtered copy of the
//! project is made.
use rdy_engine::load::load;
use rdy_engine::publication::{self, Resolved};
use rdy_engine::render_inputs::capture;
use rdy_engine::render_source::RenderSource;
use rdy_engine::*;
use rdy_model::{BlockId, EditorialClass, Hash, Justification};
use rdy_qa::fixtures::*;
use rdy_render::print::{self, Format};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::Path;

const HIDDEN: &str = "HIDDEN-SENTINEL-4471";
const SHOWN: &str = "SHOWN-SENTINEL-2290";

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// The text of every file under `root`, in path order.
fn all_text(root: &Path) -> String {
    let mut out = String::new();
    let mut stack = vec![root.to_path_buf()];
    let mut files = vec![];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                files.push(p);
            }
        }
    }
    files.sort();
    for f in files {
        out.push_str(&String::from_utf8_lossy(&std::fs::read(&f).unwrap()));
        out.push('\n');
    }
    out
}

/// What a profile's resolution decides: its entries with their versions, its shells, its
/// statuses and its selection hash.
type Outcome = (
    Vec<(String, Hash)>,
    BTreeSet<BlockId>,
    BTreeMap<BlockId, publication::ShownStatus>,
    Hash,
);

fn outcome(res: &Resolved) -> Outcome {
    (
        res.entries
            .iter()
            .map(|e| (e.id.to_string(), e.version))
            .collect(),
        res.shells.clone(),
        res.as_built.clone(),
        res.selection_hash,
    )
}

fn at_canon(r: &Rdy, profile: &str) -> Outcome {
    outcome(&publication::resolve(&r.read(), &Scope::Canon, profile).unwrap())
}

fn tag_hash(r: &Rdy, name: &str) -> Hash {
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

#[test]
fn ab4_dump_and_stream_keep_source_whole() {
    let (dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$rules", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dp", "parent": "$rules", "fields": {"statement": "Ship the planned part.", "status": "proposed"}},
            {"op": "create", "type": "decision", "id": "$dv", "parent": "$rules", "fields": {"statement": "Ship the built part.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$guide", "fields": {"title": "Usage"}},
            {"op": "create", "type": "note", "id": "$np", "parent": "$s", "body": format!("Planned {HIDDEN}."), "links": {"decided_by": ["$dp"]}},
            {"op": "create", "type": "prose", "id": "$pu", "parent": "$s", "body": format!("Shown {SHOWN}."), "links": {"decided_by": ["$dv"]}}
        ]),
    )
    .unwrap();
    let c = |k: &str| w.created[k].clone();
    let (np, pu) = (c("$np"), c("$pu"));
    let pf = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "id": "$pf", "parent": c("$admin"),
                "fields": {"key": "pubhide", "title": "Public", "audience": "public", "select": [c("$guide")]}}]),
    )
    .unwrap();
    let profile = pf.created["$pf"].clone();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();

    // the profile hides the planned note and keeps the shipped prose
    let read = r.read();
    let resolved = publication::resolve(&read, &Scope::Canon, "pubhide").unwrap();
    let kept: BTreeSet<String> = resolved.entries.iter().map(|e| e.id.to_string()).collect();
    assert!(!kept.contains(&np) && kept.contains(&pu), "{kept:?}");
    let before = outcome(&resolved);
    let blocks_before = read.materialized(&Scope::Canon).unwrap().len();

    // the source stays whole: the canon read, the history and the source export
    let note = read.block(&Scope::Canon, &bid(&np)).unwrap();
    assert!(note.body.contains(HIDDEN));
    assert_eq!(
        read.history(&Scope::Canon, &bid(&np), 10)
            .unwrap()
            .entries
            .len(),
        1
    );
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("export");
    read.export(&Scope::Canon, &export).unwrap();
    let exported = all_text(&export);
    assert!(exported.contains(HIDDEN) && exported.contains(SHOWN));

    // the tag's history stream holds the note; the profile it carries filters the same way
    let mut stream = vec![];
    rdy_engine::stream_export::write(&read, tag_hash(&r, "v1.0.0"), &BTreeSet::new(), &mut stream)
        .unwrap();
    let rs = RenderSource::read(Cursor::new(stream), capture(&read).unwrap()).unwrap();
    assert!(
        rs.materialized()
            .get(&bid(&np))
            .is_some_and(|b| b.body.contains(HIDDEN))
    );
    let streamed = publication::resolve_source(&rs, rs.materialized(), &profile).unwrap();
    let tag = read.resolve_scope("tag:v1.0.0").unwrap();
    assert_eq!(
        outcome(&streamed),
        outcome(&publication::resolve(&read, &tag, &profile).unwrap())
    );
    assert_eq!(outcome(&streamed), before);

    // a profiled Markdown artifact holds only the view
    let md = out.path().join("md");
    print::export_profile(&read, "canon", "pubhide", &md, Format::Md, None).unwrap();
    let artifact = all_text(&md);
    assert!(artifact.contains(SHOWN), "{artifact}");
    assert!(!artifact.contains(HIDDEN), "{artifact}");
    assert!(!artifact.contains(&np), "{artifact}");

    // a dump holds the note; the loaded project filters the same way
    let file = out.path().join("p.rdydump");
    read.dump(&file).unwrap();
    let loaded_dir = out.path().join("loaded");
    load(&file, &loaded_dir).unwrap();
    let mut loaded = Rdy::open(&loaded_dir).unwrap();
    assert!(
        loaded
            .read()
            .block(&Scope::Canon, &bid(&np))
            .unwrap()
            .body
            .contains(HIDDEN)
    );
    assert_eq!(at_canon(&loaded, "pubhide"), before);
    loaded.reindex().unwrap();
    assert_eq!(at_canon(&loaded, "pubhide"), before);

    // a reopen and a reindex give the same outcome, and the project holds every block still
    drop(read);
    drop(r);
    let mut r = Rdy::open(dir.path()).unwrap();
    assert_eq!(at_canon(&r, "pubhide"), before);
    r.reindex().unwrap();
    assert_eq!(at_canon(&r, "pubhide"), before);
    assert_eq!(
        r.read().materialized(&Scope::Canon).unwrap().len(),
        blocks_before
    );

    // a built-only landing ships the note; every path then shows it
    r.write(
        &writer(),
        WriteRequest {
            view: "ship".into(),
            ops: parse_test_ops(
                json!([{"op": "update", "id": c("$dp"), "fields": {"built": "1.1.0"}}]),
            )
            .unwrap(),
            justification: Some(Justification::Editorial {
                class: EditorialClass::Clarify,
                reason: "it shipped in 1.1.0".into(),
            }),
            message: String::new(),
        },
    )
    .unwrap();
    land(&mut r, "ship").unwrap();
    let shipped = publication::resolve(&r.read(), &Scope::Canon, "pubhide").unwrap();
    assert!(shipped.entries.iter().any(|e| e.id.as_str() == np));
    let md2 = out.path().join("md2");
    print::export_profile(&r.read(), "canon", "pubhide", &md2, Format::Md, None).unwrap();
    assert!(all_text(&md2).contains(HIDDEN));
}
