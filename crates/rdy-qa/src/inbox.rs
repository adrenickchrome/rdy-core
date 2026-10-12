//! ib12: the inbox scenario the golden page pins and the invariants re-run (spec §10.2, §16
//! criterion 5; D-TPZ0W, D-Z0EMP), shared so both test binaries test one scenario.
use crate::fixtures::{land, write, writer};
use crate::tensions::{ag, created, put, upd};
use rdy_engine::inbox::sub::subscribe;
use rdy_engine::{Engine, Local, Rdy};
use rdy_model::BlockId;
use serde_json::json;

/// Runs `f` on the project inside `e` as one engine write; `Local::write` settles the tension
/// set after it, as the daemon's writer does.
pub fn on<T: Send + 'static>(e: &Local, f: impl FnOnce(&mut Rdy) -> T + Send + 'static) -> T {
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

/// A doc at `path` in canon with one claim; returns the claim.
pub fn landed_claim(r: &mut Rdy, path: &str) -> BlockId {
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
pub fn build_agent(e: &Local) {
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
