//! rg3 (D-ME2HD, D-M839K): in stable content `unjustified_change` is always blocking. A pack that
//! lowers it gets `W_PACK_SEVERITY` when it loads, and the tension stays blocking. Drafts need no
//! justification.
use rdy_engine::tensions::*;
use rdy_engine::*;
use rdy_qa::edit_loop::{spec_doc, stable_spec};
use rdy_qa::fixtures::*;
use rdy_qa::intake::{assert_not_ready, confirm_before_landing, summary_for};
use rdy_qa::mcp::McpClient;
use rdy_qa::tensions::*;
use serde_json::json;

const LAX_PACK: &str = "\
name: lax
version: \"1\"
extends: rdy-standard
tensions:
  unjustified_change: advisory
";

const WARNING: &str = "pack lax lowers unjustified_change in stable content; rdy keeps it blocking";
const FIX: &str = "remove the unjustified_change severity from the pack";

/// A project whose pack `lax` sets `unjustified_change: advisory`.
fn lax_project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().expect("tempdir");
    drop(Rdy::init(dir.path(), "rdy-standard").expect("init"));
    std::fs::write(dir.path().join(".rdy/packs/lax.yaml"), LAX_PACK).expect("pack");
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: lax\n").expect("config");
    let r = Rdy::open(dir.path()).expect("open");
    (dir, r)
}

/// Proposes `view` with a summary; the approval of it as the approver.
fn propose_and_approve(r: &mut Rdy, view: &str) -> (ProposalInfo, Result<String, RdyError>) {
    let summary = summary_for(r, view);
    let p = r.propose(&writer(), view, view, summary).unwrap();
    confirm_before_landing(r, &approver(), &p);
    let landed = r.land(&approver(), &p.id);
    (p, landed)
}

#[test]
fn rg3_an_unjustified_stable_change_is_blocking_under_rdy_standard() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    put_j(&mut r, "a", upd(&s.c, "C changed."), None);
    let t = one(&r, TensionKind::UnjustifiedChange);
    assert_eq!(
        (t.severity, t.overridden),
        (TensionSeverity::Blocking, false)
    );
    let (p, landed) = propose_and_approve(&mut r, "a");
    assert!(
        p.tensions
            .iter()
            .any(|t| t.kind == TensionKind::UnjustifiedChange && t.refuses == Some(true)),
        "{:?}",
        p.tensions
    );
    let e = landed.unwrap_err();
    assert_not_ready(&r, &e, &p.r#ref, &[E_BLOCKING_TENSION]);
}

#[test]
fn rg3_a_pack_that_lowers_unjustified_change_warns_and_stays_blocking() {
    let (d, mut r) = lax_project();
    let w: Vec<_> = config_levels::config_warnings_for_env(d.path())
        .into_iter()
        .filter(|w| w.code == "W_PACK_SEVERITY")
        .collect();
    assert_eq!(w.len(), 1, "{w:#?}");
    assert_eq!(
        (
            w[0].severity,
            w[0].block.clone(),
            w[0].message.as_str(),
            w[0].fix.as_str()
        ),
        (rdy_schema::Severity::Warning, None, WARNING, FIX)
    );
    let s = stable_spec(&mut r);
    put_j(&mut r, "a", upd(&s.c, "C changed."), None);
    let t = one(&r, TensionKind::UnjustifiedChange);
    assert_eq!(
        (t.severity, t.overridden),
        (TensionSeverity::Blocking, true)
    );
    let (p, landed) = propose_and_approve(&mut r, "a");
    assert_not_ready(&r, &landed.unwrap_err(), &p.r#ref, &[E_BLOCKING_TENSION]);
    // a pack that leaves the severity alone, or raises it, warns nothing
    let (d2, _r2) = project();
    let none = config_levels::config_warnings_for_env(d2.path());
    assert!(
        none.iter().all(|w| w.code != "W_PACK_SEVERITY"),
        "{none:#?}"
    );
    let (d3, _r3) = rdy_qa::packs::qa_project();
    let none = config_levels::config_warnings_for_env(d3.path());
    assert!(
        none.iter().all(|w| w.code != "W_PACK_SEVERITY"),
        "{none:#?}"
    );
}

#[test]
fn rg3_rdy_session_lists_w_pack_severity() {
    let (d, r) = lax_project();
    drop(r);
    let mut c = McpClient::start(d.path());
    let s = c
        .call(
            "rdy_session",
            json!({"actor": "agent:qa", "run": "w", "role": "writer"}),
        )
        .ok();
    let w = s["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("no warnings in {s}"))
        .iter()
        .find(|w| w["code"] == "W_PACK_SEVERITY")
        .unwrap_or_else(|| panic!("no W_PACK_SEVERITY in {s}"))
        .clone();
    assert_eq!(
        w,
        json!({"severity": "warning", "code": "W_PACK_SEVERITY", "block": null,
               "message": WARNING, "fix": FIX}),
        "{s}"
    );
}

/// Fix round 1 (O15): `rdy config --show-origin` prints `W_PACK_SEVERITY` in its own output,
/// as it prints `W_CONFIG_UNKNOWN`, once, and not on stderr; a pack that leaves the severity
/// alone prints none.
#[test]
fn rg3_rdy_config_show_origin_lists_w_pack_severity() {
    let line = format!("warning W_PACK_SEVERITY: {WARNING}; {FIX}");
    let s = rdy_qa::daemon::Served::new();
    std::fs::write(s.dir.path().join(".rdy/packs/lax.yaml"), LAX_PACK).expect("pack");
    std::fs::write(s.dir.path().join(".rdy.yaml"), "pack: lax\n").expect("config");
    let out = s.rdy(&["config", "--show-origin"]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(
        text.lines().filter(|l| *l == line).count(),
        1,
        "{text}\n--- stderr\n{err}"
    );
    assert!(!err.contains("W_PACK_SEVERITY"), "{err}");
    // rdy-standard: no such line
    let plain = rdy_qa::daemon::Served::new();
    let out = plain.rdy(&["config", "--show-origin"]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(!text.contains("W_PACK_SEVERITY"), "{text}");
}

/// Guard (FW-JU1 net state): a later justified rewrite of the block in the view clears the
/// tension, and the version lands.
#[test]
fn rg3_a_justified_rewrite_still_clears_it() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    put_j(&mut r, "a", upd(&s.c, "C changed."), None);
    one(&r, TensionKind::UnjustifiedChange);
    put_j(
        &mut r,
        "a",
        upd(&s.c, "C, as D says."),
        j_implements(&s.decision),
    );
    let found = of_kind(&r, TensionKind::UnjustifiedChange);
    assert!(found.is_empty(), "{found:#?}");
    let (p, landed) = propose_and_approve(&mut r, "a");
    assert!(
        p.tensions.iter().all(|t| t.refuses != Some(true)),
        "{:?}",
        p.tensions
    );
    landed.unwrap();
}

/// Guard: drafts need no justification.
#[test]
fn rg3_a_draft_change_raises_no_unjustified_change() {
    let (_d, mut r) = project();
    let d = spec_doc(&mut r);
    put(&mut r, "a", upd(&d.c, "C changed."));
    let found = of_kind(&r, TensionKind::UnjustifiedChange);
    assert!(found.is_empty(), "{found:#?}");
    let (_p, landed) = propose_and_approve(&mut r, "a");
    landed.unwrap();
}
