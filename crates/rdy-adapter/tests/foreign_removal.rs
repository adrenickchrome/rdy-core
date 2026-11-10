//! rpl26 (repo links; D-CN4QJ, D-DAPR8, D-2G21C): the citing side's take of a cited project's
//! removal through the adapter, and the count of the foreign bytes a project holds.
//!
//! The cited project here is a real one: it lands a page, tags it, delivers the page through its
//! `site` profile, then removes the page's text with a person's approval. Its tag history after
//! the removal carries the portable proof. The citing project takes the delivery, then the
//! history: the store checks the proof and erases only the foreign copies it holds.
use rdy_adapter::stream::ObjectKey;
use rdy_adapter::{BlockId, DeliverySource, Hash, LinkSource, RdyError, RepoId, StoreHandle};
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::Role;
use rdy_store::{Store, StoreError, kinds};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::Path;
use std::sync::Barrier;

/// The bytes of a log frame's header: its length, kind and hash.
const FRAME_HEADER: u64 = 4 + 1 + 32;

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

fn write(r: &mut Rdy, view: &str, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "rpl26".into(),
        },
    )
    .unwrap()
}

/// Confirms each exception beside version `p`, as the person answering for themselves.
fn confirm_exceptions(r: &mut Rdy, p: &rdy_engine::ProposalInfo) {
    let s = session(Role::Approver);
    let q = r
        .read()
        .pending(&PendingQuery {
            person: s.actor.clone(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap();
    let refs: Vec<String> = q
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(p.r#ref.as_str()))
        .map(|i| i.r#ref.clone())
        .collect();
    for item in refs {
        r.answer(
            &s,
            &s.actor,
            AnswerRequest {
                r#ref: item.clone(),
                option: "confirm".into(),
                card_hash: None,
                note: None,
                relay: RelayInput::default(),
                brief: None,
            },
        )
        .unwrap_or_else(|e| panic!("confirm {item}: {e}"));
    }
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(
            &session(Role::Writer),
            "work",
            "rpl26",
            Some("test summary"),
        )
        .unwrap();
    confirm_exceptions(r, &p);
    r.land(&session(Role::Approver), &p.id).unwrap();
}

fn bid(w: &rdy_engine::WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).unwrap()
}

/// The cited project: its directory, its repo, the page text `ap` and the version of `ap` that
/// tag v1.0.0 holds.
struct Cited {
    dir: tempfile::TempDir,
    repo: RepoId,
    ap: BlockId,
    tagged: Hash,
}

/// v1.0.0: doc `a` with the prose `ap`, and the profile `site` (org, selects a). Then `ap`
/// changes and lands untagged, so the cited project holds two versions of it.
fn cited() -> Cited {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": "A leaked code 1234."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (a, ap, admin) = (bid(&w, "$a"), bid(&w, "$ap"), bid(&w, "$admin"));
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "publication_profile", "parent": admin.to_string(),
                "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a.to_string()]}}
        ]),
    );
    land(&mut r);
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": ap.to_string(), "body": "A leaked code 1234, later."}]),
    );
    land(&mut r);
    r.close().unwrap();
    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let repo = snap.identity().repo_id;
    let site = snap
        .profiles("v1.0.0")
        .unwrap()
        .into_iter()
        .find(|p| p.name == "site")
        .unwrap();
    let tagged = site.selection.iter().find(|e| e.id == ap).unwrap().version;
    handle.close().unwrap();
    Cited {
        dir,
        repo,
        ap,
        tagged,
    }
}

impl Cited {
    /// The `site` delivery at v1.0.0, written to `files/name`.
    fn delivery(&self, files: &Path, name: &str) -> File {
        let handle = StoreHandle::open(self.dir.path()).unwrap();
        let snap = handle.snapshot().unwrap();
        let mut out = Vec::new();
        snap.link_export("v1.0.0", "site", &mut out).unwrap();
        handle.close().unwrap();
        let path = files.join(name);
        std::fs::write(&path, out).unwrap();
        File::open(path).unwrap()
    }

    /// Removes the text of `ap` (every version of it) with a person's approval.
    fn remove(&self) {
        let mut r = Rdy::open(self.dir.path()).unwrap();
        let s = session(Role::Writer);
        r.redact_in(
            &s,
            "redact",
            RedactInput {
                targets: vec![self.ap.to_string()],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
        let p = r
            .propose(&s, "redact", "Remove", Some("test summary"))
            .unwrap();
        r.land(&session(Role::Approver), &p.id).unwrap();
        r.close().unwrap();
    }

    /// The complete history of v1.0.0, written to `files/name`: after a removal it carries the
    /// removal's portable proof.
    fn history(&self, files: &Path, name: &str) -> File {
        let handle = StoreHandle::open(self.dir.path()).unwrap();
        let snap = handle.snapshot().unwrap();
        let mut out = Vec::new();
        snap.write_stream("v1.0.0", &BTreeSet::new(), &mut out)
            .unwrap();
        handle.close().unwrap();
        let path = files.join(name);
        std::fs::write(&path, out).unwrap();
        File::open(path).unwrap()
    }

    /// The versions of `ap` that the cited removal approves, in version order: the tagged one
    /// and the later one.
    fn removed_versions(&self) -> Vec<(BlockId, Hash)> {
        let s = Store::open(&store_dir(self.dir.path())).unwrap();
        let r = s.reader();
        let mut out = vec![];
        r.for_each_object_of(kinds::BLOCK, |h| {
            if matches!(
                r.get_kind_raw(kinds::BLOCK, &h),
                Err(StoreError::Redacted { .. })
            ) {
                out.push((self.ap.clone(), h));
            }
            Ok(())
        })
        .unwrap();
        out.sort();
        out
    }
}

/// The store directory of the project at `project`.
fn store_dir(project: &Path) -> std::path::PathBuf {
    project.join(".rdy").join(rdy_store::layout::STORE_DIR)
}

/// An empty project on disk, closed again.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    Rdy::init(dir.path(), "rdy-standard")
        .unwrap()
        .close()
        .unwrap();
    dir
}

fn source(link: &str) -> DeliverySource {
    DeliverySource {
        link: BlockId::parse(link).unwrap(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
        set: None,
    }
}

fn link_source(link: &str) -> LinkSource {
    LinkSource {
        link: BlockId::parse(link).unwrap(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_001,
    }
}

fn refused(e: RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, &e.message[..], &e.fix[..]),
        (code, message, fix),
        "{e:?}"
    );
}

/// Opens the store at `dir` directly (no handle holds it) and tells whether the BLOCK `v` reads
/// as removed.
fn block_removed(dir: &Path, v: Hash) -> bool {
    let s = Store::open(&store_dir(dir)).unwrap();
    let removed = matches!(
        s.reader().get_kind_raw(kinds::BLOCK, &v),
        Err(StoreError::Redacted { .. })
    );
    s.close().unwrap();
    removed
}

/// The take erases the delivered copy that the cited removal approves and reports the version
/// it does not hold as missing. A second take writes nothing. A holder, this project's own repo,
/// a stream of another repo and a stream of another purpose are refused, and none of them writes.
#[test]
fn rpl26_adapter_takes_removal_on_project_only() {
    let c = cited();
    let files = tempfile::tempdir().unwrap();
    let delivery = c.delivery(files.path(), "delivery");
    c.remove();
    let history = c.history(files.path(), "history");
    let approved = c.removed_versions();
    assert_eq!(approved.len(), 2, "the tagged and the later version of ap");
    assert!(approved.contains(&(c.ap.clone(), c.tagged)));
    let later: Vec<(BlockId, Hash)> = approved
        .iter()
        .filter(|(_, v)| *v != c.tagged)
        .cloned()
        .collect();

    let dir = project();
    let handle = StoreHandle::open(dir.path()).unwrap();
    let own = handle.snapshot().unwrap().identity().repo_id;
    handle.take_delivery(&delivery, source("rl-00001")).unwrap();

    // refusals first: none of them writes
    let before = handle
        .take_foreign_removal(&delivery, c.repo)
        .map(|_| ())
        .unwrap_err();
    refused(
        before,
        "E_STREAM_PURPOSE",
        "stream purpose 1 does not match tag_history",
        "request a tag_history stream",
    );
    refused(
        handle.take_foreign_removal(&history, own).unwrap_err(),
        "E_BAD_INPUT",
        "a project cannot cite itself",
        "deliver content from another project",
    );
    let other = RepoId([9; 16]);
    refused(
        handle.take_foreign_removal(&history, other).unwrap_err(),
        "E_REPO_MISMATCH",
        &format!("stream repo {} does not match repo {other}", c.repo),
        "take the stream for the repo it names",
    );

    let taken = handle.take_foreign_removal(&history, c.repo).unwrap();
    assert_eq!(taken.repo, c.repo);
    assert!(!taken.unchanged);
    assert_eq!(taken.removals.len(), 1, "{taken:?}");
    let r = &taken.removals[0];
    assert!(r.entry.is_some());
    assert_eq!(r.applied, vec![(c.ap.clone(), c.tagged)]);
    assert_eq!(r.missing, later, "the version no delivery holds");
    assert!(r.kept_local.is_empty() && r.already_removed.is_empty());
    handle.fsck().unwrap();

    // the same proof again writes nothing and names what it erased before
    let again = handle.take_foreign_removal(&history, c.repo).unwrap();
    assert!(again.unchanged);
    assert_eq!(again.end, taken.end);
    assert_eq!(again.removals.len(), 1);
    assert_eq!(again.removals[0].entry, None);
    assert!(again.removals[0].applied.is_empty());
    assert_eq!(
        again.removals[0].already_removed,
        vec![(c.ap.clone(), c.tagged)]
    );
    handle.close().unwrap();
    assert!(
        block_removed(dir.path(), c.tagged),
        "the foreign copy is erased"
    );

    // a holder holds no foreign copy
    let holder_dir = tempfile::tempdir().unwrap();
    let holder = StoreHandle::create_holder(&holder_dir.path().join("h"), c.repo).unwrap();
    refused(
        holder.take_foreign_removal(&history, c.repo).unwrap_err(),
        "E_BAD_INPUT",
        "this store is a holder",
        "open it through the holder adapter",
    );
    holder.close().unwrap();
}

/// The count covers every held copy of the cited repo: a live delivery, a revoked source's
/// entries and copies, and the Removal entry with the erased frames it leaves. Another repo's
/// bytes are its own, and a captured snapshot keeps its count.
#[test]
fn rpl26_foreign_bytes_counts_live_revoked_and_residue() {
    let c = cited();
    let files = tempfile::tempdir().unwrap();
    let delivery = c.delivery(files.path(), "delivery");
    c.remove();
    let history = c.history(files.path(), "history");

    let dir = project();
    let handle = StoreHandle::open(dir.path()).unwrap();
    let none = handle.snapshot().unwrap();
    assert_eq!(none.foreign_bytes(&c.repo).unwrap(), 0);

    handle.take_delivery(&delivery, source("rl-00001")).unwrap();
    let live = handle.snapshot().unwrap();
    let live_bytes = live.foreign_bytes(&c.repo).unwrap();
    assert!(live_bytes > 0);
    assert_eq!(live.foreign_bytes(&RepoId([9; 16])).unwrap(), 0);

    // a revocation keeps every copy and adds its entry; the older snapshot keeps its count
    handle.revoke_link(link_source("rl-00001")).unwrap();
    let revoked = handle.snapshot().unwrap();
    let revoked_bytes = revoked.foreign_bytes(&c.repo).unwrap();
    assert!(revoked_bytes > live_bytes);
    assert_eq!(live.foreign_bytes(&c.repo).unwrap(), live_bytes);
    assert_eq!(none.foreign_bytes(&c.repo).unwrap(), 0);

    // a removal on the revoked source keeps the erased frames counted and adds its entry
    let taken = handle.take_foreign_removal(&history, c.repo).unwrap();
    let entry = taken.removals[0].entry.unwrap();
    let after = handle.snapshot().unwrap().foreign_bytes(&c.repo).unwrap();
    handle.close().unwrap();
    let s = Store::open(&store_dir(dir.path())).unwrap();
    let (_, payload) = s.reader().get_kind_raw(kinds::FOREIGN, &entry).unwrap();
    s.close().unwrap();
    assert_eq!(
        after,
        revoked_bytes + FRAME_HEADER + payload.len() as u64,
        "the count after the removal: the revoked count and the Removal entry's frame"
    );
    assert!(block_removed(dir.path(), c.tagged));

    // compaction frees the erased payloads and changes no count
    let handle = StoreHandle::open(dir.path()).unwrap();
    handle.compact().unwrap();
    assert_eq!(
        handle.snapshot().unwrap().foreign_bytes(&c.repo).unwrap(),
        after
    );
    handle.close().unwrap();
}

/// The take writes through the store's one writer from a shared handle (`&self`): a snapshot
/// captured before it answers while the take runs on another thread, and a content read of it
/// after the removal is refused (`E_REDACTED_STATE`) rather than served. A write through the
/// same handle after the take sees the removal.
#[test]
fn rpl26_removal_keeps_snapshot_and_writer_contract() {
    let c = cited();
    let files = tempfile::tempdir().unwrap();
    let delivery = c.delivery(files.path(), "delivery");
    c.remove();
    let history = c.history(files.path(), "history");

    let dir = project();
    let handle = StoreHandle::open(dir.path()).unwrap();
    handle.take_delivery(&delivery, source("rl-00001")).unwrap();
    let before = handle.snapshot().unwrap();
    let held = before.foreign_bytes(&c.repo).unwrap();
    let key = ObjectKey {
        kind: kinds::BLOCK,
        hash: c.tagged,
    };
    assert!(before.has_objects(&[key]).unwrap()[0].present);

    let barrier = Barrier::new(2);
    let taken = std::thread::scope(|s| {
        let take = s.spawn(|| {
            barrier.wait();
            handle.take_foreign_removal(&history, c.repo).unwrap()
        });
        barrier.wait();
        // the captured snapshot answers while the take runs, from its own cut
        let _ = before.foreign_bytes(&c.repo);
        take.join().unwrap()
    });
    assert_eq!(taken.removals[0].applied, vec![(c.ap.clone(), c.tagged)]);

    // the snapshot from before the removal is refused, never served the removed state
    let e = before.foreign_bytes(&c.repo).unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e}");
    let e = before.has_objects(&[key]).unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e}");

    // a new snapshot sees the copy as present and removed, and keeps the bytes counted
    let now = handle.snapshot().unwrap();
    let got = now.has_objects(&[key]).unwrap()[0];
    assert_eq!((got.present, got.redacted), (true, true));
    assert!(now.foreign_bytes(&c.repo).unwrap() > held);

    // the writer lane goes on: a revocation after the take seals its own batch
    let revoked = handle.revoke_link(link_source("rl-00001")).unwrap();
    assert!(!revoked.unchanged);
    assert_ne!(revoked.end, taken.end);
    handle.close().unwrap();
}
