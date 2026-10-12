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

/// The persisted cursor of agent `name`'s inbox.
fn persisted(e: &Local, name: &str) -> rdy_store::inbox::InboxCursor {
    e.read()
        .reader()
        .inbox(&format!("agent:{name}"), Some(name))
        .unwrap()
        .unwrap()
        .cursor
}

/// Ruling task-6.1 (ledger O5): an ack poll with nothing new writes nothing, however often it
/// runs. Batches with no raw items (the ack's own ref write) never move `next`.
#[test]
fn ib6_idle_ack_polls_write_nothing() {
    let (_d, r) = qa_project();
    let e = Local::new(r);
    subscribe(&e, &ag("a"), None).unwrap();
    let (seq, cursor) = (e.read().seq(), persisted(&e, "a"));
    for i in 0..5 {
        let v = read(&e, &ag("a"), json!({"ack": true}));
        assert_eq!(v["entries"], json!([]), "poll {i}: {v}");
        assert_eq!(e.read().seq(), seq, "poll {i} wrote a batch");
        assert_eq!(persisted(&e, "a"), cursor, "poll {i} moved the cursor");
    }
}

/// Ruling task-6.1: another session's ack (a batch of one `inbox/` ref) between two reads leaves
/// the page and its `next` unchanged.
#[test]
fn ib6_a_batch_with_only_ref_writes_does_not_move_next() {
    let (_d, r) = qa_project();
    let e = Local::new(r);
    subscribe(&e, &ag("a"), Some(vec!["payments".into()])).unwrap();
    subscribe(&e, &ag("b"), Some(vec!["payments".into()])).unwrap();
    on(&e, |r| payments_landings(r, 1));
    let before = read(&e, &ag("a"), json!({"max_tokens": 100000}));
    let seq = e.read().seq();
    let acked = read(&e, &ag("b"), json!({"ack": true, "max_tokens": 100000}));
    assert_eq!(acked["acked"], true);
    assert!(e.read().seq() > seq, "b's ack wrote its ref");
    let after = read(&e, &ag("a"), json!({"max_tokens": 100000}));
    assert_eq!(after, before, "the ref batch moves nothing");
}

/// Every entry from `since` (else the persisted cursor), one entry per page. Fails if the pages
/// never end.
fn page_by_one(e: &Local, s: &Session, since: Option<String>) -> Vec<Value> {
    let mut got = vec![];
    let mut since = since;
    for _ in 0..100 {
        let mut req = json!({"max_tokens": 1});
        if let Some(c) = &since {
            req["since"] = json!(c);
        }
        let p = read(e, s, req);
        got.extend(p["entries"].as_array().unwrap().iter().cloned());
        since = Some(p["cursor"].as_str().unwrap().to_string());
        if p["more"] == false {
            return got;
        }
    }
    panic!("the pages never end: {got:?}");
}

/// Review focus 1 in the moves part of a batch: a page cut between two `tension_opened` moves of
/// one batch, then a close of the session's view, resumes after the first move.
#[test]
fn ib6_a_cut_between_two_moves_of_one_batch_delivers_each_once() {
    let (_d, mut r) = qa_project();
    let x = landed_claim(&mut r, "payments/fees");
    let y = landed_claim(&mut r, "payments/rates");
    put(&mut r, "a", upd(&x, "a's x"));
    put(&mut r, "a", upd(&y, "a's y"));
    let e = Local::new(r);
    subscribe(&e, &ag("a"), None).unwrap();
    let start = cursor_text(&persisted(&e, "a"));
    on(&e, move |r| {
        put(
            r,
            "b",
            json!([{"op": "update", "id": x.to_string(), "body": "b's x"},
                   {"op": "update", "id": y.to_string(), "body": "b's y"}]),
        );
    });
    let all = read(&e, &ag("a"), json!({"max_tokens": 100000}))["entries"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(kinds(&all), ["tension_opened", "tension_opened"], "{all:?}");
    let first = read(&e, &ag("a"), json!({"since": start, "max_tokens": 1}));
    assert_eq!(first["entries"], json!([all[0]]), "{first}");
    assert_eq!(first["more"], true);
    on(&e, |r| {
        r.view_close(&writer(), &["a".to_string()], "by hand")
            .unwrap();
    });
    let cut = first["cursor"].as_str().unwrap().to_string();
    let rest = read(&e, &ag("a"), json!({"since": cut, "max_tokens": 100000}));
    let rest = rest["entries"].as_array().unwrap();
    assert!(
        !rest.contains(&all[0]),
        "the first move came twice: {rest:?}"
    );
    assert_eq!(
        rest.first(),
        all.get(1),
        "the second move is next: {rest:?}"
    );
}

/// Review focus 1 at re-key folds: a landing batch whose moves fold into two re-key entries,
/// read one entry per page, gives the full read once, and the pages end. A cut falls between the
/// two folds, inside the batch's moves.
#[test]
fn ib6_a_landing_with_two_rekey_folds_pages_one_entry_at_a_time() {
    let (_d, mut r) = qa_project();
    let x = landed_claim(&mut r, "payments/fees");
    let y = landed_claim(&mut r, "payments/rates");
    put(&mut r, "a", upd(&x, "a's x"));
    put(&mut r, "a", upd(&y, "a's y"));
    let e = Local::new(r);
    subscribe(&e, &ag("a"), None).unwrap();
    on(&e, move |r| {
        put(
            r,
            "b",
            json!([{"op": "update", "id": x.to_string(), "body": "b's x"},
                   {"op": "update", "id": y.to_string(), "body": "b's y"}]),
        );
        land(r, "b").unwrap();
    });
    let all = read(&e, &ag("a"), json!({"max_tokens": 100000}))["entries"]
        .as_array()
        .unwrap()
        .clone();
    let folds: Vec<usize> = all
        .iter()
        .enumerate()
        .filter(|(_, x)| x["line"].as_str().unwrap().contains(" replaces "))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(folds.len(), 2, "two re-key folds: {all:?}");
    assert_eq!(folds[1], folds[0] + 1, "{all:?}");
    assert!(
        kinds(&all[..folds[0]]).contains(&"landed"),
        "the folds are in the landing batch, after its events: {all:?}"
    );
    assert_eq!(page_by_one(&e, &ag("a"), None), all);
}

/// The location of every frame from the persisted cursor of agent `name` to the head, and
/// whether it starts a batch.
fn frames_after(e: &Local, name: &str) -> Vec<(rdy_store::segment::Location, bool)> {
    let st = e.read();
    let mut out = vec![];
    let mut starts = true;
    st.reader()
        .for_each_frame_from(persisted(e, name).at, st.seq(), |loc, kind, _, _, _| {
            out.push((loc, starts));
            starts = kind == rdy_store::kinds::BATCH_COMMIT;
            Ok(())
        })
        .unwrap();
    out
}

/// Brief "Texts": a cursor whose location is a frame inside a batch, not a batch's start, is
/// refused as a bad cursor; every batch start is a cursor.
#[test]
fn ib6_a_cursor_inside_a_batch_is_refused() {
    let (_d, mut r) = qa_project();
    let c = landed_claim(&mut r, "payments/fees");
    put(&mut r, "a", upd(&c, "a's text"));
    let e = Local::new(r);
    subscribe(&e, &ag("a"), None).unwrap();
    on(&e, move |r| {
        put(r, "b", upd(&c, "b's text"));
        land(r, "b").unwrap();
    });
    let frames = frames_after(&e, "a");
    assert!(frames.iter().filter(|(_, s)| !*s).count() > 3, "{frames:?}");
    for (loc, starts) in frames {
        let t = format!("{}.{}.0", loc.segment, loc.offset);
        let got = run_inbox(
            &e,
            &ag("a"),
            InboxRequest {
                since: Some(t.clone()),
                ..Default::default()
            },
        );
        if starts {
            assert!(got.is_ok(), "{t} starts a batch: {got:?}");
        } else {
            let err = got.expect_err(&t);
            assert_eq!(
                (err.code, err.message),
                (E_BAD_INPUT, format!("{t:?} is not an inbox cursor"))
            );
        }
    }
}

/// A cursor at the end of a segment names the first batch of the next segment: with small
/// segments, pages of one entry read the log once, through cursors that end a segment.
#[test]
fn ib6_a_cursor_at_a_segment_end_reads_on() {
    let (_d, mut r) = qa_project();
    r.store_mut().set_segment_max(2_000);
    let e = Local::new(r);
    subscribe(&e, &ag("a"), Some(vec!["payments".into()])).unwrap();
    on(&e, |r| payments_landings(r, 3));
    let all = read(&e, &ag("a"), json!({"max_tokens": 100000}))["entries"].clone();
    assert_eq!(all.as_array().unwrap().len(), 4, "{all}");
    let (mut got, mut since, mut at_ends) = (vec![], None::<String>, 0);
    for _ in 0..100 {
        let mut req = json!({"max_tokens": 1});
        if let Some(c) = &since {
            req["since"] = json!(c);
        }
        let p = read(&e, &ag("a"), req);
        got.extend(p["entries"].as_array().unwrap().iter().cloned());
        let c = parse_cursor(p["cursor"].as_str().unwrap()).unwrap();
        let st = e.read();
        let mut first = None;
        st.reader()
            .for_each_frame_from(c.at, st.seq(), |loc, _, _, _, _| {
                first.get_or_insert(loc);
                Ok(())
            })
            .unwrap();
        if first.is_some_and(|f| f.segment > c.at.segment) {
            at_ends += 1;
        }
        since = Some(cursor_text(&c));
        if p["more"] == false {
            break;
        }
    }
    assert!(at_ends > 0, "no cursor ended a segment");
    assert_eq!(Value::Array(got), all);
}

/// Final review fix round 1 (Minor 2): damage to the log after a valid cursor is a store error,
/// never "not an inbox cursor": only the first frame walked can be the cursor's fault.
#[test]
fn ib6_damage_after_a_valid_cursor_is_a_store_error() {
    let (d, mut r) = qa_project();
    let c = landed_claim(&mut r, "payments/fees");
    put(&mut r, "a", upd(&c, "a's text"));
    let e = Local::new(r);
    subscribe(&e, &ag("a"), None).unwrap();
    on(&e, move |r| {
        put(r, "b", upd(&c, "b's text"));
        land(r, "b").unwrap();
    });
    // the last frame of the second batch after the cursor: the walk reads one batch, then fails
    let frames = frames_after(&e, "a");
    let starts: Vec<usize> = (0..frames.len()).filter(|i| frames[*i].1).collect();
    assert!(starts.len() > 2, "{frames:?}");
    let (end, _) = frames[starts[2]];
    let (last, _) = frames[starts[2] - 1];
    assert_eq!(last.segment, end.segment, "{frames:?}");
    let seg = d
        .path()
        .join(format!(".rdy/store/segments/seg-{:06}.log", end.segment));
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[end.offset as usize - 1] ^= 0xff;
    std::fs::write(&seg, bytes).unwrap();
    let err = run_inbox(&e, &ag("a"), InboxRequest::default()).unwrap_err();
    assert_ne!(err.code, E_BAD_INPUT, "{err:?}");
    assert!(!err.message.contains("is not an inbox cursor"), "{err:?}");
}
