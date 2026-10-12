//! ib12: golden inbox pages. Ids are seeded and the clock pinned, so every id, hash and cursor in
//! a page is fixed. Bless with RDY_QA_BLESS=1 cargo test -p rdy-qa --release --test engine_read ib12_.
use rdy_engine::inbox::api::{InboxRequest, run_inbox};
use rdy_engine::inbox::sub::subscribe;
use rdy_engine::{Engine, Local, Rdy, Session, pin_clock};
use rdy_model::{BlockId, Role};
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Runs `f` on the project inside `e` as one engine write; `Local::write` settles the tension
/// set after it, as the daemon's writer does.
fn on<T: Send + 'static>(e: &Local, f: impl FnOnce(&mut Rdy) -> T + Send + 'static) -> T {
    let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let put = std::sync::Arc::clone(&slot);
    e.write(Box::new(move |r| {
        *put.lock().unwrap() = Some(f(r));
        Ok(serde_json::Value::Null)
    }))
    .unwrap();
    let mut out = slot.lock().unwrap();
    out.take().unwrap()
}

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

/// A doc at `path` in canon with one claim; returns the claim.
fn landed_claim(r: &mut Rdy, path: &str) -> BlockId {
    r.new_doc(&writer(), "work", path, Some("overview"), Some("T"))
        .unwrap();
    let doc = r
        .read()
        .resolve_path(&rdy_engine::Scope::View("work".into()), path)
        .unwrap();
    let w = write(
        r,
        "work",
        json!([{"op": "create", "type": "claim", "id": "$c",
        "parent": doc.to_string(), "body": "x"}]),
    )
    .unwrap();
    land(r, "work").unwrap();
    created(&w, "$c")
}

/// Agent `a` watches `payments` and changes a payments claim. Agent `b` changes the same claim
/// the other way. The human lands an `identity` change. Agent `b` posts an intent on `a`'s block.
/// The human lands a `payments` change on that block, which rebases `a`'s view with a conflict.
fn build_agent(e: &Local) {
    let (pay, ident) = on(e, |r| {
        (
            landed_claim(r, "payments/fees"),
            landed_claim(r, "identity/login"),
        )
    });
    subscribe(e, &ag("a"), Some(vec!["payments".into()])).unwrap();
    let p = pay.clone();
    on(e, move |r| {
        put(r, "a", upd(&p, "Fees are 1%."));
    });
    let p = pay.clone();
    on(e, move |r| {
        put(r, "b", upd(&p, "Fees are 2%."));
    });
    on(e, move |r| {
        write(r, "work", upd(&ident, "Login needs a code.")).unwrap();
        land(r, "work").unwrap();
    });
    let p = pay.clone();
    on(e, move |r| {
        r.intent_post(&ag("b"), Some("b"), None, &[p.to_string()], "edit", None)
            .unwrap();
    });
    on(e, move |r| {
        write(r, "work", upd(&pay, "Fees are 3%.")).unwrap();
        land(r, "work").unwrap();
    });
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
