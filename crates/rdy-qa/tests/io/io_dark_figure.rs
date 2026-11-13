//! rfr2 (D-MKSFF, D-3DAX4, D-19BZD): a figure's dark image through dump, load, export, import,
//! sync and Markdown import. Both images travel together; the feature `figure_dark` travels in
//! the dump's manifest and a load keeps it; a dump whose records hold a dark image under a
//! manifest that does not name the feature is refused as before; an imported Markdown image
//! stays literal text.
use rdy_engine::{Rdy, Scope, Session, SessionKind};
use rdy_model::{Block, BlockId, FieldValue, Hash, Role};
use rdy_qa::fixtures::{approver, land, write};
use rdy_store::dump::DumpBatch;
use rdy_store::format::{self, FEATURE_FIGURE_DARK, StoreFeatures, baseline_features};
use rdy_store::ingest::Ingest;
use rdy_store::{Store, StoreError, codec, kinds};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;

const LIGHT: &[u8] = b"\x89PNG\r\n\x1a\nlight io image";
const DARK: &[u8] = b"\x89PNG\r\n\x1a\ndark io image";

/// A project with a landed paired figure: (dir, rdy, figure id, light, dark).
fn dark_project() -> (tempfile::TempDir, Rdy, BlockId, Hash, Hash) {
    let (d, mut r) = rdy_qa::golden::tiny();
    let light = r.store_mut().put_blob(LIGHT).unwrap();
    let dark = r.store_mut().put_blob(DARK).unwrap();
    let w = write(&mut r, "w", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "figs", "title": "Figures", "doc_type": "page"}},
        {"op": "create", "type": "figure", "id": "$f", "parent": "$d", "fields": {
            "blob": light.to_string(), "media_type": "image/png",
            "dark_blob": dark.to_string(), "dark_media_type": "image/svg+xml", "alt": "A loop"}}
    ]))
    .unwrap();
    land(&mut r, "w").unwrap();
    let fig = BlockId::parse(&w.created["$f"]).unwrap();
    (d, r, fig, light, dark)
}

fn features_of(r: &Rdy) -> Vec<String> {
    let v = r
        .store()
        .reader()
        .get_ref(format::REF_STORE_FEATURES)
        .unwrap()
        .unwrap();
    StoreFeatures::parse(&v)
        .unwrap()
        .features
        .into_iter()
        .map(|f| f.name)
        .collect()
}

/// Both images of `fig` read back in `r`, with both media types.
fn assert_both(r: &Rdy, fig: &BlockId, light: &Hash, dark: &Hash, what: &str) {
    let b = r.read().block(&Scope::Canon, fig).unwrap();
    assert_eq!(
        b.field_str("blob"),
        Some(light.to_string().as_str()),
        "{what}"
    );
    assert_eq!(
        b.field_str("dark_blob"),
        Some(dark.to_string().as_str()),
        "{what}"
    );
    assert_eq!(
        b.field_str("dark_media_type"),
        Some("image/svg+xml"),
        "{what}"
    );
    assert_eq!(r.store().get_blob(light).unwrap(), LIGHT, "{what}");
    assert_eq!(r.store().get_blob(dark).unwrap(), DARK, "{what}");
}

#[test]
fn rfr2_dump_load_and_import_keep_both_variants() {
    let (_d, r, fig, light, dark) = dark_project();
    let t = tempfile::tempdir().unwrap();

    // dump and load
    let dump = t.path().join("p.rdydump");
    r.read().dump(&dump).unwrap();
    let to = t.path().join("loaded");
    rdy_engine::load::load(&dump, &to).unwrap();
    let loaded = Rdy::open(&to).unwrap();
    assert_both(&loaded, &fig, &light, &dark, "load");
    drop(loaded);

    // export: both blobs in blobs/; an import into a new project keeps both
    let out = t.path().join("export");
    let s = r.read().export(&Scope::Canon, &out).unwrap();
    assert_eq!(s.blobs, 2);
    let manifest = std::fs::read_to_string(out.join("rdy-export.yaml")).unwrap();
    assert!(manifest.starts_with("format: 3\n"), "{manifest}");
    let mut names: Vec<String> = std::fs::read_dir(out.join("blobs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut want = vec![light.to_string(), dark.to_string()];
    want.sort();
    assert_eq!(names, want);
    let (_d2, mut r2) = rdy_qa::golden::tiny();
    r2.import(&approver(), "import", &out).unwrap();
    land(&mut r2, "import").unwrap();
    let imported = r2.read().materialized(&Scope::Canon).unwrap();
    let got = imported
        .iter()
        .find(|b| b.block_type == "figure")
        .unwrap()
        .clone();
    assert_eq!(got.field_str("dark_blob"), Some(dark.to_string().as_str()));
    assert_eq!(r2.store().get_blob(&dark).unwrap(), DARK);

    // an export whose dark blob is missing: the staged import refuses with the dark field's
    // name and writes nothing
    let missing = t.path().join("missing");
    copy_dir(&out, &missing);
    std::fs::remove_file(missing.join("blobs").join(dark.to_string())).unwrap();
    let (_d3, mut r3) = rdy_qa::golden::tiny();
    let end = r3.store().log_end();
    let e = r3.import(&approver(), "import", &missing).unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_REF", "{e}");
    assert!(
        e.message.contains(&format!("names dark_blob {dark}")),
        "{e}"
    );
    assert_eq!(
        r3.store().log_end(),
        end,
        "the refused import appends nothing"
    );
    assert!(r3.read().view_head("import").is_none());
    assert!(!features_of(&r3).contains(&FEATURE_FIGURE_DARK.to_string()));
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

fn dark_block() -> Block {
    let mut b = Block {
        id: BlockId::parse("fg-FGA01").unwrap(),
        block_type: "figure".into(),
        parent: Some(BlockId::parse("d-D0C11").unwrap()),
        order: "V".into(),
        fields: Default::default(),
        body: String::new(),
        links: Default::default(),
        pins: Default::default(),
        foreign: Default::default(),
        read_identity: None,
    };
    for (k, v) in [
        ("blob", Hash::of(b"l").to_string()),
        ("media_type", "image/png".into()),
        ("dark_blob", Hash::of(b"d").to_string()),
        ("dark_media_type", "image/png".into()),
        ("alt", "A loop".into()),
    ] {
        b.fields.insert(k.into(), FieldValue::Str(v));
    }
    b
}

#[test]
fn rfr2_load_and_ingest_preserve_dark_feature() {
    let (_d, r, fig, light, dark) = dark_project();
    assert!(features_of(&r).contains(&FEATURE_FIGURE_DARK.to_string()));
    let t = tempfile::tempdir().unwrap();
    let dump = t.path().join("p.rdydump");
    r.read().dump(&dump).unwrap();

    // the dump's manifest names figure_dark; the formats do not move
    let facts = rdy_qa::foreign_fixture::dump_file_facts(&dump);
    assert_eq!(facts["dump_format"], json!(rdy_store::dump::DUMP_FORMAT));
    assert_eq!(rdy_store::dump::DUMP_FORMAT, 4);
    let names: Vec<&str> = facts["store_format"]["features"]["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&FEATURE_FIGURE_DARK), "{names:?}");

    // a load keeps the manifest and the format sentinel, with no upgrade step
    let to = t.path().join("loaded");
    let summary = rdy_engine::load::load(&dump, &to).unwrap();
    assert_eq!(summary.dump_format, 4);
    let loaded = Rdy::open(&to).unwrap();
    assert_eq!(features_of(&loaded), features_of(&r));
    assert_eq!(
        std::fs::read_to_string(to.join(".rdy/store/FORMAT")).unwrap(),
        format::format_file_text()
    );
    assert!(
        !to.join(".rdy/store").read_dir().unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("upgrade")),
        "no upgrade step"
    );
    assert_both(&loaded, &fig, &light, &dark, "load");
    drop(loaded);

    // a dump-4 ingest whose manifest hides figure_dark is refused, and appends nothing: a dark
    // figure as a BLOCK, and inside a CHANGE
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("src");
    let s = Store::create(&src).unwrap();
    let mut base = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            base.push(b);
            Ok(())
        })
        .unwrap();
    drop(s);
    let change = rdy_model::Change {
        view: "work".into(),
        ops: vec![rdy_model::Op::Create {
            block: dark_block(),
        }],
        deps: vec![],
        provenance: rdy_model::Provenance {
            actor: "agent:w".into(),
            run: Some("run-1".into()),
            role: Role::Writer,
            at_ms: 1_760_000_000_000,
        },
        justification: None,
        message: "a change".into(),
        links: Default::default(),
        read_identity: None,
    };
    for (name, item) in [
        ("block", codec::item_new(kinds::BLOCK, &dark_block())),
        ("change", codec::item(kinds::CHANGE, &change).unwrap()),
    ] {
        let to = d.path().join(format!("to-{name}"));
        let mut i = Ingest::create(&to).unwrap();
        for (k, b) in base.iter().enumerate() {
            i.batch(k as u64 + 1, b).unwrap();
        }
        let hash = Hash::of(&item.1);
        let batch = DumpBatch {
            start: i.end(),
            digest: kinds::batch_digest([&hash]),
            frames: vec![rdy_store::dump::Frame::Present {
                kind: item.0,
                hash,
                payload: item.1.clone(),
            }],
        };
        i.batch(base.len() as u64 + 1, &batch).unwrap();
        let end = i.end();
        match i.upgrade(&format::DumpStore::Features(baseline_features())) {
            Err(StoreError::Dump { what, .. }) => assert_eq!(
                what,
                "the dump's records use feature figure_dark, which its manifest does not name",
                "{name}"
            ),
            other => panic!("{name}: a hidden figure_dark was kept: {other:?}"),
        }
        assert_eq!(i.end(), end, "{name}: nothing appended");
    }
}

/// D-MKSFF and rider (d) (D-3XFMP): a tag-history batch that holds a dark figure lists
/// `figure_dark` in `BatchReport.record_features`, as a check of the same bytes does; a take of
/// it into a holder keeps the feature in the take's sealed batch. A history with no dark figure
/// lists no `figure_dark`.
#[test]
fn rfr2_sync_batches_name_and_take_the_dark_feature() {
    use rdy_engine::stream_take::take;
    use rdy_store::sync_stream::check_batch;
    use rdy_store::take_batch::UploadTarget;
    let (d, mut r, _fig, _light, dark) = dark_project();
    let founder = Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        kind: SessionKind::Human,
    };
    r.tag(&founder, "v1.0.0", None, None).unwrap();
    let (_, tag, _) = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == "v1.0.0")
        .unwrap();
    let mut bytes = vec![];
    let report =
        rdy_engine::stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut bytes).unwrap();
    let names: Vec<&str> = report
        .record_features
        .features
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert!(names.contains(&FEATURE_FIGURE_DARK), "{names:?}");
    assert_eq!(check_batch(bytes.as_slice()).unwrap(), report);

    // the take: the holder turns figure_dark on with the dark figure's records
    let repo = r.read().repo_id().unwrap();
    let landing = r.store().read_tag(&tag).unwrap().landing;
    let hold = d.path().join("holder-root");
    let mut h = Rdy::create_holder(&hold, repo).unwrap();
    let p = d.path().join("b.rdysync");
    std::fs::write(&p, &bytes).unwrap();
    take(
        &mut h,
        &[std::fs::File::open(&p).unwrap()],
        UploadTarget {
            tag: "v1.0.0".into(),
            landing,
        },
    )
    .unwrap();
    assert!(features_of(&h).contains(&FEATURE_FIGURE_DARK.to_string()));
    assert_eq!(h.store().get_blob(&dark).unwrap(), DARK);

    // a history with no dark figure lists none
    let (_d2, mut q) = rdy_qa::golden::tiny();
    write(&mut q, "w", json!([
        {"op": "create", "type": "doc", "fields": {"name": "plain", "title": "Plain", "doc_type": "page"}}
    ]))
    .unwrap();
    land(&mut q, "w").unwrap();
    q.tag(&founder, "v1.0.0", None, None).unwrap();
    let (_, qtag, _) = q.store().tags().unwrap().into_iter().next().unwrap();
    let mut qbytes = vec![];
    let qreport =
        rdy_engine::stream_export::write(&q.read(), qtag, &BTreeSet::new(), &mut qbytes).unwrap();
    assert!(
        !qreport
            .record_features
            .features
            .iter()
            .any(|f| f.name == FEATURE_FIGURE_DARK)
    );
}

fn md_writer() -> Session {
    Session {
        actor: "human:imp".into(),
        run: None,
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

#[test]
fn rfr2_imported_unsupported_images_remain_literal() {
    let src = tempfile::tempdir().unwrap();
    let md = "# Guide\n\n\
Badge ![Build](https://img.example.com/badge.svg) here.\n\n\
Linked [![Docs](https://img.example.com/docs.svg)](https://docs.example.com/) here.\n\n\
Local ![Logo](logo.png) here.\n\n\
Escaped \\![Kept](https://img.example.com/kept.png) here.\n";
    std::fs::write(src.path().join("guide.md"), md).unwrap();
    let (_d, mut r) = rdy_qa::golden::tiny();
    let rows = |u: &[rdy_engine::markdown_import::Unmapped]| -> Vec<(String, usize, String)> {
        u.iter()
            .map(|x| (x.path.clone(), x.line, x.kind.as_str().to_string()))
            .collect()
    };
    let end = r.store().log_end();
    let preview = r
        .preview_markdown(&md_writer(), "md", src.path(), None)
        .unwrap();
    assert_eq!(r.store().log_end(), end, "a preview writes nothing");
    let done = r
        .import_markdown(&md_writer(), "md", src.path(), None)
        .unwrap();
    // equal dry-run and real-run rows; one image row per image, of the existing kind
    assert_eq!(rows(&preview.unmapped), rows(&done.unmapped));
    let images: Vec<usize> = done
        .unmapped
        .iter()
        .filter(|u| u.kind.as_str() == "image")
        .map(|u| u.line)
        .collect();
    assert_eq!(images, vec![3, 5, 7], "{:?}", done.unmapped);
    let kinds: BTreeSet<&str> = done.unmapped.iter().map(|u| u.kind.as_str()).collect();
    assert!(
        kinds.is_subset(&BTreeSet::from(["image", "link", "html"])),
        "{kinds:?}"
    );

    // the imported text: a backslash before each supported image opener, so the image stays
    // literal text; the unsupported form as written; no figure and no blob
    let m = r.read().materialized(&Scope::View("md".into())).unwrap();
    let bodies: Vec<String> = m
        .iter()
        .filter(|b| b.block_type == "prose")
        .map(|b| b.body.clone())
        .collect();
    let all = bodies.join("\n");
    assert!(
        all.contains("Badge \\![Build](https://img.example.com/badge.svg) here."),
        "{all}"
    );
    assert!(
        all.contains("[\\![Docs](https://img.example.com/docs.svg)](https://docs.example.com/)"),
        "{all}"
    );
    assert!(all.contains("Local ![Logo](logo.png) here."), "{all}");
    assert!(
        all.contains("Escaped \\![Kept](https://img.example.com/kept.png) here."),
        "{all}"
    );
    assert!(m.iter().all(|b| b.block_type != "figure"));
    assert!(!features_of(&r).contains(&FEATURE_FIGURE_DARK.to_string()));

    // the site shows the literal text, not an image's alt alone
    let s = rdy_render::build_site(
        &r.read(),
        &Scope::View("md".into()),
        rdy_render::Mode::Static,
    )
    .unwrap();
    let page = s
        .files
        .iter()
        .find(|(k, _)| k.ends_with("guide.html"))
        .map(|(_, v)| String::from_utf8(v.clone()).unwrap())
        .unwrap();
    // the escaped `!` shows as itself, and what follows is the link it always was: no image
    assert!(
        page.contains("Badge !<a href=\"https://img.example.com/badge.svg\""),
        "{page}"
    );
    assert!(!page.contains("<img"), "{page}");
}
