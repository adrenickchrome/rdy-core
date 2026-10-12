//! ib6: the inbox page: order, the 200 cap, cuts inside a batch, ack and its race.
use rdy_engine::inbox::api::{InboxRequest, run_inbox};
use rdy_engine::inbox::read::{cursor_text, parse_cursor};
use rdy_engine::inbox::sub::subscribe;
use rdy_engine::{E_BAD_INPUT, Engine, Local, Rdy, Session};
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use serde_json::{Value, json};

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

fn read(e: &Local, s: &Session, req: Value) -> Value {
    run_inbox(e, s, serde_json::from_value(req).unwrap()).unwrap()
}
/// `n` landings, each changing one payments block, by agent `b`.
fn payments_landings(r: &mut Rdy, n: usize) {
    r.new_doc(
        &writer(),
        "work",
        "payments/fees",
        Some("overview"),
        Some("Fees"),
    )
    .unwrap();
    let doc = r
        .read()
        .resolve_path(&rdy_engine::Scope::View("work".into()), "payments/fees")
        .unwrap();
    let w = write(
        r,
        "work",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": doc.to_string(), "body": "0"}]),
    )
    .unwrap();
    land(r, "work").unwrap();
    let c = created(&w, "$c");
    for i in 0..n {
        put(r, "b", upd(&c, &format!("v{i}")));
        land(r, "b").unwrap();
    }
}

#[test]
fn ib6_the_cursor_text_round_trips() {
    let c = parse_cursor("3.4096.2").unwrap();
    assert_eq!(cursor_text(&c), "3.4096.2");
    for bad in ["", "1.2", "a.b.c", "1.2.3.4", "-1.0.0"] {
        assert!(parse_cursor(bad).is_none(), "{bad}");
    }
}

#[test]
fn ib6_a_first_read_starts_the_inbox() {
    let (_d, r) = qa_project();
    let e = Local::new(r);
    let v = read(&e, &ag("a"), json!({}));
    assert_eq!(v["started"], true);
    assert_eq!(v["entries"], json!([]));
}

#[test]
fn ib6_two_hundred_entries_per_page_and_ack_moves_on() {
    let (_d, r) = qa_project();
    let e = Local::new(r);
    subscribe(&e, &ag("a"), Some(vec!["payments".into()])).unwrap();
    on(&e, |r| payments_landings(r, 205));
    let p1 = read(&e, &ag("a"), json!({"ack": true, "max_tokens": 100000}));
    assert_eq!(p1["entries"].as_array().unwrap().len(), 200);
    assert_eq!(p1["more"], true);
    // the setup landing of view work also changes payments blocks: 1 + 205 entries in all
    let first = p1["entries"][0]["line"].as_str().unwrap();
    assert!(first.contains(" of view work changed "), "{first}");
    let p2 = read(&e, &ag("a"), json!({"ack": true, "max_tokens": 100000}));
    assert_eq!(p2["entries"].as_array().unwrap().len(), 6);
    assert_eq!(p2["more"], false);
    assert_eq!(read(&e, &ag("a"), json!({}))["entries"], json!([]));
}

#[test]
fn ib6_a_page_cut_inside_one_batch_resumes_after_its_last_entry() {
    let (_d, r) = qa_project();
    let e = Local::new(r);
    subscribe(&e, &ag("a"), Some(vec!["payments".into()])).unwrap();
    on(&e, |r| payments_landings(r, 3));
    let all = read(&e, &ag("a"), json!({"max_tokens": 100000}))["entries"].clone();
    // a budget that fits one entry cuts after each entry
    let mut got = vec![];
    let mut since: Option<String> = None;
    loop {
        let mut req = json!({"max_tokens": 1});
        if let Some(s) = &since {
            req["since"] = json!(s);
        }
        let p = read(&e, &ag("a"), req);
        got.extend(p["entries"].as_array().unwrap().iter().cloned());
        since = Some(p["cursor"].as_str().unwrap().to_string());
        if p["more"] == false {
            break;
        }
    }
    assert_eq!(Value::Array(got), all);
}

#[test]
fn ib6_an_older_ack_never_moves_the_cursor_back() {
    let (_d, r) = qa_project();
    let e = Local::new(r);
    subscribe(&e, &ag("a"), Some(vec!["payments".into()])).unwrap();
    on(&e, |r| payments_landings(r, 2));
    let first = e
        .read()
        .reader()
        .inbox("agent:a", Some("a"))
        .unwrap()
        .unwrap()
        .cursor;
    read(&e, &ag("a"), json!({"ack": true}));
    let acked = e
        .read()
        .reader()
        .inbox("agent:a", Some("a"))
        .unwrap()
        .unwrap()
        .cursor;
    assert!(acked > first);
    // a second connection of the same session acks from the old cursor
    read(
        &e,
        &ag("a"),
        json!({"since": cursor_text(&first), "ack": true, "max_tokens": 1}),
    );
    let after = e
        .read()
        .reader()
        .inbox("agent:a", Some("a"))
        .unwrap()
        .unwrap()
        .cursor;
    assert_eq!(after, acked, "the larger cursor stays");
}

#[test]
fn ib6_bad_cursors_are_refused() {
    let (_d, r) = qa_project();
    let e = Local::new(r);
    subscribe(&e, &ag("a"), None).unwrap();
    let err = run_inbox(
        &e,
        &ag("a"),
        InboxRequest {
            since: Some("x".into()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        (E_BAD_INPUT, "\"x\" is not an inbox cursor")
    );
    let err = run_inbox(
        &e,
        &ag("a"),
        InboxRequest {
            since: Some("9.0.0".into()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(err.message, "inbox cursor 9.0.0 is past the end of the log");
}

#[test]
fn ib6_a_tension_on_your_change_comes_from_its_batch_and_survives_a_reindex() {
    let (_d, mut r) = project();
    let spec = rdy_qa::edit_loop::spec_doc(&mut r);
    put(&mut r, "a", upd(&spec.a, "A changes."));
    let e = Local::new(r);
    subscribe(&e, &ag("a"), None).unwrap();
    let a = spec.a.clone();
    on(&e, move |r| {
        put(r, "b", upd(&a, "A changes otherwise."));
    });
    on(&e, |r| {
        r.view_close(&writer(), &["b".to_string()], "by hand")
            .unwrap();
    });
    let p = read(&e, &ag("a"), json!({}));
    let evs: Vec<&str> = p["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(evs, ["tension_opened", "tension_resolved"], "{p}");
    let line = p["entries"][1]["line"].as_str().unwrap();
    assert!(
        line.contains("no longer involves your in-flight changes: a view changed (op "),
        "{line}"
    );
    on(&e, |r| r.reindex().unwrap());
    assert_eq!(
        read(&e, &ag("a"), json!({})),
        p,
        "reindex reproduces the page"
    );
    let acked = read(&e, &ag("a"), json!({"ack": true}));
    assert_eq!(acked["entries"], p["entries"]);
    assert_eq!(
        read(&e, &ag("a"), json!({}))["entries"],
        json!([]),
        "after the ack: nothing new, no seen set"
    );
}

/// A doc at `path` in canon with one claim; returns the claim.
fn landed_claim(r: &mut Rdy, path: &str) -> rdy_model::BlockId {
    r.new_doc(&writer(), "work", path, Some("overview"), Some("T"))
        .unwrap();
    let doc = r
        .read()
        .resolve_path(&rdy_engine::Scope::View("work".into()), path)
        .unwrap();
    let w = write(
        r,
        "work",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": doc.to_string(), "body": "x"}]),
    )
    .unwrap();
    land(r, "work").unwrap();
    created(&w, "$c")
}
fn kinds(es: &[Value]) -> Vec<&str> {
    es.iter().map(|e| e["event"].as_str().unwrap()).collect()
}
/// Pages of one entry each from the persisted cursor until the page that delivers `last`; the
/// entries delivered and the cursor after them.
fn read_through(e: &Local, s: &Session, last: &str) -> (Vec<Value>, String) {
    let mut got = vec![];
    let mut since: Option<String> = None;
    loop {
        let mut req = json!({"max_tokens": 1});
        if let Some(c) = &since {
            req["since"] = json!(c);
        }
        let p = read(e, s, req);
        let es = p["entries"].as_array().unwrap();
        assert_eq!(es.len(), 1, "{p}");
        got.push(es[0].clone());
        since = Some(p["cursor"].as_str().unwrap().to_string());
        if es[0]["event"] == last {
            return (got, since.unwrap());
        }
        assert_eq!(p["more"], true, "{last} never came: {got:?}");
    }
}

/// Ruling task-4.1 at the page: a page cut after `landed` inside its batch, then a close of the
/// session's view (its areas become empty), still delivers the batch's `view_rebased`.
#[test]
fn ib6_a_cut_after_landed_survives_a_later_close() {
    let (_d, mut r) = qa_project();
    let c = landed_claim(&mut r, "payments/fees");
    put(&mut r, "a", upd(&c, "a's text"));
    let e = Local::new(r);
    subscribe(&e, &ag("a"), None).unwrap();
    on(&e, move |r| {
        put(r, "b", upd(&c, "b's text"));
        land(r, "b").unwrap();
    });
    let all = read(&e, &ag("a"), json!({"max_tokens": 100000}))["entries"]
        .as_array()
        .unwrap()
        .clone();
    let k = kinds(&all);
    let at = k.iter().position(|x| *x == "landed").expect("landed");
    assert_eq!(k.get(at + 1), Some(&"view_rebased"), "{all:?}");
    let (got, cursor) = read_through(&e, &ag("a"), "landed");
    assert_eq!(got, all[..=at]);
    on(&e, |r| {
        r.view_close(&writer(), &["a".to_string()], "by hand")
            .unwrap();
    });
    let rest = read(&e, &ag("a"), json!({"since": cursor, "max_tokens": 100000}));
    let rest = rest["entries"].as_array().unwrap();
    assert_eq!(
        rest.first(),
        all.get(at + 1),
        "the cut drops nothing: {rest:?}"
    );
}

/// Ruling task-4.1 at the page, the reverse: an area the session gains after a cut inside a
/// batch delivers nothing of that batch twice, and none of its earlier items late.
#[test]
fn ib6_an_area_gained_after_a_cut_delivers_nothing_twice() {
    let (_d, mut r) = qa_project();
    let c = landed_claim(&mut r, "payments/fees");
    put(&mut r, "a", upd(&c, "a's text"));
    let e = Local::new(r);
    subscribe(&e, &ag("a"), Some(vec!["identity".into()])).unwrap();
    on(&e, move |r| {
        put(r, "b", upd(&c, "b's text"));
        land(r, "b").unwrap();
    });
    let all = read(&e, &ag("a"), json!({"max_tokens": 100000}))["entries"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        !kinds(&all).contains(&"landed"),
        "payments is not declared: {all:?}"
    );
    let (got, cursor) = read_through(&e, &ag("a"), "view_rebased");
    subscribe(
        &e,
        &ag("a"),
        Some(vec!["identity".into(), "payments".into()]),
    )
    .unwrap();
    let rest = read(&e, &ag("a"), json!({"since": cursor, "max_tokens": 100000}));
    let rest = rest["entries"].as_array().unwrap();
    assert!(
        !kinds(rest).contains(&"landed"),
        "the landing is behind the cut: {rest:?}"
    );
    let mut seen = got.clone();
    seen.extend(rest.iter().cloned());
    assert_eq!(seen, all, "each entry once, in order");
}
