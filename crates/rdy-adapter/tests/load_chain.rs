//! hc7 through the adapter (D-2FMRR, D-QF2HQ, D-0VG72): `maintenance::load_chain` is the
//! engine's chain load. It opens no store before it loads, and a host reads the same summary,
//! stop and carried warnings as core gives them, for a project and for a holder.
use rdy_adapter::maintenance::{self, DumpStop, LoadSummary};
use rdy_adapter::{CarriedWarning, RepoId, StoreHandle};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest, stream_export, stream_take};
use rdy_model::{Hash, Role};
use rdy_store::segment::Location;
use rdy_store::take_batch::UploadTarget;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const SECRET: &str = "The launch code is ZEBRA-7731.";

fn session(role: Role) -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn add(r: &mut Rdy, n: usize, body: &str) -> String {
    let w = r
        .write(
            &session(Role::Writer),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("d{n}"), "title": format!("D{n}"), "doc_type": "page"}},
                    {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": body}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    w.created["$p"].clone()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(&session(Role::Writer), "work", "Work", None)
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
}

fn tag(r: &mut Rdy, name: &str) -> (Hash, Hash) {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
    let (_, hash, _) = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap();
    (hash, r.store().read_tag(&hash).unwrap().landing)
}

fn remove(r: &mut Rdy, target: &str) {
    r.redact_in(
        &session(Role::Writer),
        "redact",
        RedactInput {
            targets: vec![target.to_string()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let p = r
        .propose(&session(Role::Writer), "redact", "Remove the code", None)
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
}

/// The whole history of the tag `t` of `r`, as one stream batch.
fn export(r: &Rdy, t: Hash) -> Vec<u8> {
    let mut out = Vec::new();
    stream_export::write(&r.read(), t, &BTreeSet::new(), &mut out).unwrap();
    out
}

/// The summary as JSON without its target path.
fn json_of(s: &LoadSummary) -> Value {
    let mut v = serde_json::to_value(s).unwrap();
    v.as_object_mut().unwrap().remove("project");
    v
}

/// Loads `files` with `stop` through the engine and through the adapter, into two new
/// directories; checks the summaries are the same but for the target, and returns the
/// adapter's and its directory.
fn both(dir: &Path, name: &str, files: &[PathBuf], stop: Option<&str>) -> (LoadSummary, PathBuf) {
    let stop = stop.map(|s| DumpStop::parse(s).unwrap());
    let core_dir = dir.join(format!("{name}-core"));
    let adapter_dir = dir.join(format!("{name}-adapter"));
    let core = rdy_engine::load::load_chain(files, &core_dir, stop.as_ref()).unwrap();
    let got = maintenance::load_chain(files, &adapter_dir, stop.as_ref()).unwrap();
    assert_eq!(json_of(&got), json_of(&core), "{name}");
    assert_eq!(got.project, adapter_dir.display().to_string());
    (got, adapter_dir)
}

#[test]
fn hc7_adapter_load_chain_matches_engine_without_a_live_store() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();

    // a project chain whose increment removes the secret
    let root = d.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let secret = add(&mut r, 0, SECRET);
    land(&mut r);
    let (v1, v1_landing) = tag(&mut r, "v1.0.0");
    // the holder's first upload, made before the removal (a later export carries it)
    let mut exports = vec![(export(&r, v1), v1_landing)];
    let f0 = d.path().join("f0.rdydump");
    let s0 = r.read().dump(&f0).unwrap();
    remove(&mut r, &secret);
    let (v2, v2_landing) = tag(&mut r, "v2.0.0");
    let i1 = d.path().join("i1.rdydump");
    r.read().dump_after(&s0.chain_end(), &i1).unwrap();
    let repo: RepoId = r.store().repo_id().unwrap().unwrap();
    exports.push((export(&r, v2), v2_landing));
    r.close().unwrap();
    let files = vec![f0, i1];

    let (whole, _) = both(d.path(), "whole", &files, None);
    assert!(whole.warnings.is_empty() && whole.stopped_at.is_none());
    assert_eq!(whole.chain.len(), 2);
    let stop = format!("pos:{}.{}", s0.end.segment, s0.end.offset);
    let (stopped, at) = both(d.path(), "stopped", &files, Some(&stop));
    let position: Location = stopped.stopped_at.as_ref().unwrap().position;
    assert_eq!(position, s0.end);
    assert_eq!(stopped.warnings.len(), 1);
    // the host reads the same warning from the loaded store
    let h = StoreHandle::open(&at).unwrap();
    let read: Vec<CarriedWarning> = h.redaction_warnings().unwrap();
    assert_eq!(read, stopped.warnings);
    h.close().unwrap();

    // a holder chain: the take before the removal in its full dump, the removal's in its
    // increment
    let hdir = d.path().join("holder");
    let mut holder = Rdy::create_holder(&hdir, repo).unwrap();
    let take = |h: &mut Rdy, i: usize, name: &str| {
        let (bytes, landing) = &exports[i];
        let p = d.path().join(format!("{name}.rdysync"));
        std::fs::write(&p, bytes).unwrap();
        stream_take::take(
            h,
            &[std::fs::File::open(&p).unwrap()],
            UploadTarget {
                tag: name.into(),
                landing: *landing,
            },
        )
        .unwrap();
    };
    take(&mut holder, 0, "v1.0.0");
    let h0 = d.path().join("h0.rdydump");
    let hs0 = holder.read().dump(&h0).unwrap();
    take(&mut holder, 1, "v2.0.0");
    let h1 = d.path().join("h1.rdydump");
    holder.read().dump_after(&hs0.chain_end(), &h1).unwrap();
    holder.close().unwrap();
    let files = vec![h0, h1];
    let (whole, _) = both(d.path(), "holder-whole", &files, None);
    assert!(whole.warnings.is_empty());
    assert_eq!(whole.next, "read the holder through the adapter at a tag");
    let stop = format!("pos:{}.{}", hs0.end.segment, hs0.end.offset);
    let (stopped, at) = both(d.path(), "holder-stopped", &files, Some(&stop));
    assert_eq!(stopped.warnings.len(), 1);
    assert_eq!(
        stopped.warnings[0].fix,
        rdy_store::redaction_carried::HOLDER_FIX
    );
    let h = StoreHandle::open_holder(&at).unwrap();
    assert_eq!(h.redaction_warnings().unwrap(), stopped.warnings);
    h.close().unwrap();
}
