//! rpl10 (repo links; D-DAPR8, D-FK58A, D-MEMMQ): the citing side's take of a link delivery and
//! the revocation of a link, through the adapter's one writer. A take checks everything first,
//! then writes its new BLOCK versions, its FOREIGN entry and its source's ref as one sealed
//! batch. A delivery that is the same as the source's last (D-MEMMQ §5: versions, derived facts,
//! set kind and team, under one profile) writes nothing and returns `unchanged: true`. Captured
//! snapshots keep the foreign cut they were taken with.
use rdy_adapter::{
    BlockId, DeliverySource, ForeignSet, Hash, LinkSource, RdyError, RepoId, StoreHandle,
};
use rdy_model::Block;
use rdy_model::foreign::{ForeignDerived, ForeignPublication, ForeignVersion, selection_identity};
use rdy_store::codec;
use rdy_store::format::baseline_features;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{DeliveryRecord, StreamHeader, StreamRecord, StreamWriter};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::sync::Barrier;

/// The repo the deliveries cite.
const CITED: RepoId = RepoId([7; 16]);
const PROFILE: &str = "pp-00001";

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// An empty project on disk, closed again.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    rdy_engine::Rdy::init(dir.path(), "rdy-standard")
        .unwrap()
        .close()
        .unwrap();
    dir
}

/// A cited block, as the cited side delivers it.
fn block(id_: &str, body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: "decision".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn derived(status: &str) -> ForeignDerived {
    ForeignDerived {
        status: Some(status.into()),
        lifecycle: None,
        superseded_by: vec![],
    }
}

/// The delivered version of `b` with derived facts `d`.
fn version(b: &Block, d: ForeignDerived) -> ForeignVersion {
    ForeignVersion {
        id: b.id.clone(),
        version: b.version_hash(),
        content: b.content_hash(),
        encoding: 0,
        derived: d,
    }
}

/// `b` as a present BLOCK record, in its own encoding.
fn present(b: &Block) -> StreamRecord {
    let o = codec::encode_new(kinds::BLOCK, b);
    StreamRecord::Present {
        key: ObjectKey {
            kind: kinds::BLOCK,
            hash: Hash::of(&o.payload),
        },
        encoding: o.version,
        payload: o.payload,
    }
}

/// What one delivery stream holds.
struct Stream<'a> {
    repo: RepoId,
    tag: &'a str,
    profile: &'a str,
    profile_version: Option<Hash>,
    /// The selection, with each block's derived facts; sorted by id when written.
    selected: Vec<(&'a Block, ForeignDerived)>,
    /// The ids whose record the stream leaves out.
    missing: Vec<&'a str>,
}

impl<'a> Stream<'a> {
    fn of(selected: &[(&'a Block, &str)]) -> Stream<'a> {
        Stream {
            repo: CITED,
            tag: "v1",
            profile: PROFILE,
            profile_version: Some(Hash([3; 32])),
            selected: selected.iter().map(|(b, s)| (*b, derived(s))).collect(),
            missing: vec![],
        }
    }

    /// Writes the stream to `dir/name` and opens it for reading.
    fn write(&self, dir: &Path, name: &str) -> File {
        let mut selected = self.selected.clone();
        selected.sort_by(|a, b| a.0.id.cmp(&b.0.id));
        let versions: Vec<ForeignVersion> = selected
            .iter()
            .map(|(b, d)| version(b, d.clone()))
            .collect();
        let selection = selection_identity(versions.iter().map(|v| (&v.id, &v.version))).unwrap();
        let delivery = DeliveryRecord {
            publication: ForeignPublication {
                tag: self.tag.into(),
                profile: self.profile.into(),
                profile_version: self.profile_version,
            },
            selection,
            versions,
        };
        let header = StreamHeader::link_delivery(
            baseline_features(),
            self.repo,
            Hash::of(self.tag.as_bytes()),
        );
        let mut w = StreamWriter::new_delivery(Vec::new(), &header, &delivery).unwrap();
        for (b, _) in &selected {
            if !self.missing.contains(&b.id.as_str()) {
                w.record(&present(b)).unwrap();
            }
        }
        let bytes = w.finish().unwrap().0;
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        File::open(path).unwrap()
    }
}

fn source(link: &str) -> DeliverySource {
    DeliverySource {
        link: id(link),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
        set: None,
    }
}

fn link_source(link: &str) -> LinkSource {
    LinkSource {
        link: id(link),
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

/// The take writes the new BLOCK versions, the FOREIGN entry and the source's ref (and, on first
/// use, the features ref) as one sealed batch: nothing else, and no canon move. A later delivery
/// writes only the versions the store does not hold. The entry heads its source after a reopen.
#[test]
fn rpl10_take_is_one_sealed_batch() {
    let dir = project();
    let files = tempfile::tempdir().unwrap();
    let (a, b, c) = (
        block("d-00001", "A"),
        block("d-00002", "B"),
        block("d-00003", "C"),
    );
    let first = Stream::of(&[(&a, "accepted"), (&b, "accepted")]).write(files.path(), "1");
    let mut rdy = rdy_engine::Rdy::open(dir.path()).unwrap();
    let canon = rdy.read().canon_head();
    let src = rdy_model::foreign::ForeignSource::Link {
        link: "rl-00001".into(),
        site: "hub.example".into(),
        at_ms: 1,
    };
    let taken = rdy_engine::foreign_delivery::take_delivery(&mut rdy, &first, &src, None).unwrap();
    assert!(!taken.unchanged);
    assert_eq!(
        (
            taken.added,
            taken.changed,
            taken.removed,
            taken.blocks_taken
        ),
        (2, 0, 0, 2)
    );
    assert_eq!(taken.repo, CITED);
    let store = rdy.store();
    assert!(store.open_batch().is_none(), "the batch is sealed");
    assert_eq!(store.last_batch_end(), store.log_end());
    let kinds_of = |frames: &[(u8, Hash)]| {
        let mut out: BTreeMap<u8, usize> = BTreeMap::new();
        for (k, _) in frames {
            *out.entry(*k).or_default() += 1;
        }
        out
    };
    // two BLOCKs, one FOREIGN, the source ref and the first use's features ref; no move
    assert_eq!(
        kinds_of(store.last_batch()),
        BTreeMap::from([
            (kinds::BLOCK, 2),
            (kinds::FOREIGN, 1),
            (kinds::REF_UPDATE, 2)
        ])
    );
    assert!(store.last_batch().contains(&(kinds::FOREIGN, taken.entry)));
    assert_eq!(
        rdy.read().canon_head(),
        canon,
        "a delivery moves no canon head"
    );
    {
        let state = rdy.read();
        let seen = state
            .foreign()
            .unwrap()
            .link_source(&id("rl-00001"))
            .unwrap();
        assert_eq!(seen.head, taken.entry);
        assert_eq!(seen.members.len(), 2);
    }
    // a second delivery: c is new, b changed, a left; only c's BLOCK is written
    let b2 = block("d-00002", "B2");
    let second = Stream::of(&[(&b2, "accepted"), (&c, "accepted")]).write(files.path(), "2");
    let next = rdy_engine::foreign_delivery::take_delivery(&mut rdy, &second, &src, None).unwrap();
    assert_eq!(
        (next.added, next.changed, next.removed, next.blocks_taken),
        (1, 1, 1, 2)
    );
    assert_eq!(
        kinds_of(rdy.store().last_batch()),
        BTreeMap::from([
            (kinds::BLOCK, 2),
            (kinds::FOREIGN, 1),
            (kinds::REF_UPDATE, 1)
        ])
    );
    rdy.close().unwrap();
    // the adapter's handle reads the same head after a reopen
    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let view = snap
        .foreign_source_for_tests(&id("rl-00001"))
        .unwrap()
        .unwrap();
    assert_eq!(view.head, next.entry);
    assert_eq!(
        view.members.keys().cloned().collect::<Vec<_>>(),
        [id("d-00002"), id("d-00003")]
    );
    assert_eq!(
        view.members[&id("d-00002")].version.version,
        b2.version_hash()
    );
    handle.close().unwrap();
}

/// The same stream again writes nothing and returns the source's head, unchanged.
#[test]
fn rpl10_identical_delivery_is_noop() {
    let dir = project();
    let files = tempfile::tempdir().unwrap();
    let (a, b) = (block("d-00001", "A"), block("d-00002", "B"));
    let stream = Stream::of(&[(&a, "accepted"), (&b, "accepted")]).write(files.path(), "1");
    let handle = StoreHandle::open(dir.path()).unwrap();
    let first = handle.take_delivery(&stream, source("rl-00001")).unwrap();
    assert!(!first.unchanged);
    let again = handle.take_delivery(&stream, source("rl-00001")).unwrap();
    assert!(again.unchanged);
    assert_eq!(again.entry, first.entry);
    assert_eq!(again.end, first.end, "a same delivery appends nothing");
    assert_eq!(again.selection, first.selection);
    assert_eq!(
        (
            again.added,
            again.changed,
            again.removed,
            again.blocks_taken
        ),
        (0, 0, 0, 0)
    );
    handle.close().unwrap();
}

/// A new tag or a new profile version alone is the same delivery: nothing is written, and the
/// source keeps the publication of its last delivery that changed something.
#[test]
fn rpl10_tag_or_profile_version_alone_writes_nothing() {
    let dir = project();
    let files = tempfile::tempdir().unwrap();
    let (a, b) = (block("d-00001", "A"), block("d-00002", "B"));
    let handle = StoreHandle::open(dir.path()).unwrap();
    let first = handle
        .take_delivery(
            &Stream::of(&[(&a, "accepted"), (&b, "accepted")]).write(files.path(), "1"),
            source("rl-00001"),
        )
        .unwrap();
    let mut retagged = Stream::of(&[(&a, "accepted"), (&b, "accepted")]);
    retagged.tag = "v2";
    let t = handle
        .take_delivery(&retagged.write(files.path(), "2"), source("rl-00001"))
        .unwrap();
    assert!(t.unchanged);
    let mut reversioned = Stream::of(&[(&a, "accepted"), (&b, "accepted")]);
    reversioned.tag = "v3";
    reversioned.profile_version = Some(Hash([4; 32]));
    let p = handle
        .take_delivery(&reversioned.write(files.path(), "3"), source("rl-00001"))
        .unwrap();
    assert!(p.unchanged);
    reversioned.profile_version = None;
    let none = handle
        .take_delivery(&reversioned.write(files.path(), "4"), source("rl-00001"))
        .unwrap();
    assert!(none.unchanged);
    assert_eq!((t.end, p.end, none.end), (first.end, first.end, first.end));
    let view = handle
        .snapshot()
        .unwrap()
        .foreign_source_for_tests(&id("rl-00001"))
        .unwrap()
        .unwrap();
    assert_eq!(view.head, first.entry);
    assert_eq!(view.publication.tag, "v1");
    assert_eq!(view.publication.profile_version, Some(Hash([3; 32])));
    handle.close().unwrap();
}

/// A change of derived facts alone, of the set kind or team, or of the profile, writes a
/// delivery even with no new BLOCK; the same delivery after it writes nothing.
#[test]
fn rpl10_derived_fact_or_set_change_writes_delivery() {
    let dir = project();
    let files = tempfile::tempdir().unwrap();
    let (a, b) = (block("d-00001", "A"), block("d-00002", "B"));
    let handle = StoreHandle::open(dir.path()).unwrap();
    let take = |s: &Stream, name: &str, set: Option<ForeignSet>| {
        let mut src = source("rl-00001");
        src.set = set;
        handle
            .take_delivery(&s.write(files.path(), name), src)
            .unwrap()
    };
    let first = take(
        &Stream::of(&[(&a, "accepted"), (&b, "accepted")]),
        "1",
        None,
    );
    // a status change of b: one changed member, no BLOCK
    let mut status = Stream::of(&[(&a, "accepted"), (&b, "superseded")]);
    status.selected[1].1.superseded_by = vec![id("d-00001")];
    let s = take(&status, "2", None);
    assert!(!s.unchanged);
    assert_eq!(
        (s.added, s.changed, s.removed, s.blocks_taken),
        (0, 1, 0, 0)
    );
    assert_eq!(
        s.selection, first.selection,
        "the same versions are selected"
    );
    assert_ne!(s.end, first.end);
    // the set kind, then the team: an empty delta, written
    let org = take(&status, "3", Some(ForeignSet::Organization));
    assert!(!org.unchanged);
    assert_eq!((org.added, org.changed, org.removed), (0, 0, 0));
    let team = take(&status, "4", Some(ForeignSet::Team("core".into())));
    assert!(!team.unchanged);
    let other = take(&status, "5", Some(ForeignSet::Team("web".into())));
    assert!(!other.unchanged);
    let same = take(&status, "6", Some(ForeignSet::Team("web".into())));
    assert!(same.unchanged);
    assert_eq!(same.entry, other.entry);
    // another profile is another delivery
    let mut profile = Stream::of(&[(&a, "accepted"), (&b, "superseded")]);
    profile.selected[1].1.superseded_by = vec![id("d-00001")];
    profile.profile = "pp-00002";
    let p = take(&profile, "7", Some(ForeignSet::Team("web".into())));
    assert!(!p.unchanged);
    let view = handle
        .snapshot()
        .unwrap()
        .foreign_source_for_tests(&id("rl-00001"))
        .unwrap()
        .unwrap();
    assert_eq!(view.head, p.entry);
    assert_eq!(view.set, Some(ForeignSet::Team("web".into())));
    assert_eq!(view.publication.profile, "pp-00002");
    let held_b = &view.members[&id("d-00002")].version;
    assert_eq!(held_b.derived.status.as_deref(), Some("superseded"));
    assert_eq!(held_b.derived.superseded_by, [id("d-00001")]);
    handle.close().unwrap();
}

/// A take refuses this project's own repo, a holder store, a revoked link and a delivery that
/// misses a selected object, with the exact texts, and writes nothing. A revocation freezes the
/// source's view; a second revocation writes nothing; a link with no delivery has none to revoke.
#[test]
fn rpl10_take_refuses_self_holder_revoked_and_missing() {
    let dir = project();
    let files = tempfile::tempdir().unwrap();
    let (a, b) = (block("d-00001", "A"), block("d-00002", "B"));
    let handle = StoreHandle::open(dir.path()).unwrap();
    let own = handle.snapshot().unwrap().identity().repo_id;
    let base = Stream::of(&[(&a, "accepted")]).write(files.path(), "base");
    let first = handle.take_delivery(&base, source("rl-00001")).unwrap();

    // this project's own repo
    let mut own_stream = Stream::of(&[(&a, "accepted")]);
    own_stream.repo = own;
    refused(
        handle
            .take_delivery(&own_stream.write(files.path(), "own"), source("rl-00002"))
            .unwrap_err(),
        "E_BAD_INPUT",
        "a project cannot cite itself",
        "deliver content from another project",
    );

    // a selected version with no record
    let mut missing = Stream::of(&[(&a, "accepted"), (&b, "accepted")]);
    missing.missing = vec!["d-00002"];
    let e = handle
        .take_delivery(&missing.write(files.path(), "missing"), source("rl-00001"))
        .unwrap_err();
    assert_eq!(e.code, "E_UNRESOLVED_REF", "{e:?}");
    assert_eq!(
        e.message,
        format!("stream record 1 needs 1:{}", b.version_hash()),
        "{e:?}"
    );
    assert_eq!(e.fix, "send the missing object with its dependencies");

    // a dropped version this store does not hold has no payload to take
    let dropped = {
        let versions = vec![version(&b, derived("accepted"))];
        let selection = selection_identity(versions.iter().map(|v| (&v.id, &v.version))).unwrap();
        let delivery = DeliveryRecord {
            publication: ForeignPublication {
                tag: "v1".into(),
                profile: PROFILE.into(),
                profile_version: None,
            },
            selection,
            versions,
        };
        let header = StreamHeader::link_delivery(baseline_features(), CITED, Hash([1; 32]));
        let mut w = StreamWriter::new_delivery(Vec::new(), &header, &delivery).unwrap();
        let o = codec::encode_new(kinds::BLOCK, &b);
        w.record(&StreamRecord::Dropped {
            key: ObjectKey {
                kind: kinds::BLOCK,
                hash: b.version_hash(),
            },
            encoding: o.version,
            original_len: o.payload.len() as u32,
            apply: Hash([5; 32]),
            residue: Some(Hash([6; 32])),
        })
        .unwrap();
        let path = files.path().join("dropped");
        std::fs::write(&path, w.finish().unwrap().0).unwrap();
        File::open(path).unwrap()
    };
    let e = handle
        .take_delivery(&dropped, source("rl-00001"))
        .unwrap_err();
    assert_eq!(e.code, "E_UNRESOLVED_REF", "{e:?}");

    // a tag history is not a delivery
    let history = {
        let header = StreamHeader::tag_history(baseline_features(), CITED, Hash([1; 32]), vec![]);
        let w = StreamWriter::new(Vec::new(), &header).unwrap();
        let path = files.path().join("history");
        std::fs::write(&path, w.finish().unwrap().0).unwrap();
        File::open(path).unwrap()
    };
    refused(
        handle
            .take_delivery(&history, source("rl-00001"))
            .unwrap_err(),
        "E_STREAM_PURPOSE",
        "stream purpose 0 does not match link_delivery",
        "request a link_delivery stream",
    );

    // none of these wrote anything: the same delivery is still unchanged at the same end
    let same = handle.take_delivery(&base, source("rl-00001")).unwrap();
    assert!(same.unchanged);
    assert_eq!(same.end, first.end);

    // a link with no delivery has none to revoke
    refused(
        handle.revoke_link(link_source("rl-00009")).unwrap_err(),
        "E_NOT_FOUND",
        "source foreign/link/rl-00009 has no delivery to revoke",
        "revoke a repo link after its first delivery",
    );

    // a revoked link keeps its last view and takes no delivery
    let revoked = handle.revoke_link(link_source("rl-00001")).unwrap();
    assert!(!revoked.unchanged);
    assert_ne!(revoked.end, first.end);
    let again = handle.revoke_link(link_source("rl-00001")).unwrap();
    assert!(again.unchanged);
    assert_eq!((again.entry, again.end), (revoked.entry, revoked.end));
    for (name, s) in [
        ("same", Stream::of(&[(&a, "accepted")])),
        ("next", Stream::of(&[(&a, "accepted"), (&b, "accepted")])),
    ] {
        refused(
            handle
                .take_delivery(&s.write(files.path(), name), source("rl-00001"))
                .unwrap_err(),
            "E_BAD_INPUT",
            "the repo link is revoked",
            "use a live repo link",
        );
    }
    let view = handle
        .snapshot()
        .unwrap()
        .foreign_source_for_tests(&id("rl-00001"))
        .unwrap()
        .unwrap();
    assert!(view.revoked);
    assert_eq!(view.head, revoked.entry);
    assert_eq!(
        view.members.keys().cloned().collect::<Vec<_>>(),
        [id("d-00001")]
    );
    handle.close().unwrap();

    // a holder takes no delivery
    let holder_dir = tempfile::tempdir().unwrap();
    let holder = StoreHandle::create_holder(&holder_dir.path().join("h"), RepoId([9; 16])).unwrap();
    refused(
        holder.take_delivery(&base, source("rl-00001")).unwrap_err(),
        "E_BAD_INPUT",
        "this store is a holder",
        "open it through the holder adapter",
    );
    refused(
        holder.revoke_link(link_source("rl-00001")).unwrap_err(),
        "E_BAD_INPUT",
        "this store is a holder",
        "open it through the holder adapter",
    );
    holder.close().unwrap();
}

/// A snapshot captured before a take keeps its foreign cut while the take runs on another
/// thread and after it ends; a snapshot taken after the take sees the new entry.
#[test]
fn rpl10_take_does_not_block_captured_reads() {
    let dir = project();
    let files = tempfile::tempdir().unwrap();
    let (a, b) = (block("d-00001", "A"), block("d-00002", "B"));
    let first = Stream::of(&[(&a, "accepted")]).write(files.path(), "1");
    let second = Stream::of(&[(&a, "accepted"), (&b, "accepted")]).write(files.path(), "2");
    let handle = StoreHandle::open(dir.path()).unwrap();
    let before = handle.snapshot().unwrap();
    let barrier = Barrier::new(2);
    let taken = std::thread::scope(|s| {
        let take = s.spawn(|| {
            barrier.wait();
            handle.take_delivery(&first, source("rl-00001")).unwrap()
        });
        barrier.wait();
        // the captured snapshot answers while the take runs, from its own cut
        assert!(
            before
                .foreign_source_for_tests(&id("rl-00001"))
                .unwrap()
                .is_none()
        );
        assert!(before.tags().unwrap().is_empty());
        take.join().unwrap()
    });
    assert!(
        before
            .foreign_source_for_tests(&id("rl-00001"))
            .unwrap()
            .is_none(),
        "a captured snapshot never sees a later delivery"
    );
    let mid = handle.snapshot().unwrap();
    let seen = mid
        .foreign_source_for_tests(&id("rl-00001"))
        .unwrap()
        .unwrap();
    assert_eq!(seen.head, taken.entry);
    let next = handle.take_delivery(&second, source("rl-00001")).unwrap();
    assert!(!next.unchanged);
    // the snapshot captured between the takes keeps the first view
    let still = mid
        .foreign_source_for_tests(&id("rl-00001"))
        .unwrap()
        .unwrap();
    assert_eq!(still.head, taken.entry);
    assert_eq!(still.members.len(), 1);
    let after = handle.snapshot().unwrap();
    let now = after
        .foreign_source_for_tests(&id("rl-00001"))
        .unwrap()
        .unwrap();
    assert_eq!(now.head, next.entry);
    assert_eq!(now.members.len(), 2);
    handle.close().unwrap();
}

/// The input's bounds are checked before the handle is cloned or the stream is read: a site or
/// a team name over the bounded decoder's text bound is E_LIMIT, and nothing is written.
#[test]
fn rpl10_take_checks_input_bounds_first() {
    let dir = project();
    let files = tempfile::tempdir().unwrap();
    let a = block("d-00001", "A");
    let stream = Stream::of(&[(&a, "accepted")]).write(files.path(), "1");
    let handle = StoreHandle::open(dir.path()).unwrap();
    let mut long_site = source("rl-00001");
    long_site.site = "s".repeat(1_048_577);
    refused(
        handle.take_delivery(&stream, long_site).unwrap_err(),
        "E_LIMIT",
        "site exceeds 1048576 text bytes",
        "reduce the text to the stated limit",
    );
    let mut long_team = source("rl-00001");
    long_team.set = Some(ForeignSet::Team("t".repeat(1_048_577)));
    refused(
        handle.take_delivery(&stream, long_team).unwrap_err(),
        "E_LIMIT",
        "team exceeds 1048576 text bytes",
        "reduce the text to the stated limit",
    );
    let mut long_revoke = link_source("rl-00001");
    long_revoke.site = "s".repeat(1_048_577);
    assert_eq!(handle.revoke_link(long_revoke).unwrap_err().code, "E_LIMIT");
    // nothing was written: the first take of the source still writes its delivery
    let taken = handle.take_delivery(&stream, source("rl-00001")).unwrap();
    assert!(!taken.unchanged);
    assert_eq!((taken.added, taken.blocks_taken), (1, 1));
    handle.close().unwrap();
}
