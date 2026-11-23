//! hfp2 (D-99CWC, D-DZDB7, D-HX6BX, D-CDQF7): a hub forks a holder snapshot at a captured tag
//! through the adapter. The fork is a new holder with a new repo id and the ordered counted
//! origins. Its canon is the tag's landing, and it names only the tags of that landing's chain.
//! It holds no source view, held chain or replaced mark. Later removal and dispute records stay
//! as proof records, and no read reaches them. A refusal leaves the source and the target as
//! they were. A fork's baselines compare across the main line, the line and its own fork.
//!
//! The source projects are built with core calls and closed before an adapter handle opens a
//! store. Every hub write goes through the adapter.

use crate::hub_world::{
    AT, check, copy_dir, doc, export, files, land, moved, project, repo_of, session, tag,
    view_head_of, write_in,
};
use rdy_adapter::history::{Dispute, DisputeOutcome, DisputeResolution};
use rdy_adapter::publication::{Baseline, BaselineBinding, PreviousChoice};
use rdy_adapter::stream::{ObjectKey, StreamPurpose};
use rdy_adapter::sync::{CanonMove, Moved};
use rdy_adapter::{Hash, Origin, RdyError, RepoId, Snapshot, StoreHandle, UploadTarget};
use rdy_engine::Rdy;
use rdy_engine::fork::ForkStep;
use rdy_engine::redaction::RedactInput;
use rdy_model::{Provenance, Role};
use rdy_store::LandRequest;
use rdy_store::sync_stream::{StreamReader, StreamRecord, StreamWriter};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------------------
// source projects (core)

/// Removes every version of block `target` in a landing of its own; returns that landing.
fn remove(r: &mut Rdy, target: &str) -> Hash {
    let s = session(Role::Writer);
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
            assessment: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
    r.store().reader().canon_head().unwrap().unwrap()
}

fn person(at_ms: i64) -> Provenance {
    Provenance {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        at_ms,
    }
}

// ---------------------------------------------------------------------------------------------
// the main hub

/// The main hub and its source. Project P lands `p1` (with a secret block) as v1.0.0 and `p2`
/// as v2.0.0; Q, a copy of P at `p1`, lands `q2` and `q3`. P removes the secret in `p3`
/// (v3.0.0), lands the correction `p4`, and records an upheld dispute of `p1` that `p4`
/// corrects. The hub seeds `p1`, extends to `p2`, forces `q2` (replacing `p2`), forces `p4`
/// with the removal (replacing `q2`), holds `q3` and keeps P's pushed view `draft`.
struct Main {
    dir: tempfile::TempDir,
    hub: StoreHandle,
    repo: RepoId,
    p1: Hash,
    p2: Hash,
    p3: Hash,
    p4: Hash,
    q2: Hash,
    q3: Hash,
    /// The secret block's id in P.
    secret: String,
    /// A second removal of P (`p5`, of `later`), pushed as `p4`..`p5`, not taken yet.
    p5: Hash,
    e5: PathBuf,
}

impl Main {
    fn hub_dir(&self) -> PathBuf {
        self.dir.path().join("hub")
    }

    fn at(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Takes the second removal into the hub: canon moves from `p4` to `p5`.
    fn take_second_removal(&self) -> Result<(), RdyError> {
        let s = self.hub.snapshot()?;
        let c = s.check_quarantine(&files(&[&self.e5]))?;
        assert_eq!(
            self.hub.move_canon(&c, CanonMove::Extend, Some(self.p4))?,
            moved(self.p5)
        );
        Ok(())
    }
}

fn main_hub() -> Main {
    let _clock = rdy_engine::pin_clock(AT);
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().join("p");
    let qd = dir.path().join("q");
    let mut p = project(&pd);
    let repo = repo_of(&p);
    let w = doc(&mut p, "work", "Intro", "Public words.");
    let secret = write_in(
        &mut p,
        "work",
        json!([{"op": "create", "type": "prose", "id": "$x", "parent": w.created["$s"], "body": "The launch code is ZEBRA-7731."}]),
    )
    .created["$x"]
        .clone();
    let p1 = land(&mut p, "work");
    tag(&mut p, "v1.0.0");
    p.close().unwrap();
    copy_dir(&pd, &qd);

    let mut p = Rdy::open(&pd).unwrap();
    let later = doc(&mut p, "w1", "Later", "Words after the first version.").created["$p"].clone();
    let p2 = land(&mut p, "w1");
    tag(&mut p, "v2.0.0");
    doc(&mut p, "draft", "Draft", "Work in flight.");
    let d1 = view_head_of(&p, "draft");
    let mut draft_root = d1;
    while let Some(prev) = p.store().reader().view_op(&draft_root).unwrap().prev {
        draft_root = prev;
    }
    let p3 = remove(&mut p, &secret);
    tag(&mut p, "v3.0.0");
    doc(&mut p, "w4", "Fix", "The corrected words.");
    let p4 = land(&mut p, "w4");
    let (dispute, _) = p
        .store_mut()
        .put_dispute(
            &Dispute {
                landing: p1,
                opened_by: person(10),
                reason: "the first version leaked a code".into(),
                at_ms: 10,
            },
            None,
        )
        .unwrap();
    p.store_mut()
        .put_dispute_resolution(
            &DisputeResolution {
                dispute,
                outcome: DisputeOutcome::Upheld,
                by: person(20),
                correction: Some(p4),
            },
            None,
        )
        .unwrap();
    p.seal_open().unwrap();
    p.invalidate();
    p.close().unwrap();

    let mut q = Rdy::open(&qd).unwrap();
    doc(&mut q, "w2", "Other", "Another chain.");
    let q2 = land(&mut q, "w2");
    doc(&mut q, "w3", "Held", "A chain the hub holds beside canon.");
    let q3 = land(&mut q, "w3");
    q.close().unwrap();

    let at = |n: &str| dir.path().join(format!("{n}.rdysync"));
    let canon = StreamPurpose::CanonHistory;
    export(&pd, canon, p1, None, &at("e1"));
    export(&pd, canon, p2, Some(p1), &at("e2"));
    export(&qd, canon, q2, Some(p1), &at("eq2"));
    export(&pd, canon, p4, Some(p2), &at("e4"));
    export(&qd, canon, q3, Some(q2), &at("eq3"));
    export(&pd, StreamPurpose::ViewHistory, d1, None, &at("v1"));

    let hub = StoreHandle::create_holder(&dir.path().join("hub"), repo).unwrap();
    let step = |path: &Path, mv: CanonMove, expected: Option<Hash>, to: Hash| {
        let s = hub.snapshot().unwrap();
        let c = check(&s, &[path]).unwrap();
        assert_eq!(hub.move_canon(&c, mv, expected).unwrap(), moved(to));
    };
    step(&at("e1"), CanonMove::Seed, None, p1);
    step(&at("e2"), CanonMove::Extend, Some(p1), p2);
    step(&at("eq2"), CanonMove::Force, Some(p2), q2);
    step(&at("e4"), CanonMove::Force, Some(q2), p4);
    step(&at("eq3"), CanonMove::Hold, Some(p4), q3);
    step(
        &at("v1"),
        CanonMove::SetViewHead {
            name: "draft".into(),
            root: draft_root,
        },
        None,
        d1,
    );

    // a second removal, ready to push
    let mut p = Rdy::open(&pd).unwrap();
    let p5 = remove(&mut p, &later);
    p.close().unwrap();
    export(&pd, canon, p5, Some(p4), &at("e5"));
    Main {
        e5: at("e5"),
        dir,
        hub,
        repo,
        p1,
        p2,
        p3,
        p4,
        q2,
        q3,
        secret,
        p5,
    }
}

// ---------------------------------------------------------------------------------------------
// reads

fn tag_rows(snap: &Snapshot) -> Vec<(String, Hash)> {
    snap.tags()
        .unwrap()
        .into_iter()
        .map(|(_, t)| (t.name, t.landing))
        .collect()
}

/// The refs of `prefix` in the closed holder at `dir`.
fn refs_of(dir: &Path, prefix: &str) -> Vec<String> {
    let r = Rdy::open_holder(dir).unwrap();
    let out = r
        .store()
        .reader()
        .refs_with_prefix(prefix)
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    r.close().unwrap();
    out
}

/// The keys of the canon history at `landing` that `snap` exports with no base.
fn exported_keys(snap: &Snapshot, landing: Hash) -> Vec<ObjectKey> {
    snap.write_history(
        StreamPurpose::CanonHistory,
        landing,
        None,
        &BTreeSet::new(),
        std::io::sink(),
    )
    .unwrap()
    .keys
}

/// Each object of `keys` as the closed holder at `dir` holds it: its raw kind and bytes, or
/// None for a removed one.
fn raw_objects(dir: &Path, keys: &[ObjectKey]) -> Vec<Option<(u8, Vec<u8>)>> {
    let r = Rdy::open_holder(dir).unwrap();
    let out = keys
        .iter()
        .map(|k| match r.store().reader().get_kind_raw(k.kind, &k.hash) {
            Ok(raw) => Some(raw),
            Err(rdy_store::StoreError::Redacted { .. }) => None,
            Err(e) => panic!("{k}: {e:?}"),
        })
        .collect();
    r.close().unwrap();
    out
}

/// The entries of `dir`'s parent whose names start with the fork's staging prefix.
fn staging_left(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".rdy-fork-"))
        .collect()
}

fn landing_key(hash: Hash) -> ObjectKey {
    ObjectKey {
        kind: rdy_store::kinds::LANDING,
        hash,
    }
}

// ---------------------------------------------------------------------------------------------
// the tests

/// D-99CWC, D-DZDB7, D-HX6BX: the fork of v1.0.0 is a holder with a new repo id, the source's
/// counted origins and its own, canon at v1.0.0's landing, and only v1.0.0 named. No source
/// view, held chain or replaced mark comes with it, and every carried object keeps its bytes.
#[test]
fn hfp2_fork_stops_at_the_captured_tag() {
    let m = main_hub();
    let src = m.hub.snapshot().unwrap();
    assert_eq!(src.canon(), Some(m.p4));
    assert!(!src.view_heads().unwrap().is_empty());
    let names: Vec<String> = tag_rows(&src).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["v1.0.0", "v2.0.0", "v3.0.0"]);

    let dir = m.at("line");
    let line = src.fork_at("1.0.0", &dir).unwrap();
    let snap = line.snapshot().unwrap();
    let id = snap.identity();
    assert_ne!(id.repo_id, m.repo);
    assert_eq!(
        id.origin,
        Some(Origin {
            fork: true,
            repo_id: m.repo,
            tag: Some("v1.0.0".into()),
            landing: Some(m.p1),
        })
    );
    assert_eq!(snap.canon(), Some(m.p1));
    assert_eq!(tag_rows(&snap), [("v1.0.0".to_string(), m.p1)]);
    assert!(snap.view_heads().unwrap().is_empty());
    assert!(snap.features().has("hub_canon"));
    line.fsck().unwrap();

    // the source is as it was
    let after = m.hub.snapshot().unwrap();
    assert_eq!(after.canon(), Some(m.p4));
    assert_eq!(tag_rows(&after), tag_rows(&src));

    let keys = exported_keys(&src, m.p1);
    let hub_dir = m.hub_dir();
    drop((snap, after, src));
    line.close().unwrap();
    m.hub.close().unwrap();

    // a holder, whose creating batch holds the origin
    let r = Rdy::open_holder(&dir).unwrap();
    assert_eq!(r.store().mode().unwrap(), rdy_adapter::StoreMode::Holder);
    assert!(!dir.join(".rdy.yaml").exists());
    let reader = r.store().reader();
    assert_eq!(
        reader
            .counted_fork_origins_before(rdy_store::closure::END_OF_LOG)
            .unwrap(),
        [Origin {
            fork: true,
            repo_id: m.repo,
            tag: Some("v1.0.0".into()),
            landing: Some(m.p1),
        }]
    );
    // the tag refs name only landings on canon's chain
    for (name, value) in reader.refs_with_prefix("tag/").unwrap() {
        let t = reader.read_tag(&Hash(value.try_into().unwrap())).unwrap();
        assert_eq!((name.as_str(), t.landing), ("tag/v1.0.0", m.p1));
    }
    drop(reader);
    r.close().unwrap();
    for prefix in ["view/", "held/", "replaced/"] {
        assert!(refs_of(&dir, prefix).is_empty(), "{prefix}");
    }
    assert!(!refs_of(&hub_dir, "held/").is_empty());
    assert!(!refs_of(&hub_dir, "replaced/").is_empty());

    // every carried object keeps its source bytes and hash, a removed one stays removed
    let ours = raw_objects(&dir, &keys);
    assert_eq!(ours, raw_objects(&hub_dir, &keys));
    for (k, raw) in keys.iter().zip(&ours) {
        if let Some((_, bytes)) = raw {
            assert_eq!(Hash::of(bytes), k.hash, "{k}");
        }
    }
    assert!(ours.iter().any(Option::is_none), "the removal travels");
    assert!(staging_left(&dir).is_empty());
}

/// D-99CWC: the removal after v1.0.0 (`p3`) and the correction of v1.0.0's dispute (`p4`)
/// travel as proof records. The secret stays removed, the later landings are stored, and no
/// read reaches them: canon is `p1`, and their facts are `E_NOT_FOUND`. The later tags are
/// stored records with no ref.
#[test]
fn hfp2_later_proofs_stay_applied_and_unreachable() {
    let m = main_hub();
    let src = m.hub.snapshot().unwrap();
    let dir = m.at("line");
    let line = src.fork_at("v1.0.0", &dir).unwrap();
    let snap = line.snapshot().unwrap();

    // the later landings are held, and none is a read root
    let later = [m.p2, m.p3, m.p4];
    let keys: Vec<ObjectKey> = later.iter().map(|h| landing_key(*h)).collect();
    let held = snap.has_objects(&keys).unwrap();
    assert!(held.iter().all(|p| p.present && !p.redacted), "{held:?}");
    for h in later {
        assert_eq!(
            snap.landing_facts(h).unwrap_err().code,
            "E_NOT_FOUND",
            "{h}"
        );
    }
    let chain: Vec<Hash> = snap
        .canon_history()
        .unwrap()
        .into_iter()
        .map(|f| f.hash)
        .collect();
    assert_eq!(chain, [m.p1]);
    // the other source chains do not come
    let other = snap
        .has_objects(&[landing_key(m.q2), landing_key(m.q3)])
        .unwrap();
    assert!(other.iter().all(|p| !p.present), "{other:?}");

    // the dispute of v1.0.0 and its resolution come with it
    let disputes = snap.disputes(&[m.p1]).unwrap();
    assert_eq!(disputes.len(), 1);
    assert_eq!(
        disputes[0].resolution.as_ref().unwrap().1.correction,
        Some(m.p4)
    );

    // the secret stays removed in the fork as in the source
    let secret_versions: Vec<ObjectKey> = exported_keys(&src, m.p1)
        .into_iter()
        .filter(|k| {
            let p = src.has_objects(&[*k]).unwrap();
            p[0].redacted
        })
        .collect();
    assert!(!secret_versions.is_empty(), "{}", m.secret);
    for p in snap.has_objects(&secret_versions).unwrap() {
        assert!(p.present && p.redacted, "{p:?}");
    }
    assert!(line.redaction_warnings().unwrap().is_empty());
    drop((snap, src));
    line.close().unwrap();
    m.hub.close().unwrap();

    // the later tags are stored, and only v1.0.0 has a ref
    let r = Rdy::open_holder(&dir).unwrap();
    let reader = r.store().reader();
    let names: Vec<String> = reader
        .refs_with_prefix("tag/")
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(names, ["tag/v1.0.0"]);
    let mut stored = Vec::new();
    reader
        .for_each_object_of(rdy_store::kinds::TAG, |h| {
            stored.push(h);
            Ok(())
        })
        .unwrap();
    assert_eq!(stored.len(), 3, "v1.0.0, v2.0.0 and v3.0.0 travel");
    drop(reader);
    r.close().unwrap();
}

/// D-99CWC: every refusal and failure leaves the source and the target as they were, and no
/// staging directory is left. Input refusals come first; a bad proof, a seed that fails, a
/// placement that fails and a removal before the install each fail the fork. A snapshot that
/// a removal made stale is refused. The fork's own repo keeps the ordinary take's repo check.
#[test]
fn hfp2_failure_leaves_source_and_target_unchanged() {
    let m = main_hub();
    let src = m.hub.snapshot().unwrap();
    let unchanged = |m: &Main| {
        let s = m.hub.snapshot().unwrap();
        assert_eq!(s.canon(), Some(m.p4));
        assert_eq!(tag_rows(&s).len(), 3);
        m.hub.fsck().unwrap();
    };

    // a project snapshot
    let project = StoreHandle::open(&m.at("p")).unwrap();
    let e = project
        .snapshot()
        .unwrap()
        .fork_at("v1.0.0", &m.at("t0"))
        .err()
        .unwrap();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_BAD_INPUT",
            "this store is a project",
            "open it through the project adapter"
        )
    );
    project.close().unwrap();
    assert!(!m.at("t0").exists());

    // a tag the snapshot lacks, and a name that is not a version
    let e = src.fork_at("v9.0.0", &m.at("t1")).err().unwrap();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_NOT_FOUND",
            "no version v9.0.0",
            "list versions with rdy tags"
        )
    );
    let e = src.fork_at("nope", &m.at("t1")).err().unwrap();
    assert_eq!(e.code, "E_BAD_TAG_NAME");
    assert!(!m.at("t1").exists());

    // a target that holds a file, a target with a store, a target in the source's .rdy
    let full = m.at("full");
    std::fs::create_dir_all(&full).unwrap();
    std::fs::write(full.join("keep.txt"), b"mine").unwrap();
    let e = src.fork_at("v1.0.0", &full).err().unwrap();
    assert_eq!(
        (e.code, e.message, e.fix.as_str()),
        (
            "E_ALREADY_EXISTS",
            format!("{} is not empty", full.display()),
            "fork into a new directory, or an empty one"
        )
    );
    assert_eq!(std::fs::read(full.join("keep.txt")).unwrap(), b"mine");
    let e = src.fork_at("v1.0.0", &m.hub_dir()).err().unwrap();
    assert_eq!(e.code, "E_ALREADY_EXISTS");
    assert!(e.message.ends_with("already has a store"), "{e:?}");
    let inside = m.hub_dir().join(".rdy").join("fork");
    assert!(src.fork_at("v1.0.0", &inside).is_err());
    assert!(!inside.exists());
    unchanged(&m);

    // a batch whose removal proof does not fit its dropped record
    let target = m.at("t2");
    let mut forge = |step: ForkStep<'_>| {
        if let ForkStep::Exported(batch) = step {
            forge_drop(batch);
        }
        Ok(())
    };
    let e = src
        .fork_at_with_hook_for_tests("v1.0.0", &target, &mut forge)
        .err()
        .unwrap();
    assert_eq!(e.code, "E_BAD_RECORD", "{e:?}");
    assert!(e.message.contains("invalid removal proof"), "{e:?}");
    assert!(!target.exists());
    assert!(staging_left(&target).is_empty());
    unchanged(&m);

    // a seed that fails: canon is set before it
    let p1 = m.p1;
    let mut seeded = |step: ForkStep<'_>| {
        if let ForkStep::Taken(store) = step {
            let batch = store.reader().check_move_batch(vec![], p1).unwrap();
            store
                .move_canon(&batch, &rdy_store::hub_canon::CanonMove::Seed, None, AT)
                .unwrap();
        }
        Ok(())
    };
    let e = src
        .fork_at_with_hook_for_tests("v1.0.0", &target, &mut seeded)
        .err()
        .unwrap();
    assert_eq!(e.code, "E_INTERNAL", "{e:?}");
    assert!(!target.exists());
    assert!(staging_left(&target).is_empty());

    // a placement that fails: the target becomes a file before the rename, and the install
    // refuses it itself (Windows' rename would replace the file)
    let mut placed = |step: ForkStep<'_>| {
        if let ForkStep::Closed(at) = step {
            std::fs::write(at, b"not a directory").unwrap();
        }
        Ok(())
    };
    let e = src
        .fork_at_with_hook_for_tests("v1.0.0", &target, &mut placed)
        .err()
        .unwrap();
    assert_eq!(e.code, "E_IO", "{e:?}");
    assert!(e.message.ends_with(": not a directory"), "{e:?}");
    assert_eq!(std::fs::read(&target).unwrap(), b"not a directory");
    std::fs::remove_file(&target).unwrap();
    assert!(staging_left(&target).is_empty());
    unchanged(&m);

    // a removal in the source before the install
    let mut removed = |step: ForkStep<'_>| {
        if let ForkStep::Closed(_) = step {
            m.take_second_removal()?;
        }
        Ok(())
    };
    let e = src
        .fork_at_with_hook_for_tests("v1.0.0", &target, &mut removed)
        .err()
        .unwrap();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_REDACTED_STATE",
            "This read state predates content removal.",
            "Read the project again, or run the dump again."
        )
    );
    assert!(!target.exists());
    assert!(staging_left(&target).is_empty());
    assert_eq!(m.hub.snapshot().unwrap().canon(), Some(m.p5));

    // the stale snapshot is refused before anything is written
    let e = src.fork_at("v1.0.0", &target).err().unwrap();
    assert_eq!(e.code, "E_REDACTED_STATE");
    assert!(!target.exists());
    drop(src);

    // a fresh snapshot forks; the fork keeps the ordinary repo check
    let fresh = m.hub.snapshot().unwrap();
    let line = fresh.fork_at("v1.0.0", &target).unwrap();
    let upload = m.at("upload.rdysync");
    fresh
        .write_stream("v1.0.0", &BTreeSet::new(), File::create(&upload).unwrap())
        .unwrap();
    let e = line
        .take(
            &files(&[&upload]),
            UploadTarget {
                tag: "v1.0.0".into(),
                landing: m.p1,
            },
        )
        .err()
        .unwrap();
    assert_eq!(e.code, "E_REPO_MISMATCH", "{e:?}");
    let push = m.at("push.rdysync");
    fresh
        .write_history(
            StreamPurpose::CanonHistory,
            m.p5,
            None,
            &BTreeSet::new(),
            File::create(&push).unwrap(),
        )
        .unwrap();
    let e = line
        .snapshot()
        .unwrap()
        .check_quarantine(&files(&[&push]))
        .err()
        .unwrap();
    assert_eq!(e.code, "E_REPO_MISMATCH", "{e:?}");
    assert_eq!(line.snapshot().unwrap().canon(), Some(m.p1));
    drop(fresh);
    line.close().unwrap();
    m.hub.close().unwrap();
}

/// Rewrites the batch at `path` with its first dropped record one byte longer than its removal
/// proof approves.
fn forge_drop(path: &Path) {
    let (mut reader, header) = StreamReader::open(File::open(path).unwrap()).unwrap();
    let mut records = Vec::new();
    while let Some(r) = reader.next().unwrap() {
        records.push(r);
    }
    reader.finish().unwrap();
    let first = records
        .iter_mut()
        .find_map(|r| match r {
            StreamRecord::Dropped { original_len, .. } => Some(original_len),
            StreamRecord::Present { .. } => None,
        })
        .expect("the export carries a removal");
    *first += 1;
    let mut w = StreamWriter::new(File::create(path).unwrap(), &header).unwrap();
    for r in &records {
        w.record(r).unwrap();
    }
    w.finish().unwrap();
}

// ---------------------------------------------------------------------------------------------
// lineage

/// A project with the org profile `site` over `pubs`. v1.0.0: `pubs` holds the doc `base`.
/// v2.0.0: `base` has other text, and `added` holds words the project removes later. Returns
/// the landings and the block ids of `added` and its prose.
struct Published {
    v1: Hash,
    v2: Hash,
    added: String,
}

fn published(dir: &Path) -> Published {
    let mut r = project(dir);
    let w = write_in(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$pubs", "fields": {"name": "pubs"}},
            {"op": "create", "type": "doc", "id": "$base", "parent": "$pubs", "fields": {"name": "base", "title": "Base", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$bp", "parent": "$base", "body": "Base text."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let id = |n: &str| w.created[n].clone();
    write_in(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "publication_profile", "parent": id("$admin"),
             "fields": {"key": "site", "title": "Site", "audience": "org", "select": [id("$pubs")]}}
        ]),
    );
    let v1 = land(&mut r, "work");
    tag(&mut r, "v1.0.0");
    let added = write_in(
        &mut r,
        "work",
        json!([
            {"op": "update", "id": id("$bp"), "body": "Base text, changed."},
            {"op": "create", "type": "doc", "id": "$added", "parent": id("$pubs"), "fields": {"name": "added", "title": "Quiet Harbor Plan", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$added", "body": "The harbor opens at dawn."}
        ]),
    );
    let v2 = land(&mut r, "work");
    tag(&mut r, "v2.0.0");
    r.close().unwrap();
    Published {
        v1,
        v2,
        added: added.created["$added"].clone(),
    }
}

/// A hub of the project at `pd` (repo `repo`) at `dir`, seeded with `v1` and extended to `v2`.
fn hub_of(pd: &Path, repo: RepoId, p: &Published, dir: &Path) -> StoreHandle {
    let e1 = dir.with_extension("e1.rdysync");
    let e2 = dir.with_extension("e2.rdysync");
    export(pd, StreamPurpose::CanonHistory, p.v1, None, &e1);
    export(pd, StreamPurpose::CanonHistory, p.v2, Some(p.v1), &e2);
    let hub = StoreHandle::create_holder(dir, repo).unwrap();
    for (path, mv, expected, to) in [
        (&e1, CanonMove::Seed, None, p.v1),
        (&e2, CanonMove::Extend, Some(p.v1), p.v2),
    ] {
        let s = hub.snapshot().unwrap();
        let c = check(&s, &[path]).unwrap();
        assert_eq!(hub.move_canon(&c, mv, expected).unwrap(), moved(to));
    }
    hub
}

fn binding(b: &Baseline) -> BaselineBinding {
    BaselineBinding {
        repo_id: b.repo_id(),
        tag: b.tag().to_string(),
        profile_version: b.profile_version(),
        selection_identity: b.selection_identity(),
    }
}

/// Compares `candidate`'s `tag` against `baseline` through the adapter.
fn against(
    candidate: &Snapshot,
    tag: &str,
    baseline: &Baseline,
) -> Result<rdy_adapter::publication::PublicationCheck, RdyError> {
    candidate.disclosure_against(
        tag,
        "site",
        Some(PreviousChoice::Baseline {
            baseline,
            binding: &binding(baseline),
        }),
    )
}

/// The lineage text of D-CDQF7.
fn outside(e: RdyError) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_PUBLICATION_BASELINE",
            "The publication baseline comes from a project outside this project's lineage.",
            "Use a baseline from this project or from a project that shares a fork origin with it."
        )
    );
}

/// D-CDQF7, D-HX6BX, D-W6WA5: main M, its line L (a fork of M at v1.0.0) and L's line LL (a
/// fork of L) compare baselines both ways, and L with LL, through `publication_baseline` and
/// `disclosure_against`. The binding is checked first. An unrelated hub and a forged late
/// origin share no lineage. After main removes its later publication's words, its remade
/// baseline shows the line no removed title or content.
#[test]
fn hfp2_baselines_cross_main_line_and_grandchild_both_ways() {
    let _clock = rdy_engine::pin_clock(AT);
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().join("p");
    let pub_p = published(&pd);
    let repo = {
        let r = Rdy::open(&pd).unwrap();
        let id = repo_of(&r);
        r.close().unwrap();
        id
    };
    let m = hub_of(&pd, repo, &pub_p, &dir.path().join("m"));
    let ms = m.snapshot().unwrap();
    let l = ms.fork_at("v1.0.0", &dir.path().join("l")).unwrap();
    let ls = l.snapshot().unwrap();
    let ll = ls.fork_at("v1.0.0", &dir.path().join("ll")).unwrap();
    let lls = ll.snapshot().unwrap();
    assert_eq!(
        lls.identity().origin.as_ref().unwrap().repo_id,
        ls.identity().repo_id
    );

    // an unrelated hub: a copy of the project under another repo id, whose own fork origin
    // names a third repo, so the profiles match and only the lineage differs
    let ud = dir.path().join("u");
    copy_dir(&pd, &ud);
    let urepo = RepoId([0x66; 16]);
    {
        let mut r = Rdy::open(&ud).unwrap();
        let elsewhere = Origin {
            fork: true,
            repo_id: RepoId([0x55; 16]),
            tag: None,
            landing: None,
        };
        r.store_mut().put_origin(&elsewhere, Some(urepo)).unwrap();
        r.close().unwrap();
    }
    let pub_u = Published {
        v1: pub_p.v1,
        v2: pub_p.v2,
        added: pub_p.added.clone(),
    };
    let u = hub_of(&ud, urepo, &pub_u, &dir.path().join("uh"));
    let us = u.snapshot().unwrap();

    let base = |s: &Snapshot, tag: &str| s.publication_baseline(tag, "site").unwrap();
    let (m1, m2, l1, ll1, u1) = (
        base(&ms, "v1.0.0"),
        base(&ms, "v2.0.0"),
        base(&ls, "v1.0.0"),
        base(&lls, "v1.0.0"),
        base(&us, "v1.0.0"),
    );
    assert_eq!(l1.repo_id(), ls.identity().repo_id);
    assert_eq!(ll1.repo_id(), lls.identity().repo_id);

    // the binding is checked before the lineage and the comparison
    let wrong = BaselineBinding {
        selection_identity: Hash::of(b"another identity"),
        ..binding(&m1)
    };
    let e = ls
        .disclosure_against(
            "v1.0.0",
            "site",
            Some(PreviousChoice::Baseline {
                baseline: &m1,
                binding: &wrong,
            }),
        )
        .unwrap_err();
    assert_eq!(e.code, "E_PUBLICATION_BASELINE");
    assert!(
        e.message
            .contains("does not match the previous publication")
    );
    let e = us
        .disclosure_against(
            "v1.0.0",
            "site",
            Some(PreviousChoice::Baseline {
                baseline: &l1,
                binding: &BaselineBinding {
                    selection_identity: Hash::of(b"another identity"),
                    ..binding(&l1)
                },
            }),
        )
        .unwrap_err();
    assert!(
        e.message
            .contains("does not match the previous publication")
    );

    // M with L, M with LL, and L with LL, both ways
    against(&ls, "v1.0.0", &m1).unwrap();
    against(&ms, "v2.0.0", &l1).unwrap();
    against(&lls, "v1.0.0", &m1).unwrap();
    against(&ms, "v2.0.0", &ll1).unwrap();
    against(&lls, "v1.0.0", &l1).unwrap();
    against(&ls, "v1.0.0", &ll1).unwrap();
    let cmp = against(&ls, "v1.0.0", &m2).unwrap();
    assert!(cmp.comparison.is_some());

    // an unrelated hub shares no lineage with any of them
    for (candidate, b) in [(&us, &m1), (&us, &l1), (&us, &ll1)] {
        outside(against(candidate, "v1.0.0", b).unwrap_err());
    }
    for s in [&ms, &ls, &lls] {
        outside(against(s, "v1.0.0", &u1).unwrap_err());
    }

    // a forged late origin naming the unrelated repo widens nothing
    drop((lls, us));
    ll.close().unwrap();
    {
        let mut r = Rdy::open_holder(&dir.path().join("ll")).unwrap();
        r.store_mut()
            .put_origin(
                &Origin {
                    fork: true,
                    repo_id: urepo,
                    tag: Some("v1.0.0".into()),
                    landing: Some(pub_u.v1),
                },
                None,
            )
            .unwrap();
        r.close().unwrap();
    }
    let ll = StoreHandle::open_holder(&dir.path().join("ll")).unwrap();
    let lls = ll.snapshot().unwrap();
    let us = u.snapshot().unwrap();
    outside(against(&lls, "v1.0.0", &u1).unwrap_err());
    outside(against(&us, "v1.0.0", &base(&lls, "v1.0.0")).unwrap_err());
    against(&lls, "v1.0.0", &m1).unwrap();

    // main removes its later publication's words; its remade baseline shows the line none
    drop(ms);
    let mut r = Rdy::open(&pd).unwrap();
    let removal = remove(&mut r, &pub_p.added);
    r.close().unwrap();
    let e3 = dir.path().join("e3.rdysync");
    export(
        &pd,
        StreamPurpose::CanonHistory,
        removal,
        Some(pub_p.v2),
        &e3,
    );
    let s = m.snapshot().unwrap();
    let c = check(&s, &[&e3]).unwrap();
    assert_eq!(
        m.move_canon(&c, CanonMove::Extend, Some(pub_p.v2)).unwrap(),
        Moved::Moved { tip: removal }
    );
    drop((s, c));
    // the same handle remakes the baseline: the move that carried the removal started its read
    // caches over, so no read built before the removal is reused
    let ms = m.snapshot().unwrap();
    let m2 = base(&ms, "v2.0.0");
    assert_eq!(m2.redactions().len(), 1);
    for s in [&ls, &lls] {
        let check = against(s, "v1.0.0", &m2).unwrap();
        let text = serde_json::to_string(&check.review).unwrap();
        for word in ["Quiet Harbor Plan", "The harbor opens at dawn."] {
            assert!(!text.contains(word), "{word}: {text}");
        }
    }
    // the line holds no later main canon
    let later = ls
        .has_objects(&[landing_key(pub_p.v2), landing_key(removal)])
        .unwrap();
    assert!(later.iter().all(|p| !p.present), "{later:?}");
    drop((ms, ls, lls, us));
    for h in [m, l, ll, u] {
        h.close().unwrap();
    }
}
