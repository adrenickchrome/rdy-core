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
                involved: vec![Involved {
                    side: SideRef::Change(w.change),
                    block: w.block.clone(),
                    version: w.version,
                }],
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
                    involved: vec![Involved {
                        side,
                        block: w.block.clone(),
                        version: Some(v),
                    }],
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
            involved: vec![Involved {
                side,
                block: self.0.clone(),
                version: Some(v),
            }],
            summary: "canon only".into(),
            stable: true,
        });
        out.push(Found {
            kind: self.kind(),
            involved: vec![Involved {
                side: SideRef::Change(Hash([7; 32])),
                block: self.0.clone(),
                version: Some(v),
            }],
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
    };
    let y = Involved {
        side: canon.clone(),
        block: BlockId::parse("c-BBBBB").unwrap(),
        version: Some(Hash([4; 32])),
    };
    let k = TensionKind::PremiseShift;
    assert_eq!(
        tension_id(k, &[x.clone(), y.clone()]),
        tension_id(k, &[y.clone(), x.clone()]),
        "order-independent"
    );
    assert_ne!(
        tension_id(k, &[x.clone(), y.clone()]),
        tension_id(TensionKind::Fork, &[x.clone(), y.clone()])
    );
    let landed = Involved {
        side: canon.clone(),
        ..x.clone()
    };
    assert_ne!(
        tension_id(k, &[x.clone(), y.clone()]),
        tension_id(k, &[landed.clone(), y.clone()]),
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
    let moved = Involved {
        version: Some(Hash([9; 32])),
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
        r"TensionKind::(EditCollision|Fork|PremiseShift|AreaOverlap|DuplicateDefinition|UnjustifiedChange|DeprecatedReference)\b",
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
