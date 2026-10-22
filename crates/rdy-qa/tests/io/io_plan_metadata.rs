//! cp7: the engine metadata fields (`reaffirmed`, `reflects_at`) live only in the store. Export
//! and the editing buffers omit them, import and edit refuse a file that sets one, a normal edit
//! keeps them, and dump/load carries the stored bytes (p-8YP1G, D-F0904).
use rdy_engine::{Local, Rdy, Scope, block_to_yaml, load::load};
use rdy_mcp::tools::*;
use rdy_model::engine_fields::{self, Against, REAFFIRMED, REFLECTS_AT, Reaffirmation, ReflectsAt};
use rdy_model::{Block, BlockId, Change, ChangeLinks, FieldValue, Hash, Op};
use rdy_qa::fixtures::*;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn reaffirmed() -> FieldValue {
    engine_fields::encode_reaffirmed(&BTreeMap::from([(
        id("c-AAAAA"),
        Reaffirmation {
            against: Against::Retiring(vec![id("D-0AAAA")]),
            reason: "Still true.".into(),
        },
    )]))
}

fn reflects_at() -> FieldValue {
    engine_fields::encode_reflects_at(&ReflectsAt {
        files: Hash::of(b"files"),
        commits: BTreeMap::from([("core".to_string(), Some("a".repeat(40)))]),
    })
}

/// Sets both metadata fields on block `target` of `view`, straight through the store: the way
/// rdy's own writes set them, which no file or agent input can.
fn plant(r: &mut Rdy, view: &str, target: &str) {
    let scope = Scope::View(view.into());
    let mut b = r.read().block(&scope, &id(target)).unwrap();
    let base = b.version_hash();
    b.fields.insert(REAFFIRMED.into(), reaffirmed());
    b.fields.insert(REFLECTS_AT.into(), reflects_at());
    r.store_mut()
        .commit_to_view(
            view,
            Change {
                view: view.into(),
                ops: vec![Op::Update { base, block: b }],
                deps: vec![],
                provenance: writer().provenance(),
                justification: None,
                message: "plant".into(),
                links: ChangeLinks::default(),
                read_identity: None,
            },
        )
        .expect("plant the metadata");
    r.invalidate();
}

/// A project whose canon holds a doc, a section and a claim; the claim carries both fields.
fn planted() -> (tempfile::TempDir, Rdy, String, String, String) {
    let (dir, mut r) = project();
    let (d, s, c) = seed_doc(&mut r, "work");
    plant(&mut r, "work", &c);
    land(&mut r, "work").unwrap();
    (dir, r, d, s, c)
}

fn stored(r: &Rdy, c: &str) -> Block {
    r.read().block(&Scope::Canon, &id(c)).unwrap()
}

/// Every file under `dir`, as text.
fn texts(dir: &Path) -> Vec<(String, String)> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(texts(&p));
        } else if let Ok(t) = std::fs::read_to_string(&p) {
            out.push((p.display().to_string(), t));
        }
    }
    out
}

fn read_format(r: Rdy, root: &str, format: &str) -> String {
    let mut st = McpState::new(std::sync::Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer", "view": "work"}))
            .unwrap(),
    )
    .unwrap();
    let read = rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": root, "format": format, "max_tokens": 50000})).unwrap(),
    )
    .unwrap();
    read["text"].as_str().unwrap().to_string()
}

#[test]
fn cp7_checkout_and_export_omit_both_metadata_fields() {
    let (dir, r, d, _s, c) = planted();
    // the store holds both
    let b = stored(&r, &c);
    assert!(b.fields.contains_key(REAFFIRMED) && b.fields.contains_key(REFLECTS_AT));

    // export: no file names either field, and the claim's content is there
    let out = tempfile::tempdir().unwrap();
    r.read().export(&Scope::Canon, out.path()).unwrap();
    let files = texts(out.path());
    assert!(
        files.iter().any(|(_, t)| t.contains(&c)),
        "the claim is exported: {files:?}"
    );
    for (path, text) in &files {
        for f in [REAFFIRMED, REFLECTS_AT] {
            assert!(!text.contains(f), "{path} names {f}:\n{text}");
        }
    }

    // the editing buffer is the same projection
    let yaml = block_to_yaml(&b);
    assert!(yaml.contains("body: one"), "{yaml}");
    for f in [REAFFIRMED, REFLECTS_AT] {
        assert!(!yaml.contains(f), "{yaml}");
    }

    // rdy_read, as YAML and as Markdown
    r.close().unwrap();
    for format in ["yaml", "markdown"] {
        let r = Rdy::open(dir.path()).unwrap();
        let text = read_format(r, &d, format);
        assert!(text.contains(&c), "{format}: {text}");
        for f in [REAFFIRMED, REFLECTS_AT] {
            assert!(!text.contains(f), "{format} names {f}:\n{text}");
        }
    }
}

/// An import tree holding a doc, a section and a claim whose `fields` are `claim_fields`.
fn tree(claim_fields: &str) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    std::fs::create_dir_all(t.path().join("docs")).unwrap();
    std::fs::write(
        t.path().join("docs/spec.yaml"),
        format!(
            "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: Spec\n    doc_type: overview\n- id: s-AAAAA\n  type: section\n  parent: d-AAAAA\n  order: a1\n  fields:\n    title: Rules\n- id: c-AAAAA\n  type: claim\n  parent: s-AAAAA\n  order: a1\n  body: It holds.\n  fields:\n{claim_fields}"
        ),
    )
    .unwrap();
    t
}

#[test]
fn cp7_import_refuses_metadata_before_writing() {
    let cases = [
        // a well-formed value
        (
            REAFFIRMED,
            "    reaffirmed:\n      c-AAAAA:\n        against: []\n        reason: Kept.\n",
        ),
        (
            REFLECTS_AT,
            "    reflects_at:\n      commits: {}\n      files: \"0000000000000000000000000000000000000000000000000000000000000000\"\n",
        ),
        // the raw input is checked before any shape check: a bad value is refused the same way
        (REAFFIRMED, "    reaffirmed: 5\n"),
        (REFLECTS_AT, "    reflects_at: nope\n"),
        // a value that fails the field conversion (a float; a mapping with a non-string key)
        // gets the same refusal, not a file-format error
        (REAFFIRMED, "    reaffirmed: 1.5\n"),
        (REFLECTS_AT, "    reflects_at: 2.5\n"),
        (REAFFIRMED, "    reaffirmed:\n      1: x\n"),
        (REFLECTS_AT, "    reflects_at:\n      ? [a, b]\n      : x\n"),
    ];
    for (field, yaml) in cases {
        let (_d, mut r) = project();
        let log = r.store_mut().log_end();
        let refs = r.store_mut().refs_with_prefix("").unwrap();
        let e = r
            .import(&approver(), "work", tree(yaml).path())
            .unwrap_err();
        assert_eq!(e.code, "E_VALIDATION", "{e}");
        let issue = e
            .issues
            .iter()
            .find(|i| i.code == "E_FIELD_UNKNOWN")
            .unwrap_or_else(|| panic!("no E_FIELD_UNKNOWN issue: {:?}", e.issues));
        assert_eq!(
            issue.message,
            format!("c-AAAAA sets \"{field}\", and rdy sets it")
        );
        assert_eq!(
            issue.fix,
            format!("remove {field}; use reaffirm or repin with code: true")
        );
        // nothing was written
        assert_eq!(r.store_mut().log_end(), log, "{field}: the log grew");
        assert_eq!(
            r.store_mut().refs_with_prefix("").unwrap(),
            refs,
            "{field}: a ref moved"
        );
    }
    // the same tree without the field imports
    let (_d, mut r) = project();
    r.import(&approver(), "work", tree("    key: CLAIM-1\n").path())
        .unwrap();
}

#[test]
fn cp7_edit_buffer_preserves_hidden_metadata() {
    let (_dir, mut r, _d, _s, c) = planted();
    let before = stored(&r, &c);
    // the buffer a person gets has no metadata; they change the body only
    let buffer = block_to_yaml(&before).replace("body: one", "body: two");
    assert!(buffer.contains("body: two"), "{buffer}");
    r.edit_yaml(&writer(), "work", &buffer, None, None).unwrap();
    land(&mut r, "work").unwrap();
    let after = stored(&r, &c);
    assert_eq!(after.body, "two");
    assert_eq!(after.fields.get(REAFFIRMED), before.fields.get(REAFFIRMED));
    assert_eq!(
        after.fields.get(REFLECTS_AT),
        before.fields.get(REFLECTS_AT)
    );
    assert!(after.fields.contains_key(REAFFIRMED) && after.fields.contains_key(REFLECTS_AT));

    // a buffer that changes nothing is still "nothing changed", not an unset of the metadata
    let again = block_to_yaml(&after);
    let e = r
        .edit_yaml(&writer(), "work", &again, None, None)
        .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.message.starts_with("nothing changed"), "{e}");

    // the same for a value that fails the field conversion: the raw buffer is checked first
    for (field, value) in [
        (REAFFIRMED, "1.5"),
        (REFLECTS_AT, "2.5"),
        (REAFFIRMED, "{1: x}"),
        (REFLECTS_AT, "{? [a, b] : x}"),
    ] {
        let mut odd = block_to_yaml(&stored(&r, &c));
        odd.push_str(&format!("fields:\n  {field}: {value}\n"));
        let e = r
            .edit_yaml(&writer(), "work", &odd, None, None)
            .unwrap_err();
        assert_eq!(e.code, "E_VALIDATION", "{field}: {value}: {e}");
        assert!(
            e.issues.iter().any(|i| i.code == "E_FIELD_UNKNOWN"
                && i.message == format!("{c} sets \"{field}\", and rdy sets it")),
            "{field}: {value}: {:?}",
            e.issues
        );
    }

    // a buffer that sets a metadata field is refused: rdy sets it
    // (the claim has no other field, so its buffer has no `fields` key to extend)
    let mut forged = block_to_yaml(&stored(&r, &c));
    forged.push_str(&format!(
        "fields:\n  reflects_at:\n    commits: {{}}\n    files: \"{}\"\n",
        Hash::of(b"x")
    ));
    let e = r
        .edit_yaml(&writer(), "work", &forged, None, None)
        .unwrap_err();
    assert_eq!(e.code, "E_VALIDATION", "{e}");
    assert!(
        e.issues
            .iter()
            .any(|i| i.code == "E_FIELD_UNKNOWN" && i.message.contains("\"reflects_at\"")),
        "{:?}",
        e.issues
    );
    assert_eq!(
        stored(&r, &c).fields.get(REFLECTS_AT),
        before.fields.get(REFLECTS_AT)
    );
}

#[test]
fn cp7_dump_keeps_metadata_byte_for_byte() {
    let (dir, r, _d, _s, c) = planted();
    let before = stored(&r, &c);
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("p.rdydump");
    r.read().dump(&file).unwrap();
    // the dump holds the field names where the export does not
    let bytes = std::fs::read(&file).unwrap();
    for f in [REAFFIRMED, REFLECTS_AT] {
        assert!(
            bytes.windows(f.len()).any(|w| w == f.as_bytes()),
            "the dump names {f}"
        );
    }
    r.close().unwrap();
    let _keep = dir;

    let target = scratch.path().join("loaded");
    std::fs::create_dir(&target).unwrap();
    load(&file, &target).unwrap();
    let loaded = Rdy::open(&target).unwrap();
    let after = stored(&loaded, &c);
    assert_eq!(after, before, "the loaded block is the stored block");
    assert_eq!(after.fields.get(REAFFIRMED), Some(&reaffirmed()));
    assert_eq!(after.fields.get(REFLECTS_AT), Some(&reflects_at()));
    assert_eq!(
        after.version_hash(),
        before.version_hash(),
        "the same version"
    );
    // and the editable output of the loaded project omits them again
    let out = scratch.path().join("export");
    loaded.read().export(&Scope::Canon, &out).unwrap();
    for (path, text) in texts(&out) {
        for f in [REAFFIRMED, REFLECTS_AT] {
            assert!(!text.contains(f), "{path} names {f}");
        }
    }
}
