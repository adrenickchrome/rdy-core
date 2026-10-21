//! hc7 through the adapter (D-2FMRR, D-QF2HQ, D-0VG72): `maintenance::load_chain` is the
//! engine's chain load. It opens no store before it loads, and a host reads the same summary,
//! stop and carried warnings as core gives them, for a project and for a holder.
use rdy_adapter::maintenance::{self, DumpStop, LoadSummary};
use rdy_adapter::{CarriedWarning, RepoId, StoreHandle};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest, stream_export, stream_take};
use rdy_model::{Hash, Role};
use rdy_store::redaction_import::{ReceiverCaps, set_receiver_caps_for_tests};
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

/// A project chain whose increment removes the secret, and a holder chain that took the project
/// before and after the removal. Each chain is a full dump and an increment; `*_stop` is the end
/// of the full dump.
struct Chains {
    project: Vec<PathBuf>,
    project_stop: Location,
    holder: Vec<PathBuf>,
    holder_stop: Location,
}

fn chains(d: &Path) -> Chains {
    // a project chain whose increment removes the secret
    let root = d.join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let secret = add(&mut r, 0, SECRET);
    land(&mut r);
    let (v1, v1_landing) = tag(&mut r, "v1.0.0");
    // the holder's first upload, made before the removal (a later export carries it)
    let mut exports = vec![(export(&r, v1), v1_landing)];
    let f0 = d.join("f0.rdydump");
    let s0 = r.read().dump(&f0).unwrap();
    remove(&mut r, &secret);
    let (v2, v2_landing) = tag(&mut r, "v2.0.0");
    let i1 = d.join("i1.rdydump");
    r.read().dump_after(&s0.chain_end(), &i1).unwrap();
    let repo: RepoId = r.store().repo_id().unwrap().unwrap();
    exports.push((export(&r, v2), v2_landing));
    r.close().unwrap();

    // a holder chain: the take before the removal in its full dump, the removal's in its
    // increment
    let hdir = d.join("holder");
    let mut holder = Rdy::create_holder(&hdir, repo).unwrap();
    let take = |h: &mut Rdy, i: usize, name: &str| {
        let (bytes, landing) = &exports[i];
        let p = d.join(format!("{name}.rdysync"));
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
    let h0 = d.join("h0.rdydump");
    let hs0 = holder.read().dump(&h0).unwrap();
    take(&mut holder, 1, "v2.0.0");
    let h1 = d.join("h1.rdydump");
    holder.read().dump_after(&hs0.chain_end(), &h1).unwrap();
    holder.close().unwrap();
    Chains {
        project: vec![f0, i1],
        project_stop: s0.end,
        holder: vec![h0, h1],
        holder_stop: hs0.end,
    }
}

#[test]
fn hc7_adapter_load_chain_matches_engine_without_a_live_store() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let c = chains(d.path());

    let files = c.project;
    let (whole, _) = both(d.path(), "whole", &files, None);
    assert!(whole.warnings.is_empty() && whole.stopped_at.is_none());
    assert_eq!(whole.chain.len(), 2);
    let s0_end = c.project_stop;
    let stop = format!("pos:{}.{}", s0_end.segment, s0_end.offset);
    let (stopped, at) = both(d.path(), "stopped", &files, Some(&stop));
    let position: Location = stopped.stopped_at.as_ref().unwrap().position;
    assert_eq!(position, s0_end);
    assert_eq!(stopped.warnings.len(), 1);
    // the host reads the same warning from the loaded store
    let h = StoreHandle::open(&at).unwrap();
    let read: Vec<CarriedWarning> = h.redaction_warnings().unwrap();
    assert_eq!(read, stopped.warnings);
    h.close().unwrap();

    let files = c.holder;
    let (whole, _) = both(d.path(), "holder-whole", &files, None);
    assert!(whole.warnings.is_empty());
    assert_eq!(whole.next, "read the holder through the adapter at a tag");
    let hs0_end = c.holder_stop;
    let stop = format!("pos:{}.{}", hs0_end.segment, hs0_end.offset);
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

/// Lowers one field of the receiver's scan caps on this thread and restores the caps it found
/// when it drops, also on unwind.
struct LoweredCap(ReceiverCaps);

impl LoweredCap {
    fn new(lower: impl FnOnce(&mut ReceiverCaps)) -> LoweredCap {
        let kept = ReceiverCaps::current();
        let mut caps = kept;
        lower(&mut caps);
        set_receiver_caps_for_tests(Some(caps));
        LoweredCap(kept)
    }
}

impl Drop for LoweredCap {
    fn drop(&mut self) {
        set_receiver_caps_for_tests(Some(self.0));
    }
}

#[test]
fn plb5_whole_holder_limit_matches_engine_before_target_writes() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    // every fixture is built before a cap is lowered
    let files = chains(d.path()).holder;
    let inputs: Vec<Vec<u8>> = files.iter().map(|f| std::fs::read(f).unwrap()).collect();
    let fix = "Keep the dump chain and the holder it came from. This rdy cannot load this chain.";
    let cases: [(&str, fn(&mut ReceiverCaps), &str); 3] = [
        (
            "spans",
            |c| c.spans = 0,
            "The redaction exceeds 1000000 scanned frames.",
        ),
        (
            "frames",
            |c| c.frames = 0,
            "The redaction exceeds 1000000 scanned frames.",
        ),
        (
            "bytes",
            |c| c.bytes = 0,
            "The redaction exceeds 2147483648 scanned bytes.",
        ),
    ];
    let kept = ReceiverCaps::current();
    for (name, lower, message) in cases {
        let core_dir = d.path().join(format!("{name}-core"));
        let adapter_dir = d.path().join(format!("{name}-adapter"));
        {
            let _caps = LoweredCap::new(lower);
            let core = rdy_engine::load::load_chain(&files, &core_dir, None).unwrap_err();
            let got = maintenance::load_chain(&files, &adapter_dir, None).unwrap_err();
            for e in [core, got] {
                assert_eq!(
                    (e.code, e.message.as_str(), e.fix.as_str()),
                    ("E_REDACTION_LIMIT", message, fix),
                    "{name}"
                );
            }
        }
        assert_eq!(ReceiverCaps::current(), kept, "{name}: caps restored");
        assert!(!core_dir.exists() && !adapter_dir.exists(), "{name}");
        for (f, before) in files.iter().zip(&inputs) {
            assert_eq!(&std::fs::read(f).unwrap(), before, "{name}: input changed");
        }
    }
    // the same fixture loads once the caps are back: it was valid
    let (whole, _) = both(d.path(), "restored", &files, None);
    assert_eq!(whole.chain.len(), 2);
}
