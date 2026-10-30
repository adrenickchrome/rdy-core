//! cc17: import reports the baseline coverage and the onboarding path (FW-CO4).
use rdy_engine::config_levels::{PROJECT_FILE, effective};
use rdy_engine::{RdyError, Scope};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_schema::Issue;
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;

/// `rdy_qa::fixtures::write` with an editorial justification: the fixture writes stable normative
/// content, and an unjustified change to stable content blocks its landing (D-ME2HD).
fn clarified(
    r: &mut rdy_engine::Rdy,
    view: &str,
    ops: serde_json::Value,
) -> Result<rdy_engine::WriteResult, rdy_engine::RdyError> {
    r.write(
        &writer(),
        rdy_engine::WriteRequest {
            view: view.into(),
            ops: parse_test_ops(ops)?,
            justification: Some(rdy_model::Justification::Editorial {
                class: rdy_model::EditorialClass::Clarify,
                reason: "seed the test world".into(),
            }),
            message: String::new(),
        },
    )
}

/// A format-2 tree: two pages that link to each other (so Connected is full), a term `Ledger`
/// and three claims that name it without a link, in a stable section (so Governed counts too).
fn fixture_tree_with_unlinked_terms() -> tempfile::TempDir {
    let (_p, mut r) = project();
    clarified(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "alpha", "title": "Alpha", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "beta", "title": "Beta", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$sa", "parent": "$a", "fields": {"title": "Terms", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$sb", "parent": "$b", "fields": {"title": "Notes"}},
            {"op": "create", "type": "term", "id": "$t", "parent": "$sa", "fields": {"name": "Ledger"}, "body": "Holds balances."},
            {"op": "create", "type": "claim", "parent": "$sa", "body": "The Ledger is append-only."},
            {"op": "create", "type": "claim", "parent": "$sa", "body": "The Ledger rejects edits."},
            {"op": "create", "type": "claim", "parent": "$sa", "body": "Every Ledger entry has an author."},
            {"op": "create", "type": "prose", "parent": "$sa", "body": "See {{$b}}."},
            {"op": "create", "type": "prose", "parent": "$sb", "body": "See {{$a}}."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let tree = tempfile::tempdir().unwrap();
    let out = tree.path().join("tree");
    r.read().export(&Scope::Canon, &out).unwrap();
    // hand the tree to the caller at the temp dir's root
    let keep = tempfile::tempdir().unwrap();
    copy_dir(&out, keep.path());
    keep
}

fn copy_dir(from: &Path, to: &Path) {
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let dest = to.join(p.file_name().unwrap());
        if p.is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
            copy_dir(&p, &dest);
        } else {
            std::fs::copy(&p, &dest).unwrap();
        }
    }
}

/// A served project with `config` appended to its `.rdy.yaml`.
fn served_with(config: &str) -> Served {
    let s = Served::new();
    if !config.is_empty() {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(s.dir.path().join(PROJECT_FILE))
            .unwrap();
        f.write_all(config.as_bytes()).unwrap();
    }
    s
}

fn import_tree_with_config(tree: &tempfile::TempDir, config: &str) -> (Served, Value) {
    let s = served_with(config);
    let dir = tree.path().to_str().unwrap().to_string();
    let (ok, out) = s.json(&["--json", "import", &dir, "--view", "import"]);
    assert!(ok, "{out}");
    (s, out)
}

fn import_tree(tree: &tempfile::TempDir) -> (Served, Value) {
    import_tree_with_config(tree, "")
}

fn import_tree_text(tree: &tempfile::TempDir) -> (Served, String) {
    let s = served_with("");
    let dir = tree.path().to_str().unwrap().to_string();
    let out = s.rdy(&["import", &dir, "--view", "import"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (s, text)
}

fn config_dir(text: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join(PROJECT_FILE),
        format!("pack: rdy-standard\n{text}"),
    )
    .unwrap();
    d
}

fn config_error(text: &str) -> RdyError {
    let d = config_dir(text);
    effective(d.path(), None).unwrap_err()
}

fn config_warning(text: &str) -> Issue {
    let d = config_dir(text);
    let e = effective(d.path(), None).unwrap();
    let mut named: Vec<Issue> = e
        .warnings
        .into_iter()
        .filter(|w| w.message.contains("coverage.target"))
        .collect();
    assert_eq!(named.len(), 1, "{text}: {named:?}");
    named.remove(0)
}

#[test]
fn cc17_import_reports_coverage_and_the_next_phase() {
    let (_d, out) = import_tree(&fixture_tree_with_unlinked_terms());
    assert!(out["coverage"]["score"].is_u64());
    assert!(out["coverage"]["pages"].is_array());
    let o = &out["onboarding"];
    assert_eq!(o["target"], 80);
    assert_eq!(o["phases"].as_array().unwrap().len(), 4);
    assert_eq!(
        o["next"], "entities",
        "structure is done, mentions are unlinked"
    );
}

#[test]
fn cc17_the_project_target_marks_done() {
    let (_d, out) = import_tree_with_config(
        &fixture_tree_with_unlinked_terms(),
        "coverage:\n  target: 10\n",
    );
    assert_eq!(out["onboarding"]["done"], true);
    assert!(out["onboarding"]["next"].is_null());
}

#[test]
fn cc17_a_target_that_is_not_a_whole_number_is_e_config() {
    for bad in ["\"80\"", "eighty", "80.5"] {
        let e = config_error(&format!("coverage:\n  target: {bad}\n"));
        assert_eq!(e.code, "E_CONFIG", "{bad}");
        assert!(
            e.message.contains("a whole number from 0 to 100"),
            "{bad}: {}",
            e.message
        );
    }
}

/// Orchestrator ruling on Q13: a number outside 0 to 100 warns, and rdy keeps going with 80.
#[test]
fn cc17_a_target_out_of_range_warns_and_uses_80() {
    for bad in ["101", "-1"] {
        let w = config_warning(&format!("coverage:\n  target: {bad}\n"));
        assert_eq!(w.code, "W_CONFIG_UNKNOWN", "{bad}");
        assert_eq!(w.message, "coverage.target must be 0–100; using 80");
        assert_eq!(
            w.fix,
            "set coverage.target to a whole number from 0 to 100 in .rdy.yaml"
        );
    }
    let (_d, out) = import_tree_with_config(
        &fixture_tree_with_unlinked_terms(),
        "coverage:\n  target: 101\n",
    );
    assert_eq!(
        out["onboarding"]["target"], 80,
        "the project opens and uses the default"
    );
}

#[test]
fn cc17_a_misspelt_target_key_warns() {
    assert_eq!(
        config_warning("coverage:\n  targett: 50\n").code,
        "W_CONFIG_UNKNOWN"
    );
}

#[test]
fn cc17_the_cli_prints_the_onboarding_line() {
    let (_d, text) = import_tree_text(&fixture_tree_with_unlinked_terms());
    assert!(
        text.lines().any(|l| l.starts_with("coverage ")
            && l.contains(" of a target 80; next phase: entities (grounded ")),
        "{text}"
    );
}
