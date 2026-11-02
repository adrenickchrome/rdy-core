//! ui9 (D-WPE4R, TASK-979, TASK-982 O8): the approval pages' reload stream. A UI stream reuses the
//! site's event route, script, connection limits, redaction epoch checks, idle holds and shutdown.
//! Every published project write and config refresh reloads it, also one that leaves every site
//! byte unchanged, and no event carries a card's text, an actor, an option or an error. `serve.ui`
//! off reloads it and closes it.
use crate::daemon_approver_ui::{Site, body, local};
use rdy_daemon::Shared;
use rdy_daemon::site::guard::{Refusal, ScopeRef, SiteTarget, refusal};
use rdy_daemon::site::live::{LiveEvent, LiveSite, MAX_STREAMS};
use rdy_daemon::site::server::MAX_CONNECTIONS;
use rdy_engine::Rdy;
use rdy_engine::engine::write_typed;
use rdy_engine::redaction::RedactInput;
use rdy_model::Hash;
use rdy_qa::daemon::short_tempdir;
use rdy_qa::fixtures::{approver, land, seed_doc, write, writer};
use rdy_qa::http::{self, Sse};
use rdy_qa::intake::answer_open_items;
use serde_json::{Value, json};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::Duration;
use tokio::sync::mpsc::error::TryRecvError;

/// The deadline of a wait for an event.
const T: Duration = Duration::from_secs(10);
const PERSON: &str = "actor: human:qa\n";

/// A landed seed doc and one open question-free version, served: the section and the stream's
/// page revision.
fn world(config: &str) -> (Site, String) {
    let dir = short_tempdir();
    let mut r = Rdy::init(dir.path(), "rdy-standard").expect("init");
    let (_, section, _) = seed_doc(&mut r, "seed");
    let p = r
        .propose(&writer(), "seed", "Seed", Some("Seeds."))
        .expect("propose seed");
    r.land(&approver(), &p.id).expect("land seed");
    write(
        &mut r,
        "c",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "A claim of c."}]),
    )
    .unwrap();
    answer_open_items(&mut r, &writer(), "c");
    r.propose(&writer(), "c", "c", Some("Adds a claim."))
        .unwrap();
    r.close().expect("close");
    (Site::start(dir, config), section)
}

fn base(s: &Site) -> String {
    format!("http://{}/", s.host())
}

/// The events URL the pending page names.
fn events_of(s: &Site) -> String {
    let page = s.get("/~ui/");
    assert_eq!(page.status, 200, "{}", page.head);
    http::events_url(&base(s), &body(&page))
}

fn rev_of(url: &str) -> String {
    url.rsplit_once("rev=").expect("a rev").1.to_string()
}

fn on_writer<T: serde::Serialize + 'static>(
    sh: &Arc<Shared>,
    f: impl FnOnce(&mut Rdy) -> T + Send + 'static,
) -> Value {
    write_typed(&*sh.engine(), move |r| Ok(f(r))).unwrap()
}

/// An ANSWER: its publish moves the store and no site byte (a question-free reject).
fn answer_something(s: &Site) {
    let page = body(&s.get("/~ui/"));
    let form = http::ui_forms(&page).remove(0);
    let done = s.send(&http::ui_answer(&s.host(), &form, "reject"));
    assert_eq!(done.status, 303, "{}", body(&done));
}

const RELOAD: Option<(&str, &str)> = Some(("reload", "{}"));

#[track_caller]
fn next(sse: &mut Sse) -> Option<(String, String)> {
    sse.next_event(T)
}

#[track_caller]
fn assert_reload(sse: &mut Sse, what: &str) {
    let got = next(sse);
    assert_eq!(
        got.as_ref().map(|(e, d)| (e.as_str(), d.as_str())),
        RELOAD,
        "{what}"
    );
}

/// An ANSWER publish wakes the UI's stream with `reload` and carries nothing, while an open
/// documentation page whose bytes did not change gets no event; a stale revision and a publish
/// raced with the subscribe, held at a barrier, are never missed.
#[test]
fn ui9_ui_stream_reloads_on_answer_without_site_byte_changes() {
    let (s, _) = world(PERSON);
    let c = SiteTarget::Source(ScopeRef::Canon);
    let b = s.sh.site.refresh(&s.sh, &c).unwrap();
    let mut doc =
        s.sh.site
            .subscribe(
                &s.sh,
                &c,
                "index.html",
                &b.live.revs["index.html"],
                &b.live.label_rev,
            )
            .unwrap();
    let url = events_of(&s);
    let rev = rev_of(&url);
    let st = s.sh.state.load_full();
    assert_eq!(rev, LiveSite::ui_revision(&st), "the page's revision");
    assert_eq!(rev.len(), 16, "{rev}");
    drop(st);
    let mut ui = Sse::open(&url);
    assert_eq!(s.sh.site.streams(), 2);
    answer_something(&s);
    assert_reload(&mut ui, "the ANSWER");
    // the fan-out and the build run here too, and a build of canon's unchanged bytes sends nothing
    s.sh.site.step(&s.sh);
    assert_eq!(
        doc.rx.try_recv(),
        Err(TryRecvError::Empty),
        "no false reload: an ANSWER leaves canon's bytes"
    );
    drop(doc);
    // a page opened at the old revision reloads at once
    let mut late = Sse::open(&url);
    assert_reload(&mut late, "a stale revision");
    // a matching revision stays quiet
    let fresh = events_of(&s);
    assert_ne!(rev_of(&fresh), rev);
    let mut same = Sse::open(&fresh);
    let mut quiet = s.sh.site.subscribe_ui(&s.sh, &rev_of(&fresh)).unwrap();
    s.sh.site.step(&s.sh);
    assert_eq!(
        quiet.rx.try_recv(),
        Err(TryRecvError::Empty),
        "the current revision"
    );
    drop(quiet);

    // the race: the subscribe has registered and has not compared when a publish comes
    let rev = LiveSite::ui_revision(&s.sh.state.load_full());
    let (at_tx, at_rx) = channel();
    let at_tx = std::sync::Mutex::new(at_tx);
    let (go_tx, go_rx) = channel::<()>();
    let go_rx = std::sync::Mutex::new(go_rx);
    s.sh.site.set_before_ui_compare(Some(Box::new(move || {
        at_tx.lock().unwrap().send(()).unwrap();
        go_rx.lock().unwrap().recv().unwrap();
    })));
    let sh = Arc::clone(&s.sh);
    let sub = std::thread::spawn(move || sh.site.subscribe_ui(&sh, &rev));
    at_rx.recv().unwrap();
    s.sh.site.set_before_ui_compare(None);
    on_writer(&s.sh, |r| {
        write(
            r,
            "other",
            json!([{"op": "create", "type": "doc", "fields": {"name": "n", "title": "N", "doc_type": "page"}}]),
        )
        .unwrap();
    });
    go_tx.send(()).unwrap();
    let mut sub = sub.join().unwrap().expect("the subscription");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let got = rt.block_on(async { tokio::time::timeout(T, sub.rx.recv()).await });
    assert_eq!(got, Ok(Some(LiveEvent::Reload)), "the raced publish");
    // a view write that leaves canon's pages also reloads every UI stream, and the others wait
    assert_reload(&mut same, "the raced publish, on the open stream");
    drop(sub);
}

/// A UI stream counts in the 32 streams and each holds a connection of the 64; the 33rd and the
/// 65th are the existing busy answer; a removal's epoch, `serve.ui` off, a drop and a shutdown each
/// release what they hold. The existing script closes a hidden tab's stream and opens it again.
#[test]
fn ui9_ui_stream_shares_limits_epoch_and_shutdown() {
    let (s, section) = world(PERSON);
    let rev = LiveSite::ui_revision(&s.sh.state.load_full());
    // the shared stream count, with site streams among them
    let c = SiteTarget::Source(ScopeRef::Canon);
    let b = s.sh.site.refresh(&s.sh, &c).unwrap();
    let site =
        s.sh.site
            .subscribe(
                &s.sh,
                &c,
                "index.html",
                &b.live.revs["index.html"],
                &b.live.label_rev,
            )
            .unwrap();
    let mut open: Vec<_> = (1..MAX_STREAMS)
        .map(|_| {
            s.sh.site
                .subscribe_ui(&s.sh, &rev)
                .expect("under the limit")
        })
        .collect();
    assert_eq!(s.sh.site.streams(), MAX_STREAMS);
    assert!(
        matches!(s.sh.site.subscribe_ui(&s.sh, &rev), Err(Refusal::Busy)),
        "the 33rd"
    );
    let over = s.get(&format!("/_rdy/events?ui=1&rev={rev}"));
    assert_eq!(over.status, 503, "{}", over.head);
    assert_eq!(over.body, refusal(Refusal::Busy).body, "the existing text");
    assert!(
        s.sh.site
            .subscribe(
                &s.sh,
                &c,
                "index.html",
                &b.live.revs["index.html"],
                &b.live.label_rev,
            )
            .is_err(),
        "a site stream shares the count"
    );
    // a drop releases a stream: one more fits
    drop(open.pop());
    assert_eq!(s.sh.site.streams(), MAX_STREAMS - 1);
    let mut one = s.sh.site.subscribe_ui(&s.sh, &rev).expect("the freed slot");
    assert_eq!(one.rx.try_recv(), Err(TryRecvError::Empty));
    drop(one);
    drop(open);
    drop(site);
    assert_eq!(s.sh.site.streams(), 0, "the drops released every stream");

    // backpressure: a stream that does not read drops off, as a site stream does
    let mut lazy = s.sh.site.subscribe_ui(&s.sh, &rev).unwrap();
    let sect = section.clone();
    for i in 0..(rdy_daemon::site::live::STREAM_QUEUE * 4) {
        let parent = sect.clone();
        on_writer(&s.sh, move |r| {
            write(
                r,
                "bp",
                json!([{"op": "create", "type": "claim", "parent": parent, "body": format!("Claim {i}.")}]),
            )
            .unwrap();
        });
        s.sh.site.step(&s.sh);
    }
    assert_eq!(s.sh.site.streams(), 0, "the full stream was dropped");
    for _ in 0..rdy_daemon::site::live::STREAM_QUEUE {
        assert_eq!(lazy.rx.try_recv(), Ok(LiveEvent::Reload));
    }
    assert_eq!(lazy.rx.try_recv(), Err(TryRecvError::Disconnected));

    // the 64 connections: a real UI stream holds one, and the 65th gets Busy
    let url = events_of(&s);
    let mut streams: Vec<Sse> = (0..MAX_STREAMS).map(|_| Sse::open(&url)).collect();
    let held: Vec<TcpStream> = (0..MAX_CONNECTIONS - MAX_STREAMS)
        .map(|_| TcpStream::connect(s.addr()).unwrap())
        .collect();
    // a pause of the accept loop never orders these: the head is read by the connection's task,
    // so the answer below proves the permit count
    let busy = s.get("/~ui/");
    assert_eq!(busy.status, 503, "{}", busy.head);
    assert_eq!(busy.body, refusal(Refusal::Busy).body);
    drop(held);
    streams.truncate(1);
    let mut kept = streams.pop().unwrap();

    // a removal: the stream's next event is reload; nothing of the removed text in it
    let id = on_writer(&s.sh, move |r| {
        let w = write(
            r,
            "secret",
            json!([{"op": "create", "type": "claim", "id": "$c", "parent": section, "body": "SECRET."}]),
        )
        .unwrap();
        answer_open_items(r, &writer(), "secret");
        land(r, "secret").unwrap();
        w.created["$c"].clone()
    });
    let mut epoch = Sse::open(&events_of(&s));
    let target = id.as_str().unwrap().to_string();
    on_writer(&s.sh, move |r| {
        r.redact_in(
            &writer(),
            "rm",
            RedactInput {
                targets: vec![target],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
        let v = r
            .propose(&writer(), "rm", "Remove", Some("Removes it."))
            .unwrap();
        r.land(&approver(), &v.id).unwrap();
    });
    assert_reload(&mut epoch, "the removal");
    assert_reload(&mut kept, "the removal, on a stream opened earlier");

    // serve.ui off: a config refresh queues reload and closes the UI stream, not a site stream
    let page = body(&s.get("/index.html"));
    let doc = Sse::open(&http::events_url(&base(&s), &page));
    let mut ui = Sse::open(&events_of(&s));
    local(s.h.dir.path(), &format!("{PERSON}serve:\n  ui: false\n"));
    // the next UI path reads it, and answers as an ordinary path
    let off = s.get("/~ui/");
    assert_eq!(off.status, 404, "{}", off.head);
    assert_reload(&mut ui, "off");
    assert_eq!(ui.next_event(T), None, "closed");
    assert!(ui.ended());
    drop(kept);
    drop(epoch);
    // a new stream is refused as an ordinary event request, and nothing registers
    let refused = s.get(&format!(
        "/_rdy/events?ui=1&rev={}",
        LiveSite::ui_revision(&s.sh.state.load_full())
    ));
    assert_eq!(refused.status, 400, "{}", refused.head);
    assert_eq!(refused.body, refusal(Refusal::Malformed).body);
    assert!(!doc.ended(), "a site stream stays");
    drop(doc);
    // a refresh with no UI request also closes it: a write's publish reads the file
    local(s.h.dir.path(), PERSON);
    let mut ui = Sse::open(&events_of(&s));
    local(s.h.dir.path(), &format!("{PERSON}serve:\n  ui: false\n"));
    on_writer(&s.sh, |r| {
        write(
            r,
            "w2",
            json!([{"op": "create", "type": "doc", "fields": {"name": "w2", "title": "W2", "doc_type": "page"}}]),
        )
        .unwrap();
    });
    assert_reload(&mut ui, "a write's refresh");
    assert_eq!(ui.next_event(T), None, "closed");
    local(s.h.dir.path(), PERSON);

    // a config refresh with the UI on reloads it and keeps it
    let mut ui = Sse::open(&events_of(&s));
    local(
        s.h.dir.path(),
        &format!("{PERSON}serve:\n  idle_timeout: 61\n"),
    );
    let _ = s.get("/~ui/");
    assert_reload(&mut ui, "a config refresh");

    // the idle hold and the shutdown: an open stream ends with the daemon
    let mut last = Sse::open(&events_of(&s));
    let streams_before = s.sh.site.streams();
    assert!(streams_before >= 1);
    s.sh.site.close();
    assert_eq!(s.sh.site.streams(), 0, "the close releases every stream");
    let _ = last.next_event(T);
    assert!(last.ended());
    assert!(
        matches!(
            s.sh.site.subscribe_ui(&s.sh, "0000000000000000"),
            Err(Refusal::Busy)
        ),
        "a closed site opens nothing"
    );
}

/// New defect (browser smoke check): the writer publishes after every message, written or not. A
/// `Reload` for each publish reloaded the page while its form was on its way, which canceled the
/// page that answers it: a Later's notice and a refusal's details never showed. An answer that
/// writes nothing reloads no stream; one that writes does, and a config refresh does.
#[test]
fn ui9_an_answer_that_writes_nothing_reloads_no_stream() {
    let (s, section) = world(PERSON);
    let page = body(&s.get("/~ui/"));
    let form = http::ui_forms(&page).remove(0);
    let rev = rev_of(&events_of(&s));
    let mut sub = s.sh.site.subscribe_ui(&s.sh, &rev).unwrap();
    let quiet = |sub: &mut rdy_daemon::site::live::Subscription, what: &str| {
        // a step's fan-out is done when it returns; the worker's own step reads the same state
        s.sh.site.step(&s.sh);
        assert_eq!(sub.rx.try_recv(), Err(TryRecvError::Empty), "{what}");
    };
    quiet(&mut sub, "at the page's revision");
    // a publish that wrote nothing
    on_writer(&s.sh, |_| ());
    quiet(&mut sub, "an empty publish");
    // a UI Later and two refusals: each runs on the writer, and each writes nothing
    let later = s.send(&http::ui_answer(&s.host(), &form, "later"));
    assert_eq!(later.status, 200, "{}", later.head);
    quiet(&mut sub, "a Later");
    let wrong = http::UiForm {
        card_hash: Hash::of(b"another card").to_string(),
        ..form.clone()
    };
    let moved = s.send(&http::ui_answer(&s.host(), &wrong, "approve"));
    assert_eq!(moved.status, 409, "{}", moved.head);
    quiet(&mut sub, "a moved card");
    let bad = s.send(&http::ui_answer(&s.host(), &form, "nosuch"));
    assert_eq!(bad.status, 400, "{}", bad.head);
    quiet(&mut sub, "an unknown option");
    // a write reloads once; another message after it adds none
    on_writer(&s.sh, move |r| {
        write(
            r,
            "w",
            json!([{"op": "create", "type": "claim", "parent": section, "body": "One more."}]),
        )
        .unwrap();
    });
    s.sh.site.step(&s.sh);
    assert_eq!(sub.rx.try_recv(), Ok(LiveEvent::Reload), "the write");
    on_writer(&s.sh, |_| ());
    quiet(&mut sub, "after the write's reload");
}

/// The existing script drives a UI stream: a hidden tab closes it, a visible one opens it again at
/// the same URL, and a `reload` event reloads the page. No other handler is needed.
#[test]
fn ui9_the_existing_script_handles_a_hidden_tab() {
    if !rdy_qa::skip::node_available() {
        rdy_qa::skip::skip(
            "ui9_the_existing_script_handles_a_hidden_tab",
            rdy_qa::skip::Why::Prerequisite,
            "node is not installed",
        );
        return;
    }
    let script = r#"
const url = '/_rdy/events?ui=1&rev=0011223344556677';
const streams = []; let reloads = 0;
class ES { constructor(u) { this.url = u; this.closed = false; this.readyState = 1; this.l = {}; streams.push(this); }
  addEventListener(t, f) { this.l[t] = f; } close() { this.closed = true; } }
const listeners = {};
const document = { currentScript: { getAttribute: (k) => k === 'data-events' ? url : null }, visibilityState: 'visible',
  querySelectorAll: () => [], addEventListener: (t, f) => { listeners[t] = f; } };
const window = { EventSource: ES };
const sandbox = new Function('document', 'window', 'location', 'sessionStorage', 'addEventListener', 'scrollTo', 'scrollY', 'EventSource', process.argv[1]);
const storage = { getItem: () => null, removeItem() {}, setItem() {} };
sandbox(document, window, { pathname: '/~ui/', reload: () => { reloads += 1; } }, storage, () => {}, () => {}, 0, ES);
const out = { first: streams.length, url: streams[0].url };
document.visibilityState = 'hidden'; listeners.visibilitychange();
out.hidden_closed = streams[0].closed; out.hidden_open = streams.length;
document.visibilityState = 'visible'; listeners.visibilitychange();
out.reopened = streams.length; out.same_url = streams[1].url === url;
streams[1].l.reload({}); out.reloads = reloads; out.closed = streams[1].closed;
console.log(JSON.stringify(out));
"#;
    let o = std::process::Command::new("node")
        .args(["-e", script, rdy_render::live::LIVE_JS])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        v,
        json!({"first": 1, "url": "/_rdy/events?ui=1&rev=0011223344556677",
            "hidden_closed": true, "hidden_open": 1, "reopened": 2, "same_url": true,
            "reloads": 1, "closed": true})
    );
}
