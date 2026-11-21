//! wck7 (D-ZRXY0): the site shows no module rule row, as it shows no filter rule row. A project
//! whose module rules fail and trap on every claim renders canon and a view with no rule text,
//! and the render runs no module.
use rdy_engine::check::module_run::module_runs_on_this_thread_for_tests;
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::packs::use_pack;
use rdy_qa::tensions::{j_typo, put_j};
use serde_json::json;
use std::path::{Path, PathBuf};

const FAIL_TEXT: &str = include_str!("../../fixtures/wasm/fail-text.b64");
const TRAP: &str = include_str!("../../fixtures/wasm/trap.b64");

/// Pack `wck7`: a module rule of claims that fails with its own text, one that traps, and a
/// filter rule that every claim fails, all advisory.
fn pack() -> String {
    let m = |name: &str, module: &str| {
        format!(
            "  - name: {name}\n    select: type = claim\n    module: {}\n    message: {name} message\n    fix: {name} fix\n",
            module.trim()
        )
    };
    format!(
        "name: wck7\nversion: \"1\"\nextends: rdy-standard\nrules:\n{}{}  - name: d-filter\n    select: type = claim\n    require: type = note\n    message: d-filter message\n    fix: d-filter fix\n",
        m("a-text", FAIL_TEXT),
        m("b-traps", TRAP)
    )
}

/// Every file under `dir`, recursively, in a stable order.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn wck7_site_has_no_module_rule_rows() {
    let (d, r) = project();
    drop(r);
    use_pack(
        d.path(),
        "wck7",
        &pack(),
        "coverage:\n  gate: off\nreviews:\n  require_up_to_date: false\n",
    );
    let mut r = Rdy::open(d.path()).unwrap();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "parent": "$s", "body": "Landed claims hold."}
        ]),
        j_typo(),
    );
    // the landing's propose judges the claim: the module rules run on it
    let before = module_runs_on_this_thread_for_tests();
    land(&mut r, "seed").unwrap();
    assert!(
        module_runs_on_this_thread_for_tests() > before,
        "guard: the propose ran the modules"
    );
    let v = put_j(
        &mut r,
        "open",
        json!([{"op": "create", "type": "claim", "parent": w.created["$s"], "body": "Open claims hold."}]),
        j_typo(),
    );
    drop(v);
    let runs = module_runs_on_this_thread_for_tests();
    let out = tempfile::tempdir().unwrap();
    for (name, scope) in [("site", Scope::Canon), ("view", Scope::View("open".into()))] {
        let site = out.path().join(name);
        rdy_render::render_site(&r.read(), &scope, &site).unwrap();
        let all = files(&site);
        assert!(!all.is_empty(), "the site has files");
        for f in all {
            let Ok(t) = std::fs::read_to_string(&f) else {
                continue;
            };
            for word in [
                "W_RULE_CHECK",
                "E_RULE_CHECK",
                "could not judge",
                "the module says no",
                "the module trapped",
                "a-text",
                "b-traps",
                "d-filter",
            ] {
                assert!(!t.contains(word), "{word} in {}", f.display());
            }
        }
    }
    assert_eq!(
        module_runs_on_this_thread_for_tests(),
        runs,
        "the render runs no module"
    );
}
