//! ha13 (D-D2NPN "verify-on-receive", D-BPYN8, D-CC90E, D-3M81W; hub p-97JVE, f-NE0Z4, r-DR5Z9,
//! r-M1GA9): the engine's take of one upload into a holder. Every batch file of the upload goes
//! to one store take that writes the closure, its removals and the tag in one marker; the root
//! tag's version is judged by the existing parser against the held tags; every new landing's
//! snapshot keeps the Publications limits with no pack; an uncovered receiver change refuses
//! with the cross-upload fix until the sibling's own removal is taken; a sealed take whose reply
//! was lost is repaired from the holder's tags and object keys.
mod common;

use ed25519_dalek::Signer as _;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::history_facts::presence;
use rdy_engine::redaction::RedactInput;
use rdy_engine::signature_check::signature_checks;
use rdy_engine::signing_crypto::blob_fingerprint;
use rdy_engine::stream_check::check_batch;
use rdy_engine::stream_take::{StorePosition, take};
use rdy_engine::*;
use rdy_model::signature::{Signature, StatementSource, statement};
use rdy_model::*;
use rdy_store::LandRequest;
use rdy_store::closure::{self, END_OF_LOG, StoreSource};
use rdy_store::kinds;
use rdy_store::object_refs::{ObjectKey, ObjectRef};
use rdy_store::redaction::StoredFrame;
use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint};
use rdy_store::segment::Location;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::{SYNC_KINDS, StreamHeader};
use rdy_store::take_batch::UploadTarget;
use serde_json::{Value, json};
use ssh_encoding::Encode;
use ssh_key::public::{Ed25519PublicKey, KeyData};
use ssh_key::{Algorithm, HashAlg, SshSig};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

const CANARY: &str = "ZEBRA-7731";

// ---------------------------------------------------------------------------------------------
// source projects

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer)
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver)
}

fn write(r: &mut Rdy, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "ha13".into(),
        },
    )
    .unwrap()
}

/// Proposes view `work` and lands it as the founder; returns the landing.
fn land(r: &mut Rdy) -> Hash {
    let p = r
        .propose(&writer(), "work", "batch", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap().parse().unwrap()
}

/// Tags canon's head `name` with `message`; returns the tag's hash.
fn tag_msg(r: &mut Rdy, name: &str, message: Option<&str>) -> Hash {
    r.tag(&founder(), name, None, message.map(str::to_string))
        .unwrap();
    tag_hash(r, name)
}

fn tag(r: &mut Rdy, name: &str) -> Hash {
    tag_msg(r, name, None)
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

fn landing_of(r: &Rdy, tag: Hash) -> Hash {
    r.store().read_tag(&tag).unwrap().landing
}

fn repo(r: &Rdy) -> RepoId {
    r.read().repo_id().unwrap()
}

/// A project with a doc and a prose block that holds the canary, landed. Returns its directory,
/// the project, the doc's id and the prose block's id.
fn project(dir: &Path) -> (Rdy, String, String) {
    std::fs::create_dir_all(dir).unwrap();
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": format!("The launch code is {CANARY}.")}
        ]),
    );
    land(&mut r);
    (r, w.created["$d"].clone(), w.created["$p"].clone())
}

/// Removes every version of block `target` through the store's own landing path.
fn remove(r: &mut Rdy, target: &str) {
    let s = writer();
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: vec![target.to_string()],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r.propose(&s, "work", "Remove the code", None).unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .land_atomic(LandRequest {
            proposal: version,
            accept: None,
            approver: Provenance {
                actor: "human:t".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), t).unwrap();
        }
    }
}

/// Two projects of one repo that share the first landing of [`project`] and diverge after it.
fn twins(dir: &Path) -> (Rdy, Rdy, String, String) {
    let (a, doc, prose) = project(&dir.join("a"));
    drop(a);
    copy_dir(&dir.join("a"), &dir.join("b"));
    (
        Rdy::open(&dir.join("a")).unwrap(),
        Rdy::open(&dir.join("b")).unwrap(),
        doc,
        prose,
    )
}

/// Retitles the doc `doc` and lands it; returns the landing.
fn retitle(r: &mut Rdy, doc: &str, title: &str) -> Hash {
    write(
        r,
        json!([{"op": "update", "id": doc, "fields": {"title": title}}]),
    );
    land(r)
}

// ---------------------------------------------------------------------------------------------
// batches

/// The closure of TAG `tag` in `r` as `n` batch files: record `i` of the closure order goes to
/// file `i % n`. Each file's prerequisites are exactly the keys its records need from the other
/// files, plus the root when it does not hold it.
fn split(r: &Rdy, tag: Hash, n: usize) -> Vec<Vec<u8>> {
    let reader = r.store().reader();
    let c = closure::closure(&reader, tag, &BTreeSet::new()).unwrap();
    let all: BTreeSet<ObjectKey> = c.records.iter().copied().collect();
    let root = ObjectKey {
        kind: kinds::TAG,
        hash: tag,
    };
    let mut source = StoreSource {
        reader: &reader,
        end: END_OF_LOG,
    };
    let resolve = |l: ObjectRef| match l.kind {
        Some(kind) => ObjectKey { kind, hash: l.hash },
        None => SYNC_KINDS
            .iter()
            .map(|&kind| ObjectKey { kind, hash: l.hash })
            .find(|k| all.contains(k))
            .unwrap(),
    };
    (0..n)
        .map(|part| {
            let records: Vec<ObjectKey> = c
                .records
                .iter()
                .enumerate()
                .filter(|(i, _)| i % n == part)
                .map(|(_, k)| *k)
                .collect();
            let mine: BTreeSet<ObjectKey> = records.iter().copied().collect();
            let mut needed = BTreeSet::new();
            for k in &records {
                let rec = source.record(*k).unwrap().unwrap();
                for l in rec.refs().unwrap() {
                    needed.insert(resolve(l));
                }
                needed.extend(rec.proof());
            }
            needed.insert(root);
            needed.retain(|k| !mine.contains(k));
            let header = StreamHeader::tag_history(repo(r), tag, needed.into_iter().collect());
            closure::write_batch(&mut source, &header, &records, Vec::new())
                .unwrap()
                .0
        })
        .collect()
}

/// The whole history of `tag` as one batch, as the export writes it.
fn export(r: &Rdy, tag: Hash) -> Vec<u8> {
    let mut out = Vec::new();
    rdy_engine::stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut out).unwrap();
    out
}

/// `batches` written under `dir` with names from `name`, opened for reading.
fn files(dir: &Path, name: &str, batches: &[&Vec<u8>]) -> Vec<File> {
    batches
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let p = dir.join(format!("{name}-{i}.rdysync"));
            std::fs::write(&p, b).unwrap();
            File::open(&p).unwrap()
        })
        .collect()
}

fn target(tag: &str, landing: Hash) -> UploadTarget {
    UploadTarget {
        tag: tag.into(),
        landing,
    }
}

/// The keys of the whole closure of `tag` in `r`.
fn closure_keys(r: &Rdy, tag: Hash) -> Vec<ObjectKey> {
    closure::closure(&r.store().reader(), tag, &BTreeSet::new())
        .unwrap()
        .records
}

// ---------------------------------------------------------------------------------------------
// holders

fn holder(dir: &Path, repo: RepoId) -> (PathBuf, Rdy) {
    let root = dir.join("holder");
    let h = Rdy::create_holder(&root, repo).unwrap();
    (root, h)
}

fn segments(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(root.join(".rdy/store/segments")).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("seg-") && name.ends_with(".log") {
            out.insert(name, std::fs::read(e.path()).unwrap());
        }
    }
    out
}

fn holds_canary(root: &Path) -> bool {
    segments(root)
        .values()
        .any(|b| b.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()))
}

/// The holder's tag and relay refs.
fn refs(h: &Rdy) -> Vec<(String, Vec<u8>)> {
    let mut out = h.store().refs_with_prefix("tag/").unwrap();
    out.extend(h.store().refs_with_prefix("answer/").unwrap());
    out
}

/// What a refused take must leave as it was: the log's end and bytes and the refs.
type Unchanged = (Location, BTreeMap<String, Vec<u8>>, Vec<(String, Vec<u8>)>);

fn unchanged(root: &Path, h: &Rdy) -> Unchanged {
    (h.store().log_end(), segments(root), refs(h))
}

/// The number of batch markers in the holder's log.
fn markers(h: &Rdy) -> usize {
    let mut n = 0;
    h.store()
        .reader()
        .for_each_record_from(
            Location {
                segment: 0,
                offset: 0,
            },
            h.store().log_end(),
            |_, f, _| {
                if matches!(
                    f,
                    StoredFrame::Present {
                        kind: kinds::BATCH_COMMIT,
                        ..
                    }
                ) {
                    n += 1;
                }
                Ok(())
            },
        )
        .unwrap();
    n
}

fn refusal(e: &RdyError) -> (&str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn ha13_all_batch_files_take_in_one_marker() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _doc, secret) = project(&dir.path().join("src"));
    tag(&mut r, "v1.0.0");
    remove(&mut r, &secret);
    let v2 = tag(&mut r, "v2.0.0");
    let keys = closure_keys(&r, v2);
    assert!(keys.iter().any(|k| k.kind == kinds::REDACTION_APPLY));
    let parts = split(&r, v2, 3);
    // each file alone is a valid batch; none of them is a whole upload
    for p in &parts {
        let report = check_batch(p.as_slice(), Some(repo(&r))).unwrap();
        assert!(report.keys.len() < keys.len());
    }
    let (root, mut h) = holder(dir.path(), repo(&r));
    let before = markers(&h);
    // in any order, one of them twice
    let fs = files(
        dir.path(),
        "up",
        &[&parts[2], &parts[0], &parts[1], &parts[0]],
    );
    let got = take(&mut h, &fs, target("v2.0.0", landing_of(&r, v2))).unwrap();
    // the closure, its removal and the tag: one batch
    assert_eq!(markers(&h), before + 1);
    assert_eq!(got.records_taken, keys.len() as u64);
    assert_eq!(got.frames_written, keys.len() as u64 + 1);
    assert!(got.redacted_objects >= 1, "{got:?}");
    assert_eq!(
        (
            got.tag.as_str(),
            got.tag_hash,
            got.landing,
            got.canon,
            got.unchanged
        ),
        ("v2.0.0", v2, landing_of(&r, v2), None, false)
    );
    let end = h.store().log_end();
    assert_eq!(
        got.end,
        StorePosition {
            segment: end.segment,
            offset: end.offset
        }
    );
    // every key is there, the removed ones as drops; no removed byte reached the holder
    let seen = presence(&h.read(), &keys).unwrap();
    assert!(seen.iter().all(|p| p.present), "{seen:?}");
    assert!(seen.iter().any(|p| p.redacted));
    assert!(!holds_canary(&root));
    assert_eq!(refs(&h), vec![("tag/v2.0.0".to_string(), v2.0.to_vec())]);
    // the published state reads the new tag
    let versions = h.read().versions().unwrap();
    assert_eq!(
        versions.iter().map(|v| v.tag.clone()).collect::<Vec<_>>(),
        vec![v2.to_string()]
    );
    h.fsck().unwrap();
    // the reply's JSON: full lowercase hex hashes, no canon
    let json = serde_json::to_value(&got).unwrap();
    assert_eq!(json["tag_hash"], v2.to_string());
    assert_eq!(json["landing"], landing_of(&r, v2).to_string());
    assert_eq!(json["canon"], Value::Null);
    assert_eq!(
        json["end"],
        json!({"segment": end.segment, "offset": end.offset})
    );
}

#[test]
fn ha13_take_accepts_divergence_without_canon() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, doc, _) = twins(dir.path());
    let la = retitle(&mut a, &doc, "A side");
    let ta = tag(&mut a, "v1.1.0");
    let lb = retitle(&mut b, &doc, "B side");
    let tb = tag(&mut b, "v2.0.0");
    assert_ne!(la, lb);
    let (root, mut h) = holder(dir.path(), repo(&a));
    take(
        &mut h,
        &files(dir.path(), "a", &[&export(&a, ta)]),
        target("v1.1.0", la),
    )
    .unwrap();
    take(
        &mut h,
        &files(dir.path(), "b", &[&export(&b, tb)]),
        target("v2.0.0", lb),
    )
    .unwrap();
    // two divergent chains, no canon: each reads at its own tag
    let st = h.read();
    assert_eq!(st.canon_head(), None);
    let id = BlockId::parse(&doc).unwrap();
    for (l, title) in [(la, "A side"), (lb, "B side")] {
        let block = st.block(&Scope::At(l), &id).unwrap();
        assert_eq!(block.fields["title"], FieldValue::Str(title.into()));
    }
    h.fsck().unwrap();
    drop(st);
    assert!(root.exists());

    // the mode: a project is no holder, and nothing is written
    let src_root = dir.path().join("a");
    let before = (a.store().log_end(), segments(&src_root));
    let e = take(
        &mut a,
        &files(dir.path(), "m", &[&export(&b, tb)]),
        target("v2.0.0", lb),
    )
    .unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_BAD_INPUT,
            "this store is a project",
            "open it through the project adapter"
        )
    );
    assert_eq!((a.store().log_end(), segments(&src_root)), before);

    // the repo: a holder of another repo cannot adopt this one
    let other = RepoId([0x77; 16]);
    let o = dir.path().join("other");
    let mut h2 = Rdy::create_holder(&o, other).unwrap();
    let before = unchanged(&o, &h2);
    let e = take(
        &mut h2,
        &files(dir.path(), "o", &[&export(&a, ta)]),
        target("v1.1.0", la),
    )
    .unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_REPO_MISMATCH,
            format!(
                "stream repo {} does not match holder repo {other}",
                repo(&a)
            )
            .as_str(),
            "use the holder for the stream's repo"
        )
    );
    assert_eq!(unchanged(&o, &h2), before);
}

#[test]
fn ha13_semver_collision_names_existing_landing_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, doc, _) = twins(dir.path());
    let la = retitle(&mut a, &doc, "A side");
    let ta = tag(&mut a, "v1.2.0");
    let lb = retitle(&mut b, &doc, "B side");
    let lower = tag(&mut b, "0.9.0");
    let tb = tag(&mut b, "1.2.0");
    let (root, mut h) = holder(dir.path(), repo(&a));
    take(
        &mut h,
        &files(dir.path(), "a", &[&export(&a, ta)]),
        target("v1.2.0", la),
    )
    .unwrap();
    let before = unchanged(&root, &h);
    // 1.2.0 is v1.2.0 by SemVer precedence, and it names another landing
    let e = take(
        &mut h,
        &files(dir.path(), "b", &[&export(&b, tb)]),
        target("1.2.0", lb),
    )
    .unwrap_err();
    let fix = "use another version name";
    assert_eq!(
        refusal(&e),
        (
            E_TAG_EXISTS,
            format!("version 1.2.0 already names landing {la}").as_str(),
            fix
        )
    );
    assert_eq!(e.issues.len(), 1);
    let i = &e.issues[0];
    assert_eq!(
        (
            i.severity,
            i.code,
            i.block.clone(),
            i.message.as_str(),
            i.fix.as_str()
        ),
        (
            rdy_schema::Severity::Error,
            E_TAG_EXISTS,
            None,
            format!("existing_landing: {la}").as_str(),
            fix
        )
    );
    assert_eq!(unchanged(&root, &h), before);
    // a version below the held one is taken: versions need not increase
    let got = take(
        &mut h,
        &files(dir.path(), "l", &[&export(&b, lower)]),
        target("0.9.0", lb),
    )
    .unwrap();
    assert_eq!((got.tag.as_str(), got.tag_hash), ("0.9.0", lower));
    h.fsck().unwrap();
}

#[test]
fn ha13_same_landing_preserves_first_name_and_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, _, _) = twins(dir.path());
    let ta = tag_msg(&mut a, "v1.2.0", Some("first"));
    let tb = tag_msg(&mut b, "1.2.0", Some("second"));
    assert_ne!(ta, tb);
    let l = landing_of(&a, ta);
    assert_eq!(landing_of(&b, tb), l);
    let (root, mut h) = holder(dir.path(), repo(&a));
    let fa = files(dir.path(), "a", &[&export(&a, ta)]);
    let first = take(&mut h, &fa, target("v1.2.0", l)).unwrap();
    assert_eq!((first.tag.as_str(), first.tag_hash), ("v1.2.0", ta));
    // the same version at the same landing: the held tag survives, by its name and record
    let fb = files(dir.path(), "b", &[&export(&b, tb)]);
    let got = take(&mut h, &fb, target("1.2.0", l)).unwrap();
    assert_eq!(
        (
            got.tag.as_str(),
            got.tag_hash,
            got.landing,
            got.records_taken,
            got.frames_written,
            got.unchanged
        ),
        ("v1.2.0", ta, l, 1, 1, false)
    );
    assert_eq!(refs(&h), vec![("tag/v1.2.0".to_string(), ta.0.to_vec())]);
    let versions = h.read().versions().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(
        (
            versions[0].name.as_str(),
            versions[0].tag.clone(),
            versions[0].message.as_deref()
        ),
        ("v1.2.0", ta.to_string(), Some("first"))
    );
    h.fsck().unwrap();
    // each repeat appends nothing and names the held tag
    let end = h.store().log_end();
    for (fs, name) in [(&fb, "1.2.0"), (&fa, "v1.2.0")] {
        let again = take(&mut h, fs, target(name, l)).unwrap();
        assert_eq!(
            (
                again.tag.as_str(),
                again.tag_hash,
                again.records_taken,
                again.frames_written,
                again.unchanged
            ),
            ("v1.2.0", ta, 0, 0, true)
        );
        assert_eq!(h.store().log_end(), end);
    }
    assert!(!segments(&root).is_empty());
}

#[test]
fn ha13_build_metadata_uses_existing_tag_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _, _) = project(&dir.path().join("src"));
    let held = tag(&mut r, "v1.2.0");
    let l = landing_of(&r, held);
    // a tag no rdy tag writes: version 1.2.0 with build metadata, at the same landing
    let name = "1.2.0+build.5";
    let raw = r
        .store_mut()
        .put_tag(&Tag {
            name: name.into(),
            landing: l,
            message: None,
            tagged_by: Provenance {
                actor: "human:founder".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1,
            },
        })
        .unwrap();
    r.seal_open().unwrap();
    let want = parse_version(name).unwrap_err();
    assert_eq!(want.code, E_BAD_TAG_NAME);
    let raw_files = files(dir.path(), "raw", &[&split(&r, raw, 1)[0]]);
    // an empty holder and a holder of v1.2.0 at that landing refuse it alike: the parser's own
    // refusal, never a match with the held version
    let (root, mut h) = holder(dir.path(), repo(&r));
    for round in 0..2 {
        let before = unchanged(&root, &h);
        let e = take(&mut h, &raw_files, target(name, l)).unwrap_err();
        assert_eq!(refusal(&e), refusal(&want), "round {round}");
        assert_eq!(unchanged(&root, &h), before);
        if round == 0 {
            take(
                &mut h,
                &files(dir.path(), "v", &[&export(&r, held)]),
                target("v1.2.0", l),
            )
            .unwrap();
        }
    }
}

#[test]
fn ha13_take_rechecks_dependencies_after_batch_reports() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, doc, prose) = project(&dir.path().join("src"));
    let v1 = tag(&mut r, "v1.0.0");
    let l = landing_of(&r, v1);
    let parts = split(&r, v1, 2);
    for p in &parts {
        check_batch(p.as_slice(), Some(repo(&r))).unwrap();
    }
    let (root, mut h) = holder(dir.path(), repo(&r));
    let before = unchanged(&root, &h);
    // each file was checked alone; the take needs every dependency over the whole upload
    for p in &parts {
        let e = take(&mut h, &files(dir.path(), "one", &[p]), target("v1.0.0", l)).unwrap_err();
        assert_eq!(e.code, "E_UNRESOLVED_REF", "{e:?}");
        assert!(
            e.message.starts_with("stream prerequisite ") && e.message.ends_with(" is missing"),
            "{}",
            e.message
        );
        assert_eq!(unchanged(&root, &h), before);
    }
    // a file changed after its report: the take reads it again
    let at = (0..2)
        .find(|&i| {
            parts[i]
                .windows(CANARY.len())
                .any(|w| w == CANARY.as_bytes())
        })
        .unwrap();
    let mut tampered = parts.clone();
    let pos = tampered[at]
        .windows(CANARY.len())
        .position(|w| w == CANARY.as_bytes())
        .unwrap();
    tampered[at][pos] ^= 0x20;
    let e = take(
        &mut h,
        &files(dir.path(), "t", &[&tampered[0], &tampered[1]]),
        target("v1.0.0", l),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_RECORD", "{e:?}");
    assert!(e.message.ends_with(": hash mismatch"), "{}", e.message);
    assert!(check_batch(tampered[at].as_slice(), Some(repo(&r))).is_err());
    assert_eq!(unchanged(&root, &h), before);
    // the files as reported, together, are the upload
    let got = take(
        &mut h,
        &files(dir.path(), "ok", &[&parts[1], &parts[0]]),
        target("v1.0.0", l),
    )
    .unwrap();
    assert_eq!(got.records_taken, closure_keys(&r, v1).len() as u64);

    // a well-formed history whose snapshot names, for the doc, a version of the prose block:
    // every batch passes its check, and the take refuses it before anything is written
    let rec = r.store().landing(&l).unwrap();
    let mut snap = r.store().state_of(Some(l)).unwrap();
    let doc_id = BlockId::parse(&doc).unwrap();
    let prose_id = BlockId::parse(&prose).unwrap();
    let foreign = snap.blocks[&prose_id];
    snap.blocks.insert(doc_id.clone(), foreign);
    let s = r.store_mut().put(kinds::SNAPSHOT, &snap).unwrap();
    let forged = r
        .store_mut()
        .put(
            kinds::LANDING,
            &LandingRecord {
                state: s,
                ..rec.clone()
            },
        )
        .unwrap();
    let t = r
        .store_mut()
        .put_tag(&Tag {
            name: "v9.0.0".into(),
            landing: forged,
            message: None,
            tagged_by: rec.approved_by.clone(),
        })
        .unwrap();
    r.seal_open().unwrap();
    let batch = split(&r, t, 1).remove(0);
    check_batch(batch.as_slice(), Some(repo(&r))).unwrap();
    let before = unchanged(&root, &h);
    let e = take(
        &mut h,
        &files(dir.path(), "f", &[&batch]),
        target("v9.0.0", forged),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            "E_BAD_RECORD",
            format!("snapshot {s}: block {doc_id} names version {foreign}, which is a version of {prose_id}")
                .as_str()
        )
    );
    assert_eq!(unchanged(&root, &h), before);
}

#[test]
fn ha13_upload_target_must_match_all_batch_roots() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, doc, _) = twins(dir.path());
    let la = retitle(&mut a, &doc, "A side");
    let ta = tag(&mut a, "v1.2.0");
    let lb = retitle(&mut b, &doc, "B side");
    let tb = tag(&mut b, "1.2.0");
    let t3 = tag(&mut b, "3.0.0");
    let (root, mut h) = holder(dir.path(), repo(&a));
    take(
        &mut h,
        &files(dir.path(), "a", &[&export(&a, ta)]),
        target("v1.2.0", la),
    )
    .unwrap();
    let before = unchanged(&root, &h);
    let fb = files(dir.path(), "b", &[&export(&b, tb)]);
    // the target is the root tag's exact name and landing, checked before the version rule
    for t in [
        target("v1.2.0", lb),
        target("1.2.0 ", lb),
        target("1.2.0", la),
    ] {
        let e = take(&mut h, &fb, t.clone()).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (
                "E_BAD_RECORD",
                "upload target does not match the batch root tag"
            ),
            "{t:?}"
        );
        assert_eq!(unchanged(&root, &h), before);
    }
    // every file names the same root
    let mixed = files(dir.path(), "mix", &[&export(&b, tb), &export(&b, t3)]);
    let e = take(&mut h, &mixed, target("1.2.0", lb)).unwrap_err();
    assert_eq!(e.code, "E_BAD_RECORD");
    assert!(
        e.message.ends_with(": batch header differs"),
        "{}",
        e.message
    );
    assert_eq!(unchanged(&root, &h), before);
    // with the exact target, the version rule speaks
    let e = take(&mut h, &fb, target("1.2.0", lb)).unwrap_err();
    assert_eq!(e.code, E_TAG_EXISTS);
}

#[test]
fn ha13_unknown_pack_types_do_not_fail_holder_take() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, doc, _) = project(&dir.path().join("src"));
    // a block of a type no pack declares, written and landed past the engine's schema
    let widget = Block {
        id: BlockId::parse("c-WDGT1").unwrap(),
        block_type: "widget-x".into(),
        parent: Some(BlockId::parse(&doc).unwrap()),
        order: "V".into(),
        fields: BTreeMap::from([("dial".to_string(), FieldValue::Str("11".into()))]),
        body: "a widget".into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    };
    let by = |role| Provenance {
        actor: "human:founder".into(),
        run: None,
        role,
        at_ms: 5,
    };
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .commit_to_view(
            "work",
            Change {
                view: "work".into(),
                ops: vec![Op::Create {
                    block: widget.clone(),
                }],
                deps: vec![],
                provenance: by(Role::Writer),
                justification: None,
                message: String::new(),
                links: ChangeLinks::default(),
                read_identity: None,
            },
        )
        .unwrap();
    let p = r
        .store_mut()
        .propose("work", "widget", by(Role::Writer))
        .unwrap();
    let l = r.store_mut().land(&p, by(Role::Approver)).unwrap();
    let t = r
        .store_mut()
        .put_tag(&Tag {
            name: "v1.0.0".into(),
            landing: l,
            message: None,
            tagged_by: by(Role::Approver),
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
    let (_root, mut h) = holder(dir.path(), repo(&r));
    let got = take(
        &mut h,
        &files(dir.path(), "w", &[&split(&r, t, 2)[0], &split(&r, t, 2)[1]]),
        target("v1.0.0", l),
    )
    .unwrap();
    assert_eq!(got.tag_hash, t);
    let b = h.read().block(&Scope::At(l), &widget.id).unwrap();
    assert_eq!(
        (b.block_type.as_str(), b.body.as_str()),
        ("widget-x", "a widget")
    );
    h.fsck().unwrap();
}

#[test]
fn ha13_new_snapshots_keep_publication_limits() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    let mut r = Rdy::init(&dir.path().join("src"), "rdy-standard").unwrap();
    let bytes = vec![7u8; (1 << 20) + 5];
    let blob = r.store_mut().put_blob(&bytes).unwrap();
    write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "Some prose."},
            {"op": "create", "type": "figure", "id": "$f", "parent": "$d",
             "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "fig"}}
        ]),
    );
    land(&mut r);
    let v1 = tag(&mut r, "v1.0.0");
    let l = landing_of(&r, v1);
    let fs = files(dir.path(), "v1", &[&export(&r, v1)]);
    let (root, mut h) = holder(dir.path(), repo(&r));
    let before = unchanged(&root, &h);
    let base = rdy_engine::publication::limits();
    // one below the snapshot's selection, then one below its blob: the Publications refusal,
    // and nothing written
    for (lowered, part) in [
        (
            rdy_engine::publication::Limits {
                selection: 2,
                ..base
            },
            "selection",
        ),
        (
            rdy_engine::publication::Limits {
                blob: bytes.len() as u64 - 1,
                ..base
            },
            "blob",
        ),
    ] {
        let _guard = rdy_engine::publication::override_limits_for_tests(lowered);
        let e = take(&mut h, &fs, target("v1.0.0", l)).unwrap_err();
        assert_eq!(e.code, E_LIMIT, "{e:?}");
        assert!(
            e.message
                .starts_with(&format!("Publication {part} exceeds the limit of ")),
            "{}",
            e.message
        );
        assert_eq!(unchanged(&root, &h), before);
    }
    // at the bounds, the snapshot is taken
    let _guard =
        rdy_engine::publication::override_limits_for_tests(rdy_engine::publication::Limits {
            selection: 3,
            blob: bytes.len() as u64,
            ..base
        });
    take(&mut h, &fs, target("v1.0.0", l)).unwrap();
}

#[test]
fn ha13_sibling_removal_unblocks_the_original_upload() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, doc, prose) = twins(dir.path());
    let id = BlockId::parse(&prose).unwrap();
    let secret = a.read().block(&Scope::Canon, &id).unwrap().version_hash();
    // B edits the prose away and back: a change of its own embeds the shared secret version
    write(
        &mut b,
        json!([{"op": "update", "id": prose, "body": "nothing to see"}]),
    );
    land(&mut b);
    let back = write(
        &mut b,
        json!([{"op": "update", "id": prose, "body": format!("The launch code is {CANARY}.")}]),
    );
    land(&mut b);
    let embedding: Hash = back.change.parse().unwrap();
    assert_eq!(
        b.read().block(&Scope::Canon, &id).unwrap().version_hash(),
        secret
    );
    let b1 = tag(&mut b, "v2.0.0");
    let (root, mut h) = holder(dir.path(), repo(&a));
    take(
        &mut h,
        &files(dir.path(), "b1", &[&export(&b, b1)]),
        target("v2.0.0", landing_of(&b, b1)),
    )
    .unwrap();
    assert!(holds_canary(&root));

    // A removes the prose: its approval covers A's changes, not B's
    retitle(&mut a, &doc, "A side");
    remove(&mut a, &prose);
    let a1 = tag(&mut a, "v3.0.0");
    let fa = files(dir.path(), "a1", &[&export(&a, a1)]);
    let before = unchanged(&root, &h);
    let e = take(&mut h, &fa, target("v3.0.0", landing_of(&a, a1))).unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_REDACTION_STALE,
            format!("the imported removal does not cover receiver change {embedding}").as_str(),
            format!(
                "redact this content in the project that made change {embedding}, upload that removal, then upload again"
            )
            .as_str()
        )
    );
    assert_ne!(e.fix, REDACTION_STALE_FIX);
    assert!(e.issues.is_empty(), "no warning: {:?}", e.issues);
    assert_eq!(unchanged(&root, &h), before);
    assert!(holds_canary(&root));

    // B's own approved removal covers B's change
    remove(&mut b, &prose);
    let b2 = tag(&mut b, "v4.0.0");
    take(
        &mut h,
        &files(dir.path(), "b2", &[&export(&b, b2)]),
        target("v4.0.0", landing_of(&b, b2)),
    )
    .unwrap();
    assert!(!holds_canary(&root));
    // the original upload now passes as it was
    let got = take(&mut h, &fa, target("v3.0.0", landing_of(&a, a1))).unwrap();
    assert!(got.records_taken > 0);
    assert!(!holds_canary(&root));
    h.fsck().unwrap();
}

#[test]
fn ha13_uncertain_reply_is_repaired_by_tags_and_keys() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _, secret) = project(&dir.path().join("src"));
    let v1 = tag(&mut r, "v1.0.0");
    let (root, mut h) = holder(dir.path(), repo(&r));
    let f1 = files(dir.path(), "v1", &[&export(&r, v1)]);
    take(&mut h, &f1, target("v1.0.0", landing_of(&r, v1))).unwrap();
    assert!(holds_canary(&root));
    remove(&mut r, &secret);
    let v2 = tag(&mut r, "v2.0.0");
    let l2 = landing_of(&r, v2);
    let f2 = files(dir.path(), "v2", &[&export(&r, v2)]);
    // the take seals its removal, then fails before the erasure finishes: no success
    arm_failpoint(Some((Failpoint::AfterApplySync, FailAction::Fail)));
    let e = take(&mut h, &f2, target("v2.0.0", l2)).unwrap_err();
    arm_failpoint(None);
    assert_eq!(e.code, E_REDACTION_PENDING, "{e:?}");
    // the handle serves no content
    assert!(
        h.read()
            .block(
                &Scope::At(landing_of(&r, v1)),
                &BlockId::parse(&secret).unwrap()
            )
            .is_err()
    );
    drop(h);
    // a new snapshot repairs what the reply did not say: the tag and every key are there, and
    // the reopen finished the removal
    let mut h = Rdy::open_holder(&root).unwrap();
    assert!(refs(&h).contains(&("tag/v2.0.0".to_string(), v2.0.to_vec())));
    let keys = closure_keys(&r, v2);
    let seen = presence(&h.read(), &keys).unwrap();
    assert!(seen.iter().all(|p| p.present), "{seen:?}");
    assert!(!holds_canary(&root));
    h.fsck().unwrap();
    // the retry of the same upload appends nothing
    let end = h.store().log_end();
    for (fs, name, l, t) in [
        (&f2, "v2.0.0", l2, v2),
        (&f1, "v1.0.0", landing_of(&r, v1), v1),
    ] {
        let again = take(&mut h, fs, target(name, l)).unwrap();
        assert_eq!(
            (
                again.unchanged,
                again.frames_written,
                again.records_taken,
                again.tag_hash
            ),
            (true, 0, 0, t)
        );
        assert_eq!(h.store().log_end(), end);
    }
}

// ---- the relay-tag signature (O11) ----

/// A software ed25519 signer from a fixed seed (test only, as in `signature_check.rs`).
fn signing_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[0x61; 32])
}

fn key_data() -> KeyData {
    KeyData::Ed25519(Ed25519PublicKey(signing_key().verifying_key().to_bytes()))
}

fn sshsig(ns: &str, msg: &[u8]) -> Vec<u8> {
    let data = SshSig::signed_data(ns, HashAlg::Sha512, msg).unwrap();
    let sig = ssh_key::Signature::new(
        Algorithm::Ed25519,
        signing_key().sign(&data).to_bytes().to_vec(),
    )
    .unwrap();
    let mut out = vec![];
    SshSig::new(key_data(), ns, HashAlg::Sha512, sig)
        .unwrap()
        .encode(&mut out)
        .unwrap();
    out
}

/// Signs the relay statement of tag `t` in `r` (its relay answer is the one `answer/tag:<name>`
/// names in `r`) as made in repo `repo`, and stores the signature, made at `at_ms`.
fn sign_relay(r: &mut Rdy, t: Hash, repo: RepoId, at_ms: i64) {
    let record = r.store().read_tag(&t).unwrap();
    let (answer_hash, answer) = r
        .store()
        .answer_of(&format!("tag:{}", record.name))
        .unwrap()
        .unwrap();
    let mut blob = vec![];
    key_data().encode(&mut blob).unwrap();
    let stmt = statement(
        StatementSource::RelayTag {
            hash: t,
            tag: &record,
            answer_hash,
            answer: &answer,
        },
        Some(repo),
        &blob_fingerprint(&blob),
    )
    .unwrap();
    let ns = rdy_model::signature::parse_statement(&stmt)
        .unwrap()
        .namespace()
        .to_string();
    let g = Signature {
        subject: t,
        namespace: ns.clone(),
        sig: sshsig(&ns, &stmt),
        statement: stmt,
        signed_by: Provenance {
            actor: "agent:s".into(),
            run: Some("s/1".into()),
            role: Role::Writer,
            at_ms,
        },
    };
    r.store_mut().put_signature(&g).unwrap();
    r.seal_open().unwrap();
}

/// Tags canon's head `name` through a relay (an agent for the founder) and signs the relay
/// statement in `r`'s own repo; returns the tag.
fn relay_tag(r: &mut Rdy, name: &str) -> Hash {
    let agent = Session::agent("agent:a".into(), Some("run-1".into()), Role::Writer).unwrap();
    let relay = RelayInput {
        evidence: Some("Yes, tag it.".into()),
        ..RelayInput::default()
    };
    let info = r
        .tag_as(&agent, "human:founder", &relay, true, name, None, None)
        .unwrap();
    assert!(info.relayed_by.is_some());
    let t = tag_hash(r, name);
    let id = repo(r);
    sign_relay(r, t, id, 1_000);
    t
}

/// The relay answer of tag `name` in project `r`.
fn relay_answer(r: &Rdy, name: &str) -> Hash {
    r.store()
        .answer_of(&format!("tag:{name}"))
        .unwrap()
        .unwrap()
        .0
}

fn checks_json(checks: &[rdy_engine::signature_check::SignatureCheck]) -> Value {
    serde_json::to_value(checks).unwrap()
}

#[test]
fn ha13_holder_relay_tag_signature_checks_through_its_answer_ref() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _, _) = project(&dir.path().join("src"));
    let t = relay_tag(&mut r, "v1.0.0");
    let answer_hash = relay_answer(&r, "v1.0.0");
    let source = signature_checks(&r.read(), &t, None).unwrap();
    assert_eq!(source.len(), 1);
    assert_eq!(
        (
            source[0].signed.as_str(),
            source[0].statement.as_str(),
            source[0].code.as_deref()
        ),
        ("valid", "match", None),
        "{:?}",
        source[0]
    );

    let (_root, mut h) = holder(dir.path(), repo(&r));
    take(
        &mut h,
        &files(dir.path(), "s", &[&export(&r, t)]),
        target("v1.0.0", landing_of(&r, t)),
    )
    .unwrap();
    // in a holder the relay answer is found by the tag object, not by the tag name
    assert_eq!(
        refs(&h),
        vec![
            ("tag/v1.0.0".to_string(), t.0.to_vec()),
            (format!("answer/tag-object:{t}"), answer_hash.0.to_vec()),
        ]
    );
    let held = signature_checks(&h.read(), &t, None).unwrap();
    assert_eq!(checks_json(&held), checks_json(&source));
    h.fsck().unwrap();
}

#[test]
fn ha13_twin_tags_of_one_version_keep_their_relay_signatures() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, _, _) = twins(dir.path());
    // clones A and B of one repo, at one landing: A's plain 1.0.0, B's relayed and signed 1.0.0
    let ta = tag(&mut a, "1.0.0");
    let tb = relay_tag(&mut b, "1.0.0");
    let l = landing_of(&a, ta);
    assert_eq!(landing_of(&b, tb), l);
    assert_ne!(ta, tb);
    let source = signature_checks(&b.read(), &tb, None).unwrap();
    assert_eq!(
        (
            source.len(),
            source[0].signed.as_str(),
            source[0].statement.as_str()
        ),
        (1, "valid", "match")
    );
    let (root, mut h) = holder(dir.path(), repo(&a));
    take(
        &mut h,
        &files(dir.path(), "a", &[&export(&a, ta)]),
        target("1.0.0", l),
    )
    .unwrap();
    let got = take(
        &mut h,
        &files(dir.path(), "b", &[&export(&b, tb)]),
        target("1.0.0", l),
    )
    .unwrap();
    // the holder keeps A's tag as the version, and B's tag object with its relay answer
    assert_eq!((got.tag.as_str(), got.tag_hash), ("1.0.0", ta));
    let want = vec![
        ("tag/1.0.0".to_string(), ta.0.to_vec()),
        (
            format!("answer/tag-object:{tb}"),
            relay_answer(&b, "1.0.0").0.to_vec(),
        ),
    ];
    assert_eq!(refs(&h), want);
    // B's signature checks in the holder as it does in B
    let held = signature_checks(&h.read(), &tb, None).unwrap();
    assert_eq!(checks_json(&held), checks_json(&source));
    h.fsck().unwrap();
    // a repeat writes nothing
    let again = take(
        &mut h,
        &files(dir.path(), "b2", &[&export(&b, tb)]),
        target("1.0.0", l),
    )
    .unwrap();
    assert!(again.unchanged, "{again:?}");
    // durable: the reopened and reindexed holder reads the same
    drop(h);
    let mut h = Rdy::open_holder(&root).unwrap();
    h.reindex().unwrap();
    assert_eq!(refs(&h), want);
    let held = signature_checks(&h.read(), &tb, None).unwrap();
    assert_eq!(checks_json(&held), checks_json(&source));
    h.fsck().unwrap();
}

#[test]
fn ha13_holder_shows_signatures_of_another_repo_as_unverified() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _, _) = project(&dir.path().join("src"));
    let t = relay_tag(&mut r, "v1.0.0");
    let first = repo(&r);
    // forked twice with the real clone API
    let st = r.read();
    rdy_engine::clone::clone(
        rdy_engine::clone::CloneSource::State(&st),
        &dir.path().join("f1"),
        true,
    )
    .unwrap();
    drop(st);
    let f1 = Rdy::open(&dir.path().join("f1")).unwrap();
    let st = f1.read();
    rdy_engine::clone::clone(
        rdy_engine::clone::CloneSource::State(&st),
        &dir.path().join("f2"),
        true,
    )
    .unwrap();
    drop(st);
    let mut f2 = Rdy::open(&dir.path().join("f2")).unwrap();
    let last = repo(&f2);
    assert_eq!(
        BTreeSet::from([first, repo(&f1), last]).len(),
        3,
        "each fork has a new repo id"
    );
    // in the last fork the first signature verifies through the recorded fork lineage; a
    // second signature of the same relay is made in the last fork
    sign_relay(&mut f2, t, last, 2_000);
    let source = signature_checks(&f2.read(), &t, None).unwrap();
    assert_eq!(source.len(), 2);
    assert!(
        source
            .iter()
            .all(|c| (c.signed.as_str(), c.statement.as_str()) == ("valid", "match")),
        "{source:?}"
    );
    f2.fsck().unwrap();

    // a holder of the last fork's repo id: it holds no fork history and trusts none
    let (_root, mut h) = holder(dir.path(), last);
    take(
        &mut h,
        &files(dir.path(), "f", &[&export(&f2, t)]),
        target("v1.0.0", landing_of(&f2, t)),
    )
    .unwrap();
    let held = signature_checks(&h.read(), &t, None).unwrap();
    assert_eq!(held.len(), 2);
    let json = checks_json(&held);
    for (i, check) in held.iter().enumerate() {
        let names = check.foreign_repo.is_some();
        if names {
            // signed in the first project: valid over its subject, not verified here
            assert_eq!(
                (
                    check.signed.as_str(),
                    check.statement.as_str(),
                    check.trust.as_str(),
                    check.code.as_deref(),
                    check.foreign_repo.as_deref(),
                ),
                (
                    "valid",
                    "foreign_repo",
                    "unknown",
                    None,
                    Some(first.to_string().as_str())
                ),
                "{check:?}"
            );
            assert!(check.warnings.is_empty(), "{check:?}");
            assert_eq!(json[i]["foreign_repo"], first.to_string());
            let line = rdy_engine::signing_read::signature_line(&json[i]);
            assert!(
                line.contains(&format!(
                    "signed in another project ({first}): not verified here"
                )),
                "{line}"
            );
        } else {
            // made in the last fork: verifies as in its source
            let same = source
                .iter()
                .find(|c| c.at_ms == check.at_ms)
                .expect("the same signature in the source");
            assert_eq!(check, same);
            assert!(json[i].get("foreign_repo").is_none(), "{}", json[i]);
        }
    }
    assert_eq!(held.iter().filter(|c| c.foreign_repo.is_some()).count(), 1);
    // holder fsck passes, and counts the signature it cannot verify as information
    let report = h.fsck().unwrap();
    assert_eq!((report.signatures, report.foreign_signatures), (2, 1));
    let v = serde_json::to_value(&report).unwrap();
    assert_eq!(v["foreign_signatures"], 1);
}

// ---- the structural rules a holder's fsck keeps, checked before the append ----

/// Tags `landing` in `r`'s store as `name` (no rule of `rdy tag`), seals it, and returns the
/// whole closure as one batch that passes its own check.
fn forged_upload(r: &mut Rdy, name: &str, landing: Hash, by: Provenance) -> (Hash, Vec<u8>) {
    let t = r
        .store_mut()
        .put_tag(&Tag {
            name: name.into(),
            landing,
            message: None,
            tagged_by: by,
        })
        .unwrap();
    r.seal_open().unwrap();
    let batch = split(r, t, 1).remove(0);
    check_batch(batch.as_slice(), Some(repo(r))).unwrap();
    (t, batch)
}

const STRUCTURE_FIX: &str = "upload the history as its source project exported it";

#[test]
fn ha13_take_keeps_the_landing_and_chain_rules() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _, _) = project(&dir.path().join("src"));
    let v1 = tag(&mut r, "v1.0.0");
    let l = landing_of(&r, v1);
    let rec = r.store().landing(&l).unwrap();
    let (root, mut h) = holder(dir.path(), repo(&r));
    take(
        &mut h,
        &files(dir.path(), "v1", &[&export(&r, v1)]),
        target("v1.0.0", l),
    )
    .unwrap();
    // a landing whose assurance is relayed, with no relay fields or evidence
    let relayed = r
        .store_mut()
        .put(
            kinds::LANDING,
            &LandingRecord {
                assurance: Assurance::Relayed,
                relayed_by: None,
                relay_evidence: None,
                evidence: None,
                ..rec.clone()
            },
        )
        .unwrap();
    // a landing on top of `l` that lands `l`'s changes again
    let again = r
        .store_mut()
        .put(
            kinds::LANDING,
            &LandingRecord {
                prev: Some(l),
                ..rec.clone()
            },
        )
        .unwrap();
    let c = rec.changes[0];
    for (name, landing, message) in [
        (
            "v9.0.0",
            relayed,
            format!("landing {relayed}: assurance is relayed but the relay fields are unset"),
        ),
        (
            "v9.1.0",
            again,
            format!("tag/v9.1.0: change {c} lands in landing {again} and in landing {l}"),
        ),
    ] {
        let (_, batch) = forged_upload(&mut r, name, landing, rec.approved_by.clone());
        let before = unchanged(&root, &h);
        let e = take(
            &mut h,
            &files(dir.path(), name, &[&batch]),
            target(name, landing),
        )
        .unwrap_err();
        assert_eq!(
            refusal(&e),
            ("E_BAD_RECORD", message.as_str(), STRUCTURE_FIX),
            "{name}"
        );
        assert_eq!(unchanged(&root, &h), before, "{name}");
    }
    h.fsck().unwrap();
}

#[test]
fn ha13_take_keeps_the_blob_chunk_lengths() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, doc, _) = project(&dir.path().join("src"));
    let v1 = tag(&mut r, "v1.0.0");
    let l = landing_of(&r, v1);
    let rec = r.store().landing(&l).unwrap();
    let (root, mut h) = holder(dir.path(), repo(&r));
    // one correctly hashed chunk of three bytes
    let chunk = r
        .store_mut()
        .write(vec![(kinds::BLOB_CHUNK, vec![1, 2, 3])])
        .unwrap()[0];
    r.seal_open().unwrap();
    // a canonical manifest of one chunk that claims four bytes (short), then two (oversized),
    // reached through a figure in the landing's snapshot
    for (name, len) in [("v9.0.0", 4u64), ("v9.1.0", 2)] {
        let manifest = r
            .store_mut()
            .put(
                kinds::BLOB_MANIFEST,
                &kinds::BlobManifest {
                    len,
                    chunks: vec![chunk],
                },
            )
            .unwrap();
        let figure = Block {
            id: BlockId::parse("c-FGR01").unwrap(),
            block_type: "figure".into(),
            parent: Some(BlockId::parse(&doc).unwrap()),
            order: "W".into(),
            fields: BTreeMap::from([
                ("blob".to_string(), FieldValue::Str(manifest.to_string())),
                (
                    "media_type".to_string(),
                    FieldValue::Str("application/octet-stream".into()),
                ),
                ("alt".to_string(), FieldValue::Str("fig".into())),
            ]),
            body: String::new(),
            links: BTreeMap::new(),
            pins: BTreeMap::new(),
            read_identity: None,
        };
        let version = r.store_mut().put(kinds::BLOCK, &figure).unwrap();
        let mut snap = r.store().state_of(Some(l)).unwrap();
        snap.blocks.insert(figure.id.clone(), version);
        let s = r.store_mut().put(kinds::SNAPSHOT, &snap).unwrap();
        let landing = r
            .store_mut()
            .put(
                kinds::LANDING,
                &LandingRecord {
                    state: s,
                    ..rec.clone()
                },
            )
            .unwrap();
        let (_, batch) = forged_upload(&mut r, name, landing, rec.approved_by.clone());
        let before = unchanged(&root, &h);
        let e = take(
            &mut h,
            &files(dir.path(), name, &[&batch]),
            target(name, landing),
        )
        .unwrap_err();
        assert_eq!(
            refusal(&e),
            (
                "E_BAD_RECORD",
                format!("blob {manifest} has a chunk of 3 bytes where {len} are due").as_str(),
                STRUCTURE_FIX
            ),
            "{name}"
        );
        assert_eq!(unchanged(&root, &h), before, "{name}");
    }
    // the holder holds none of it, and its fsck passes
    assert!(!h.store().has_kind(kinds::BLOB_CHUNK, &chunk).unwrap());
    h.fsck().unwrap();
}
