//! pub5 (D-KDFZ3, D-69AY3, D-F0904): a publication profile is an ordinary versioned block. It
//! edits, lands, is pinned by a tag's snapshot, travels through dump, load, clone and reindex,
//! and stays out of a working copy, with no special case in any of them.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::clone::{CloneSource, clone};
use rdy_engine::engine::{Local, write_typed};
use rdy_engine::load::load;
use rdy_engine::{Rdy, Scope};
use rdy_model::{Block, BlockId, Hash, LandingRecord, StateSnapshot};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{approver, human_writer, land, write};
use rdy_qa::wc;
use rdy_store::kinds;
use serde_json::{Value, json};
use std::path::Path;

/// A project with a profile that was created, landed and tagged `v1.0.0`, then edited through
/// the YAML edit operation, landed and tagged `v1.1.0`.
struct Fixture {
    dir: tempfile::TempDir,
    profile: String,
    other: String,
    folder: String,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let e = Local::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let s = human_writer();
    let cx = OpContext {
        engine: &e,
        session: &s,
        human: "human:t",
        cwd: dir.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };
    let seeded = write_typed(&e, |r| {
        write(
            r,
            "work",
            json!([
                {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
                {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "Other", "doc_type": "page"}},
                {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "notes"}}
            ]),
        )
    })
    .unwrap();
    let id = |k: &str| seeded["created"][k].as_str().unwrap().to_string();
    let (other, folder) = (id("$o"), id("$f"));
    let v = dispatch(
        &cx,
        "new_profile",
        json!({"key": "site", "parent": id("$g"), "title": "Site", "audience": "public",
               "select": [other, folder]}),
    )
    .unwrap();
    let profile = v["created"]["$profile"].as_str().unwrap().to_string();
    let land_and_tag = |name: &'static str| {
        write_typed(&e, move |r| {
            land(r, "work")?;
            r.tag(&approver(), name, None, None)
        })
        .unwrap();
    };
    land_and_tag("v1.0.0");
    // the YAML edit: the same text `rdy edit` hands the editor, with the title changed
    let b = dispatch(&cx, "block", json!({"id": profile})).unwrap();
    let yaml = b["yaml"].as_str().unwrap();
    assert!(yaml.contains("title: Site"), "{yaml}");
    dispatch(
        &cx,
        "edit_block",
        json!({"yaml": yaml.replace("title: Site", "title: Site, second edition"),
               "base": b["version"]}),
    )
    .unwrap();
    land_and_tag("v1.1.0");
    e.close().unwrap();
    Fixture {
        dir,
        profile,
        other,
        folder,
    }
}

/// Everything a project says about the profile, from its store alone: each version's tag and
/// landing, the profile's id and version in canon and in each tag's snapshot, the block read
/// from that snapshot, and canon's selection.
fn observed(project: &Path, profile: &str) -> Value {
    let r = Rdy::open(project).unwrap();
    let id = BlockId::parse(profile).unwrap();
    let st = r.read();
    let canon = st.block(&Scope::Canon, &id).unwrap();
    let mut versions = vec![];
    for v in st.versions().unwrap() {
        let landing: Hash = v.landing.parse().unwrap();
        let rec: LandingRecord = st.reader().get_typed(kinds::LANDING, &landing).unwrap();
        let snap: StateSnapshot = st.reader().get_typed(kinds::SNAPSHOT, &rec.state).unwrap();
        let version = snap.blocks[&id];
        let raw = st.reader().get_block(&version).unwrap();
        versions.push(json!({
            "name": v.name, "tag": v.tag, "landing": v.landing, "state": rec.state.to_string(),
            "profile_version": version.to_string(),
            "profile": block_json(&raw),
        }));
    }
    let out = json!({
        "id": canon.id.to_string(),
        "version": canon.version_hash().to_string(),
        "select": block_json(&canon)["fields"]["select"],
        "versions": versions,
    });
    drop(st);
    r.close().unwrap();
    out
}

fn block_json(b: &Block) -> Value {
    serde_json::to_value(b).unwrap()
}

#[test]
fn pub5_edit_and_tag_pin_the_profile_revision() {
    let f = fixture();
    let o = observed(f.dir.path(), &f.profile);
    let versions = o["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2, "{o}");
    assert_eq!(versions[0]["name"], "v1.0.0");
    assert_eq!(versions[1]["name"], "v1.1.0");
    // each tag pins the revision that was landed when it was made
    let (first, second) = (&versions[0], &versions[1]);
    assert_ne!(first["profile_version"], second["profile_version"]);
    assert_eq!(first["profile"]["fields"]["title"], json!({"Str": "Site"}));
    assert_eq!(
        second["profile"]["fields"]["title"],
        json!({"Str": "Site, second edition"})
    );
    // the newer tag names canon's version of the block, the older one an earlier version
    assert_eq!(second["profile_version"], o["version"]);
    // the raw snapshot block carries the selection exactly as it was written
    for v in versions {
        assert_eq!(v["profile"]["block_type"], "publication_profile");
        assert_eq!(
            v["profile"]["fields"]["select"],
            json!({"List": [{"Str": f.other}, {"Str": f.folder}]}),
            "{v}"
        );
        assert_eq!(v["profile"]["fields"]["audience"], json!({"Str": "public"}));
    }
    assert_eq!(o["id"], f.profile.as_str());
}

#[test]
fn pub5_dump_load_clone_and_reindex_preserve_profile_hashes() {
    let f = fixture();
    let want = observed(f.dir.path(), &f.profile);
    assert_eq!(want["versions"].as_array().unwrap().len(), 2);
    let scratch = tempfile::tempdir().unwrap();

    // dump, then load
    let file = scratch.path().join("p.rdydump");
    let r = Rdy::open(f.dir.path()).unwrap();
    r.read().dump(&file).unwrap();
    // clone, from the open source
    let cloned = scratch.path().join("clone");
    clone(CloneSource::State(&r.read()), &cloned, false).unwrap();
    r.close().unwrap();
    let loaded = scratch.path().join("loaded");
    let summary = load(&file, &loaded).unwrap();
    assert_eq!(summary.tags, 2, "{summary:?}");
    assert_eq!(observed(&loaded, &f.profile), want, "load");
    assert_eq!(observed(&cloned, &f.profile), want, "clone");

    // reindex: the index rebuilds from the log, and the same facts read back
    let mut r = Rdy::open(f.dir.path()).unwrap();
    r.reindex().unwrap();
    r.close().unwrap();
    assert_eq!(observed(f.dir.path(), &f.profile), want, "reindex");
    // and so does a reindex of the loaded copy
    let mut r = Rdy::open(&loaded).unwrap();
    r.reindex().unwrap();
    r.close().unwrap();
    assert_eq!(observed(&loaded, &f.profile), want, "reindex of the load");
}

/// Plans a commit of the staged files, as `wc_plan` does.
fn plan(s: &Served) -> Result<rdy_engine::wc::plan::CommitPlan, rdy_engine::wc::plan::WcInvalid> {
    let place = rdy_engine::wc::state::Place::locate(s.dir.path()).unwrap();
    let st = rdy_engine::wc::state::WcState::load(&place).unwrap();
    let pack = rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap();
    rdy_engine::wc::plan::plan_commit(&place, &st, &pack)
}

/// Every file of the project the working copy writes: the tree outside `.rdy`, and the working
/// copy's own state under `.rdy/wc`.
fn working_copy_files(root: &Path) -> Vec<(String, String)> {
    let mut out = vec![];
    let mut todo = vec![root.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if rel == ".rdy/store" || rel == ".rdy/run" {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                todo.push(p);
            } else if let Ok(text) = std::fs::read_to_string(&p) {
                out.push((rel, text));
            }
        }
    }
    out
}

#[test]
fn pub5_profile_stays_out_of_working_copy_content() {
    let (s, seed) = wc::seeded();
    let v = wc::ok(
        &s,
        &[
            "new",
            "profile",
            "site-wc",
            "--parent",
            &seed.intro,
            "--title",
            "Site",
            "--audience",
            "org",
            "--select",
            &seed.notes,
        ],
    );
    let id = v
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("created publication profile site-wc ("))
        .and_then(|l| l.strip_suffix(')'))
        .unwrap_or_else(|| panic!("{v:?}"))
        .to_string();
    wc::ok(&s, &["checkout", "--force"]);
    // no file the working copy writes names the profile: its id, key or type
    let files = working_copy_files(s.dir.path());
    assert!(
        files.iter().any(|(rel, _)| rel == "guides/intro.yaml"),
        "{files:?}"
    );
    for (rel, text) in &files {
        for needle in [id.as_str(), "site-wc", "publication_profile"] {
            assert!(!text.contains(needle), "{rel} holds {needle}: {text}");
        }
    }

    // a file that tries to create one is refused as every supporting block is
    wc::edit(&s, "guides/intro.yaml", |l| {
        l.push(
            serde_yaml_ng::from_str(&format!(
                "type: publication_profile\nparent: {}\nfields:\n  key: forged\n  title: Forged\n  audience: org\n  select: [{}]\n",
                seed.intro, seed.notes
            ))
            .unwrap(),
        );
    });
    let place = rdy_engine::wc::state::Place::locate(s.dir.path()).unwrap();
    let bytes = std::fs::read(wc::at(&s, "guides/intro.yaml")).unwrap();
    rdy_engine::wc::state::stage_put(&place, "guides/intro.yaml", &bytes).unwrap();
    let e = plan(&s).unwrap_err();
    assert!(
        e.diags.iter().any(|d| d.code == "E_WC_SUPPORTING"
            && d.file == "guides/intro.yaml"
            && d.message.contains("publication_profile")),
        "{:?}",
        e.diags
    );
}
