//! ha12 (TASK-293; final review): a dump belongs to one redaction epoch. A state read before a
//! removal, or one whose removal lands while its dump is being written, is `E_REDACTED_STATE`,
//! and no dump is placed: a dump of the old log with the new removal's dropped spans would not
//! load. Projects and holders alike.
use crate::error::E_REDACTED_STATE;
use crate::redaction::RedactInput;
use crate::stream_take::take;
use crate::{ProposalInfo, Rdy, Session, WriteRequest};
use rdy_model::{Hash, Role};
use rdy_store::take_batch::UploadTarget;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Write;
use std::path::Path;

const SECRET: &str = "The launch code is ZEBRA-7731.";

fn writer() -> Session {
    Session::human("human:t".into(), Role::Writer)
}

fn approver() -> Session {
    Session::human("human:t".into(), Role::Approver)
}

/// A project with a landed prose block that holds the secret, tagged `v1.0.0`, and a proposal
/// (view `redact`) that removes it, not landed.
fn removable(dir: &Path) -> (Rdy, ProposalInfo) {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let w = r
        .write(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
                    {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": SECRET}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    let base = r.propose(&writer(), "work", "Base", None).unwrap();
    r.land(&approver(), &base.id).unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    r.redact_in(
        &writer(),
        "redact",
        RedactInput {
            targets: vec![w.created["$p"].clone()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let prop = r
        .propose(&writer(), "redact", "Remove the code", None)
        .unwrap();
    (r, prop)
}

/// A sink that runs `act` once, at its first write (the dump's header), then keeps the bytes.
struct During<'a> {
    act: Option<Box<dyn FnOnce() + 'a>>,
    bytes: Vec<u8>,
}

impl std::fmt::Debug for During<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "During({} bytes)", self.bytes.len())
    }
}

impl Write for During<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(act) = self.act.take() {
            act();
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// No file of `dir` is a dump's partial file.
fn no_partial(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".partial-")
    })
}

#[test]
fn ha12_a_project_dump_refuses_a_state_read_before_a_removal() {
    let _clock = crate::pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let (mut r, prop) = removable(&d.path().join("p"));
    let old = r.read();
    r.land(&approver(), &prop.id).unwrap();
    let out = d.path().join("old.rdydump");
    let e = old.dump(&out).unwrap_err();
    assert_eq!(e.code, E_REDACTED_STATE, "{e:?}");
    assert!(!out.exists());
    assert!(no_partial(d.path()));
    // a state read after the removal dumps, and its dump loads
    let fresh = d.path().join("fresh.rdydump");
    r.read().dump(&fresh).unwrap();
    crate::load::load(&fresh, &d.path().join("loaded")).unwrap();
    Rdy::open(&d.path().join("loaded")).unwrap().fsck().unwrap();
}

#[test]
fn ha12_a_project_dump_refuses_a_removal_that_lands_while_it_is_written() {
    let _clock = crate::pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let (mut r, prop) = removable(&d.path().join("p"));
    let st = r.read();
    let files = super::project_files(&d.path().join("p")).unwrap();
    // the state is current when the dump starts; the removal lands at its first write
    st.ensure_current_epoch().unwrap();
    let sink = During {
        act: Some(Box::new(|| {
            r.land(&approver(), &prop.id).unwrap();
        })),
        bytes: vec![],
    };
    let e = st.write_dump(sink, &files).unwrap_err();
    assert_eq!(e.code, E_REDACTED_STATE, "{e:?}");
}

/// A holder that took `v1.0.0` of a project holding the secret, and the upload of that
/// project's `v2.0.0`, which removes it.
fn holder_and_removal(dir: &Path) -> (Rdy, Vec<File>, UploadTarget) {
    let (mut r, prop) = removable(&dir.join("src"));
    let export = |r: &Rdy, name: &str, file: &str| -> (Vec<File>, UploadTarget) {
        let (_, hash, tag) = r
            .store()
            .tags()
            .unwrap()
            .into_iter()
            .find(|(n, _, _)| n == name)
            .unwrap();
        let mut bytes = vec![];
        crate::stream_export::write(&r.read(), hash, &BTreeSet::new(), &mut bytes).unwrap();
        let path = dir.join(file);
        std::fs::write(&path, bytes).unwrap();
        (
            vec![File::open(&path).unwrap()],
            UploadTarget {
                tag: name.into(),
                landing: tag.landing,
            },
        )
    };
    let mut h = Rdy::create_holder(&dir.join("holder"), r.read().repo_id().unwrap()).unwrap();
    let (v1, t1) = export(&r, "v1.0.0", "v1.rdysync");
    take(&mut h, &v1, t1).unwrap();
    r.land(&approver(), &prop.id).unwrap();
    r.tag(&approver(), "v2.0.0", None, None).unwrap();
    let (v2, t2) = export(&r, "v2.0.0", "v2.rdysync");
    (h, v2, t2)
}

#[test]
fn ha12_a_holder_dump_refuses_a_state_read_before_a_removal() {
    let _clock = crate::pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let (mut h, v2, t2) = holder_and_removal(d.path());
    let old = h.read();
    let got = take(&mut h, &v2, t2).unwrap();
    assert!(got.redacted_objects >= 1, "{got:?}");
    let out = d.path().join("old.rdydump");
    let e = old.dump(&out).unwrap_err();
    assert_eq!(e.code, E_REDACTED_STATE, "{e:?}");
    assert!(!out.exists());
    assert!(no_partial(d.path()));
    let fresh = d.path().join("fresh.rdydump");
    h.read().dump(&fresh).unwrap();
    crate::load::load(&fresh, &d.path().join("loaded")).unwrap();
    Rdy::open_holder(&d.path().join("loaded"))
        .unwrap()
        .fsck()
        .unwrap();
}

#[test]
fn ha12_a_holder_dump_refuses_a_removal_taken_while_it_is_written() {
    let _clock = crate::pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let (mut h, v2, t2) = holder_and_removal(d.path());
    let st = h.read();
    st.ensure_current_epoch().unwrap();
    let removed: std::cell::Cell<Option<Hash>> = std::cell::Cell::new(None);
    let sink = During {
        act: Some(Box::new(|| {
            removed.set(Some(take(&mut h, &v2, t2).unwrap().tag_hash));
        })),
        bytes: vec![],
    };
    let e = st.write_dump(sink, &[]).unwrap_err();
    assert_eq!(e.code, E_REDACTED_STATE, "{e:?}");
    assert!(
        removed.get().is_some(),
        "the removal was taken during the dump"
    );
}
