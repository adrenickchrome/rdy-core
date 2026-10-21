#![cfg(unix)]
// Unix only: it runs the working-copy CLI against a served project. FW-XP1 Phase 4 ports it.
//! cp7: the engine metadata fields (`reaffirmed`, `reflects_at`) live only in the store. A
//! checkout and a refresh omit them, a staged file that sets one is refused with the store-only
//! diagnostic, and a normal edit of a file keeps the stored values (p-8YP1G, D-F0904).
use rdy_engine::wc::state::*;
use rdy_engine::*;
use rdy_model::engine_fields::{self, Against, REAFFIRMED, REFLECTS_AT, Reaffirmation, ReflectsAt};
use rdy_model::{BlockId, Change, ChangeLinks, FieldValue, Hash, Op};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::writer;
use rdy_qa::wc::*;
use std::collections::BTreeMap;

const FIELDS: [&str; 2] = [REAFFIRMED, REFLECTS_AT];

fn reaffirmed() -> FieldValue {
    engine_fields::encode_reaffirmed(&BTreeMap::from([(
        BlockId::parse("c-AAAAA").unwrap(),
        Reaffirmation {
            against: Against::Retiring(vec![BlockId::parse("D-0AAAA").unwrap()]),
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

/// Sets both metadata fields on block `target` of `view`, straight through the store, and stops
/// the daemon (the next rdy command starts it again).
fn plant(s: &Served, view: &str, target: &str) {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).expect("open");
    let scope = Scope::View(view.into());
    let id = BlockId::parse(target).unwrap();
    let mut b = r.read().block(&scope, &id).unwrap();
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
    r.close().expect("close");
}

/// The seeded project with both fields on the notes doc's prose.
fn planted() -> (Served, Seed) {
    let (s, seed) = seeded();
    plant(&s, "work", &seed.note);
    (s, seed)
}

fn stored_fields(s: &Served, id: &str) -> (Option<FieldValue>, Option<FieldValue>) {
    let b = store_block(s, "work", id);
    (
        b.fields.get(REAFFIRMED).cloned(),
        b.fields.get(REFLECTS_AT).cloned(),
    )
}

#[test]
fn cp7_checkout_writes_no_metadata_fields() {
    let (s, seed) = planted();
    assert_eq!(
        stored_fields(&s, &seed.note),
        (Some(reaffirmed()), Some(reflects_at()))
    );
    ok(&s, &["checkout"]);
    let text = read(&s, "notes.yaml");
    assert!(text.contains(&seed.note), "{text}");
    for f in FIELDS {
        assert!(!text.contains(f), "the checkout names {f}:\n{text}");
    }
    // the base copy the commit diffs against holds none either
    let place = Place::locate(s.dir.path()).unwrap();
    let base = std::fs::read_to_string(place.state.join("base/files/notes.yaml")).unwrap();
    for f in FIELDS {
        assert!(!base.contains(f), "the base copy names {f}:\n{base}");
    }
}

#[test]
fn cp7_stage_refuses_forged_engine_fields() {
    for field in FIELDS {
        let (s, seed) = planted();
        ok(&s, &["checkout"]);
        edit(&s, "notes.yaml", |l| {
            let n = l
                .iter_mut()
                .find(|e| e["id"].as_str() == Some(seed.note.as_str()))
                .unwrap();
            n.as_mapping_mut().unwrap().insert(
                "fields".into(),
                serde_yaml_ng::from_str(&format!("{field}: {{}}\n")).unwrap(),
            );
        });
        // rdy check names it
        let o = s.rdy(&["--json", "check"]);
        assert!(!o.status.success(), "{field}: check passed");
        let diags: serde_json::Value = serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|_| serde_json::from_slice(&o.stderr).unwrap());
        let hit = |d: &serde_json::Value| {
            d["code"] == "E_WC_STORE_ONLY"
                && d["file"] == "notes.yaml"
                && d["message"] == format!("{field} is set by rdy and lives only in the store")
                && d["fix"] == format!("remove {field} from the file")
        };
        assert!(
            diags.as_array().unwrap().iter().any(hit),
            "{field}: {diags}"
        );
        // wc_check.rs reuses lint_tree's STORE_ONLY_FIELDS loop (ruling task-7): the same
        // diagnostic comes from `check_wc` itself, called on the stopped project
        s.stop();
        {
            let r = Rdy::open(s.dir.path()).expect("open");
            let st = r.read();
            let mat = st.materialized(&Scope::View("work".into())).unwrap();
            let diags = check_wc(st.pack(), &mat, s.dir.path(), None).unwrap();
            assert!(
                diags.iter().any(|d| d.code == E_WC_STORE_ONLY
                    && d.file == "notes.yaml"
                    && d.message == format!("{field} is set by rdy and lives only in the store")
                    && d.fix == format!("remove {field} from the file")),
                "{field}: {diags:?}"
            );
            drop(st);
            r.close().expect("close");
        }
        // the commit is refused with E_WC_INVALID and the same diagnostic
        ok(&s, &["add", "notes.yaml"]);
        let (okk, v) = json(&s, &["commit", "-m", "forged"]);
        assert!(!okk && v["code"] == E_WC_INVALID, "{field}: {v}");
        assert!(
            v["diagnostics"].as_array().unwrap().iter().any(hit),
            "{field}: {v}"
        );
        // nothing was recorded: the stored values are the planted ones
        assert_eq!(
            stored_fields(&s, &seed.note),
            (Some(reaffirmed()), Some(reflects_at())),
            "{field}"
        );
    }
}

#[test]
fn cp7_normal_file_edit_preserves_hidden_metadata() {
    let (s, seed) = planted();
    ok(&s, &["checkout"]);
    set_body(&s, "notes.yaml", &seed.note, "Edited.");
    ok(&s, &["add", "notes.yaml"]);
    ok(&s, &["commit", "-m", "edit the note"]);
    let b = store_block(&s, "work", &seed.note);
    assert_eq!(b.body, "Edited.");
    assert_eq!(b.fields.get(REAFFIRMED), Some(&reaffirmed()));
    assert_eq!(b.fields.get(REFLECTS_AT), Some(&reflects_at()));

    // a change made elsewhere comes in by a refresh, which writes no metadata into the files
    store_write(
        &s,
        "work",
        serde_json::json!([{"op": "update", "id": seed.prose, "body": "Theirs."}]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    let text = read(&s, "notes.yaml");
    assert!(text.contains("Edited."), "{text}");
    for f in FIELDS {
        assert!(!text.contains(f), "the refresh wrote {f}:\n{text}");
    }
    // and a second edit after the refresh keeps both maps again
    set_body(&s, "notes.yaml", &seed.note, "Edited twice.");
    ok(&s, &["add", "notes.yaml"]);
    ok(&s, &["commit", "-m", "edit the note again"]);
    let b = store_block(&s, "work", &seed.note);
    assert_eq!(b.body, "Edited twice.");
    assert_eq!(b.fields.get(REAFFIRMED), Some(&reaffirmed()));
    assert_eq!(b.fields.get(REFLECTS_AT), Some(&reflects_at()));
}
