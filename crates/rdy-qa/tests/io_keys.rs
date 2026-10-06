//! Keys at import and in rdy check (key1); skeleton warnings from check and import (skel1).
use rdy_engine::{E_KEY_CLASH, Scope, check_path};
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_schema::Severity;

const SPEC: &str = "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: Spec\n    doc_type: overview\n- id: p-AAAAA\n  type: prose\n  parent: d-AAAAA\n  order: a1\n  body: See {{REQ-1|the rule}} and {{req-1}}.\n- id: s-AAAAA\n  type: section\n  parent: d-AAAAA\n  order: a2\n  fields:\n    title: Summary\n- id: r-AAAAA\n  type: requirement\n  parent: s-AAAAA\n  order: a1\n  fields:\n    key: REQ-1\n  body: It must.\n";
const DUP: &str = "- id: r-BBBBB\n  type: requirement\n  parent: s-AAAAA\n  order: a2\n  fields:\n    key: req-1\n  body: It also must.\n";

fn tree(extra: &str) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 2\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    std::fs::create_dir_all(t.path().join("docs")).unwrap();
    std::fs::write(t.path().join("docs/spec.yaml"), format!("{SPEC}{extra}")).unwrap();
    t
}
fn pack() -> rdy_schema::Pack {
    rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap()
}

#[test]
fn key1_import_stores_key_references_as_ids() {
    let (_d, mut r) = project();
    let t = tree("");
    r.import(&approver(), "work", t.path()).unwrap();
    let p = r
        .read()
        .block(
            &Scope::View("work".into()),
            &BlockId::parse("p-AAAAA").unwrap(),
        )
        .unwrap();
    // FW-F2a: a key typed bare is stored as the id with the key-typed mark
    assert_eq!(p.body, "See {{r-AAAAA|the rule}} and {{r-AAAAA|@key}}.");
}

#[test]
fn key1_import_refuses_a_duplicate_key_naming_both_blocks() {
    let (_d, mut r) = project();
    let e = r.import(&approver(), "work", tree(DUP).path()).unwrap_err();
    assert_eq!(e.code, E_KEY_CLASH);
    assert!(
        e.message.contains("r-AAAAA") && e.message.contains("r-BBBBB"),
        "{}",
        e.message
    );
}

#[test]
fn key1_check_accepts_key_references_and_reports_a_clash_at_the_key_line() {
    let ok = check_path(&pack(), None, tree("").path()).unwrap();
    assert!(ok.iter().all(|d| d.code != "E_UNKNOWN_REF"), "{ok:?}");
    let bad = check_path(&pack(), None, tree(DUP).path()).unwrap();
    let clash = bad
        .iter()
        .find(|d| d.code == "E_KEY_CLASH")
        .unwrap_or_else(|| panic!("{bad:?}"));
    let line_of_dup_key =
        SPEC.lines().count() + 1 + DUP.lines().position(|l| l.contains("key:")).unwrap();
    assert_eq!(clash.line, line_of_dup_key);
}

#[test]
fn skel1_check_and_import_report_a_missing_required_section() {
    let diags = check_path(&pack(), None, tree("").path()).unwrap();
    let w = diags
        .iter()
        .find(|d| d.code == "W_SKELETON_MISSING")
        .unwrap_or_else(|| panic!("{diags:?}"));
    assert_eq!(w.severity, Severity::Warning);
    assert!(w.message.contains("Goals"), "{}", w.message);
    assert_eq!(w.line, 6, "points at the doc's doc_type line");
    let (_d, mut r) = project();
    let res = r.import(&approver(), "work", tree("").path()).unwrap();
    assert!(
        res.warnings.iter().any(|i| i.code == "W_SKELETON_MISSING"
            && i.block.as_ref().map(|b| b.to_string()) == Some("d-AAAAA".into())),
        "{:?}",
        res.warnings
    );
}

#[test]
fn skel1_the_cli_prints_import_warnings() {
    let (d, _r) = project();
    drop(_r);
    let t = tree("");
    // direct mode: no test leaves a detached daemon behind
    let out = std::process::Command::new(rdy_bin())
        .current_dir(d.path())
        .args(["--no-daemon", "import"])
        .arg(t.path())
        .args(["--view", "work"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("imported 4 blocks into view work")
            && text.contains(
                "\nwarning W_SKELETON_MISSING: Spec (d-AAAAA): required section \"Goals\" is missing"
            ),
        "{text}"
    );
}
