//! tdis1: dismissing a tension writes a review; the role and actor rules.
use rdy_engine::tensions::*;
use rdy_engine::{EventKind, Rdy, RdyError, Session};
use rdy_model::{BlockId, Hash, Role, Verdict};
use rdy_qa::edit_loop::spec_doc;
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
                involved: vec![Involved {
                    side: SideRef::Change(w.change),
                    block: w.block.clone(),
                    version: w.version,
                }],
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
    r.set_detectors_for_tests(&DETS);
    let s = spec_doc(&mut r);
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
