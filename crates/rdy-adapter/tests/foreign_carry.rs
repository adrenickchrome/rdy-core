//! rpl32 (repo links; D-NXHX3, D-FK58A, D-CN4QJ): what carries a project's foreign history, and
//! what does not, through the paths that exist before the Sync client's pull.
//!
//! The Sync client's pull does not exist yet. D-NXHX3 moves the real-pull test (a clone, its
//! catch-up and a local replay through a public pull) to the Sync plan's acceptance, and until
//! pull ships a clone gets a delivery only through native dump and load and the follower. The test
//! ids keep the word "pull"; the paths they drive are those:
//!
//! - Dump and load, and the follower's increments, carry a delivery, a revocation and a removal as
//!   the citing project holds them: the same entries, sources, pins, byte counts and erased
//!   copies. In a loaded clone the shared held-pin check (`ReadState::check_carried_pins`) holds:
//!   view replay takes the clone's change that cites, and refuses a carried change whose pin no
//!   delivery of the clone carried.
//! - A carried change never brings a delivery: a citing project's tag history carries its changes
//!   and their pins, and no FOREIGN entry; a change carried into another project replays there
//!   only once that project took the delivery itself.
//!
//! The cited project is a real one: it lands a page, tags it, delivers it through its `site`
//! profile, changes the page and tags it again.
use rdy_adapter::stream::ObjectKey;
use rdy_adapter::take::UploadTarget;
use rdy_adapter::{
    BlockId, DeliverySource, Follower, Hash, LinkSource, RdyError, RepoId, StoreHandle, maintenance,
};
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Scope, Session, SessionKind, WriteRequest};
use rdy_model::foreign::{ForeignId, ForeignPin};
use rdy_model::{Block, Change, ChangeLinks, Op, Role};
use rdy_store::{Store, StoreError, kinds};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

/// The text of the cited page; every delivered version of it holds these words.
const LEAKED: &str = "A leaked code 1234";

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
            message: "rpl32".into(),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
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

fn land(r: &mut Rdy, view: &str) {
    let p = r
        .propose(&session(Role::Writer), view, "rpl32", Some("test summary"))
        .unwrap();
    confirm_exceptions(r, &p);
    r.land(&session(Role::Approver), &p.id)
        .unwrap_or_else(|e| panic!("land {view}: {e:?}"));
}

fn bid(w: &rdy_engine::WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).unwrap()
}

/// The cited project: its directory, its repo, the page text `ap`, the `site` profile's id, and
/// the versions of `ap` that tags v1.0.0 and v1.1.0 hold.
struct Cited {
    dir: tempfile::TempDir,
    repo: RepoId,
    ap: BlockId,
    profile: BlockId,
    v1: Hash,
    v2: Hash,
}

/// v1.0.0: doc `a` with the prose `ap`, and the profile `site` (org, selects a). Then `ap`
/// changes, lands and is tagged v1.1.0.
fn cited() -> Cited {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": format!("{LEAKED}.")},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (a, ap, admin) = (bid(&w, "$a"), bid(&w, "$ap"), bid(&w, "$admin"));
    let p = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "publication_profile", "id": "$pf", "parent": admin.to_string(),
                "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a.to_string()]}}
        ]),
    );
    let profile = bid(&p, "$pf");
    land(&mut r, "work");
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": ap.to_string(), "body": format!("{LEAKED}, later.")}]),
    );
    land(&mut r, "work");
    r.tag(&session(Role::Approver), "v1.1.0", None, None)
        .unwrap();
    r.close().unwrap();
    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let repo = snap.identity().repo_id;
    let version_at = |tag: &str| {
        let site = snap
            .profiles(tag)
            .unwrap()
            .into_iter()
            .find(|p| p.name == "site")
            .unwrap();
        assert_eq!(site.id.as_ref(), Some(&profile));
        site.selection.iter().find(|e| e.id == ap).unwrap().version
    };
    let (v1, v2) = (version_at("v1.0.0"), version_at("v1.1.0"));
    drop(snap);
    handle.close().unwrap();
    Cited {
        dir,
        repo,
        ap,
        profile,
        v1,
        v2,
    }
}

impl Cited {
    /// The `site` delivery at `tag`, written to `files/name`.
    fn delivery(&self, files: &Path, tag: &str, name: &str) -> File {
        let handle = StoreHandle::open(self.dir.path()).unwrap();
        let snap = handle.snapshot().unwrap();
        let mut out = Vec::new();
        snap.link_export(tag, "site", &mut out).unwrap();
        drop(snap);
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
        drop(snap);
        handle.close().unwrap();
        let path = files.join(name);
        std::fs::write(&path, out).unwrap();
        File::open(path).unwrap()
    }
}

/// A citing project at `dir`: a doc, a section and the repo link `core` to `c`'s `site`
/// profile, landed. Returns the section and the link.
fn citing(dir: &Path, c: &Cited) -> (BlockId, BlockId) {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "d", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "repo_link", "id": "$l", "parent": "$d",
             "fields": {"alias": "core", "repo": c.repo.to_string(), "profile": c.profile.to_string()}}
        ]),
    );
    land(&mut r, "seed");
    r.close().unwrap();
    (bid(&w, "$s"), bid(&w, "$l"))
}

/// Writes and lands a claim under `section` that cites `ap` through `core`. Returns the claim and
/// the change that created it.
fn cite(dir: &Path, section: &BlockId, ap: &BlockId) -> (BlockId, Hash) {
    let mut r = Rdy::open(dir).unwrap();
    let w = write(
        &mut r,
        "cite",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": section.to_string(),
                "body": format!("Follow {{{{core:{ap}}}}}.")}]),
    );
    land(&mut r, "cite");
    r.close().unwrap();
    (bid(&w, "$c"), w.change.parse().unwrap())
}

fn source(link: &BlockId) -> DeliverySource {
    DeliverySource {
        link: link.clone(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
        set: None,
    }
}

fn link_source(link: &BlockId) -> LinkSource {
    LinkSource {
        link: link.clone(),
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

/// The store directory of the project at `project`.
fn store_dir(project: &Path) -> PathBuf {
    project.join(".rdy").join(rdy_store::layout::STORE_DIR)
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

/// Whether any segment file of the store at `project` holds `needle`'s bytes.
fn on_disk(project: &Path, needle: &str) -> bool {
    std::fs::read_dir(store_dir(project).join("segments"))
        .unwrap()
        .any(|e| {
            let bytes = std::fs::read(e.unwrap().path()).unwrap();
            bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
        })
}

/// What a snapshot holds of the cited repo's foreign history: the canon citations with their
/// held and current versions, the link's source view, and the foreign byte count.
type Facts = (
    Vec<rdy_adapter::foreign_facts::CitedRepoRefs>,
    Option<rdy_store::foreign::SourceView>,
    u64,
);

fn facts(snap: &rdy_adapter::Snapshot, link: &BlockId, repo: &RepoId) -> Facts {
    (
        snap.foreign_refs().unwrap(),
        snap.foreign_source_for_tests(link).unwrap(),
        snap.foreign_bytes(repo).unwrap(),
    )
}

/// The FOREIGN entries `reader` holds, in hash order. FOREIGN is no portable kind, so no
/// snapshot read names it.
fn entries_of(reader: &rdy_store::StoreReader) -> Vec<Hash> {
    let mut out = vec![];
    reader
        .for_each_object_of(kinds::FOREIGN, |h| {
            out.push(h);
            Ok(())
        })
        .unwrap();
    out.sort();
    out
}

/// Opens the store at `dir` directly (no handle holds it) and lists its FOREIGN entries.
fn foreign_entries(dir: &Path) -> Vec<Hash> {
    let s = Store::open(&store_dir(dir)).unwrap();
    let out = entries_of(&s.reader());
    s.close().unwrap();
    out
}

fn sorted(mut hashes: Vec<Hash>) -> Vec<Hash> {
    hashes.sort();
    hashes
}

fn block_key(hash: Hash) -> ObjectKey {
    ObjectKey {
        kind: kinds::BLOCK,
        hash,
    }
}

/// A carried change: the block `b`, written as `op` (a create, or an update of `base`), with the
/// foreign pins `pins`, committed straight to the store of `r` in view `view` (no write checks
/// it), sealed and published. Returns its hash.
fn carry(
    r: &mut Rdy,
    view: &str,
    mut b: Block,
    base: Option<Hash>,
    pins: &[(ForeignId, ForeignPin)],
) -> Hash {
    b.read_identity = None;
    b.foreign.pins = pins.iter().cloned().collect();
    let op = match base {
        Some(base) => Op::Update { base, block: b },
        None => Op::Create { block: b },
    };
    let change = Change {
        view: view.into(),
        ops: vec![op],
        deps: vec![],
        provenance: session(Role::Writer).provenance(),
        justification: None,
        message: format!("carried into {view}"),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    let h = r.store_mut().commit_to_view(view, change).unwrap();
    r.seal_open().unwrap();
    r.invalidate();
    h
}

/// Replays `changes` into a new view `view` owned by the writer.
fn replay(
    r: &mut Rdy,
    view: &str,
    changes: &[Hash],
) -> Result<rdy_engine::views::ReplayResult, RdyError> {
    let s = session(Role::Writer);
    r.view_create(&s, view, None, false).unwrap();
    let input: Vec<String> = changes.iter().map(ToString::to_string).collect();
    r.view_replay(&s, view, &input)
}

fn missing_version(id: &BlockId, content: &Hash) -> (String, String) {
    (
        format!(
            "core:{id} has no delivered version {}",
            &content.to_string()[..12]
        ),
        "wait for the delivery that holds it, or pin a delivered version: rdy_links {op: \"show\", alias} / rdy links show core".to_string(),
    )
}

/// The citing project takes a delivery, cites from it and lands, revokes the link, then takes
/// the cited project's removal. A follower seeded before the delivery follows one increment per
/// step and holds, after each, what the project held at that step: the same citations and held
/// versions, the same source view (live, then revoked), the same foreign byte count and the
/// same entries. A clone loaded from the final dump holds the same: the revocation still refuses
/// a delivery, and the erased copy stays erased. In the clone, view replay takes the change that
/// cites (its pin is held through the residue, and no byte comes back), and refuses a carried
/// change whose pin no delivery carried.
#[test]
fn rpl32_pull_carries_delivery_revocation_and_removal() {
    let c = cited();
    let files = tempfile::tempdir().unwrap();
    let delivery = c.delivery(files.path(), "v1.0.0", "delivery");
    c.remove();
    let history = c.history(files.path(), "history");

    let dir = tempfile::tempdir().unwrap();
    let (section, link) = citing(dir.path(), &c);
    let handle = StoreHandle::open(dir.path()).unwrap();
    let d0 = handle.dump(&files.path().join("d0")).unwrap();
    let delivered = handle.take_delivery(&delivery, source(&link)).unwrap();
    handle.close().unwrap();
    let (claim, cited_change) = cite(dir.path(), &section, &c.ap);
    let handle = StoreHandle::open(dir.path()).unwrap();
    let mut steps = vec![];
    let d1 = handle
        .dump_after(&d0.chain_end(), &files.path().join("d1"))
        .unwrap();
    steps.push(("d1", facts(&handle.snapshot().unwrap(), &link, &c.repo)));
    let revoked = handle.revoke_link(link_source(&link)).unwrap();
    let d2 = handle
        .dump_after(&d1.chain_end(), &files.path().join("d2"))
        .unwrap();
    steps.push(("d2", facts(&handle.snapshot().unwrap(), &link, &c.repo)));
    let taken = handle.take_foreign_removal(&history, c.repo).unwrap();
    assert_eq!(taken.removals[0].applied, vec![(c.ap.clone(), c.v1)]);
    let removal = taken.removals[0].entry.unwrap();
    handle
        .dump_after(&d2.chain_end(), &files.path().join("d3"))
        .unwrap();
    let all = vec![delivered.entry, revoked.entry, removal];
    let last = facts(&handle.snapshot().unwrap(), &link, &c.repo);
    steps.push(("d3", last.clone()));
    handle.dump(&files.path().join("full")).unwrap();
    handle.close().unwrap();

    // what the project holds at the end: the pin held as the removed version's residue, the
    // source revoked
    let (refs, view, bytes) = &last;
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].repo, c.repo);
    let row = &refs[0].refs[0];
    assert_eq!((&row.citer, row.held), (&claim, Some(c.v1)));
    assert_eq!(row.entry, Some(delivered.entry));
    assert!(view.as_ref().unwrap().revoked);
    assert!(*bytes > 0);
    assert!(block_removed(dir.path(), c.v1));
    assert_eq!(foreign_entries(dir.path()), sorted(all.clone()));

    // the follower, one increment per step
    let fdir = files.path().join("follower");
    maintenance::seed_follower(&files.path().join("d0"), &fdir).unwrap();
    let mut f = Follower::open(&fdir).unwrap();
    let snap = f.snapshot().unwrap();
    assert_eq!(snap.foreign_bytes(&c.repo).unwrap(), 0);
    assert_eq!(snap.foreign_source_for_tests(&link).unwrap(), None);
    drop(snap);
    for (name, want) in &steps {
        f.follow(&files.path().join(name)).unwrap();
        let snap = f.snapshot().unwrap();
        assert_eq!(&facts(&snap, &link, &c.repo), want, "after {name}");
    }
    drop(f);
    let core = rdy_engine::follower::FollowerCore::open(&fdir).unwrap();
    let st = core.read().unwrap();
    assert_eq!(entries_of(st.reader()), sorted(all.clone()));
    assert!(
        matches!(
            st.reader().get_kind_raw(kinds::BLOCK, &c.v1),
            Err(StoreError::Redacted { .. })
        ),
        "the follower keeps the erasure"
    );
    drop((st, core));

    // the clone, loaded from the final dump
    let clone = files.path().join("clone");
    maintenance::load(&files.path().join("full"), &clone).unwrap();
    let h = StoreHandle::open(&clone).unwrap();
    let snap = h.snapshot().unwrap();
    assert_eq!(facts(&snap, &link, &c.repo), last);
    drop(snap);
    let again = c.delivery(files.path(), "v1.1.0", "delivery-2");
    refused(
        h.take_delivery(&again, source(&link)).unwrap_err(),
        "E_BAD_INPUT",
        "the repo link is revoked",
        "use a live repo link",
    );
    h.close().unwrap();
    assert_eq!(foreign_entries(&clone), sorted(all));
    assert!(block_removed(&clone, c.v1), "the clone keeps the erasure");
    assert!(!on_disk(&clone, LEAKED));

    // the held-pin check in the clone: the change that cites replays, a carried change whose pin
    // no delivery carried does not
    let mut r = Rdy::open(&clone).unwrap();
    let got = replay(&mut r, "again", &[cited_change]).unwrap();
    assert_eq!(got.replayed.len(), 1);
    let held_pin = r
        .read()
        .block(&Scope::Canon, &claim)
        .unwrap()
        .foreign
        .pins
        .clone();
    let replayed = r
        .read()
        .block(&Scope::View("again".into()), &claim)
        .unwrap();
    assert_eq!(replayed.foreign.pins, held_pin);
    let ghost = ForeignPin {
        content: Hash::of(b"content no delivery carried"),
        encoding: 0,
    };
    let base = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(&claim);
    let block = r.read().block(&Scope::Canon, &claim).unwrap();
    let target = ForeignId::new(c.repo, c.ap.clone());
    let bad = carry(&mut r, "carried", block, base, &[(target, ghost.clone())]);
    let s = session(Role::Writer);
    r.view_create(&s, "again-2", None, false).unwrap();
    let seq = r.read().seq();
    let e = r
        .view_replay(&s, "again-2", &[bad.to_string()])
        .unwrap_err();
    let (message, fix) = missing_version(&c.ap, &ghost.content);
    refused(e, "E_FOREIGN_VERSION", &message, &fix);
    assert!(r.read().included("again-2").is_empty());
    assert_eq!(r.read().seq(), seq, "the refused replay writes nothing");
    r.close().unwrap();
    assert!(!on_disk(&clone, LEAKED), "the replays restore no byte");
}

/// A citing project's tag history carries the change that cites and its pin, and no FOREIGN
/// entry and no delivered copy: a holder that takes it holds no delivery and counts no foreign
/// bytes. The same change
/// carried into another project that links the same profile, but took only a later delivery,
/// replays there only once that project takes the delivery that holds the pinned version itself;
/// the refused replay takes nothing.
#[test]
fn rpl32_carried_change_does_not_smuggle_delivery() {
    let c = cited();
    let files = tempfile::tempdir().unwrap();
    let first = c.delivery(files.path(), "v1.0.0", "delivery-1");
    let later = c.delivery(files.path(), "v1.1.0", "delivery-2");

    // the citing project: it takes v1.0.0's delivery, cites from it, lands and tags
    let p = tempfile::tempdir().unwrap();
    let (section, link) = citing(p.path(), &c);
    let handle = StoreHandle::open(p.path()).unwrap();
    let delivered = handle.take_delivery(&first, source(&link)).unwrap();
    handle.close().unwrap();
    let (claim, _) = cite(p.path(), &section, &c.ap);
    let mut r = Rdy::open(p.path()).unwrap();
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    let cited_block = r.read().block(&Scope::Canon, &claim).unwrap();
    let claim_version = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(&claim)
        .unwrap();
    r.close().unwrap();
    assert_eq!(foreign_entries(p.path()), vec![delivered.entry]);
    let target = ForeignId::new(c.repo, c.ap.clone());
    let pin = cited_block.foreign.pins[&target].clone();

    // its tag history, taken into a holder: the claim travels with its pin, the delivery does not
    let handle = StoreHandle::open(p.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let repo = snap.identity().repo_id;
    let (_, tag) = snap
        .tags()
        .unwrap()
        .into_iter()
        .find(|(_, t)| t.name == "v1.0.0")
        .unwrap();
    let path = files.path().join("history");
    let mut out = Vec::new();
    snap.write_stream("v1.0.0", &BTreeSet::new(), &mut out)
        .unwrap();
    std::fs::write(&path, out).unwrap();
    drop(snap);
    handle.close().unwrap();
    let hdir = tempfile::tempdir().unwrap();
    let holder = StoreHandle::create_holder(&hdir.path().join("h"), repo).unwrap();
    holder
        .take(
            &[File::open(&path).unwrap()],
            UploadTarget {
                tag: "v1.0.0".into(),
                landing: tag.landing,
            },
        )
        .unwrap();
    let snap = holder.snapshot().unwrap();
    let got = snap.has_objects(&[block_key(claim_version)]).unwrap();
    assert!(got[0].present, "the claim travels");
    // the landing's assessment names the held version of the pin, a foreign-only edge that no
    // tag history follows: the delivered copy stays out of the holder
    let got = snap.has_objects(&[block_key(c.v1)]).unwrap();
    assert!(!got[0].present, "no delivered copy travels: {got:?}");
    assert_eq!(snap.foreign_bytes(&c.repo).unwrap(), 0);
    drop(snap);
    holder.close().unwrap();
    assert!(
        foreign_entries(&hdir.path().join("h")).is_empty(),
        "no delivery travels"
    );
    let s = Store::open(&store_dir(&hdir.path().join("h"))).unwrap();
    assert_eq!(
        s.reader().get_block(&claim_version).unwrap().foreign,
        cited_block.foreign,
        "the carried claim keeps its pin"
    );
    s.close().unwrap();

    // another project links the same profile and took only v1.1.0's delivery
    let q = tempfile::tempdir().unwrap();
    let (q_section, q_link) = citing(q.path(), &c);
    let handle = StoreHandle::open(q.path()).unwrap();
    let second = handle.take_delivery(&later, source(&q_link)).unwrap();
    let bytes = handle.snapshot().unwrap().foreign_bytes(&c.repo).unwrap();
    handle.close().unwrap();

    // the claim carried into it is refused at replay, and the refusal takes nothing
    let mut r = Rdy::open(q.path()).unwrap();
    let mut carried_block = cited_block.clone();
    carried_block.parent = Some(q_section.clone());
    let carried = carry(
        &mut r,
        "carried",
        carried_block,
        None,
        &[(target.clone(), pin.clone())],
    );
    let e = replay(&mut r, "target", &[carried]).unwrap_err();
    let (message, fix) = missing_version(&c.ap, &pin.content);
    refused(e, "E_FOREIGN_VERSION", &message, &fix);
    assert!(r.read().included("target").is_empty());
    r.close().unwrap();
    let handle = StoreHandle::open(q.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    assert_eq!(snap.foreign_bytes(&c.repo).unwrap(), bytes);
    let got = snap.has_objects(&[block_key(c.v1)]).unwrap();
    assert!(!got[0].present, "no copy of the pinned version: {got:?}");
    let view = snap.foreign_source_for_tests(&q_link).unwrap().unwrap();
    assert_eq!(view.head, second.entry);
    assert_eq!(view.members[&c.ap].version.version, c.v2);
    drop(snap);
    handle.close().unwrap();
    assert_eq!(foreign_entries(q.path()), vec![second.entry]);
    let handle = StoreHandle::open(q.path()).unwrap();

    // once this project takes v1.0.0's delivery itself, the same carried change replays
    handle.take_delivery(&first, source(&q_link)).unwrap();
    handle.close().unwrap();
    let mut r = Rdy::open(q.path()).unwrap();
    let got = replay(&mut r, "target-2", &[carried]).unwrap();
    assert_eq!(got.replayed.len(), 1);
    let replayed = r
        .read()
        .block(&Scope::View("target-2".into()), &claim)
        .unwrap();
    assert_eq!(replayed.foreign.pins[&target], pin);
    assert_eq!(
        r.read().foreign_held_pin(&target, &pin).unwrap(),
        Some(c.v1)
    );
    r.close().unwrap();
}
