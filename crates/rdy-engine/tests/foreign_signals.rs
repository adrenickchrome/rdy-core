//! rpl14 (repo links, D-Y49PX, D-MEMMQ §6): a delivery moves no pin and no canon head. It
//! writes the suspect moves it makes in its own batch's TRANSITION, with cause `delivered` and
//! the FOREIGN entry as the cause's id, and each moved suspect names its cited target as a
//! foreign id. The index's open suspects, a reindex and fsck reproduce those moves, and a reopen
//! finds nothing unlogged.
mod common;

use rdy_engine::foreign_delivery::take_delivery;
use rdy_engine::*;
use rdy_model::foreign::{
    ForeignDerived, ForeignId, ForeignPin, ForeignPublication, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::*;
use rdy_store::codec;
use rdy_store::format::baseline_features;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{DeliveryRecord, StreamHeader, StreamRecord, StreamWriter};
use rdy_store::transition::{Cause, SuspectTarget, TransitionsV1, foreign_suspect_key};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

const AGENTS: &str = "pf-AGNT1";

fn cited() -> RepoId {
    RepoId([7; 16])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
}

fn land(r: &mut Rdy, view: &str) {
    let p = r
        .propose(&writer(), view, view, Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id)
        .unwrap_or_else(|e| panic!("land {view}: {e:?}"));
}

/// A cited decision, as the cited side delivers it.
fn block(id_: &str, body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: "decision".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::from([("title".to_string(), FieldValue::Str(body.into()))]),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn accepted(superseded_by: &[&str]) -> ForeignDerived {
    ForeignDerived {
        status: Some("accepted".into()),
        lifecycle: None,
        superseded_by: superseded_by.iter().map(|s| id(s)).collect(),
    }
}

fn version(b: &Block, d: &ForeignDerived) -> ForeignVersion {
    ForeignVersion {
        id: b.id.clone(),
        version: b.version_hash(),
        content: b.content_hash(),
        encoding: 0,
        derived: d.clone(),
    }
}

fn pin_of(b: &Block) -> ForeignPin {
    ForeignPin {
        content: b.content_hash(),
        encoding: 0,
    }
}

/// A link delivery stream of `selected` (any order), written to `dir/name` and opened.
fn stream(dir: &Path, name: &str, selected: &[(&Block, ForeignDerived)]) -> File {
    let mut selected = selected.to_vec();
    selected.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    let versions: Vec<ForeignVersion> = selected.iter().map(|(b, d)| version(b, d)).collect();
    let delivery = DeliveryRecord {
        publication: ForeignPublication {
            tag: name.into(),
            profile: AGENTS.into(),
            profile_version: None,
        },
        selection: selection_identity(versions.iter().map(|v| (&v.id, &v.version))).unwrap(),
        versions,
        shells: vec![],
    };
    let header =
        StreamHeader::link_delivery(baseline_features(), cited(), Hash::of(name.as_bytes()));
    let mut w = StreamWriter::new_delivery(Vec::new(), &header, &delivery).unwrap();
    for (b, _) in &selected {
        let o = codec::encode_new(kinds::BLOCK, *b);
        w.record(&StreamRecord::Present {
            key: ObjectKey {
                kind: kinds::BLOCK,
                hash: Hash::of(&o.payload),
            },
            encoding: o.version,
            payload: o.payload,
        })
        .unwrap();
    }
    let path = dir.join(name);
    std::fs::write(&path, w.finish().unwrap().0).unwrap();
    File::open(path).unwrap()
}

/// The hash of the TRANSITION record of the batch the last take sealed, if any.
fn last_record_hash(r: &Rdy) -> Option<Hash> {
    r.store()
        .last_batch()
        .iter()
        .find(|(k, _)| *k == kinds::TRANSITION)
        .map(|(_, h)| *h)
}

/// The TRANSITION record of the batch the last take sealed, if any.
fn last_record(r: &Rdy) -> Option<TransitionsV1> {
    let h = last_record_hash(r)?;
    Some(r.read().reader().get_typed(kinds::TRANSITION, &h).unwrap())
}

/// The canon foreign suspects as (block, target, reason, successor).
fn suspects(r: &Rdy) -> Vec<(BlockId, ForeignId, String, Option<BlockId>)> {
    r.read()
        .foreign_suspects(&Scope::Canon)
        .unwrap()
        .into_iter()
        .map(|s| (s.block, s.target, s.reason, s.successor))
        .collect()
}

#[test]
fn rpl14_delivery_seals_moves_without_moving_pins() {
    let d = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(&d.path().join("p"), "rdy-standard").unwrap();
    let seeded = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "repo_link", "id": "$a", "parent": "$d",
             "fields": {"alias": "core", "repo": cited().to_string(), "profile": AGENTS}}
        ]),
    );
    land(&mut r, "seed");
    let src = ForeignSource::Link {
        link: seeded.created["$a"].clone(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
    };
    let x1 = block("d-FRGNX", "Use X.");
    let y = block("d-FRGNY", "Use Y.");
    take_delivery(
        &mut r,
        &stream(files.path(), "v1", &[(&x1, accepted(&[]))]),
        &src,
        None,
    )
    .unwrap();

    // a landed claim cites X by reference and by a premise link
    let cited_by = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": seeded.created["$s"],
                "body": "Follow {{core:d-FRGNX}}.",
                "links": {"decided_by": [{"target": "core:d-FRGNX"}]}}]),
    );
    land(&mut r, "w");
    let claim = id(&cited_by.created["$c"]);
    let x = ForeignId::new(cited(), x1.id.clone());
    let pins = |r: &Rdy| {
        let st = r.read();
        let m = st.materialized(&Scope::Canon).unwrap();
        m.get(&claim).unwrap().foreign.pins.clone()
    };
    assert_eq!(pins(&r), BTreeMap::from([(x.clone(), pin_of(&x1))]));
    assert!(suspects(&r).is_empty());

    // a delivery that changes X: its own batch logs the new suspect, caused by its entry
    let canon = r.read().canon_head();
    let x2 = block("d-FRGNX", "Use X, version two.");
    let taken = take_delivery(
        &mut r,
        &stream(files.path(), "v2", &[(&x2, accepted(&[]))]),
        &src,
        None,
    )
    .unwrap();
    assert!(
        r.store()
            .last_batch()
            .contains(&(kinds::FOREIGN, taken.entry))
    );
    let rec = last_record(&r).expect("the delivery batch logs its moves");
    assert!(rec.resolved.is_empty() && rec.opened.is_empty() && rec.gone.is_empty());
    assert_eq!(rec.new.len(), 1, "{rec:?}");
    let m = &rec.new[0];
    assert_eq!(
        (&m.block, &m.target, m.reason.as_str()),
        (&claim, &SuspectTarget::Foreign(x.clone()), "changed")
    );
    assert_eq!(
        (m.why.cause, m.why.id),
        (Cause::Delivered, Some(taken.entry))
    );
    assert_eq!(m.key(), foreign_suspect_key(&claim, &x, "changed"));
    assert_eq!(rec.invalid(), None);
    // the batch's events carry the move, its target in the stored reference form
    let events = rdy_engine::events::events_for_batch(
        &r.store().reader(),
        r.store().last_batch_end(),
        r.store().last_batch(),
    )
    .unwrap();
    let moved: Vec<Value> = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::SuspectNew(SuspectMoved::Foreign(_))))
        .map(|e| serde_json::to_value(e).unwrap())
        .collect();
    assert_eq!(moved.len(), 1, "{events:?}");
    assert_eq!(moved[0]["kind"]["event"], "suspect_new");
    assert_eq!(moved[0]["kind"]["target"], format!("{}:d-FRGNX", cited()));
    assert_eq!(moved[0]["kind"]["why"]["cause"], "delivered");
    // a session that writes the citing block reads it in its inbox, the target by its alias
    let sub = rdy_store::inbox::InboxSub {
        actor: "agent:a".into(),
        run: Some("a".into()),
        cursor: rdy_store::inbox::InboxCursor {
            at: r.store().last_batch_end(),
            skip: 0,
        },
        areas: vec![],
        role: "writer".into(),
    };
    let infl = rdy_engine::inbox::inflight::RunInFlight {
        blocks: [claim.clone()].into(),
        ..Default::default()
    };
    let entries = rdy_engine::inbox::moves::transition_entries_shown(
        &sub,
        &infl,
        &[rdy_engine::inbox::moves::MoveRef::NewForeign(m)],
        0,
        r.store().last_batch_end(),
        &|_| "core".to_string(),
    );
    assert_eq!(entries.len(), 1);
    let e = &entries[0].1;
    assert_eq!(
        (e.event, &e.ids, e.line.as_str()),
        (
            "suspect_new",
            &vec![claim.to_string(), format!("{}:d-FRGNX", cited())],
            format!(
                "{claim} is suspect: core:d-FRGNX changed: a delivery (entry {})",
                taken.entry.short()
            )
            .as_str()
        )
    );
    // no pin and no canon head moved
    assert_eq!(r.read().canon_head(), canon);
    assert_eq!(pins(&r), BTreeMap::from([(x.clone(), pin_of(&x1))]));
    assert_eq!(
        suspects(&r),
        vec![(claim.clone(), x.clone(), "changed".to_string(), None)]
    );
    let open = r.read().reader().open_suspects().unwrap();
    assert!(
        open.contains(&(m.key(), last_record_hash(&r).unwrap())),
        "{open:?}"
    );

    // superseded by a block the selection does not hold: no successor to offer
    let by = accepted(&["d-FRGNY"]);
    take_delivery(
        &mut r,
        &stream(files.path(), "v3", &[(&x2, by.clone())]),
        &src,
        None,
    )
    .unwrap();
    let rec = last_record(&r).expect("a status change logs its moves");
    let reasons = |l: &[rdy_store::transition::SuspectMoveV1]| -> Vec<String> {
        l.iter().map(|m| m.reason.clone()).collect()
    };
    assert_eq!(
        (reasons(&rec.gone), reasons(&rec.new)),
        (vec!["changed".to_string()], vec!["superseded".to_string()])
    );
    assert!(
        rec.gone
            .iter()
            .chain(&rec.new)
            .all(|m| m.why.cause == Cause::Delivered)
    );
    assert_eq!(
        suspects(&r),
        vec![(claim.clone(), x.clone(), "superseded".to_string(), None)]
    );
    // the selection gains the successor: the suspect names it, and the moves stay the same
    take_delivery(
        &mut r,
        &stream(files.path(), "v4", &[(&x2, by), (&y, accepted(&[]))]),
        &src,
        None,
    )
    .unwrap();
    assert_eq!(last_record(&r), None, "the same suspect moves nothing");
    assert_eq!(
        suspects(&r),
        vec![(
            claim.clone(),
            x.clone(),
            "superseded".to_string(),
            Some(y.id.clone())
        )]
    );
    assert_eq!(pins(&r), BTreeMap::from([(x.clone(), pin_of(&x1))]));

    // the log is the record: a reindex rebuilds the same open suspects, fsck folds them, and a
    // reopen logs nothing new
    let open = r.read().reader().open_suspects().unwrap();
    let from = r.read().seq();
    r.reindex().unwrap();
    assert_eq!(r.read().reader().open_suspects().unwrap(), open);
    let f = r.fsck().unwrap();
    assert_eq!(f.unlogged_moves, 0, "{f:?}");
    r.close().unwrap();
    let r = Rdy::open(&d.path().join("p")).unwrap();
    assert_eq!(r.read().reader().open_suspects().unwrap(), open);
    assert_eq!(r.fsck().unwrap().unlogged_moves, 0);
    assert!(
        r.replay_events(from)
            .unwrap()
            .iter()
            .all(|e| !matches!(e.kind, EventKind::SuspectNew(_) | EventKind::SuspectGone(_))),
        "a reindex moves no suspect"
    );
}

/// Fix round 1 (review finding 1): the facts a real export derives make the citer suspect. A
/// decision revoked through `Rdy::revoke` and one wholly superseded by a landed decision travel
/// as `revoked` and `superseded` (the source graph's derived status at the exported landing,
/// not the stored `status` field), and the successor stays inside the selection. Fed to the
/// foreign graph, a citer's non-premise `affects` edge to each is suspect.
#[test]
fn rpl14_a_real_revoke_or_supersede_export_is_suspect_downstream() {
    use rdy_graph::foreign::{ForeignNodes, suspects_of};
    use rdy_model::foreign::{ForeignLink, ForeignParts};

    let d = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(&d.path().join("src"), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$gone", "parent": "$doc",
             "fields": {"statement": "Use the gone way.", "status": "proposed"}},
            {"op": "create", "type": "decision", "id": "$old", "parent": "$doc",
             "fields": {"statement": "Use the old way.", "status": "proposed"}}
        ]),
    );
    land(&mut r, "seed");
    let (doc, gone, old) = (
        w.created["$doc"].clone(),
        id(&w.created["$gone"]),
        id(&w.created["$old"]),
    );
    r.revoke(&writer(), "retire", gone.as_str(), "No longer.", &doc, None)
        .unwrap();
    let w2 = write(
        &mut r,
        "retire",
        json!([
            {"op": "create", "type": "decision", "id": "$new", "parent": doc,
             "fields": {"statement": "Use the new way.", "status": "proposed"},
             "links": {"supersedes": [{"target": old.to_string()}]}}
        ]),
    );
    land(&mut r, "retire");
    let new = id(&w2.created["$new"]);

    // the source graph retires both
    let st = r.read();
    let g = st.graph(&Scope::Canon).unwrap();
    assert_eq!(g.status(&gone).map(|s| s.tag()), Some("revoked"));
    assert_eq!(g.status(&old).map(|s| s.tag()), Some("superseded"));

    let landing = st.canon_head().unwrap();
    let mut out = Vec::new();
    let report = rdy_engine::link_export::write(
        &st,
        "v1.0.0",
        Hash::of(b"tag"),
        landing,
        "implicit:org",
        &mut out,
    )
    .unwrap();
    let by_id: BTreeMap<BlockId, ForeignVersion> = report
        .delivery
        .versions
        .iter()
        .map(|v| (v.id.clone(), v.clone()))
        .collect();
    assert_eq!(by_id[&gone].derived.status.as_deref(), Some("revoked"));
    assert!(by_id[&gone].derived.superseded_by.is_empty());
    assert_eq!(by_id[&old].derived.status.as_deref(), Some("superseded"));
    assert_eq!(by_id[&old].derived.superseded_by, vec![new.clone()]);
    assert_eq!(by_id[&new].derived.status.as_deref(), Some("accepted"));
    assert!(by_id[&new].derived.superseded_by.is_empty());

    // consumed by the citing side: `affects` alone is no premise, so only retirement counts
    let repo = cited();
    let mut nodes = ForeignNodes::default();
    nodes.add_selection(repo, &report.delivery.versions);
    let mut c = Block {
        id: id("c-CCCCC"),
        block_type: "claim".into(),
        parent: Some(id("s-SSSSS")),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: ForeignParts::default(),
        read_identity: None,
    };
    for t in [&gone, &old, &new] {
        let target = ForeignId::new(repo, t.clone());
        c.foreign
            .links
            .entry("affects".into())
            .or_default()
            .push(ForeignLink {
                target: target.clone(),
                part: None,
            });
        c.foreign.pins.insert(
            target,
            ForeignPin {
                content: by_id[t].content,
                encoding: by_id[t].encoding,
            },
        );
    }
    let mut got: Vec<(BlockId, String, Option<BlockId>)> = suspects_of(&c, &nodes)
        .into_iter()
        .map(|s| (s.target.id, s.reason, s.successor))
        .collect();
    got.sort();
    let mut want = vec![
        (gone.clone(), "revoked".to_string(), None),
        (old.clone(), "superseded".to_string(), Some(new.clone())),
    ];
    want.sort();
    assert_eq!(got, want);
}
