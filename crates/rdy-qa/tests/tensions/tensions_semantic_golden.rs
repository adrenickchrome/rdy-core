//! se11: golden case files. Ids are seeded and the clock pinned, so every id, version and side
//! label is fixed. Bless with RDY_QA_BLESS=1 cargo test -p rdy-qa --release --test tensions se11_.
use rdy_engine::pin_clock;
use rdy_engine::semantic::case::case_hash;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/review")
}
fn check(name: &str, text: String) {
    let file = golden_dir().join(format!("{name}.json"));
    if std::env::var_os("RDY_QA_BLESS").is_some() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&file, &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&file).unwrap_or_else(|_| {
        panic!(
            "{name}: no golden at {}; write it with RDY_QA_BLESS=1",
            file.display()
        )
    });
    assert_eq!(
        text, want,
        "{name}: the case changed from the golden (bless with RDY_QA_BLESS=1 if intended)"
    );
}
fn first_case(view_edit: impl FnOnce(&mut rdy_engine::Rdy, &rdy_qa::edit_loop::Spec)) -> String {
    rdy_model::seed_ids_for_tests(0x5345_3131);
    let _clock = pin_clock(1_760_000_000_000);
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    view_edit(&mut r, &s);
    let st = r.read();
    let q = st
        .next_request("agent:r", &BTreeSet::new())
        .unwrap()
        .unwrap();
    let case = st.case_of(&q.key, &q.items).unwrap();
    let out = serde_json::json!({"case": case, "case_hash": case_hash(&case).to_string()});
    rdy_model::unseed_ids_for_tests();
    format!("{}\n", serde_json::to_string_pretty(&out).unwrap())
}

#[test]
fn se11_the_section_case_matches_its_golden() {
    check(
        "case--section",
        first_case(|r, s| {
            put(r, "a", upd(&s.c, "C changed."));
        }),
    );
}

#[test]
fn se11_the_premise_case_matches_its_golden() {
    check(
        "case--premise",
        first_case(|r, s| {
            put(r, "a", upd(&s.a, "A holds, mostly."));
        }),
    );
}

/// guard: a second run in the same process gives the same bytes.
#[test]
fn se11_a_case_is_the_same_on_every_run() {
    let a = first_case(|r, s| {
        put(r, "a", upd(&s.c, "C changed."));
    });
    let b = first_case(|r, s| {
        put(r, "a", upd(&s.c, "C changed."));
    });
    assert_eq!(a, b);
}
