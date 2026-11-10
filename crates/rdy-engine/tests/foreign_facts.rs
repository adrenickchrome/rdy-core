//! rpl17 (repo links, D-Y49PX, D-8VJAZ): the shared foreign readiness facts. One pure collector
//! lists the open tensions that have a foreign side, each with its kind, severity, repo, entry
//! and the author's answer. An assessment records the foreign versions a version's reach pins and
//! the foreign sides of its tensions in its foreign part (ASSESSMENT encoding 2), and keeps the
//! local part as it was. An advisory foreign tension is an item of the change plan.
mod common;

use rdy_engine::foreign_delivery::take_delivery;
use rdy_engine::foreign_facts::{ForeignGateFact, foreign_tension_gate};
use rdy_engine::plan::api::{ItemState, PlanAnswerInput, PlanRow};
use rdy_engine::readiness::assessment::assess_wire;
use rdy_engine::readiness::{Candidate, assess};
use rdy_engine::tensions::{TensionKind, TensionSeverity, TensionStatus};
use rdy_engine::*;
use rdy_model::assessment::{
    AssessmentForeignReach, AssessmentForeignTension, AssessmentPlanAnswerRef,
};
use rdy_model::foreign::{
    ForeignDerived, ForeignId, ForeignPublication, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::plan::{PlanAct, PlanKind};
use rdy_model::*;
use rdy_store::codec;
use rdy_store::format::baseline_features;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{DeliveryRecord, StreamHeader, StreamRecord, StreamWriter};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

const PROFILE: &str = "pf-AGNT1";

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

/// A cited claim, as the cited side delivers it.
fn claim(id_: &str, body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: "claim".into(),
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

fn live() -> ForeignDerived {
    ForeignDerived {
        status: None,
        lifecycle: None,
        superseded_by: vec![],
    }
}

fn deprecated() -> ForeignDerived {
    ForeignDerived {
        status: None,
        lifecycle: Some("deprecated".into()),
        superseded_by: vec![],
    }
}

/// A link delivery stream of `selected` (any order), written to `dir/name` and opened.
fn stream(dir: &Path, name: &str, selected: &[(&Block, ForeignDerived)]) -> File {
    let mut selected = selected.to_vec();
    selected.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    let versions: Vec<ForeignVersion> = selected
        .iter()
        .map(|(b, d)| ForeignVersion {
            id: b.id.clone(),
            version: b.version_hash(),
            content: b.content_hash(),
            encoding: 0,
            derived: d.clone(),
        })
        .collect();
    let delivery = DeliveryRecord {
        publication: ForeignPublication {
            tag: name.into(),
            profile: PROFILE.into(),
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

/// A project that cites a deprecated rule and a live one from a linked project, with one write
/// in view `p` that follows the deprecated rule and builds on the live one.
struct Fx {
    _d: tempfile::TempDir,
    r: Rdy,
    entry: Hash,
    live: Block,
    change: Hash,
}

fn fixture() -> Fx {
    let d = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(&d.path().join("p"), "rdy-standard").unwrap();
    let seeded = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Guide", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "repo_link", "id": "$a", "parent": "$d",
             "fields": {"alias": "core", "repo": cited().to_string(), "profile": PROFILE}}
        ]),
    );
    land(&mut r, "seed");
    let old = claim("c-FRGND", "Old services answer on port eighty.");
    let retry = claim("c-FRGNP", "Retries back off.");
    let src = ForeignSource::Link {
        link: seeded.created["$a"].clone(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
    };
    let taken = take_delivery(
        &mut r,
        &stream(
            files.path(),
            "v1",
            &[(&old, deprecated()), (&retry, live())],
        ),
        &src,
        None,
    )
    .unwrap();
    let w = write(
        &mut r,
        "p",
        json!([
            {"op": "create", "type": "claim", "id": "$o", "parent": seeded.created["$s"],
             "body": "Follow {{core:c-FRGND}}."},
            {"op": "create", "type": "claim", "id": "$b", "parent": seeded.created["$s"],
             "body": "Builds on {{core:c-FRGNP}}."}
        ]),
    );
    Fx {
        _d: d,
        r,
        entry: taken.entry,
        live: retry,
        change: w.change.parse().unwrap(),
    }
}

/// The tensions of the in-flight change, each with its status.
fn rows(
    r: &Rdy,
    change: Hash,
) -> Vec<(std::sync::Arc<rdy_engine::tensions::Tension>, TensionStatus)> {
    let st = r.read();
    st.tensions_of_changes(&[change])
        .unwrap()
        .into_iter()
        .map(|t| {
            let s = st.tension_status(&t);
            (t, s)
        })
        .collect()
}

#[test]
fn rpl17_gate_facts_include_kind_repo_severity_and_answer() {
    let fx = fixture();
    let rows = rows(&fx.r, fx.change);
    let t = rows
        .iter()
        .find(|(t, _)| t.kind == TensionKind::DeprecatedReference && t.has_foreign())
        .map(|(t, _)| t.clone())
        .expect("the deprecated reference across projects");
    // no answer yet
    let facts = foreign_tension_gate(&rows, &BTreeMap::new());
    let f: Vec<&ForeignGateFact> = facts.iter().filter(|f| f.tension == t.id).collect();
    assert_eq!(f.len(), 1, "{facts:#?}");
    assert_eq!(f[0].kind, TensionKind::DeprecatedReference);
    assert_eq!(f[0].severity, TensionSeverity::Advisory);
    assert_eq!((f[0].repo, f[0].entry), (cited(), fx.entry));
    assert_eq!(f[0].target, ForeignId::new(cited(), id("c-FRGND")));
    assert_eq!(f[0].answer, None);
    assert_eq!(f[0].blocks.len(), 1);
    // the author's answer rides on the fact
    let a = AssessmentPlanAnswerRef {
        plan: Hash::of(b"plan"),
        item: Hash::of(b"item"),
    };
    let facts = foreign_tension_gate(&rows, &BTreeMap::from([(t.id, a.clone())]));
    assert_eq!(
        facts.iter().find(|f| f.tension == t.id).unwrap().answer,
        Some(a)
    );
    // a dismissed or deviated tension is no gate fact, and a local tension has none
    let dismissed: Vec<_> = rows
        .iter()
        .map(|(t, _)| (t.clone(), TensionStatus::Dismissed(Hash::of(b"review"))))
        .collect();
    assert!(foreign_tension_gate(&dismissed, &BTreeMap::new()).is_empty());
    let deviated: Vec<_> = rows
        .iter()
        .map(|(t, _)| (t.clone(), TensionStatus::Deviated(id("d-FRGNX"))))
        .collect();
    assert!(foreign_tension_gate(&deviated, &BTreeMap::new()).is_empty());
    // the facts come in a fixed order: tension, repo, entry, cited block
    let key = |f: &ForeignGateFact| (f.tension, f.repo, f.entry, f.target.id.clone());
    assert!(
        facts.windows(2).all(|w| key(&w[0]) < key(&w[1])),
        "{facts:#?}"
    );
}

/// Answers every open item of view `p`, proposes it and returns its candidate.
fn proposed(fx: &mut Fx) -> (Candidate, Vec<PlanAnswerInput>) {
    let answered = common::answer_open_items(&mut fx.r, &writer(), "p");
    let p =
        fx.r.propose(&writer(), "p", "p", Some(common::TEST_SUMMARY))
            .unwrap_or_else(|e| panic!("propose p: {e} {:?}", e.issues));
    let pref = fx.r.resolve_proposal(&p.id).unwrap();
    let st = fx.r.read();
    let rec = st.reader().proposal(&p.id.parse().unwrap()).unwrap();
    (Candidate::stored(&st, pref, rec).unwrap(), answered)
}

#[test]
fn rpl17_assessment_records_foreign_reach() {
    let mut fx = fixture();
    let (c, _) = proposed(&mut fx);
    let st = fx.r.read();
    let gate = st.gate_config();
    let local = assess(&st, &c, &gate).unwrap();
    let (readiness, wire) = assess_wire(&st, &c, &gate).unwrap();
    assert!(readiness.issues.is_empty(), "{:#?}", readiness.issues);
    let wire = wire.expect("a ready version has a record");
    // the local part is the local assessment, as it was
    let a = local.assessment.expect("ready");
    assert_eq!(wire.local(), a);
    assert!(wire.has_foreign());
    assert!(wire.into_local().is_none());
    let wire = assess_wire(&st, &c, &gate).unwrap().1.unwrap();
    // the reach holds the held version of each pin, and of the cited side of each tension
    let held = AssessmentForeignReach {
        target: ForeignId::new(cited(), fx.live.id.clone()),
        version: fx.live.version_hash(),
    };
    assert!(wire.foreign.reach.contains(&held), "{:#?}", wire.foreign);
    let dep = ForeignId::new(cited(), id("c-FRGND"));
    assert!(wire.foreign.reach.iter().any(|r| r.target == dep));
    // the tension rows name the repo and the entry that delivered the side
    let t = a
        .tensions
        .iter()
        .find(|t| t.kind == TensionKind::DeprecatedReference.name())
        .expect("the local row of the same tension");
    assert_eq!(
        wire.foreign.tensions,
        vec![AssessmentForeignTension {
            tension: t.id,
            repo: cited(),
            entry: fx.entry,
        }]
    );
    // sorted, distinct and valid; an unsorted foreign part is refused
    assert_eq!(wire.validate(), Ok(()));
    let mut unsorted = wire.clone();
    unsorted.foreign.reach.reverse();
    assert!(unsorted.foreign.reach.len() < 2 || unsorted.validate().is_err());
    let mut sorted = wire.foreign.reach.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(wire.foreign.reach, sorted);
    // a cited block is no local reach and no local coverage: the local part names local blocks
    assert!(
        a.reach
            .iter()
            .all(|b| b.as_str() != "c-FRGND" && b.as_str() != "c-FRGNP")
    );
}

#[test]
fn rpl17_advisory_foreign_tension_has_plan_item() {
    let mut fx = fixture();
    let plan = fx.r.read().view_plan("p").unwrap();
    let items: Vec<_> = plan
        .items
        .iter()
        .filter_map(|r| match r {
            PlanRow::Live(i) if i.kind == PlanKind::Tension => Some(i),
            _ => None,
        })
        .collect();
    assert_eq!(items.len(), 1, "{:#?}", plan.items);
    assert_eq!(items[0].state, ItemState::Open);
    // the item is the foreign tension's: its id is the tension's short id
    let rows = rows(&fx.r, fx.change);
    let t = rows
        .iter()
        .find(|(t, _)| t.has_foreign())
        .map(|(t, _)| t.clone())
        .unwrap();
    assert_eq!(items[0].id, format!("tn-{}", t.id.short()));
    // the version does not propose while the item is open; answering it makes it ready
    let refused =
        fx.r.propose(&writer(), "p", "p", Some(common::TEST_SUMMARY));
    let refused_open = match refused {
        Ok(p) => {
            let pref = fx.r.resolve_proposal(&p.id).unwrap();
            let st = fx.r.read();
            let rec = st.reader().proposal(&p.id.parse().unwrap()).unwrap();
            let c = Candidate::stored(&st, pref, rec).unwrap();
            !assess(&st, &c, &st.gate_config())
                .unwrap()
                .readiness
                .issues
                .is_empty()
        }
        Err(_) => true,
    };
    assert!(refused_open, "an open tension item gates the version");
    let (c, answered) = proposed(&mut fx);
    assert!(
        answered
            .iter()
            .any(|a| a.item == items[0].id && a.act == PlanAct::Confirm)
    );
    let st = fx.r.read();
    let (readiness, wire) = assess_wire(&st, &c, &st.gate_config()).unwrap();
    assert!(readiness.issues.is_empty(), "{:#?}", readiness.issues);
    // the answer rides on the foreign gate fact through the local row
    let wire = wire.unwrap();
    let row = wire
        .tensions
        .iter()
        .find(|r| r.id == t.id)
        .expect("the tension row");
    assert!(row.answer.is_some());
}
