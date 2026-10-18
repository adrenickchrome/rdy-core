//! rdx6 (O20): the working-copy checkout (`wc_view` with `blobs_dir`) writes no file for a blob
//! that lost a chunk it shares with a removed blob, writes every other file, and succeeds; any
//! other blob error keeps its text. The store holds Task 2's raw authorized batch, written by the
//! shared fixture while the project is closed.
#[path = "../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_daemon::{OpContext, dispatch};
use rdy_engine::engine::Local;
use rdy_engine::{E_NOT_FOUND, Rdy, Session, WriteRequest};
use rdy_model::redaction::RedactTarget;
use rdy_model::{Hash, Role};
use rdy_store::kinds::{self, BlobManifest};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::{Value, json};
use std::path::Path;

fn human(role: Role) -> Session {
    Session::human("human:t".into(), role)
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &human(Role::Writer),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap()
}

fn cx<'a>(e: &'a Local, s: &'a Session, cwd: &'a Path) -> OpContext<'a> {
    OpContext {
        engine: e,
        session: s,
        human: "human:t",
        cwd,
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    }
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

#[test]
fn rdx6_export_and_checkout_skip_an_unreadable_blob() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let removed_bytes = vec![7u8; 1 << 20];
    let mut shared_bytes = removed_bytes.clone();
    shared_bytes.extend_from_slice(b"tail");
    let removed_blob = r.store_mut().put_blob(&removed_bytes).unwrap();
    let shared_blob = r.store_mut().put_blob(&shared_bytes).unwrap();
    let plain_blob = r.store_mut().put_blob(b"plain bytes").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "figure", "parent": "$d", "fields": {"blob": shared_blob.to_string(), "media_type": "application/octet-stream", "alt": "shared"}},
            {"op": "create", "type": "figure", "parent": "$d", "fields": {"blob": plain_blob.to_string(), "media_type": "application/octet-stream", "alt": "plain"}}
        ]),
    );
    let doc = w.created["$d"].clone();
    let p = r
        .propose(&human(Role::Writer), "work", "plan", None)
        .unwrap();
    r.land(&human(Role::Approver), &p.id).unwrap();
    r.close().unwrap();
    let fixture = authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![RedactTarget::Blob(removed_blob)],
        )],
        &FixtureOptions::approved(Proof::local_human()),
    );
    assert_eq!(
        fixture.requests[0].draft.request.manifest.shared_blobs,
        vec![shared_blob]
    );
    let e = Local::new(Rdy::open(dir.path()).unwrap());
    let s = human(Role::Writer);
    let cwd = tempfile::tempdir().unwrap();
    let c = cx(&e, &s, cwd.path());
    // checkout: the figure still names its blob; only the readable blob gets a file
    let v = dispatch(
        &c,
        "wc_view",
        json!({"view": "work", "blobs_dir": "_blobs"}),
    )
    .unwrap();
    let name_of = |h: &Hash| -> String {
        v["blobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["hash"] == json!(h.to_string()))
            .unwrap_or_else(|| panic!("{h} in {v}"))["name"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let plain = name_of(&plain_blob);
    assert!(name_of(&shared_blob).starts_with(&shared_blob.to_string()));
    assert_eq!(files_in(&cwd.path().join("_blobs")), vec![plain.clone()]);
    assert_eq!(
        std::fs::read(cwd.path().join("_blobs").join(&plain)).unwrap(),
        b"plain bytes"
    );
    // a second checkout over the same files also succeeds and writes nothing new
    dispatch(
        &c,
        "wc_view",
        json!({"view": "work", "blobs_dir": "_blobs"}),
    )
    .unwrap();
    assert_eq!(files_in(&cwd.path().join("_blobs")), vec![plain]);
    // an unrelated blob error keeps its text: a manifest whose chunk the store never held
    let missing = Hash::of(b"a chunk the store never held");
    let manifest = BlobManifest {
        len: 4,
        chunks: vec![missing],
    };
    let broken = rdy_engine::engine::write_typed(&e, move |r| {
        let h = r
            .store_mut()
            .write(vec![(
                kinds::BLOB_MANIFEST,
                redaction_fixture::enc(&manifest),
            )])
            .map_err(rdy_engine::RdyError::from)?[0];
        r.write(
            &human(Role::Writer),
            WriteRequest {
                view: "broken".into(),
                ops: serde_json::from_value(json!([{"op": "create", "type": "figure", "parent": doc, "fields": {"blob": h.to_string(), "media_type": "application/octet-stream", "alt": "broken"}}])).unwrap(),
                justification: None,
                message: String::new(),
            },
        )?;
        Ok(h)
    })
    .unwrap();
    assert_ne!(broken, json!(shared_blob.to_string()));
    let err = dispatch(
        &c,
        "wc_view",
        json!({"view": "broken", "blobs_dir": "_broken"}),
    )
    .unwrap_err();
    assert_eq!(err.code, E_NOT_FOUND);
    assert_eq!(err.message, format!("object {} not found", missing.short()));
}
