//! ib12: golden inbox pages. Ids are seeded and the clock pinned, so every id, hash and cursor in
//! a page is fixed. Bless with RDY_QA_BLESS=1 cargo test -p rdy-qa --release --test engine_read ib12_.
//! Both pages' `cursor` (a log offset) was re-taken when change plans arrived (D-4JMPH: every
//! propose batch writes a plan version, an item-ful landing a sealed one): the fixtures' proposes
//! and landings write PLAN frames, so the log ends later. Every entry kept its bytes, as in the
//! two MCP inbox goldens.
use rdy_engine::inbox::api::{InboxRequest, run_inbox};
use rdy_engine::inbox::sub::subscribe;
use rdy_engine::{Local, Session, pin_clock};
use rdy_model::Role;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::inbox::{build_agent, on};
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/inbox")
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
        "{name}: the page changed from the golden (bless with RDY_QA_BLESS=1 if intended)"
    );
}

/// The page of session `s` from its persisted cursor, as pretty JSON with a closing newline.
fn page_text(e: &Local, s: &Session) -> String {
    let req: InboxRequest = serde_json::from_value(json!({})).unwrap();
    let page: Value = run_inbox(e, s, req).unwrap();
    format!("{}\n", serde_json::to_string_pretty(&page).unwrap())
}

fn agent_page() -> String {
    rdy_model::seed_ids_for_tests(0x4942_3132);
    let _clock = pin_clock(1_760_000_000_000);
    let (_d, r) = qa_project();
    let e = Local::new(r);
    build_agent(&e);
    let text = page_text(&e, &ag("a"));
    rdy_model::unseed_ids_for_tests();
    text
}

/// A reviewer is subscribed before agent `a` changes `spec.c`.
fn reviewer_page() -> String {
    rdy_model::seed_ids_for_tests(0x4942_3133);
    let _clock = pin_clock(1_760_000_000_000);
    let (_d, mut r) = project();
    let spec = spec_doc(&mut r);
    let e = Local::new(r);
    let rv = Session::agent("agent:rv".into(), Some("rv".into()), Role::Reviewer).unwrap();
    subscribe(&e, &rv, None).unwrap();
    let c = spec.c.clone();
    on(&e, move |r| {
        put(r, "a", upd(&c, "C contradicts B."));
    });
    let text = page_text(&e, &rv);
    rdy_model::unseed_ids_for_tests();
    text
}

#[test]
fn ib12_an_agent_page_matches_its_golden() {
    check("page--agent", agent_page());
}

#[test]
fn ib12_a_reviewer_page_matches_its_golden() {
    check("page--reviewer", reviewer_page());
}

/// Guard: nothing in a page depends on the process, the project's path or the run.
#[test]
fn ib12_a_page_is_the_same_on_every_run() {
    assert_eq!(agent_page(), agent_page());
    assert_eq!(reviewer_page(), reviewer_page());
}
