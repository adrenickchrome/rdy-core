//! mht4 (D-6M0AF, D-4PHYQ, D-44NVC): listens and elicited answers over MCP-over-HTTP. A modern
//! `subscriptions/listen` POST is admitted only with its claim, inside its filter bounds and the
//! daemon's listen capacity, and only then acknowledged. Each listen has its own sink, identity,
//! filter and bounded queue. The two older revisions get no push channel and no callback
//! elicitation, and cannot resume a modern card. In memory, through the production admission
//! layer and rmcp's `StreamableHttpService`; no socket. The registry clock is injected; nothing
//! waits on the wall clock.
use rdy_engine::answer::presented_item;
use rdy_engine::engine::{Engine, Local};
use rdy_engine::instructions::{
    self as instructions, Caller, Contribution, Layer, LoadRequest, PersonHost,
};
use rdy_engine::instructions_config::{PathEnv, PersonSource, Platform, resolve_person};
use rdy_engine::pending::Item;
use rdy_engine::{Rdy, RdyError};
use rdy_mcp::elicitation::{EXPIRY, Registry as Cards};
use rdy_mcp::http::{HttpMcp, MAX_LISTEN_URI_BYTES, MAX_LISTEN_URIS, MAX_LISTENS, Opened};
use rdy_mcp::notify::{MAX_QUEUED, NotifyHub};
use rdy_model::EvidenceKind;
use rdy_qa::fixtures::{approver, seed_doc, write, writer};
use rdy_qa::intake::PERSON;
use serde_json::{Value, json};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MODERN: &str = "2026-07-28";
const OLDER: [&str; 2] = ["2025-06-18", "2025-11-25"];

const UNAVAILABLE: (&str, &str, &str) = (
    "E_ELICITATION_UNAVAILABLE",
    "this client cannot show rdy's card itself",
    "show the card in chat and relay the person's answer: rdy_answer with option and evidence",
);
const GONE: (&str, &str, &str) = (
    "E_BAD_INPUT",
    "this elicited answer is no longer available",
    "read rdy_pending, then ask again with rdy_answer and elicit: true",
);
const MIXED: (&str, &str, &str) = (
    "E_BAD_INPUT",
    "elicit takes only ref, card_hash and brief",
    "pass ref, card_hash, elicit: true and an optional brief, or relay an answer with option and evidence",
);
const LISTENS_FULL: (&str, &str, &str) = (
    "E_LIMIT",
    "MCP over HTTP has 32 open listen requests",
    "cancel an older subscriptions/listen request",
);
const FILTER_COUNT: (&str, &str, &str) = (
    "E_LIMIT",
    "the listen filter has over 128 resource URIs",
    "use at most 128 resource URIs in one listen request",
);
const FILTER_URI: (&str, &str, &str) = (
    "E_LIMIT",
    "a listen resource URI is over 4096 bytes",
    "use a resource URI of at most 4096 bytes",
);

/// A person host over an isolated home: nothing here reads the real person layer.
struct Home(PathBuf);

impl PersonHost for Home {
    fn locate(&self) -> Result<PersonSource, RdyError> {
        resolve_person(
            Platform::HOST,
            &PathEnv {
                home: Some(self.0.clone().into()),
                user_profile: Some(self.0.clone().into()),
                xdg_config_home: Some(self.0.join("cfg").into()),
                ..PathEnv::default()
            },
        )
    }

    fn fetch(
        &self,
        meta: &Path,
        load: &LoadRequest,
        _caller: &Caller,
    ) -> Result<Contribution, RdyError> {
        let r = Rdy::open(meta)?;
        let part = instructions::collect(&r.read(), Layer::Person, load);
        r.close()?;
        part
    }
}

/// One project in process, its home isolated, served over HTTP with `hub`.
struct Fixture {
    _dir: tempfile::TempDir,
    _home: tempfile::TempDir,
    engine: Arc<dyn Engine>,
    hub: Arc<NotifyHub>,
    mcp: HttpMcp,
    next: AtomicU64,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture::build(|_, _| {}, NotifyHub::new())
    }

    /// A project that `seed` fills before it is served on `hub`.
    fn build(seed: impl FnOnce(&Path, &mut Rdy), hub: Arc<NotifyHub>) -> Fixture {
        let (dir, mut r) = rdy_qa::fixtures::project();
        seed(dir.path(), &mut r);
        r.close().unwrap();
        let home = tempfile::tempdir().unwrap();
        let engine: Arc<dyn Engine> = Arc::new(
            Local::open(dir.path())
                .unwrap()
                .with_person_host(Arc::new(Home(home.path().to_path_buf()))),
        );
        let mcp = HttpMcp::new(Arc::clone(&engine), Arc::clone(&hub));
        Fixture {
            _dir: dir,
            _home: home,
            engine,
            hub,
            mcp,
            next: AtomicU64::new(0),
        }
    }

    /// One JSON-RPC request of revision `revision`, as a client sends it, and its open response.
    async fn open(&self, revision: &str, method: &str, params: Value) -> (u64, Opened) {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let mut h: Vec<(&str, String)> = vec![
            ("host", "localhost".into()),
            ("accept", "application/json, text/event-stream".into()),
            ("content-type", "application/json".into()),
            ("mcp-protocol-version", revision.into()),
        ];
        if revision == MODERN {
            h.push(("mcp-method", method.into()));
            if let Some(name) = params["name"].as_str() {
                h.push(("mcp-name", name.into()));
            }
        }
        let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        (id, self.mcp.open(&h, body.to_string().into_bytes()).await)
    }

    /// The one reply of a request that is no stream.
    async fn reply(&self, revision: &str, method: &str, params: Value) -> Value {
        let (id, mut o) = self.open(revision, method, params).await;
        assert_eq!(o.status, 200, "{method}");
        let reply = guard(o.next()).await.expect("a reply");
        assert_eq!(reply["id"], json!(id), "{reply}");
        assert!(guard(o.next()).await.is_none(), "one reply only");
        reply
    }

    /// A modern listen under `claim` for `uris`, as it was answered.
    async fn listen(&self, claim: Value, uris: &[String]) -> Opened {
        let params = json!({"_meta": modern(claim),
            "notifications": {"resourceSubscriptions": uris}});
        self.open(MODERN, "subscriptions/listen", params).await.1
    }

    /// A modern listen rdy acknowledged, with the URIs it accepted. It returns once the hub
    /// holds the listen.
    async fn listening(&self, claim: Value, uris: &[String]) -> (Opened, Vec<String>, u64) {
        let before: Vec<u64> = self.hub.targets().iter().map(|t| t.sid).collect();
        let mut o = self.listen(claim, uris).await;
        assert_eq!(
            o.header("content-type"),
            Some("text/event-stream"),
            "an acknowledged listen streams"
        );
        let ack = guard(o.next()).await.expect("the acknowledgement");
        assert_eq!(
            ack["method"], "notifications/subscriptions/acknowledged",
            "{ack}"
        );
        let accepted = ack["params"]["notifications"]["resourceSubscriptions"]
            .as_array()
            .map(|a| a.iter().map(|u| u.as_str().unwrap().to_string()).collect())
            .unwrap_or_default();
        let hub = Arc::clone(&self.hub);
        let sid = guard(async move {
            loop {
                if let Some(t) = hub.targets().iter().find(|t| !before.contains(&t.sid)) {
                    return t.sid;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        (o, accepted, sid)
    }

    /// Waits until `f` holds. A condition that never holds fails at the hang guard.
    async fn until(&self, what: &str, f: impl Fn(&Fixture) -> bool) {
        let wait = async {
            while !f(self) {
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(Duration::from_secs(60), wait)
            .await
            .unwrap_or_else(|_| panic!("never: {what}"));
    }

    /// One `rdy_answer` call's `result`, with `extra` beside its parameters.
    async fn answer(&self, revision: &str, claim: Value, args: Value, extra: Value) -> Value {
        let mut args = args;
        rdy_qa::mcp::supply_kungfu("rdy_answer", &mut args);
        let meta = if revision == MODERN {
            modern(claim)
        } else {
            claim
        };
        let mut params = json!({"name": "rdy_answer", "arguments": args, "_meta": meta});
        for (k, v) in extra.as_object().unwrap() {
            params[k] = v.clone();
        }
        let reply = self.reply(revision, "tools/call", params).await;
        assert!(reply.get("error").is_none(), "a tool result: {reply}");
        reply["result"].clone()
    }

    /// The card of `r` in the person's complete queue.
    fn card(&self, r: &str) -> Item {
        presented_item(&self.engine.read(), PERSON, r, None)
            .unwrap_or_else(|e| panic!("the card of {r}: {e:#?}"))
    }

    fn seq(&self) -> rdy_engine::Location {
        self.engine.read().seq()
    }

    /// The stored answer of `key`, when one was written.
    fn stored(&self, key: &str) -> Option<rdy_model::Answer> {
        self.engine
            .read()
            .reader()
            .answer_of(key)
            .expect("answer_of")
            .map(|(_, a)| a)
    }
}

/// `f`, under a hang guard: an answer that never comes fails the test instead of hanging it.
async fn guard<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), f)
        .await
        .expect("the answer came")
}

/// The modern per-request metadata, with a client that can show a form, and `claim`.
fn modern(claim: Value) -> Value {
    let mut m = json!({
        "io.modelcontextprotocol/protocolVersion": MODERN,
        "io.modelcontextprotocol/clientCapabilities": {"elicitation": {"form": {}}},
        "io.modelcontextprotocol/clientInfo": {"name": "mht4", "version": "0"},
    });
    for (k, v) in claim.as_object().unwrap() {
        m[k] = v.clone();
    }
    m
}

fn claim(actor: &str, run: &str) -> Value {
    json!({"rdy/actor": actor, "rdy/run": run})
}

fn views(prefix: &str, n: usize) -> Vec<String> {
    (0..n).map(|i| format!("rdy://view/{prefix}{i}")).collect()
}

/// The refusal of a listen: one JSON reply, no acknowledgement and no stream. Its rdy error.
async fn refused(mut o: Opened) -> (String, String, String) {
    assert_eq!(o.status, 200);
    assert_eq!(o.header("content-type"), Some("application/json"));
    let reply = guard(o.next()).await.expect("the refusal");
    assert!(guard(o.next()).await.is_none(), "no acknowledgement");
    let data = &reply["error"]["data"];
    let s = |k: &str| data[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

fn triple(t: (&str, &str, &str)) -> (String, String, String) {
    (t.0.into(), t.1.into(), t.2.into())
}

/// The next resource update a listen stream carries: its URI and its subscription id.
async fn updated(o: &mut Opened) -> (String, Value) {
    let m = guard(o.next()).await.expect("an update");
    assert_eq!(m["method"], "notifications/resources/updated", "{m}");
    (
        m["params"]["uri"].as_str().unwrap().to_string(),
        m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"].clone(),
    )
}

/// A tool result's value, or its refusal.
fn done(result: &Value) -> Result<Value, Value> {
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("a tool result: {result}"));
    let v: Value = serde_json::from_str(text).unwrap();
    if result["isError"] == json!(true) {
        Err(v)
    } else {
        Ok(v)
    }
}

#[track_caller]
fn refused_with(result: &Value, want: (&str, &str, &str)) {
    let e = done(result).expect_err("a refusal");
    assert_eq!(
        (
            e["code"].as_str().unwrap(),
            e["message"].as_str().unwrap(),
            e["fix"].as_str().unwrap()
        ),
        want,
        "{e}"
    );
}

// ---------------------------------------------------------------- elicitation worlds

/// A project whose canon holds a seed doc, a claim proposed in view `one` with a summary, and
/// `human:qa` as its person, served on `hub`. The world and the proposal's ref.
fn approval(hub: Arc<NotifyHub>) -> (Fixture, String) {
    let made = Arc::new(Mutex::new(String::new()));
    let put = Arc::clone(&made);
    let w = Fixture::build(
        move |dir, r| {
            let (_, section, _) = seed_doc(r, "seed");
            let p = r
                .propose(&writer(), "seed", "Seed", Some("Seeds."))
                .unwrap();
            r.land(&approver(), &p.id).unwrap();
            write(
                r,
                "one",
                json!([{"op": "create", "type": "claim", "parent": section, "body": "A claim of one."}]),
            )
            .unwrap();
            rdy_qa::intake::answer_open_items(r, &writer(), "one");
            let p = r
                .propose(&writer(), "one", "one", Some("Adds one claim."))
                .unwrap();
            std::fs::write(dir.join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
            *put.lock().unwrap() = p.r#ref;
        },
        hub,
    );
    let r = made.lock().unwrap().clone();
    (w, r)
}

fn ask(item: &Item) -> Value {
    json!({"ref": item.r#ref, "card_hash": item.card_hash, "elicit": true})
}

fn choice(id: &str) -> Value {
    json!({"action": "accept", "content": {"choice": id}})
}

/// The retry of a 2026-07-28 card: `requestState` and the one response.
fn retry(handle: &str, entry: Value) -> Value {
    json!({"requestState": handle, "inputResponses": {"answer": entry}})
}

/// Asks for a 2026-07-28 card over HTTP under `claim` and returns its handle.
async fn handle_of(w: &Fixture, claim: Value, item: &Item) -> String {
    let r = w.answer(MODERN, claim, ask(item), json!({})).await;
    assert_eq!(r["resultType"], "input_required", "{r}");
    r["requestState"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------- listens

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht4_listen_checks_claim_and_limit_before_ack() {
    let w = Fixture::new();
    // no claim, or an options-only one: no session, before any bound or capacity
    for meta in [json!({}), json!({"rdy/role": "reader"})] {
        let e = refused(w.listen(meta, &views("v", MAX_LISTEN_URIS + 1)).await).await;
        assert_eq!(e.0, "E_NO_SESSION", "{e:?}");
    }
    // the filter's bounds, before the capacity: over 128 URIs, and a URI over 4096 bytes
    let c = claim("agent:l", "l1");
    let e = refused(w.listen(c.clone(), &views("v", MAX_LISTEN_URIS + 1)).await).await;
    assert_eq!(e, triple(FILTER_COUNT));
    let long = format!(
        "rdy://view/{}",
        "x".repeat(MAX_LISTEN_URI_BYTES + 1 - "rdy://view/".len())
    );
    assert_eq!(long.len(), MAX_LISTEN_URI_BYTES + 1);
    let e = refused(w.listen(c.clone(), &[long]).await).await;
    assert_eq!(e, triple(FILTER_URI));
    assert_eq!(w.mcp.listen_streams(), 0, "no refusal holds a listen");
    assert!(w.hub.targets().is_empty(), "no refusal reaches the hub");
    // both bounds themselves are accepted
    let edge = format!(
        "rdy://view/{}",
        "x".repeat(MAX_LISTEN_URI_BYTES - "rdy://view/".len())
    );
    let mut most = views("v", MAX_LISTEN_URIS - 1);
    most.push(edge.clone());
    let (first, accepted, _) = w.listening(c.clone(), &most).await;
    assert_eq!(accepted.len(), MAX_LISTEN_URIS);
    assert!(accepted.contains(&edge));
    // 32 listens per daemon, of any claims; the 33rd is refused before its acknowledgement
    let mut open = vec![first];
    for n in 1..MAX_LISTENS {
        let (o, _, _) = w
            .listening(claim("agent:l", &format!("l{n}")), &views("p", 1))
            .await;
        open.push(o);
    }
    assert_eq!(w.mcp.listen_streams(), MAX_LISTENS);
    let e = refused(w.listen(claim("agent:z", "z1"), &views("p", 1)).await).await;
    assert_eq!(e, triple(LISTENS_FULL));
    assert_eq!(
        w.hub.targets().len(),
        MAX_LISTENS,
        "the refused one never listened"
    );
    // claim first, even when the capacity is full
    let e = refused(w.listen(json!({}), &views("p", 1)).await).await;
    assert_eq!(e.0, "E_NO_SESSION");
    // one listen goes: its capacity comes back, and the next listen is acknowledged
    drop(open.pop());
    w.until("a cancelled listen frees its capacity", |w| {
        w.mcp.listen_streams() == MAX_LISTENS - 1 && w.hub.targets().len() == MAX_LISTENS - 1
    })
    .await;
    let (again, _, _) = w.listening(claim("agent:z", "z1"), &views("p", 1)).await;
    assert_eq!(w.mcp.listen_streams(), MAX_LISTENS);
    drop(again);
    drop(open);
    w.until("every listen is released", |w| {
        w.mcp.listen_streams() == 0 && w.hub.targets().is_empty()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht4_same_claim_can_hold_two_independent_listens() {
    let w = Fixture::new();
    let c = claim("agent:t", "t1");
    let (mut a, _, sid_a) = w.listening(c.clone(), &["rdy://project".into()]).await;
    let (mut b, _, sid_b) = w.listening(c.clone(), &["rdy://project".into()]).await;
    assert_ne!(sid_a, sid_b, "each listen has its own registration");
    let t = w.hub.targets();
    assert_eq!(t.len(), 2, "the second listen replaced nothing: {t:?}");
    for t in &t {
        assert_eq!(
            (t.actor.as_deref(), t.run.as_deref()),
            (Some("agent:t"), Some("t1"))
        );
        assert!(t.uris.contains("rdy://project"));
    }
    // each stream carries its own updates, under its own subscription id
    w.hub.notify(sid_a, "rdy://project");
    w.hub.notify(sid_b, "rdy://project");
    let (ua, ida) = updated(&mut a).await;
    let (ub, idb) = updated(&mut b).await;
    assert_eq!(
        (ua.as_str(), ub.as_str()),
        ("rdy://project", "rdy://project")
    );
    assert_ne!(ida, idb, "two subscriptions");
    // the first goes; the second is untouched and still hears
    drop(a);
    w.until("the first listen ends", |w| {
        w.hub.targets().iter().all(|t| t.sid != sid_a)
    })
    .await;
    assert!(w.hub.targets().iter().any(|t| t.sid == sid_b));
    w.hub.notify(sid_b, "rdy://project");
    assert_eq!(updated(&mut b).await.1, idb);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht4_listen_filters_identity_and_resource_uri() {
    let w = Fixture::new();
    // discovery needs no claim, on every revision: the closed set a listen filters against
    for revision in [MODERN, OLDER[0], OLDER[1]] {
        let meta = if revision == MODERN {
            modern(json!({}))
        } else {
            json!({})
        };
        let list = w
            .reply(revision, "resources/list", json!({"_meta": meta}))
            .await;
        let uris: Vec<&str> = list["result"]["resources"]
            .as_array()
            .unwrap_or_else(|| panic!("{revision}: {list}"))
            .iter()
            .map(|r| r["uri"].as_str().unwrap())
            .collect();
        assert_eq!(
            uris,
            ["rdy://project", "rdy://pack", "rdy://inbox"],
            "{revision}"
        );
        let list = w
            .reply(revision, "resources/templates/list", json!({"_meta": meta}))
            .await;
        let templates: Vec<&str> = list["result"]["resourceTemplates"]
            .as_array()
            .unwrap_or_else(|| panic!("{revision}: {list}"))
            .iter()
            .map(|r| r["uriTemplate"].as_str().unwrap())
            .collect();
        assert_eq!(
            templates,
            ["rdy://doc/{id}", "rdy://view/{name}", "rdy://proposal/{id}"],
            "{revision}"
        );
    }
    let asked: Vec<String> = [
        "rdy://inbox",
        "rdy://project",
        "file:///etc",
        "rdy://doc/no/such",
        "rdy://nope",
    ]
    .map(String::from)
    .to_vec();
    let (mut a, accepted, sid_a) = w.listening(claim("agent:a", "a1"), &asked).await;
    assert_eq!(
        accepted,
        ["rdy://inbox", "rdy://project"],
        "the known URIs only"
    );
    let (mut b, accepted_b, sid_b) = w
        .listening(claim("agent:a", "a2"), &["rdy://inbox".into()])
        .await;
    assert_eq!(accepted_b, ["rdy://inbox"]);
    // each listen is its claim, with its own accepted URIs
    let t = w.hub.targets();
    let of = |sid: u64| t.iter().find(|t| t.sid == sid).unwrap();
    assert_eq!(
        (of(sid_a).actor.as_deref(), of(sid_a).run.as_deref()),
        (Some("agent:a"), Some("a1"))
    );
    assert_eq!(
        of(sid_a)
            .uris
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["rdy://inbox", "rdy://project"]
    );
    assert_eq!(
        (of(sid_b).actor.as_deref(), of(sid_b).run.as_deref()),
        (Some("agent:a"), Some("a2"))
    );
    assert_eq!(
        of(sid_b)
            .uris
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["rdy://inbox"]
    );
    // the writer's inbox routing sees each run under its own identity
    assert_eq!(
        w.hub.subscribers("rdy://inbox"),
        [
            ("agent:a".to_string(), Some("a1".to_string())),
            ("agent:a".to_string(), Some("a2".to_string()))
        ]
    );
    assert_eq!(
        w.hub.subscribers("rdy://project"),
        [("agent:a".to_string(), Some("a1".to_string()))],
        "a2 never asked for the project"
    );
    // a URI outside a stream's filter never reaches it
    w.hub.notify(sid_a, "rdy://pack");
    w.hub.notify(sid_a, "rdy://project");
    assert_eq!(updated(&mut a).await.0, "rdy://project");
    w.hub.notify(sid_b, "rdy://project");
    w.hub.notify(sid_b, "rdy://inbox");
    assert_eq!(updated(&mut b).await.0, "rdy://inbox");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht4_slow_stream_has_a_bounded_queue() {
    let w = Fixture::new();
    let uris = views("q", MAX_LISTEN_URIS);
    // `slow` never reads its stream; `quick` does
    let (slow, _, sid_s) = w.listening(claim("agent:s", "s1"), &uris).await;
    let (mut quick, _, sid_q) = w
        .listening(claim("agent:s", "s2"), &["rdy://project".into()])
        .await;
    // one URI over and over is coalesced while it waits: the stream stays open
    for _ in 0..1000 {
        w.hub.notify(sid_s, &uris[0]);
        let q = w.hub.queued(sid_s).expect("the slow stream is open");
        assert!(q <= 1, "repeats of one URI coalesce: {q}");
    }
    // distinct URIs fill the queue to its bound, and the next one closes that stream only
    for uri in &uris[1..] {
        w.hub.notify(sid_s, uri);
        if let Some(q) = w.hub.queued(sid_s) {
            assert!(q <= MAX_QUEUED, "{q} queued");
        }
    }
    w.until("the overflowing stream closes", |w| {
        w.hub.targets().iter().all(|t| t.sid != sid_s) && w.mcp.listen_streams() == 1
    })
    .await;
    // the other stream still hears
    w.hub.notify(sid_q, "rdy://project");
    assert_eq!(updated(&mut quick).await.0, "rdy://project");
    // the closed stream ends for its client: what reached rmcp's bounded channel, then the end
    let mut slow = slow;
    let mut updates = 0;
    let last = loop {
        let m = guard(slow.next())
            .await
            .expect("the stream ends with a result");
        if m["method"] == "notifications/resources/updated" {
            updates += 1;
            continue;
        }
        break m;
    };
    assert!(last.get("result").is_some(), "{last}");
    assert!(guard(slow.next()).await.is_none(), "the stream ends");
    assert!(
        updates <= 17,
        "{updates} updates passed the bounded channel"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht4_cancel_releases_only_its_stream() {
    let w = Fixture::new();
    let c = claim("agent:c", "c1");
    let key = ("agent:c".to_string(), Some("c1".to_string()));
    let mut open = Vec::new();
    for _ in 0..3 {
        open.push(w.listening(c.clone(), &["rdy://project".into()]).await);
    }
    assert_eq!(w.mcp.listen_streams(), 3);
    // the listens hold no turn of their claim: its calls run beside them
    assert_eq!(w.mcp.queued(&key), 0, "a listen waits in no queue");
    let ov = w
        .reply(
            OLDER[1],
            "tools/call",
            json!({"name": "rdy_overview", "arguments": {}, "_meta": c}),
        )
        .await;
    let v = done(&ov["result"]).expect("the call runs beside its claim's listens");
    assert_eq!(v["session"]["actor"], "agent:c");
    // the middle one goes
    let (gone, _, sid_gone) = open.remove(1);
    drop(gone);
    w.until("only the cancelled listen ends", |w| {
        w.mcp.listen_streams() == 2 && w.hub.targets().iter().all(|t| t.sid != sid_gone)
    })
    .await;
    assert_eq!(w.hub.targets().len(), 2);
    for (o, _, sid) in open.iter_mut() {
        w.hub.notify(*sid, "rdy://project");
        assert_eq!(updated(o).await.0, "rdy://project");
    }
    drop(open);
    w.until("the rest end", |w| {
        w.mcp.listen_streams() == 0 && w.hub.targets().is_empty()
    })
    .await;
}

// ---------------------------------------------------------------- older revisions

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht4_old_http_revisions_have_no_push_or_callback_elicitation() {
    let (w, r) = approval(NotifyHub::new());
    let item = w.card(&r);
    let before = w.seq();
    let c = claim("agent:relay", "r1");
    for revision in OLDER {
        // no listen and no legacy subscription: rmcp's protocol refusal, and no hidden sink
        let p = json!({"_meta": c, "notifications": {"resourceSubscriptions": ["rdy://project"]}});
        let reply = w.reply(revision, "subscriptions/listen", p).await;
        assert_eq!(reply["error"]["code"], -32601, "{revision}: {reply}");
        for method in ["resources/subscribe", "resources/unsubscribe"] {
            let p = json!({"uri": "rdy://project", "_meta": c});
            let reply = w.reply(revision, method, p).await;
            assert_eq!(
                reply["error"]["code"], -32601,
                "{revision} {method}: {reply}"
            );
            assert_eq!(reply["error"]["message"], method, "rmcp's text");
            assert!(reply["error"].get("data").is_none(), "{reply}");
        }
        assert!(w.hub.targets().is_empty(), "{revision}: no push channel");
        w.until("rmcp's refusal releases the listen's reservation", |w| {
            w.mcp.listen_streams() == 0
        })
        .await;
        // a malformed elicited call keeps its parse error
        let mut mixed = ask(&item);
        mixed["option"] = json!("reject");
        let out = w.answer(revision, c.clone(), mixed, json!({})).await;
        refused_with(&out, MIXED);
        // a valid one gets no callback: the existing refusal, and no card waits
        let out = w.answer(revision, c.clone(), ask(&item), json!({})).await;
        refused_with(&out, UNAVAILABLE);
        assert_eq!(w.hub.elicitation().len(), 0, "{revision}");
    }
    assert_eq!(w.seq(), before, "nothing is written");
    assert!(w.stored(&item.r#ref).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht4_old_http_cannot_resume_modern_elicitation() {
    let (w, r) = approval(NotifyHub::new());
    let item = w.card(&r);
    let before = w.seq();
    let c = claim("agent:relay", "r1");
    let h = handle_of(&w, c.clone(), &item).await;
    assert_eq!(w.hub.elicitation().len(), 1);
    for revision in OLDER {
        // a malformed retry keeps its parse error, ahead of the revision
        let mut mixed = ask(&item);
        mixed["option"] = json!("reject");
        let out = w
            .answer(revision, c.clone(), mixed, retry(&h, choice("reject")))
            .await;
        refused_with(&out, MIXED);
        // the valid handle of the same claim: refused before it is looked up or used
        let out = w
            .answer(revision, c.clone(), ask(&item), retry(&h, choice("reject")))
            .await;
        refused_with(&out, UNAVAILABLE);
        assert_eq!(
            w.hub.elicitation().len(),
            1,
            "{revision}: the card still waits"
        );
        assert_eq!(w.seq(), before, "{revision}: no answer is recorded");
        assert!(w.stored(&item.r#ref).is_none());
    }
    // the modern retry still answers, once
    let out = w
        .answer(MODERN, c.clone(), ask(&item), retry(&h, choice("reject")))
        .await;
    let v = done(&out).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(v["act"], "reject_proposal", "{v}");
    assert_eq!(w.hub.elicitation().len(), 0);
    let a = w.stored(&item.r#ref).expect("the answer");
    assert_eq!(a.evidence_kind, Some(EvidenceKind::Elicited));
    let after = w.seq();
    let out = w
        .answer(MODERN, c, ask(&item), retry(&h, choice("reject")))
        .await;
    refused_with(&out, GONE);
    assert_eq!(w.seq(), after);
}

// ---------------------------------------------------------------- modern elicitation

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht4_elicitation_resumes_across_requests_for_same_claim() {
    let (w, r) = approval(NotifyHub::new());
    let item = w.card(&r);
    let c = claim("agent:relay", "r1");
    // the first request ends with its card waiting; its client goes before it reads the reply
    let mut args = ask(&item);
    rdy_qa::mcp::supply_kungfu("rdy_answer", &mut args);
    let (_, o) = w
        .open(
            MODERN,
            "tools/call",
            json!({"name": "rdy_answer", "arguments": args, "_meta": modern(c.clone())}),
        )
        .await;
    drop(o);
    assert_eq!(
        w.hub.elicitation().len(),
        1,
        "a lost response keeps its card"
    );
    // a second card, read whole, is resumed by another request of the same claim
    let h = handle_of(&w, c.clone(), &item).await;
    assert_eq!(w.hub.elicitation().len(), 2);
    let out = w
        .answer(MODERN, c.clone(), ask(&item), retry(&h, choice("reject")))
        .await;
    let v = done(&out).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(v["act"], "reject_proposal", "{v}");
    assert_eq!(v["assurance"], "local", "{v}");
    let a = w.stored(&item.r#ref).expect("the answer");
    assert_eq!(a.evidence_kind, Some(EvidenceKind::Elicited));
    assert!(a.relayed_by.is_none(), "{a:#?}");
    assert_eq!(
        w.hub.elicitation().len(),
        1,
        "only the resumed card is used"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht4_elicitation_rejects_expired_reused_or_changed_state() {
    let base = Instant::now();
    let at = Arc::new(Mutex::new(base));
    let clock = Arc::clone(&at);
    let hub =
        NotifyHub::with_elicitation(Cards::with_clock(Arc::new(move || *clock.lock().unwrap())));
    let (w, r) = approval(hub);
    let item = w.card(&r);
    let before = w.seq();
    let c = claim("agent:relay", "r1");
    let reject = choice("reject");
    // at 600 seconds a card is gone
    let h = handle_of(&w, c.clone(), &item).await;
    *at.lock().unwrap() = base + EXPIRY;
    let out = w
        .answer(MODERN, c.clone(), ask(&item), retry(&h, reject.clone()))
        .await;
    refused_with(&out, GONE);
    assert_eq!(w.seq(), before);
    // at 599 seconds it still waits; changed state never consumes it
    let start = base + EXPIRY;
    let h = handle_of(&w, c.clone(), &item).await;
    *at.lock().unwrap() = start + EXPIRY - Duration::from_secs(1);
    for (who, args, extra) in [
        // another actor, and another run of the same actor
        (
            claim("agent:other", "r1"),
            ask(&item),
            retry(&h, reject.clone()),
        ),
        (
            claim("agent:relay", "r2"),
            ask(&item),
            retry(&h, reject.clone()),
        ),
        // another card hash, and a tampered handle
        (
            c.clone(),
            json!({"ref": item.r#ref, "card_hash": "0".repeat(64), "elicit": true}),
            retry(&h, reject.clone()),
        ),
        (
            c.clone(),
            ask(&item),
            retry(
                &format!("{}{}", if h.starts_with('0') { "1" } else { "0" }, &h[1..]),
                reject.clone(),
            ),
        ),
    ] {
        let out = w.answer(MODERN, who, args, extra).await;
        refused_with(&out, GONE);
    }
    assert_eq!(w.hub.elicitation().len(), 1, "the card still waits");
    assert_eq!(w.seq(), before, "no unwanted write");
    // the same claim uses it once; a reuse is refused
    let out = w
        .answer(MODERN, c.clone(), ask(&item), retry(&h, reject.clone()))
        .await;
    assert_eq!(done(&out).unwrap()["act"], "reject_proposal");
    let after = w.seq();
    let out = w
        .answer(MODERN, c.clone(), ask(&item), retry(&h, reject.clone()))
        .await;
    refused_with(&out, GONE);
    assert_eq!(w.seq(), after, "a reuse answers nothing twice");
}
