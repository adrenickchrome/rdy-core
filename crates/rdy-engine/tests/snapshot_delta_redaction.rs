//! sdl4 (snapshot deltas, D-918YB): a removal report reads each landing's full map through the
//! store's one map read, so a landing whose map still names an affected version is affected,
//! and so is every tag on it, though its own delta does not repeat the version. A delta whose
//! set names an affected version is a row of its own kind (`snapshot_delta`); a held SNAPSHOT
//! that names one keeps its row. The figure's light and dark images stay references.
//!
//! Until the delta writer ships, a landing writes its SNAPSHOT. The fixtures append each
//! landing's delta afterwards. A holder that takes the history then holds the deltas and no
//! SNAPSHOT, which is the delta-only history the writer will make.
mod common;

use rdy_engine::publication_take::{PublicationRefusal, PublicationTarget, take_publication};
use rdy_engine::redaction::RedactInput;
use rdy_engine::*;
use rdy_model::redaction::{ImpactKind, ImpactRelation, ImpactRow};
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::codec;
use rdy_store::format::stage_snapshot_delta_for_tests;
use rdy_store::kinds;
use rdy_store::snapshot::folded_deltas_for_tests;
use rdy_store::sync_stream::StreamHeader;
use rdy_store::take_batch::UploadTarget;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::Path;

const SECRET: &str = "The launch code is ZEBRA-7731.";

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("sdl4 {view}"),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e}"))
}

fn land(r: &mut Rdy, view: &str) -> Hash {
    let p = r
        .propose(&writer(), view, "sdl4", Some(common::TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"));
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id).unwrap().parse().unwrap()
}

fn input(targets: &[String], dry_run: bool) -> RedactInput {
    RedactInput {
        targets: targets.to_vec(),
        reason_class: "secret".into(),
        reason: "a leaked code".into(),
        dry_run,
    }
}

fn row(
    kind: ImpactKind,
    record: impl ToString,
    relation: ImpactRelation,
    erasable: bool,
) -> ImpactRow {
    ImpactRow {
        kind,
        record: record.to_string(),
        relation,
        erasable,
    }
}

fn refs(kind: ImpactKind, record: impl ToString) -> ImpactRow {
    row(kind, record, ImpactRelation::References, false)
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

/// The delta batch of the history: each canon landing's delta, oldest first, from its held
/// SNAPSHOT (a raw typed read, not the map read under test). The same items as the upgrade step
/// appends to a store written before the delta writer.
fn delta_batch(r: &Rdy) -> Vec<SnapshotDelta> {
    let mut chain = r.store().landings().unwrap();
    chain.reverse();
    let mut out = vec![];
    let mut prev: Option<(BTreeMap<BlockId, Hash>, Hash)> = None;
    for (landing, rec) in chain {
        let map = r.store().get::<StateSnapshot>(&rec.state).unwrap().blocks;
        let empty = BTreeMap::new();
        let (before, base) = match &prev {
            Some((m, d)) => (m, Some(*d)),
            None => (&empty, None),
        };
        let d = diff_snapshots(landing, base, before, &map);
        prev = Some((map, d.id()));
        out.push(d);
    }
    out
}

/// Appends [`delta_batch`] in one batch and returns the deltas, oldest landing first.
fn add_deltas(r: &mut Rdy) -> Vec<SnapshotDelta> {
    let deltas = delta_batch(r);
    let items = deltas
        .iter()
        .map(|d| codec::item_new(kinds::SNAPSHOT_DELTA, d))
        .collect();
    r.store_mut().write(items).unwrap();
    r.invalidate();
    deltas
}

/// Tag `tag`'s closure in project `r` as a holder takes it: a holder of the same repository
/// under `dir`, which holds the landings' deltas and no SNAPSHOT.
fn holder_of(r: &Rdy, dir: &Path, tag: Hash) -> Rdy {
    let file = dir.join("history.rdysync");
    {
        let reader = r.store().reader();
        let c = closure(&reader, tag, &BTreeSet::new()).unwrap();
        let header = StreamHeader::tag_history(
            reader.store_features().unwrap(),
            r.store().repo_id().unwrap().unwrap(),
            tag,
            c.prerequisites,
        );
        let mut source = StoreSource {
            reader: &reader,
            end: END_OF_LOG,
        };
        write_batch(
            &mut source,
            &header,
            &c.records,
            File::create(&file).unwrap(),
        )
        .unwrap();
    }
    let mut h =
        Rdy::create_holder(&dir.join("holder"), r.store().repo_id().unwrap().unwrap()).unwrap();
    let t = r.store().read_tag(&tag).unwrap();
    let target = PublicationTarget {
        upload: UploadTarget {
            tag: t.name,
            landing: t.landing,
        },
        profile: "implicit:org".into(),
    };
    take_publication(&mut h, &[File::open(&file).unwrap()], target, |_| {
        Ok::<_, ()>(())
    })
    .map_err(|e| match e {
        PublicationRefusal::Core(e) => e,
        PublicationRefusal::Verdict(()) => panic!("the verdict refused"),
    })
    .unwrap();
    h
}

/// The rows of `kind` in `rows`, as records.
fn records(rows: &[ImpactRow], kind: ImpactKind) -> BTreeSet<String> {
    rows.iter()
        .filter(|r| r.kind == kind)
        .map(|r| r.record.clone())
        .collect()
}

/// A project whose first landing creates a doc and a prose block holding `SECRET`, and whose
/// second lands an independent change (a new folder). The second landing is tagged `v2.0.0`.
struct Two {
    dir: tempfile::TempDir,
    r: Rdy,
    prose: BlockId,
    /// The prose block's one version.
    v: Hash,
    /// The prose block's create change.
    change: Hash,
    l0: Hash,
    l1: Hash,
}

fn two_landings() -> Two {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(&dir.path().join("src"), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": SECRET}
        ]),
    );
    let prose = BlockId::parse(&w.created["$p"]).unwrap();
    let change: Hash = w.change.parse().unwrap();
    let l0 = land(&mut r, "work");
    let c = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "folder", "fields": {"name": "other", "title": "Other"}}]),
    );
    let l1 = land(&mut r, "work");
    r.tag(&approver(), "v2.0.0", None, None).unwrap();
    // the second landing's change depends on nothing, so only its map reaches the version
    let c: Hash = c.change.parse().unwrap();
    assert!(
        r.store().change(&c).unwrap().deps.is_empty(),
        "an independent change"
    );
    assert_eq!(r.store().landing(&l1).unwrap().changes, vec![c]);
    let v = r
        .read()
        .block(&Scope::Canon, &prose)
        .unwrap()
        .version_hash();
    Two {
        dir,
        r,
        prose,
        v,
        change,
        l0,
        l1,
    }
}

#[test]
fn sdl4_unchanged_versions_affect_later_landings_and_tags() {
    let _clock = pin_clock(1_000_000);
    let _staged = stage_snapshot_delta_for_tests();
    let mut t = two_landings();
    let deltas = add_deltas(&mut t.r);
    // the first delta sets the version; the second sets only the new folder
    assert!(deltas[0].set.iter().any(|(_, v)| *v == t.v));
    assert!(!deltas[1].set.iter().any(|(_, v)| *v == t.v));
    assert!(!deltas[1].drop.contains(&t.prose));
    let tag = tag_hash(&t.r, "v2.0.0");
    let h = holder_of(&t.r, t.dir.path(), tag);
    let state = t.r.store().landing(&t.l1).unwrap().state;
    assert!(
        !h.store().has_kind(kinds::SNAPSHOT, &state).unwrap(),
        "the holder holds no SNAPSHOT"
    );
    let target = [format!("{}@{}", t.prose, t.v)];

    // the delta-only history: the later landing's folded map still names the version
    let start = folded_deltas_for_tests();
    let d = h.read().redaction_report(&input(&target, true)).unwrap();
    assert!(
        folded_deltas_for_tests() > start,
        "the report reads the landing maps by the fold"
    );
    assert_eq!(
        records(&d.inside, ImpactKind::Landing),
        BTreeSet::from([t.l0.to_string(), t.l1.to_string()]),
        "{:#?}",
        d.inside
    );
    assert!(
        d.inside.contains(&refs(ImpactKind::Tag, "v2.0.0")),
        "{:#?}",
        d.inside
    );
    // only the delta that sets the version is a delta row; no SNAPSHOT row in the holder
    assert_eq!(
        records(&d.inside, ImpactKind::SnapshotDelta),
        BTreeSet::from([deltas[0].id().to_string()])
    );
    assert!(records(&d.inside, ImpactKind::StateSnapshot).is_empty());

    // the source reads the same landings and tags by the fold, its SNAPSHOTs read past
    t.r.store().fold_held_snapshots_for_tests(true);
    t.r.invalidate();
    let start = folded_deltas_for_tests();
    let s = t.r.read().redaction_report(&input(&target, true)).unwrap();
    assert!(folded_deltas_for_tests() > start);
    for kind in [
        ImpactKind::Landing,
        ImpactKind::Tag,
        ImpactKind::SnapshotDelta,
    ] {
        assert_eq!(
            records(&s.inside, kind),
            records(&d.inside, kind),
            "{kind:?}"
        );
    }
    // a third landing that replaces the version leaves it: its map names it no more, and its
    // independent tag reaches it only through its history
    write(
        &mut t.r,
        "work",
        json!([{"op": "update", "id": t.prose.to_string(), "base": t.v.to_string(), "body": "Rotated."}]),
    );
    let l2 = land(&mut t.r, "work");
    write(
        &mut t.r,
        "work",
        json!([{"op": "create", "type": "folder", "fields": {"name": "third", "title": "Third"}}]),
    );
    let l3 = land(&mut t.r, "work");
    let all = add_deltas_after(&mut t.r, &deltas);
    t.r.tag(&approver(), "v3.0.0", None, None).unwrap();
    let s = t.r.read().redaction_report(&input(&target, true)).unwrap();
    let landings = records(&s.inside, ImpactKind::Landing);
    assert!(
        landings.contains(&l2.to_string()),
        "l2 lands a change of the version"
    );
    assert!(
        !landings.contains(&l3.to_string()),
        "l3's map no longer names the version: {landings:?}"
    );
    assert!(s.inside.contains(&refs(ImpactKind::Tag, "v3.0.0")));
    // l2's delta drops nothing and sets the new version: it names the old one nowhere
    assert!(!all[2].set.iter().any(|(_, v)| *v == t.v));
    assert_eq!(
        records(&s.inside, ImpactKind::SnapshotDelta),
        BTreeSet::from([deltas[0].id().to_string()])
    );
    assert!(t.change != Hash([0; 32]));
}

/// Appends the deltas of the landings after `held` (the deltas already written, oldest first),
/// and returns every delta of the chain.
fn add_deltas_after(r: &mut Rdy, held: &[SnapshotDelta]) -> Vec<SnapshotDelta> {
    let all = delta_batch(r);
    assert_eq!(&all[..held.len()], held);
    let items = all[held.len()..]
        .iter()
        .map(|d| codec::item_new(kinds::SNAPSHOT_DELTA, d))
        .collect();
    r.store_mut().write(items).unwrap();
    r.invalidate();
    all
}

#[test]
fn sdl4_delta_rows_and_held_snapshot_rows_coexist() {
    let _clock = pin_clock(1_000_000);
    let _staged = stage_snapshot_delta_for_tests();
    let mut t = two_landings();
    let target = [format!("{}@{}", t.prose, t.v)];
    let before = t.r.read().redaction_report(&input(&target, true)).unwrap();
    let deltas = add_deltas(&mut t.r);
    let after = t.r.read().redaction_report(&input(&target, true)).unwrap();
    // every row of the held history stays, in its place; the delta row joins after them all
    let delta_row = refs(ImpactKind::SnapshotDelta, deltas[0].id());
    let mut want = before.inside.clone();
    want.push(delta_row.clone());
    assert_eq!(after.inside, want);
    let st = t.r.read();
    let snaps: BTreeSet<String> = [t.l0, t.l1]
        .iter()
        .map(|l| st.reader().landing(l).unwrap().state.to_string())
        .collect();
    drop(st);
    assert_eq!(records(&after.inside, ImpactKind::StateSnapshot), snaps);
    assert_eq!(
        records(&after.inside, ImpactKind::Landing),
        BTreeSet::from([t.l0.to_string(), t.l1.to_string()])
    );
    // a delta is a reference, never erased and never quoted: only the block and its change drop
    assert!(!delta_row.erasable);
    assert_eq!(
        (after.drop_frames, after.drop_bytes),
        (before.drop_frames, before.drop_bytes)
    );
    assert_eq!(after.quotes, before.quotes);
    assert_eq!(after.versions, vec![(t.prose.clone(), t.v)]);
    assert!(after.inside.contains(&row(
        ImpactKind::Change,
        t.change,
        ImpactRelation::Embeds,
        true
    )));
    // the report's own hash moves with the new row
    assert_ne!(after.report_hash, before.report_hash);
    // the wire name of the new row
    let v = serde_json::to_value(&after.inside).unwrap();
    assert_eq!(
        v.as_array().unwrap().last().unwrap()["kind"],
        json!("snapshot_delta")
    );
    assert_eq!(
        rdy_engine::redaction::presentation::kind_name(ImpactKind::SnapshotDelta),
        "snapshot_delta"
    );
}

/// The removal request's approval after the history gains its deltas.
///
/// DEPENDS ON SD1 Task 5: the upgrade executor that appends the deltas does not exist yet. This
/// test appends the exact delta batch the upgrade step writes (each landing's delta, oldest
/// first, in one batch) in its place. Task 5 runs it again through the real executor; until
/// then it does not show that the executor makes the request stale.
#[test]
fn sdl4_upgrade_makes_a_prepared_removal_stale() {
    let _clock = pin_clock(1_000_000);
    let _staged = stage_snapshot_delta_for_tests();
    let mut t = two_landings();
    let target = vec![format!("{}@{}", t.prose, t.v)];
    let res =
        t.r.redact_in(&writer(), "redact", input(&target, false))
            .unwrap();
    assert!(res.redaction.is_some(), "the request is recorded");
    let prop =
        t.r.propose(
            &writer(),
            "redact",
            "Remove the code",
            Some("Removes the leaked code."),
        )
        .unwrap();
    let old = res.report.report_hash;
    let pre = t.r.read().redaction_report(&input(&target, true)).unwrap();
    // the upgrade's delta batch is written after the request
    let deltas = add_deltas(&mut t.r);
    let fresh = t.r.read().redaction_report(&input(&target, true)).unwrap();
    assert_ne!(fresh.report_hash, old, "the report gains the delta row");
    assert_ne!(fresh.report_hash, pre.report_hash);
    let mut want = pre.inside.clone();
    want.push(refs(ImpactKind::SnapshotDelta, deltas[0].id()));
    assert_eq!(fresh.inside, want, "the delta row is the only change");
    // the approval carrying the old report is stale: nothing is refreshed or written
    let end = t.r.store().log_end();
    let e = match t.r.land(&approver(), &prop.id) {
        Ok(l) => panic!("the stale removal landed as {l}"),
        Err(e) => e,
    };
    assert_eq!(e.code, E_REDACTION_STALE, "{e}");
    assert_eq!(
        e.message,
        "The redaction report changed after the request was recorded."
    );
    assert_eq!(
        e.fix,
        "Record a new request, show its report, and ask for approval again."
    );
    assert_eq!(t.r.store().log_end(), end, "nothing is written");
    assert_eq!(
        t.r.read().block(&Scope::Canon, &t.prose).unwrap().body,
        SECRET,
        "the content stays"
    );
}

/// A PNG's signature and a tail, so each picture is a distinct blob.
fn picture(tail: &str) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    b.extend_from_slice(tail.as_bytes());
    b
}

#[test]
fn sdl4_removal_reports_keep_dark_and_removed_references() {
    let _clock = pin_clock(1_000_000);
    let _staged = stage_snapshot_delta_for_tests();
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(&dir.path().join("src"), "rdy-standard").unwrap();
    let light = r.store_mut().put_blob(&picture("light")).unwrap();
    let dark = r.store_mut().put_blob(&picture("dark")).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
            {"op": "create", "type": "figure", "id": "$g", "parent": "$s", "fields": {"blob": light.to_string(), "media_type": "image/png", "dark_blob": dark.to_string(), "dark_media_type": "image/png", "alt": "A figure"}}
        ]),
    );
    let fig = BlockId::parse(&w.created["$g"]).unwrap();
    let l0 = land(&mut r, "work");
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "folder", "fields": {"name": "other", "title": "Other"}}]),
    );
    let l1 = land(&mut r, "work");
    r.tag(&approver(), "v2.0.0", None, None).unwrap();
    let fv = r.read().block(&Scope::Canon, &fig).unwrap().version_hash();
    let deltas = add_deltas(&mut r);
    let h = holder_of(&r, dir.path(), tag_hash(&r, "v2.0.0"));
    let figure = format!("{fig}@{fv}");
    for (name, store) in [("source", &r), ("holder", &h)] {
        let report = |t: String| store.read().redaction_report(&input(&[t], true)).unwrap();
        // each image is a reference of the figure's version: its light and its dark one
        for blob in [light, dark] {
            let d = report(format!("blob:{blob}"));
            assert!(
                d.inside.contains(&refs(ImpactKind::BlockVersion, &figure)),
                "{name} {blob}: {:#?}",
                d.inside
            );
            assert_eq!(
                records(&d.inside, ImpactKind::Landing),
                BTreeSet::from([l0.to_string(), l1.to_string()]),
                "{name} {blob}"
            );
            assert!(
                d.inside.contains(&refs(ImpactKind::Tag, "v2.0.0")),
                "{name}"
            );
            assert_eq!(
                records(&d.inside, ImpactKind::SnapshotDelta),
                BTreeSet::from([deltas[0].id().to_string()]),
                "{name} {blob}"
            );
        }
        // the figure's version itself is removed, and both its images go with it
        let d = report(figure.clone());
        assert!(
            d.inside.contains(&row(
                ImpactKind::BlockVersion,
                &figure,
                ImpactRelation::Removed,
                true
            )),
            "{name}: {:#?}",
            d.inside
        );
        assert_eq!(
            records(&d.inside, ImpactKind::Landing),
            BTreeSet::from([l0.to_string(), l1.to_string()]),
            "{name}"
        );
        assert_eq!(
            records(&d.inside, ImpactKind::SnapshotDelta),
            BTreeSet::from([deltas[0].id().to_string()]),
            "{name}"
        );
        // the held SNAPSHOT rows are the source's alone
        assert_eq!(
            records(&d.inside, ImpactKind::StateSnapshot).len(),
            if name == "source" { 2 } else { 0 },
            "{name}"
        );
    }
}
