//! ha16 (D-D2NPN, D-DAGNB, D-BPYN8, D-C8C70, D-WDKWQ, D-3M81W, D-CC90E; hub i-QD955, p-97JVE,
//! r-FAG02, r-TJAGY, i-SG442, i-FGX7D): the hub's whole use of core, as a consumer that names
//! only `rdy_adapter` and `std`. It exports a source history, splits it into two persistent batch
//! files, checks each in a call of its own, keeps the keys, restarts between the steps, and takes
//! the upload. It then takes a divergent tag, is refused a version collision, repeats an upload,
//! is refused a removal that leaves a sibling's change uncovered, takes the sibling's removal and
//! retries the same files. A dump of the holder loads to the same holder.
//!
//! The source projects and the counts a hub cannot read (the log's commit markers, the holder's
//! segment bytes) come from core calls in [`fixture`]; the hub's own steps are in [`hub`].

/// Source projects and log facts, through core.
mod fixture {
    use rdy_engine::redaction::RedactInput;
    use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
    use rdy_model::{Hash, Provenance, Role};
    use rdy_store::LandRequest;
    use rdy_store::kinds;
    use rdy_store::redaction::StoredFrame;
    use rdy_store::segment::Location;
    use serde_json::{Value, json};
    use std::path::Path;

    pub const CANARY: &str = "ZEBRA-7731";

    fn session(role: Role) -> Session {
        Session {
            actor: if role == Role::Approver {
                "human:founder".into()
            } else {
                "agent:a".into()
            },
            run: None,
            role,
            kind: SessionKind::Human,
        }
    }

    fn write(r: &mut Rdy, ops: Value) -> rdy_engine::WriteResult {
        r.write(
            &session(Role::Writer),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(ops).unwrap(),
                justification: None,
                message: "ha16".into(),
            },
        )
        .unwrap()
    }

    fn land(r: &mut Rdy, title: &str) -> Hash {
        let p = r
            .propose(&session(Role::Writer), "work", title, Some("test summary"))
            .unwrap();
        r.land(&session(Role::Approver), &p.id)
            .unwrap()
            .parse()
            .unwrap()
    }

    fn tag(r: &mut Rdy, name: &str) {
        r.tag(&session(Role::Approver), name, None, None).unwrap();
    }

    fn retitle(r: &mut Rdy, doc: &str, title: &str) {
        write(
            r,
            json!([{"op": "update", "id": doc, "fields": {"title": title}}]),
        );
        land(r, title);
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

    /// The two source projects of one repo, `a` and `b`, closed. Both share a first landing with
    /// a doc and a prose block that holds the canary. `a` tags it `v1.0.0`. `b` retitles the doc
    /// and tags that `1.0.0`; then it edits the prose away and back (a change of its own that
    /// embeds the shared secret version) and tags that `v2.0.0`. Returns the doc and prose ids.
    pub fn sources(dir: &Path) -> (String, String) {
        let a = dir.join("a");
        std::fs::create_dir_all(&a).unwrap();
        let mut r = Rdy::init(&a, "rdy-standard").unwrap();
        let w = write(
            &mut r,
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": format!("The launch code is {CANARY}.")}
            ]),
        );
        land(&mut r, "first");
        r.close().unwrap();
        let (doc, prose) = (w.created["$d"].clone(), w.created["$p"].clone());
        copy_dir(&a, &dir.join("b"));

        let mut r = Rdy::open(&a).unwrap();
        tag(&mut r, "v1.0.0");
        r.close().unwrap();

        let mut b = Rdy::open(&dir.join("b")).unwrap();
        retitle(&mut b, &doc, "B side");
        tag(&mut b, "1.0.0");
        write(
            &mut b,
            json!([{"op": "update", "id": prose, "body": "nothing to see"}]),
        );
        land(&mut b, "away");
        write(
            &mut b,
            json!([{"op": "update", "id": prose, "body": format!("The launch code is {CANARY}.")}]),
        );
        land(&mut b, "back");
        tag(&mut b, "v2.0.0");
        b.close().unwrap();
        (doc, prose)
    }

    /// In the closed project at `dir`: retitles `doc` when `title` is given, removes every
    /// version of `prose` through the store's own landing path, and tags the result `name`.
    pub fn remove_and_tag(dir: &Path, doc: &str, title: Option<&str>, prose: &str, name: &str) {
        let mut r = Rdy::open(dir).unwrap();
        if let Some(title) = title {
            retitle(&mut r, doc, title);
        }
        let s = session(Role::Writer);
        let req = r
            .redact(
                &s,
                RedactInput {
                    targets: vec![prose.to_string()],
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
        tag(&mut r, name);
        r.close().unwrap();
    }

    /// rf4 (D-4WC6R): a project of its own at `dir` whose claim is rewritten by a source-only
    /// and then a code-only Reflects change (typed commits), landed and tagged `v1.0.0`. Returns
    /// the two changes.
    pub fn reflects_source(dir: &Path) -> [Hash; 2] {
        std::fs::create_dir_all(dir).unwrap();
        let mut r = Rdy::init(dir, "rdy-standard").unwrap();
        let w = write(
            &mut r,
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
                {"op": "create", "type": "source", "id": "$s", "parent": "$d", "fields": {"title": "The spec", "kind": "spec", "locator": "spec/v1.md"}},
                {"op": "create", "type": "claim", "id": "$c", "parent": "$d", "body": "One."}
            ]),
        );
        land(&mut r, "first");
        let claim = rdy_model::BlockId::parse(&w.created["$c"]).unwrap();
        let source = rdy_model::BlockId::parse(&w.created["$s"]).unwrap();
        let forms = [
            rdy_model::Justification::Reflects {
                sources: vec![source],
                code: false,
            },
            rdy_model::Justification::Reflects {
                sources: vec![],
                code: true,
            },
        ];
        let mut changes = [Hash([0; 32]); 2];
        for (i, j) in forms.into_iter().enumerate() {
            let scope = rdy_engine::Scope::View("work".into());
            let mut next = r.read().block(&scope, &claim).unwrap();
            let base = next.version_hash();
            next.body = format!("Version {i}.");
            let deps = r.read().included("work").to_vec();
            changes[i] = r
                .store_mut()
                .commit_to_view(
                    "work",
                    rdy_model::Change {
                        view: "work".into(),
                        ops: vec![rdy_model::Op::Update { base, block: next }],
                        deps,
                        provenance: Provenance {
                            actor: "agent:a".into(),
                            run: None,
                            role: Role::Writer,
                            at_ms: 7,
                        },
                        justification: Some(j),
                        message: "rf4".into(),
                        links: Default::default(),
                        read_identity: None,
                    },
                )
                .unwrap();
            r.invalidate();
        }
        land(&mut r, "reflects");
        tag(&mut r, "v1.0.0");
        r.close().unwrap();
        changes
    }

    /// The number of batch commit markers in the closed holder at `root`.
    pub fn markers(root: &Path) -> usize {
        let h = Rdy::open_holder(root).unwrap();
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
        h.close().unwrap();
        n
    }

    /// Whether any segment of the store at `root` still holds the canary's bytes.
    pub fn holds_canary(root: &Path) -> bool {
        std::fs::read_dir(root.join(".rdy/store/segments"))
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".log"))
            .any(|e| {
                let b = std::fs::read(e.path()).unwrap();
                b.windows(CANARY.len()).any(|w| w == CANARY.as_bytes())
            })
    }
}

/// The hub: `rdy_adapter` and `std` only.
mod hub {
    use rdy_adapter::history::LandingFacts;
    use rdy_adapter::stream::{
        BatchReport, ObjectKey, ObjectPresence, StreamError, StreamHeader, StreamReader,
        StreamRecord, StreamWriter, check_batch,
    };
    use rdy_adapter::take::{TakeOutcome, UploadTarget};
    use rdy_adapter::{Hash, RdyError, RepoId, Snapshot, StoreHandle, Tag};
    use std::collections::BTreeSet;
    use std::fs::File;
    use std::path::{Path, PathBuf};

    fn to_rdy(e: &StreamError) -> RdyError {
        RdyError::new(e.code, e.message.clone(), e.fix.clone())
    }

    /// One upload as the hub keeps it: the publication it is for, the repo its checks named, its
    /// two persistent batch files and the keys each check returned.
    pub struct Upload {
        pub target: UploadTarget,
        pub repo: RepoId,
        pub files: [PathBuf; 2],
        pub keys: [Vec<ObjectKey>; 2],
    }

    /// The captured tag `name` of `snap`.
    pub fn publication(snap: &Snapshot, name: &str) -> Result<(Hash, Tag), RdyError> {
        let found = snap.tags()?.into_iter().find(|(_, t)| t.name == name);
        Ok(found.expect("the publication's tag is captured"))
    }

    /// Exports tag `name` of the closed source project at `project`, less `known`, and splits
    /// its ordered records into two batch files under `quarantine`: the first half, cut before
    /// the root tag, and the rest. Each file is checked by a call of its
    /// own; only then are its keys kept.
    pub fn prepare(
        project: &Path,
        name: &str,
        known: &BTreeSet<ObjectKey>,
        quarantine: &Path,
    ) -> Result<Upload, RdyError> {
        let source = StoreHandle::open(project)?;
        let snap = source.snapshot()?;
        let (_, tag) = publication(&snap, name)?;
        let target = UploadTarget {
            tag: tag.name.clone(),
            landing: tag.landing,
        };
        let mut whole: Vec<u8> = Vec::new();
        let exported: BatchReport = snap.write_stream(name, known, &mut whole)?;
        source.close()?;

        let (mut reader, header) = StreamReader::open(whole.as_slice()).map_err(|e| to_rdy(&e))?;
        let mut records: Vec<StreamRecord> = Vec::new();
        while let Some(rec) = reader.next().map_err(|e| to_rdy(&e))? {
            records.push(rec);
        }
        reader.finish().map_err(|e| to_rdy(&e))?;
        let root = header.root();
        let at_root = records
            .iter()
            .position(|r| r.key() == root)
            .expect("an export holds its root");
        let all: BTreeSet<ObjectKey> = exported
            .keys
            .iter()
            .chain(&header.prerequisites)
            .copied()
            .collect();
        // the first half never reaches the root tag
        let half = (records.len() / 2).min(at_root);
        let parts = [&records[..half], &records[half..]];
        let mut files: Vec<PathBuf> = Vec::new();
        let mut keys: Vec<Vec<ObjectKey>> = Vec::new();
        std::fs::create_dir_all(quarantine).map_err(io)?;
        for (i, part) in parts.into_iter().enumerate() {
            let mine: BTreeSet<ObjectKey> = part.iter().map(StreamRecord::key).collect();
            let mut needed: BTreeSet<ObjectKey> = BTreeSet::new();
            for rec in part {
                let refs = rec.refs().map_err(|e| {
                    RdyError::new("E_BAD_RECORD", e.to_string(), "export the history again")
                })?;
                for r in refs {
                    let key = match r.kind {
                        Some(kind) => ObjectKey { kind, hash: r.hash },
                        None => *all
                            .iter()
                            .find(|k| k.hash == r.hash)
                            .expect("a kind-free link names one exported key"),
                    };
                    needed.insert(key);
                }
                needed.extend(rec.proof());
            }
            needed.insert(root);
            needed.retain(|k| !mine.contains(k));
            let h = StreamHeader::tag_history(
                rdy_store::format::baseline_features(),
                header.repo_id,
                header.tag,
                needed.into_iter().collect(),
            );
            let mut w = StreamWriter::new(Vec::new(), &h).map_err(|e| to_rdy(&e))?;
            for rec in part {
                w.record(rec).map_err(|e| to_rdy(&e))?;
            }
            let (bytes, _) = w.finish().map_err(|e| to_rdy(&e))?;
            let path = quarantine.join(format!("{name}-{i}.rdysync"));
            std::fs::write(&path, &bytes).map_err(io)?;
            files.push(path);
        }
        // a later call, as after a restart: each persistent file is checked alone
        let mut repo = None;
        for path in &files {
            let report = check_batch(File::open(path).map_err(io)?, repo)?;
            repo = Some(report.header.repo_id);
            keys.push(report.keys);
        }
        assert!(!keys[0].contains(&root), "the first batch has no root");
        assert!(keys[1].contains(&root));
        Ok(Upload {
            target,
            repo: repo.expect("two checked batches"),
            files: [files[0].clone(), files[1].clone()],
            keys: [keys[0].clone(), keys[1].clone()],
        })
    }

    fn io(e: std::io::Error) -> RdyError {
        RdyError::new(
            "E_IO",
            format!("hub file: {e}"),
            "check the hub's quarantine",
        )
    }

    /// Takes both files of `up`, in reversed order, into the holder at `root`, opened for this
    /// call alone.
    pub fn take(root: &Path, up: &Upload) -> Result<TakeOutcome, RdyError> {
        let mut holder = StoreHandle::open_holder(root)?;
        let files = [
            File::open(&up.files[1]).map_err(io)?,
            File::open(&up.files[0]).map_err(io)?,
        ];
        let got = holder.take(&files, up.target.clone());
        holder.close()?;
        got
    }

    /// The presence of `keys` in the holder at `root`, opened for this call alone.
    pub fn presence(root: &Path, keys: &[ObjectKey]) -> Result<Vec<ObjectPresence>, RdyError> {
        let holder = StoreHandle::open_holder(root)?;
        let got = holder.snapshot()?.has_objects(keys)?;
        holder.close()?;
        Ok(got)
    }

    /// What the hub knows the holder has: every key of `keys` the holder holds.
    pub fn known(root: &Path, keys: &[ObjectKey]) -> Result<BTreeSet<ObjectKey>, RdyError> {
        Ok(presence(root, keys)?
            .into_iter()
            .filter(|p| p.present)
            .map(|p| p.key)
            .collect())
    }

    /// The tag history of `name` in the store at `root` (a holder when `holder`), newest first.
    pub fn history(root: &Path, holder: bool, name: &str) -> Result<Vec<LandingFacts>, RdyError> {
        let h = if holder {
            StoreHandle::open_holder(root)?
        } else {
            StoreHandle::open(root)?
        };
        let got = h.snapshot()?.tag_history(name)?;
        h.close()?;
        Ok(got)
    }

    /// The captured tag names of the holder at `root`, by version.
    pub fn tags(root: &Path) -> Result<Vec<(Hash, String)>, RdyError> {
        let h = StoreHandle::open_holder(root)?;
        let got = h
            .snapshot()?
            .tags()?
            .into_iter()
            .map(|(hash, t)| (hash, t.name))
            .collect();
        h.close()?;
        Ok(got)
    }
}

use rdy_adapter::maintenance;
use rdy_adapter::stream::ObjectKey;
use rdy_adapter::{Hash, StoreHandle};
use std::collections::BTreeSet;

/// `features` and the `assessment` store feature: a holder that takes a landing takes the
/// assessment it names, so its store uses the feature.
fn and_assessment(features: rdy_store::format::StoreFeatures) -> rdy_store::format::StoreFeatures {
    let mut f = features.features;
    f.extend(
        rdy_store::format::registered_features()
            .into_iter()
            .filter(|x| x.name == rdy_store::format::FEATURE_ASSESSMENT),
    );
    rdy_store::format::StoreFeatures::of(f)
}

#[test]
fn ha16_hub_contract_resumes_with_persistent_batch_files() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    let (doc, prose) = fixture::sources(dir.path());
    let quarantine = dir.path().join("quarantine");
    let root = dir.path().join("holder");
    let none = BTreeSet::new();

    // A's v1.0.0: two checked batch files; the checks keep nothing between calls
    let v1 = hub::prepare(&a, "v1.0.0", &none, &quarantine).unwrap();
    let all1: Vec<ObjectKey> = v1.keys.concat();
    assert!(!v1.keys[0].is_empty() && !v1.keys[1].is_empty());
    StoreHandle::create_holder(&root, v1.repo)
        .unwrap()
        .close()
        .unwrap();
    let before = fixture::markers(&root);
    // after a restart the holder holds none of it; what it holds and what the checks returned
    // together are the whole history, so a new export of the tag has nothing left to send
    let seen = hub::presence(&root, &all1).unwrap();
    assert!(seen.iter().all(|p| !p.present && !p.redacted));
    let mut known = hub::known(&root, &all1).unwrap();
    known.extend(all1.iter().copied());
    let source = StoreHandle::open(&a).unwrap();
    let rest = source
        .snapshot()
        .unwrap()
        .write_stream("v1.0.0", &known, std::io::sink())
        .unwrap();
    source.close().unwrap();
    assert!(rest.keys.is_empty(), "{:?}", rest.keys);

    // both files, in reversed order, for the publication: one marker, the whole tag, no canon
    let got = hub::take(&root, &v1).unwrap();
    assert_eq!(fixture::markers(&root), before + 1);
    assert_eq!(got.records_taken, all1.len() as u64);
    assert_eq!(
        (got.tag.as_str(), got.canon, got.unchanged),
        ("v1.0.0", None, false)
    );
    assert!(
        hub::presence(&root, &all1)
            .unwrap()
            .iter()
            .all(|p| p.present)
    );
    let hashes = |f: Vec<rdy_adapter::history::LandingFacts>| -> Vec<Hash> {
        f.into_iter().map(|f| f.hash).collect()
    };
    assert_eq!(
        hashes(hub::history(&root, true, "v1.0.0").unwrap()),
        hashes(hub::history(&a, false, "v1.0.0").unwrap())
    );
    let holder = StoreHandle::open_holder(&root).unwrap();
    let snap = holder.snapshot().unwrap();
    assert!(snap.canon().is_none() && snap.canon_chain().unwrap().is_empty());
    let l1 = got.landing;
    drop(snap);
    holder.close().unwrap();

    // B's divergent v2.0.0, less what the holder holds
    let known = hub::known(&root, &all1).unwrap();
    let v2 = hub::prepare(&b, "v2.0.0", &known, &quarantine).unwrap();
    let got2 = hub::take(&root, &v2).unwrap();
    assert_eq!(got2.records_taken, v2.keys.concat().len() as u64);
    assert_eq!(
        hashes(hub::history(&root, true, "v2.0.0").unwrap()),
        hashes(hub::history(&b, false, "v2.0.0").unwrap())
    );
    assert_ne!(got2.landing, l1);
    let names = |root: &std::path::Path| -> Vec<String> {
        hub::tags(root)
            .unwrap()
            .into_iter()
            .map(|(_, n)| n)
            .collect()
    };
    assert_eq!(names(&root), ["v1.0.0", "v2.0.0"]);

    // B's 1.0.0 is v1.0.0 by version, at another landing: refused, nothing written
    let all2: Vec<ObjectKey> = [all1.clone(), v2.keys.concat()].concat();
    let collide =
        hub::prepare(&b, "1.0.0", &hub::known(&root, &all2).unwrap(), &quarantine).unwrap();
    let marks = fixture::markers(&root);
    let e = hub::take(&root, &collide).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_TAG_EXISTS",
            format!("version 1.0.0 already names landing {l1}").as_str(),
            "use another version name"
        )
    );
    assert_eq!(fixture::markers(&root), marks);
    assert_eq!(names(&root), ["v1.0.0", "v2.0.0"]);

    // the same upload again, from the same files: nothing appended, the held tag named
    let again = hub::take(&root, &v1).unwrap();
    assert_eq!(
        (
            again.unchanged,
            again.records_taken,
            again.frames_written,
            again.tag_hash
        ),
        (true, 0, 0, got.tag_hash)
    );
    assert_eq!(fixture::markers(&root), marks);

    // A removes the secret: its approval does not cover B's own change that embeds it
    fixture::remove_and_tag(&a, &doc, Some("A side"), &prose, "v3.0.0");
    let v3 = hub::prepare(
        &a,
        "v3.0.0",
        &hub::known(&root, &all2).unwrap(),
        &quarantine,
    )
    .unwrap();
    let e = hub::take(&root, &v3).unwrap_err();
    assert_eq!(e.code, "E_REDACTION_STALE", "{e:?}");
    assert!(
        e.message
            .starts_with("the imported removal does not cover receiver change "),
        "{}",
        e.message
    );
    assert!(
        e.fix
            .starts_with("redact this content in the project that made change ")
    );
    assert_eq!(fixture::markers(&root), marks);
    assert!(fixture::holds_canary(&root));

    // B's own approved removal is taken; then A's files, kept as they were, pass
    fixture::remove_and_tag(&b, &doc, None, &prose, "v4.0.0");
    let v4 = hub::prepare(
        &b,
        "v4.0.0",
        &hub::known(&root, &all2).unwrap(),
        &quarantine,
    )
    .unwrap();
    hub::take(&root, &v4).unwrap();
    assert!(!fixture::holds_canary(&root));
    let got3 = hub::take(&root, &v3).unwrap();
    assert!(got3.records_taken > 0 && !got3.unchanged);
    assert_eq!(names(&root), ["v1.0.0", "v2.0.0", "v3.0.0", "v4.0.0"]);
    for up in [&v1, &v2, &collide, &v3, &v4] {
        for f in &up.files {
            assert!(f.exists(), "core removes no hub file: {}", f.display());
        }
    }
    assert!(!fixture::holds_canary(&root));

    // profiles, disclosure and recorded facts read the same in the holder and in the source
    let source = StoreHandle::open(&b).unwrap();
    let from = source.snapshot().unwrap();
    let holder = StoreHandle::open_holder(&root).unwrap();
    let held = holder.snapshot().unwrap();
    for name in ["v2.0.0", "v4.0.0"] {
        // the implicit profile's title is the store's own name, not the source's (O10); every
        // other field is the same
        let retitled = |mut p: rdy_adapter::publication::ProfileSummary| {
            assert_eq!(p.id, None);
            p.title = "Holder".into();
            p
        };
        let profiles = held.profiles(name).unwrap();
        assert_eq!(profiles[0].selector, "implicit:org");
        assert_eq!(profiles[0].title, "Holder");
        let mut want = from.profiles(name).unwrap();
        want[0] = retitled(want[0].clone());
        assert_eq!(profiles, want);
        let check = held.disclosure(name, "implicit:org", None).unwrap();
        assert!(check.comparison.is_none());
        let mut want = from.disclosure(name, "implicit:org", None).unwrap();
        want.profile = retitled(want.profile);
        assert_eq!(check, want);
        assert_eq!(
            held.tag_history(name).unwrap(),
            from.tag_history(name).unwrap()
        );
    }
    let keys: Vec<ObjectKey> = [all2.clone(), v4.keys.concat(), v3.keys.concat()].concat();
    let presence = held.has_objects(&keys).unwrap();
    assert!(presence.iter().all(|p| p.present));
    assert!(presence.iter().any(|p| p.redacted));
    let facts = held.landing_facts(l1).unwrap();
    let tags = held.tags().unwrap();
    let identity = held.identity().clone();
    drop((from, held));
    source.close().unwrap();

    // a dump loads to the same holder: mode, identity, tags and removed projections
    let out = dir.path().join("holder.rdydump");
    holder.dump(&out).unwrap();
    holder.close().unwrap();
    let formats = maintenance::inspect_dump(&out).unwrap();
    let writes = maintenance::formats();
    assert_eq!(
        (formats.dump, formats.store),
        (
            writes.dump,
            maintenance::DumpStore::Features(and_assessment(writes.store.clone()))
        )
    );
    let copy = dir.path().join("copy");
    maintenance::load(&out, &copy).unwrap();
    assert!(
        StoreHandle::open(&copy).is_err(),
        "a loaded holder is no project"
    );
    let loaded = StoreHandle::open_holder(&copy).unwrap();
    let snap = loaded.snapshot().unwrap();
    assert_eq!(snap.identity(), &identity);
    assert_eq!(snap.tags().unwrap(), tags);
    assert!(snap.canon().is_none());
    assert_eq!(snap.has_objects(&keys).unwrap(), presence);
    assert_eq!(snap.landing_facts(l1).unwrap(), facts);
    drop(snap);
    loaded.fsck().unwrap();
    loaded.close().unwrap();
    assert!(!fixture::holds_canary(&copy));
}

#[test]
fn rf4_hub_contract_takes_a_reflects_history() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("reflects");
    let changes = fixture::reflects_source(&src);
    let quarantine = dir.path().join("quarantine");
    let root = dir.path().join("holder");
    let up = hub::prepare(&src, "v1.0.0", &BTreeSet::new(), &quarantine).unwrap();
    let all: Vec<ObjectKey> = up.keys.concat();
    for c in changes {
        assert!(
            all.iter().any(|k| k.hash == c),
            "each Reflects change is a checked key"
        );
    }
    StoreHandle::create_holder(&root, up.repo)
        .unwrap()
        .close()
        .unwrap();
    let got = hub::take(&root, &up).unwrap();
    assert_eq!(got.records_taken, all.len() as u64);
    assert!(
        hub::presence(&root, &all)
            .unwrap()
            .iter()
            .all(|p| p.present)
    );
    assert_eq!(
        hub::history(&root, true, "v1.0.0").unwrap(),
        hub::history(&src, false, "v1.0.0").unwrap()
    );
    // the holder's dump is of the pair this rdy writes, and loads to a holder that checks clean
    let holder = StoreHandle::open_holder(&root).unwrap();
    let out = dir.path().join("reflects.rdydump");
    holder.dump(&out).unwrap();
    holder.close().unwrap();
    let seen = maintenance::inspect_dump(&out).unwrap();
    assert_eq!(
        (seen.dump, seen.store),
        (
            4,
            maintenance::DumpStore::Features(
                and_assessment(rdy_store::format::baseline_features())
            )
        )
    );
    let copy = dir.path().join("reflects-copy");
    maintenance::load(&out, &copy).unwrap();
    assert!(
        hub::presence(&copy, &all)
            .unwrap()
            .iter()
            .all(|p| p.present)
    );
    let loaded = StoreHandle::open_holder(&copy).unwrap();
    loaded.fsck().unwrap();
    loaded.close().unwrap();
}
