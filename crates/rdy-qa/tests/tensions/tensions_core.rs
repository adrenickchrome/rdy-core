//! tset1: the tension core: ids, sides, severities, the registry and the info shape.
use rdy_engine::tensions::*;
use rdy_engine::{RdyError, Scope};
use rdy_model::{BlockId, Hash};
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use std::collections::BTreeSet;

/// Reports every live write as a one-sided tension of `kind`.
struct EveryWrite {
    kind: TensionKind,
    stable: bool,
}
impl Detector for EveryWrite {
    fn kind(&self) -> TensionKind {
        self.kind
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
                kind: self.kind,
                involved: vec![cx.of_write(w)],
                summary: format!("{} written", w.block),
                stable: self.stable,
            });
        }
        Ok(())
    }
}

/// Reports, for every live write, a tension whose only side is canon's: never kept.
struct CanonOnly;
impl Detector for CanonOnly {
    fn kind(&self) -> TensionKind {
        TensionKind::DeprecatedReference
    }
    fn detect(
        &self,
        cx: &DetectCx<'_>,
        _seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        for w in cx.writes.live() {
            if let Some((side, v)) = cx.canon_side(&w.block)? {
                out.push(Found {
                    kind: self.kind(),
                    involved: vec![cx.involved(side, &w.block, Some(v))?],
                    summary: "canon only".into(),
                    stable: false,
                });
            }
        }
        Ok(())
    }
}

/// In the canon scope only: block `0` against canon (kept: every side is canon), and the same
/// item with a made-up change side (dropped: a canon-scope tension has canon sides only).
struct CanonPair(BlockId);
impl Detector for CanonPair {
    fn kind(&self) -> TensionKind {
        TensionKind::Fork
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
        let Some((side, v)) = cx.canon_side(&self.0)? else {
            return Ok(());
        };
        out.push(Found {
            kind: self.kind(),
            involved: vec![cx.involved(side, &self.0, Some(v))?],
            summary: "canon only".into(),
            stable: true,
        });
        out.push(Found {
            kind: self.kind(),
            involved: vec![cx.involved(SideRef::Change(Hash([7; 32])), &self.0, Some(v))?],
            summary: "not canon".into(),
            stable: true,
        });
        Ok(())
    }
}

#[test]
fn tset1_the_registry_has_one_detector_per_kind_in_kind_order() {
    let kinds: Vec<TensionKind> = DETECTORS.iter().map(|d| d.kind()).collect();
    assert_eq!(kinds, TensionKind::ALL.to_vec());
}

#[test]
fn tset1_ids_cover_sides_and_versions_and_the_vkey_ignores_side_labels() {
    let b = BlockId::parse("c-AAAAA").unwrap();
    let c = SideRef::Change(Hash([2; 32]));
    let canon = SideRef::Canon {
        landing: Some(Hash([3; 32])),
    };
    let x = Involved {
        side: c.clone(),
        block: b.clone(),
        version: Some(Hash([1; 32])),
        content: Some(Hash([5; 32])),
    };
    let y = Involved {
        side: canon.clone(),
        block: BlockId::parse("c-BBBBB").unwrap(),
        version: Some(Hash([4; 32])),
        content: Some(Hash([6; 32])),
    };
    let k = TensionKind::PremiseShift;
    assert_eq!(
        tension_id(k, TensionSeverity::Blocking, &[x.clone(), y.clone()]),
        tension_id(k, TensionSeverity::Blocking, &[y.clone(), x.clone()]),
        "order-independent"
    );
    assert_ne!(
        tension_id(k, TensionSeverity::Blocking, &[x.clone(), y.clone()]),
        tension_id(
            TensionKind::Fork,
            TensionSeverity::Blocking,
            &[x.clone(), y.clone()]
        )
    );
    assert_ne!(
        tension_id(k, TensionSeverity::Blocking, &[x.clone(), y.clone()]),
        tension_id(k, TensionSeverity::Advisory, &[x.clone(), y.clone()]),
        "the severity is in the id, as in the vkey"
    );
    let landed = Involved {
        side: canon.clone(),
        ..x.clone()
    };
    assert_ne!(
        tension_id(k, TensionSeverity::Blocking, &[x.clone(), y.clone()]),
        tension_id(k, TensionSeverity::Blocking, &[landed.clone(), y.clone()]),
        "a re-key changes the id"
    );
    assert_eq!(
        tension_vkey(k, TensionSeverity::Blocking, &[x.clone(), y.clone()]),
        tension_vkey(k, TensionSeverity::Blocking, &[landed, y.clone()]),
        "but not the vkey"
    );
    assert_ne!(
        tension_vkey(k, TensionSeverity::Blocking, &[x.clone(), y.clone()]),
        tension_vkey(k, TensionSeverity::Advisory, &[x.clone(), y.clone()]),
        "a severity change gives a new vkey"
    );
    // a new version with the same content (an accept at landing, a repin, a tracking edit)
    // keeps the vkey (FW-CH7); a content change does not
    let repinned = Involved {
        version: Some(Hash([8; 32])),
        ..x.clone()
    };
    assert_eq!(
        tension_vkey(k, TensionSeverity::Blocking, &[x.clone(), y.clone()]),
        tension_vkey(k, TensionSeverity::Blocking, &[repinned, y.clone()])
    );
    let moved = Involved {
        version: Some(Hash([9; 32])),
        content: Some(Hash([9; 32])),
        ..x.clone()
    };
    assert_ne!(
        tension_vkey(k, TensionSeverity::Blocking, &[x, y.clone()]),
        tension_vkey(k, TensionSeverity::Blocking, &[moved, y])
    );
    assert_eq!(tension_label(&Hash([0xab; 32])), "tn-abababababab");
    assert_eq!(c.label(), Hash([2; 32]).to_string());
    assert_eq!(canon.label(), format!("canon@{}", Hash([3; 32])));
    assert_eq!(SideRef::Canon { landing: None }.label(), "canon");
}

#[test]
fn tset1_a_change_in_two_views_is_one_side_and_only_live_writes_count() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let c1 = change_of(&put(&mut r, "a", upd(&s.c, "C one.")));
    r.view_include(&ag("b"), "b", &[c1.to_string()]).unwrap();
    let every = EveryWrite {
        kind: TensionKind::DeprecatedReference,
        stable: false,
    };
    let set = r.read().tensions_with(&[&every]).unwrap();
    let ts: Vec<_> = set.iter().cloned().collect();
    assert_eq!(ts.len(), 1);
    assert_eq!(ts[0].sides, vec![SideRef::Change(c1)]);
    let info = r.read().tension_info(&ts[0]);
    assert_eq!(info.sides[0].views, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(info.sides[0].actor.as_deref(), Some("agent:a"));
    // a later change in a builds on c1: c1's write is no longer current in a, but still in b
    let c2 = change_of(&put(&mut r, "a", upd(&s.c, "C two.")));
    let sides: BTreeSet<Vec<SideRef>> = r
        .read()
        .tensions_with(&[&every])
        .unwrap()
        .iter()
        .map(|t| t.sides.clone())
        .collect();
    assert_eq!(
        sides,
        BTreeSet::from([vec![SideRef::Change(c1)], vec![SideRef::Change(c2)]])
    );
    r.view_close(&ag("b"), &["b".to_string()], "done").unwrap();
    let sides: Vec<Vec<SideRef>> = r
        .read()
        .tensions_with(&[&every])
        .unwrap()
        .iter()
        .map(|t| t.sides.clone())
        .collect();
    assert_eq!(
        sides,
        vec![vec![SideRef::Change(c2)]],
        "a closed view holds nothing in flight"
    );
}

#[test]
fn tset1_an_in_flight_found_without_a_change_side_is_dropped() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C one."));
    assert!(r.read().tensions_with(&[&CanonOnly]).unwrap().is_empty());
}

#[test]
fn tset1_a_canon_scope_tension_is_kept_when_every_side_is_in_canon() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let det = CanonPair(s.a.clone());
    let ts: Vec<_> = r
        .read()
        .tensions_with(&[&det])
        .unwrap()
        .iter()
        .cloned()
        .collect();
    assert_eq!(ts.len(), 1, "{ts:#?}");
    assert!(
        ts[0]
            .sides
            .iter()
            .all(|x| matches!(x, SideRef::Canon { landing: Some(_) }))
    );
    assert!(change_sides(&ts[0]).is_empty());
    let info = r.read().tension_info(&ts[0]);
    assert!(info.sides[0].landing.is_some() && info.sides[0].change.is_none());
    assert!(info.sides[0].views.is_empty());
    // an in-flight write elsewhere leaves it as it is
    put(&mut r, "a", upd(&s.c, "C one."));
    let again: Vec<_> = r
        .read()
        .tensions_with(&[&det])
        .unwrap()
        .iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(again, vec![ts[0].id]);
}

#[test]
fn tset1_severity_comes_from_the_pack_and_softens_in_draft_for_some_kinds() {
    let (_d, mut r) = qa_project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C one."));
    let st = r.read();
    let first = |kind, stable| {
        st.tensions_with(&[&EveryWrite { kind, stable }])
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .clone()
    };
    let u = first(TensionKind::UnjustifiedChange, false);
    assert_eq!(
        (u.severity, u.overridden),
        (TensionSeverity::Blocking, true),
        "the qa pack"
    );
    let e = first(TensionKind::EditCollision, false);
    assert_eq!(
        (e.severity, e.overridden),
        (TensionSeverity::Blocking, false)
    );
    assert_eq!(
        first(TensionKind::DuplicateDefinition, true).severity,
        TensionSeverity::Blocking
    );
    assert_eq!(
        first(TensionKind::DuplicateDefinition, false).severity,
        TensionSeverity::Advisory,
        "blocking in stable content, advisory in draft"
    );
    assert_eq!(
        first(TensionKind::EditCollision, false).severity,
        TensionSeverity::Blocking
    );
}

#[test]
fn tset1_info_names_the_tension_its_sides_and_status_and_a_label_resolves() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let c1 = change_of(&put(&mut r, "a", upd(&s.c, "C one.")));
    let every = EveryWrite {
        kind: TensionKind::DeprecatedReference,
        stable: false,
    };
    let set = r.read().tensions_with(&[&every]).unwrap();
    let t = set.iter().next().unwrap().clone();
    assert_eq!(r.read().tension_status(&t), TensionStatus::Open);
    let info = serde_json::to_value(r.read().tension_info(&t)).unwrap();
    assert_eq!(info["id"], tension_label(&t.id));
    assert_eq!(info["tension"], t.id.to_string());
    assert_eq!(info["kind"], "deprecated_reference");
    assert_eq!(info["severity"], "advisory");
    assert_eq!(info["status"], "open");
    assert_eq!(info["sides"][0]["side"], c1.to_string());
    assert_eq!(info["sides"][0]["change"], c1.to_string());
    assert_eq!(info["sides"][0]["blocks"][0]["block"], s.c.to_string());
    assert_eq!(info["blocks"], serde_json::json!([s.c.to_string()]));
    assert!(info.get("dismissal").is_none() && info.get("refuses").is_none());
    assert!(info.get("overridden").is_none() && info.get("read_time").is_none());
    assert_eq!(set.resolve(&tension_label(&t.id)).unwrap().id, t.id);
    assert_eq!(set.resolve(&t.id.to_string()[..8]).unwrap().id, t.id);
    assert_eq!(
        set.resolve("tn-000000000000").unwrap_err().code,
        rdy_engine::E_NOT_FOUND
    );
    assert!(
        r.read().tensions().unwrap().is_empty(),
        "the registry's detectors are stubs here"
    );
    let _ = Scope::Canon;
}

#[test]
fn tset1_no_code_outside_the_detectors_names_a_kind() {
    // the extension point: lifecycle, gate, briefing, write responses and surfaces never match on
    // a kind; only rdy-schema and the detector files (each holds `impl Detector for`) do
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../rdy-engine/src");
    let variant = regex::Regex::new(
        r"TensionKind::(EditCollision|Fork|PremiseShift|AreaOverlap|DuplicateDefinition|UnjustifiedChange|DeprecatedReference|ValueDisagreement|RestatementDrift)\b",
    )
    .unwrap();
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(src.join("tensions"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            !std::fs::read_to_string(p)
                .unwrap()
                .contains("impl Detector for")
        })
        .collect();
    for f in [
        "gate.rs",
        "consequence.rs",
        "brief.rs",
        "inflight.rs",
        "alt.rs",
        "search.rs",
        "lib.rs",
        "read_state.rs",
        "publish.rs",
        "handle.rs",
        "fsck.rs",
        "events.rs",
    ] {
        files.push(src.join(f));
    }
    assert!(
        files.len() > 12,
        "the tensions directory has non-detector files: {files:?}"
    );
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(
            !variant.is_match(&text),
            "{} names a tension kind",
            f.display()
        );
    }
}

#[test]
fn tset1_accepting_a_decision_at_landing_keeps_the_vkey() {
    // FW-CH6a, FW-CH7: landing accepts a proposed decision (status, decided_at), which changes
    // its version and not its content, so a fork with that side re-keys with the same vkey.
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let mut created = vec![];
    for who in ["a", "b"] {
        let w = r
            .supersede(
                &ag(who),
                who,
                &s.decision.to_string(),
                None,
                rdy_engine::DecisionInput {
                    parent: s.section.to_string(),
                    statement: format!("{who} replaces it"),
                    ..Default::default()
                },
            )
            .unwrap();
        created.push(created_of(&w));
    }
    let before = one(&r, TensionKind::Fork);
    let da = created[0].clone();
    let proposed = r.read().block(&Scope::Inflight, &da).unwrap();
    rdy_qa::fixtures::land(&mut r, "a").unwrap();
    let accepted = r.read().block(&Scope::Canon, &da).unwrap();
    assert_eq!(accepted.field_str("status"), Some("accepted"));
    assert_ne!(accepted.version_hash(), proposed.version_hash());
    let after = one(&r, TensionKind::Fork);
    assert!(has_canon(&after));
    assert_ne!(after.id, before.id, "re-keyed");
    assert_eq!(after.vkey, before.vkey, "accepting is not a content change");
}

fn created_of(w: &rdy_engine::WriteResult) -> BlockId {
    created(w, "$decision")
}

/// Counts its canon-scope runs (each is a build of the canon part) and reports every live write.
struct CountsCanon;
static CANON_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
impl Detector for CountsCanon {
    fn kind(&self) -> TensionKind {
        TensionKind::AreaOverlap
    }
    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        if cx.scope() == DetectScope::Canon {
            CANON_RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Ok(());
        }
        for w in cx.writes.live() {
            if seeds.is_some_and(|s| !s.contains(&w.block)) {
                continue;
            }
            out.push(Found {
                kind: TensionKind::AreaOverlap,
                involved: vec![cx.of_write(w)],
                summary: "written".into(),
                stable: true,
            });
        }
        Ok(())
    }
}
static COUNTS_CANON: CountsCanon = CountsCanon;
static COUNTING: [&dyn Detector; 1] = [&COUNTS_CANON];

/// Checklist line 15: the canon part is built once per canon build: a commit (carried or not), a
/// view op and an intent never build it again; a landing does.
#[test]
fn tset1_the_canon_part_is_built_once_per_canon_build() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.set_detectors_for_tests(&COUNTING);
    let runs = || CANON_RUNS.load(std::sync::atomic::Ordering::SeqCst);
    let read = |r: &rdy_engine::Rdy| {
        r.read().tensions().unwrap();
    };
    read(&r);
    let n = runs();
    let ca = change_of(&put(&mut r, "a", upd(&s.a, "A one.")));
    read(&r);
    assert_eq!(r.read().tensions_carried(), Some(true));
    put(
        &mut r,
        "a",
        serde_json::json!([{"op": "delete", "id": s.c.to_string()}]),
    );
    read(&r);
    assert_eq!(
        r.read().tensions_carried(),
        Some(false),
        "a delete rebuilds"
    );
    r.view_include(&ag("b"), "b", &[ca.to_string()]).unwrap();
    read(&r);
    r.intent_post(
        &ag("i"),
        Some("i"),
        None,
        &[s.b.to_string()],
        "B",
        Some(60_000),
    )
    .unwrap();
    read(&r);
    assert_eq!(runs(), n, "canon did not move");
    land(&mut r, "a").unwrap();
    read(&r);
    assert_eq!(runs(), n + 1, "a landing builds it again");
    let st = r.read();
    let ids = |set: &TensionSet| set.iter().map(|t| t.id).collect::<Vec<Hash>>();
    assert_eq!(
        ids(&st.tensions().unwrap()),
        ids(&st.tensions_rebuilt().unwrap())
    );
}

/// Checklist line 15: a publish that moves nothing the tension set reads keeps the cell (the
/// same set): a proposal (`Seq`), a tag (`Tags`) and an intent (`Coord` with the alternative
/// sets' members unchanged). A `Coord` that changes the members starts it again.
#[test]
fn tset1_the_cell_is_kept_by_seq_tags_and_a_coord_that_keeps_the_members() {
    use rdy_engine::alt::AltMemberInput;
    use std::sync::Arc;
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    let set = |r: &rdy_engine::Rdy| r.read().tensions().unwrap();
    let before = set(&r);
    r.propose(&writer(), "a", "a").unwrap();
    assert!(Arc::ptr_eq(&before, &set(&r)), "a proposal (Seq)");
    r.intent_post(
        &ag("i"),
        Some("i"),
        None,
        &[s.b.to_string()],
        "B",
        Some(60_000),
    )
    .unwrap();
    assert!(
        Arc::ptr_eq(&before, &set(&r)),
        "an intent (Coord, members kept)"
    );
    let m = |label: &str, view: &str| AltMemberInput {
        label: label.into(),
        view: view.into(),
    };
    r.alt_create(&ag("o"), "A's wording", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    let after_alt = set(&r);
    assert!(!Arc::ptr_eq(&before, &after_alt), "the members moved");
    assert!(
        of_kind(&r, TensionKind::EditCollision).is_empty(),
        "members do not collide"
    );
    put(&mut r, "w", upd(&s.c, "C by w."));
    land(&mut r, "w").unwrap();
    let landed = set(&r);
    r.tag(&approver(), "v0.1.0", None, None).unwrap();
    assert!(Arc::ptr_eq(&landed, &set(&r)), "a tag (Tags)");
}
