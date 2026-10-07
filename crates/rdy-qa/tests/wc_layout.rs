//! wcly1: which files belong to the working tree (spec §12.1, D-7BKK5), what is untracked, and the
//! working-copy reader, where new entries need no id, order or parent.
use rdy_engine::wc::ignore::Ignore;
use rdy_engine::wc::layout::*;
use std::path::Path;

fn pack() -> rdy_schema::Pack {
    rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap()
}

fn put(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const DOC: &str = "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: Intro\n    doc_type: page\n- id: p-AAAAA\n  type: prose\n  parent: d-AAAAA\n  order: a1\n  body: Hi.\n";
const FOLDER: &str = "id: fd-AAAAA\ntype: folder\norder: a1\nfields:\n  title: Guides\n";

#[test]
fn wcly1_members_are_folders_docs_and_blobs_and_the_rest_is_skipped() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    put(r, "guides/_folder.yaml", FOLDER);
    put(r, "guides/intro.yaml", DOC);
    put(r, "README.md", "# hi\n");
    put(r, "src/code.yaml", "- not: a doc\n");
    put(r, ".git/config.yaml", "x: 1\n");
    put(r, "guides/intro.theirs.yaml", DOC);
    put(r, "_blobs/0123.png", "png");
    put(r, "scratch.yaml", "just: a map\n");
    put(r, "broken/_folder.yaml", "id: [\n");
    put(r, "broken/inner.yaml", DOC);
    let w = read_tree(r, &Ignore::empty(), &pack());
    let files: Vec<&str> = w.files.keys().map(String::as_str).collect();
    assert_eq!(files, ["guides/_folder.yaml", "guides/intro.yaml"]);
    assert_eq!(w.files["guides/intro.yaml"].kind, FileKind::Doc);
    assert_eq!(w.files["guides/intro.yaml"].head.to_string(), "d-AAAAA");
    assert_eq!(w.files["guides/_folder.yaml"].kind, FileKind::Folder);
    assert_eq!(w.theirs, ["guides/intro.theirs.yaml"]);
    assert_eq!(w.blobs, ["_blobs/0123.png"]);
    let untracked: Vec<&str> = w.untracked.iter().map(|u| u.path.as_str()).collect();
    assert_eq!(untracked, ["broken/_folder.yaml", "scratch.yaml"]);
    let doc = w
        .blocks
        .iter()
        .find(|b| b.block.id.to_string() == "d-AAAAA")
        .unwrap();
    assert_eq!(
        doc.block.parent.as_ref().map(|p| p.to_string()),
        Some("fd-AAAAA".into())
    );
    assert_eq!(doc.block.field_str("name"), Some("intro"));
    assert!(w.diags.is_empty(), "{:?}", w.diags);
}

#[test]
fn wcly1_ignored_paths_are_skipped() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    put(r, "notes.yaml", DOC);
    put(r, "scratch.yaml", "just: a map\n");
    let w = read_tree(
        r,
        &Ignore::parse("scratch.yaml\nnotes.yaml\n").unwrap(),
        &pack(),
    );
    assert!(
        w.files.is_empty() && w.untracked.is_empty(),
        "{:?} {:?}",
        w.files.keys(),
        w.untracked.len()
    );
}

#[test]
fn wcly1_new_entries_need_no_id_order_or_parent() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    put(
        r,
        "intro.yaml",
        &format!(
            "{DOC}- type: prose\n  body: New.\n- type: claim\n  parent: d-AAAAA\n  body: Also new.\n"
        ),
    );
    put(
        r,
        "fresh.yaml",
        "- type: doc\n  fields:\n    title: Fresh\n    doc_type: page\n- type: prose\n  body: Body.\n",
    );
    let p = pack();
    let w = read_tree(r, &Ignore::empty(), &p);
    assert!(w.diags.is_empty(), "{:?}", w.diags);
    let new: Vec<&WcBlock> = w.blocks.iter().filter(|b| b.new).collect();
    assert_eq!(new.len(), 4, "two in intro, the fresh doc and its prose");
    for b in &new {
        assert!(!b.order_given);
        assert_eq!(
            b.block.id.prefix(),
            p.block_types[&b.block.block_type].prefix,
            "client ids use the type's prefix"
        );
    }
    let added = new.iter().find(|b| b.block.body == "New.").unwrap();
    assert_eq!(
        added.block.parent.as_ref().map(|x| x.to_string()),
        Some("d-AAAAA".into()),
        "a new entry's parent is the doc"
    );
    assert_eq!(added.seq, 2);
    let fresh = &w.files["fresh.yaml"];
    assert!(fresh.new_head);
    let body = new.iter().find(|b| b.block.body == "Body.").unwrap();
    assert_eq!(body.block.parent.as_ref(), Some(&fresh.head));
}

#[test]
fn wcly1_hostile_entries_are_untracked_never_followed() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    put(r, "ok.yaml", DOC);
    std::os::unix::fs::symlink("/", r.join("root.yaml")).unwrap();
    std::os::unix::fs::symlink("/", r.join("rootdir")).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(r.join("pipe.yaml"))
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(r.join("huge.yaml"), vec![b'#'; MAX_MEMBER_BYTES + 1]).unwrap();
    let mut deep = r.to_path_buf();
    for i in 0..(rdy_model::tree::MAX_FOLDER_DEPTH + 2) {
        deep = deep.join(format!("d{i}"));
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(
            deep.join("_folder.yaml"),
            FOLDER.replace("fd-AAAAA", &format!("fd-{i:05}")),
        )
        .unwrap();
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        std::fs::write(r.join(std::ffi::OsStr::from_bytes(b"bad\xff.yaml")), DOC).unwrap();
    }
    let w = read_tree(r, &Ignore::empty(), &pack());
    assert!(w.files.contains_key("ok.yaml"));
    let reasons: std::collections::BTreeMap<&str, &str> = w
        .untracked
        .iter()
        .map(|u| (u.path.as_str(), u.reason.as_str()))
        .collect();
    assert!(
        reasons["root.yaml"].contains("symbolic link"),
        "{reasons:?}"
    );
    assert!(reasons["rootdir"].contains("symbolic link"), "{reasons:?}");
    assert!(
        reasons["pipe.yaml"].contains("not a regular file"),
        "{reasons:?}"
    );
    assert!(reasons["huge.yaml"].contains("over 16 MiB"), "{reasons:?}");
    assert!(
        reasons.values().any(|v| v.contains("too deep")),
        "{reasons:?}"
    );
    #[cfg(target_os = "linux")]
    assert!(
        reasons
            .iter()
            .any(|(k, v)| k.starts_with("bad") && v.contains("UTF-8")),
        "{reasons:?}"
    );
}
