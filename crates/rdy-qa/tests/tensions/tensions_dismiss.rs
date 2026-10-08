//! tdis1: dismissing a tension writes a review; the role and actor rules.
use rdy_engine::tensions::*;
use rdy_engine::{EventKind, Rdy, RdyError, Session};
use rdy_model::{BlockId, Hash, Role, Verdict};
use rdy_qa::edit_loop::{brief_of, spec_doc, write_cited};
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use std::collections::BTreeSet;
use std::sync::Arc;

/// One one-sided tension of `kind` per live write.
struct OnWrite(TensionKind);
impl Detector for OnWrite {
    fn kind(&self) -> TensionKind {
        self.0
    }
    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        for w in cx.writes.live() {
            if seeds.is_some_and(|s| !s.contains(&w.block)) {
                continue;
            }
            out.push(Found {
                kind: self.0,
                involved: vec![cx.of_write(w)],
                summary: "written".into(),
                stable: true,
            });
        }
        Ok(())
    }
}
static BLOCKING: OnWrite = OnWrite(TensionKind::EditCollision);
static ADVISORY: OnWrite = OnWrite(TensionKind::AreaOverlap);
static DETS: [&dyn Detector; 2] = [&BLOCKING, &ADVISORY];

fn setup(who: &Session, view: &str) -> (tempfile::TempDir, Rdy, Hash) {
    let (d, mut r) = project();
    // the spec lands under the shipped detectors: the test one would block its landing (tgat1)
    let s = spec_doc(&mut r);
    r.set_detectors_for_tests(&DETS);
    let w = write_as(&mut r, who, view, upd(&s.c, "C one.")).unwrap();
    (d, r, change_of(&w))
}
fn pick(r: &Rdy, kind: TensionKind) -> Arc<Tension> {
    one(r, kind)
}
fn reviewer(actor: &str, run: &str) -> Session {
    Session::agent(actor.into(), Some(run.into()), Role::Reviewer).unwrap()
}

#[test]
fn tdis1_anyone_who_writes_may_dismiss_an_advisory_tension_and_a_reader_may_not() {
    let (_d, mut r, _c) = setup(&ag("a"), "a");
    let t = pick(&r, TensionKind::AreaOverlap);
    assert_eq!(
        r.dismiss_tension(&reader(), &tension_label(&t.id), "no")
            .unwrap_err()
            .code,
        rdy_engine::E_PERMISSION
    );
    let info = r
        .dismiss_tension(&ag("a"), &tension_label(&t.id), "fine as is")
        .unwrap();
    assert_eq!(info.status, "dismissed");
    assert!(matches!(
        r.read().tension_status(&t),
        TensionStatus::Dismissed(_)
    ));
    assert!(
        r.read().tensions().unwrap().get(&t.id).is_some(),
        "a dismissal is state on a still-computed tension, not a deletion"
    );
    let ti = r.read().tension_info(&t);
    assert_eq!(ti.status, "dismissed");
    let d = ti.dismissal.unwrap();
    assert_eq!(
        (d.reason.as_str(), d.actor.as_str(), d.role.as_str()),
        ("fine as is", "agent:a", "writer")
    );
    let review = r.store().review(&info.review.parse().unwrap()).unwrap();
    assert_eq!(review.verdict, Verdict::Dismiss);
    assert_eq!(review.vkey(), Some(t.vkey));
}

#[test]
fn tdis1_a_blocking_tension_needs_a_reviewer_or_a_human_of_another_actor() {
    let (_d, mut r, _c) = setup(&ag("a"), "a");
    let tn = tension_label(&pick(&r, TensionKind::EditCollision).id);
    assert_eq!(
        r.dismiss_tension(&ag("b"), &tn, "x").unwrap_err().code,
        rdy_engine::E_PERMISSION
    );
    let e = r
        .dismiss_tension(&reviewer("agent:a", "another-run"), &tn, "x")
        .unwrap_err();
    assert_eq!(
        e.code,
        rdy_engine::E_SELF_REVIEW,
        "another run of the same actor is not independent"
    );
    r.dismiss_tension(&reviewer("agent:b", "rb"), &tn, "x")
        .unwrap();
}

#[test]
fn tdis1_a_human_who_wrote_a_side_cannot_dismiss_it() {
    let (_d, mut r, _c) = setup(&writer(), "work");
    let tn = tension_label(&pick(&r, TensionKind::EditCollision).id);
    assert_eq!(
        r.dismiss_tension(&approver(), &tn, "mine")
            .unwrap_err()
            .code,
        rdy_engine::E_SELF_REVIEW
    );
    r.dismiss_tension(
        &Session::human("human:other".into(), Role::Writer),
        &tn,
        "ok",
    )
    .unwrap();
}

#[test]
fn tdis1_a_dismissal_holds_until_a_side_changes() {
    let (_d, mut r, _c) = setup(&ag("a"), "a");
    let t = pick(&r, TensionKind::EditCollision);
    r.dismiss_tension(&reviewer("agent:r", "rr"), &tension_label(&t.id), "ok")
        .unwrap();
    let c = r.read().tensions().unwrap().get(&t.id).unwrap().blocks()[0].clone();
    put(&mut r, "a", upd(&c, "C two."));
    let t2 = pick(&r, TensionKind::EditCollision);
    assert_ne!(t2.id, t.id);
    assert_eq!(r.read().tension_status(&t2), TensionStatus::Open);
}

#[test]
fn tdis1_a_dismissal_survives_reindex_and_fsck_counts_it() {
    let (_d, mut r, _c) = setup(&ag("a"), "a");
    let t = pick(&r, TensionKind::EditCollision);
    r.dismiss_tension(&reviewer("agent:r", "rr"), &tension_label(&t.id), "ok")
        .unwrap();
    r.reindex().unwrap();
    assert!(matches!(
        r.read().tension_status(&t),
        TensionStatus::Dismissed(_)
    ));
    assert_eq!(r.fsck().unwrap().reviews, 1);
}

#[test]
fn tdis1_twice_unknown_or_without_a_reason_is_refused() {
    let (_d, mut r, _c) = setup(&ag("a"), "a");
    let tn = tension_label(&pick(&r, TensionKind::AreaOverlap).id);
    assert_eq!(
        r.dismiss_tension(&ag("a"), &tn, "  ").unwrap_err().code,
        rdy_engine::E_BAD_INPUT
    );
    assert_eq!(
        r.dismiss_tension(&ag("a"), &tn, &"x".repeat(MAX_REASON + 1))
            .unwrap_err()
            .code,
        rdy_engine::E_BAD_INPUT
    );
    r.dismiss_tension(&ag("a"), &tn, "ok").unwrap();
    let e = r.dismiss_tension(&ag("a"), &tn, "again").unwrap_err();
    assert_eq!(e.code, rdy_engine::E_BAD_INPUT);
    assert!(e.message.contains("already dismissed"), "{}", e.message);
    assert_eq!(
        r.dismiss_tension(&ag("a"), "tn-000000000000", "x")
            .unwrap_err()
            .code,
        rdy_engine::E_NOT_FOUND
    );
}

#[test]
fn tdis1_a_dismissal_emits_tension_dismissed() {
    let (_d, mut r, _c) = setup(&ag("a"), "a");
    let t = pick(&r, TensionKind::AreaOverlap);
    let rx = r.subscribe();
    let info = r
        .dismiss_tension(&ag("b"), &tension_label(&t.id), "ok")
        .unwrap();
    let ev: Vec<EventKind> = rx.try_iter().map(|e| e.kind.clone()).collect();
    assert!(
        ev.iter().any(
            |k| matches!(k, EventKind::TensionDismissed { tension, review, actor, .. }
        if *tension == t.id && review.to_string() == info.review && actor == "agent:b")
        ),
        "{ev:?}"
    );
}

/// Canon scope only: one tension over every canon block whose body says "paired", each on its
/// canon side (one landing carries them all: one side, several producers).
struct CanonPaired;
impl Detector for CanonPaired {
    fn kind(&self) -> TensionKind {
        TensionKind::EditCollision
    }
    fn detect(
        &self,
        cx: &DetectCx<'_>,
        _seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        if cx.scope() != DetectScope::Canon {
            return Ok(());
        }
        let m = cx.canon_m()?;
        let mut involved = vec![];
        for b in m.iter().filter(|b| b.body.contains("paired")) {
            if let Some((side, v)) = cx.canon_side(&b.id)? {
                involved.push(cx.involved(side, &b.id, Some(v))?);
            }
        }
        if !involved.is_empty() {
            out.push(Found {
                kind: TensionKind::EditCollision,
                involved,
                summary: "paired".into(),
                stable: true,
            });
        }
        Ok(())
    }
}
static CANON_PAIRED: CanonPaired = CanonPaired;
static CANON_DETS: [&dyn Detector; 1] = [&CANON_PAIRED];

/// In flight: a live write on a block canon no longer has, against canon's absent side.
struct GoneInCanon;
impl Detector for GoneInCanon {
    fn kind(&self) -> TensionKind {
        TensionKind::EditCollision
    }
    fn detect(
        &self,
        cx: &DetectCx<'_>,
        _seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        if cx.scope() != DetectScope::Inflight {
            return Ok(());
        }
        let canon = cx.canon_m()?;
        for w in cx.writes.live() {
            if canon.get(&w.block).is_some() {
                continue;
            }
            out.push(Found {
                kind: TensionKind::EditCollision,
                involved: vec![
                    cx.of_write(w),
                    cx.involved(SideRef::Canon { landing: None }, &w.block, None)?,
                ],
                summary: "gone in canon".into(),
                stable: true,
            });
        }
        Ok(())
    }
}
static GONE: GoneInCanon = GoneInCanon;
static GONE_DETS: [&dyn Detector; 1] = [&GONE];

/// A duplicate definition per live write: it softens in draft, so its severity follows the
/// block's effective lifecycle.
struct Softens;
impl Detector for Softens {
    fn kind(&self) -> TensionKind {
        TensionKind::DuplicateDefinition
    }
    fn detect(
        &self,
        cx: &DetectCx<'_>,
        _seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        for w in cx.writes.live() {
            out.push(Found {
                kind: TensionKind::DuplicateDefinition,
                involved: vec![cx.of_write(w)],
                summary: "softens".into(),
                stable: cx.stable(&w.block)?,
            });
        }
        Ok(())
    }
}
static SOFTENS: Softens = Softens;
static SOFTENS_DETS: [&dyn Detector; 1] = [&SOFTENS];

#[test]
fn tdis1_every_producer_on_a_canon_side_is_an_author() {
    let (_d, mut r) = project();
    r.set_detectors_for_tests(&CANON_DETS);
    let s = spec_doc(&mut r);
    let other = Session::human("human:other".into(), Role::Writer);
    write_as(&mut r, &writer(), "work", upd(&s.c, "C paired.")).unwrap();
    write_as(
        &mut r,
        &other,
        "work",
        serde_json::json!([{"op": "create", "type": "claim", "id": "$e",
            "parent": s.section.to_string(), "body": "E paired."}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let t = pick(&r, TensionKind::EditCollision);
    assert_eq!(t.sides.len(), 1, "one landing carried both: {t:#?}");
    assert!(has_canon(&t));
    assert_eq!(t.involved.len(), 2);
    let tn = tension_label(&t.id);
    for who in [
        approver(),
        Session::human("human:other".into(), Role::Approver),
    ] {
        assert_eq!(
            r.dismiss_tension(&who, &tn, "x").unwrap_err().code,
            rdy_engine::E_SELF_REVIEW,
            "{} wrote a block on the canon side",
            who.actor
        );
    }
    r.dismiss_tension(&reviewer("agent:r", "rr"), &tn, "ok")
        .unwrap();
}

#[test]
fn tdis1_the_actor_who_deleted_a_canon_block_is_an_author_of_its_absent_side() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.set_detectors_for_tests(&GONE_DETS);
    put(&mut r, "a", upd(&s.c, "C one."));
    write(
        &mut r,
        "work",
        serde_json::json!([{"op": "delete", "id": s.c.to_string()}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let t = pick(&r, TensionKind::EditCollision);
    assert!(
        t.involved
            .iter()
            .any(|i| matches!(i.side, SideRef::Canon { .. }) && i.version.is_none()),
        "{t:#?}"
    );
    let tn = tension_label(&t.id);
    assert_eq!(
        r.dismiss_tension(&approver(), &tn, "x").unwrap_err().code,
        rdy_engine::E_SELF_REVIEW,
        "human:qa deleted C in canon"
    );
    assert_eq!(
        r.dismiss_tension(&reviewer("agent:a", "ra"), &tn, "x")
            .unwrap_err()
            .code,
        rdy_engine::E_SELF_REVIEW
    );
    r.dismiss_tension(
        &Session::human("human:other".into(), Role::Writer),
        &tn,
        "ok",
    )
    .unwrap();
}

#[test]
fn tdis1_an_advisory_time_dismissal_does_not_hold_once_the_tension_is_blocking() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.set_detectors_for_tests(&SOFTENS_DETS);
    put(&mut r, "a", upd(&s.c, "C one."));
    let t = pick(&r, TensionKind::DuplicateDefinition);
    assert_eq!(t.severity, TensionSeverity::Advisory, "C is draft");
    r.dismiss_tension(&ag("a"), &tension_label(&t.id), "fine in draft")
        .unwrap();
    // a human makes the parent section stable in canon (the write cites a briefing of it)
    let token = brief_of(&r, &rdy_engine::Scope::Canon, &[&s.section])
        .base
        .token;
    write_cited(
        &mut r,
        "st",
        serde_json::json!([{"op": "update", "id": s.section.to_string(),
            "fields": {"lifecycle": "stable"}}]),
        Some(&token),
    )
    .unwrap();
    // landed under the shipped detectors: the test one reports st's own write, now blocking
    r.set_detectors_for_tests(&DETECTORS);
    land(&mut r, "st").unwrap();
    r.set_detectors_for_tests(&SOFTENS_DETS);
    let t2 = pick(&r, TensionKind::DuplicateDefinition);
    // fix round 3 of Task 10 (ruling N1/N2): the id hashes the severity too, so id and vkey
    // stay one-to-one; the same blocks at the same versions under another severity are a new id
    assert_ne!(t2.id, t.id, "the id covers the severity");
    assert_eq!(t2.blocks(), t.blocks(), "the same blocks");
    assert_ne!(t2.vkey, t.vkey, "the vkey covers the severity");
    assert_eq!(t2.severity, TensionSeverity::Blocking, "C is stable now");
    assert_eq!(
        r.read().tension_status(&t2),
        TensionStatus::Open,
        "a dismissal made while advisory does not waive it once blocking"
    );
    assert_eq!(r.read().tension_info(&t2).status, "open");
    let tn = tension_label(&t2.id);
    assert_eq!(
        r.dismiss_tension(&ag("a"), &tn, "again").unwrap_err().code,
        rdy_engine::E_PERMISSION
    );
    r.dismiss_tension(&reviewer("agent:r", "rr"), &tn, "ok")
        .unwrap();
    assert!(matches!(
        r.read().tension_status(&t2),
        TensionStatus::Dismissed(_)
    ));
}

/// A pack whose decisions carry the tracking fields `disposition` and `trigger` (trk1).
const TRK_PACK: &str = "\
name: trk
version: \"1\"
extends: rdy-standard
block_types:
  decision:
    prefix: D
    traits: [normative]
    fields:
      disposition: { kind: str }
      trigger: { kind: str }
    tracking: [disposition, trigger]
";

#[test]
fn tdis1_a_repin_or_tracking_edit_keeps_a_dismissal() {
    // FW-CH7: a dismissal lapses when a side's content changes; a repin changes only pins.
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.set_detectors_for_tests(&DETS);
    let y = created(&put(&mut r, "b", cite(&s.section, &s.a)), "$y");
    let t = pick(&r, TensionKind::EditCollision);
    r.dismiss_tension(&reviewer("agent:r", "rr"), &tension_label(&t.id), "ok")
        .unwrap();
    // A moves in canon (landed under the shipped detectors: the test one would block it), so
    // y's pin is stale; the repin changes only y's pins
    r.set_detectors_for_tests(&DETECTORS);
    put(&mut r, "x", upd(&s.a, "A changed."));
    land(&mut r, "x").unwrap();
    r.set_detectors_for_tests(&DETS);
    put(
        &mut r,
        "b",
        serde_json::json!([{"op": "repin", "id": y.to_string()}]),
    );
    let t2 = one_involving(&r, TensionKind::EditCollision, &y);
    assert_ne!(t2.id, t.id, "the repin is a new version");
    assert_eq!(t2.vkey, t.vkey, "but not new content");
    assert!(matches!(
        r.read().tension_status(&t2),
        TensionStatus::Dismissed(_)
    ));

    // a tracking-only edit of an accepted decision
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), TRK_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy/config.yaml"), "pack: trk\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    let dec = r
        .decide(
            &writer(),
            "seed",
            rdy_engine::DecisionInput {
                parent: section,
                statement: "Ship weekly".into(),
                fields: [("disposition".to_string(), serde_json::json!("M2d1"))]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "seed").unwrap();
    r.set_detectors_for_tests(&DETS);
    let track = |r: &mut Rdy, v: &str| {
        r.write(
            &writer(),
            rdy_engine::WriteRequest {
                view: "u".into(),
                ops: serde_json::from_value(
                    serde_json::json!([{"op": "update", "id": dec, "fields": {"disposition": v}}]),
                )
                .unwrap(),
                justification: Some(rdy_model::Justification::Editorial {
                    class: rdy_model::EditorialClass::Clarify,
                    reason: "re-sliced".into(),
                }),
                message: String::new(),
            },
        )
        .unwrap();
    };
    track(&mut r, "M2d2");
    let t = pick(&r, TensionKind::EditCollision);
    r.dismiss_tension(&reviewer("agent:r", "rr"), &tension_label(&t.id), "ok")
        .unwrap();
    track(&mut r, "M2d3");
    let t2 = pick(&r, TensionKind::EditCollision);
    assert_ne!(t2.id, t.id, "the tracking edit is a new version");
    assert_eq!(t2.vkey, t.vkey, "but not new content");
    assert!(matches!(
        r.read().tension_status(&t2),
        TensionStatus::Dismissed(_)
    ));
}
