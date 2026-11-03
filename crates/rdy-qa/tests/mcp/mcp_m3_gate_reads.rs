//! mg9 (D-6M0AF, D-WPE4R, D-3HRH9): the M3 gate's read contracts over a real rmcp server. An
//! elicited Later is an answer of the caller's run (its chat session): the same caller's
//! `rdy_pending` and the answer reply's `next` must not give the card back at once. The built
//! relayed Later, which names its chat session in `session_ref`, is the control. Known-issues
//! row 139 is this contract; the test reproduces it through the real caller and run.
use rdy_engine::engine::{Engine, Local};
use rdy_engine::pending::{ItemKind, PendingQuery};
use rdy_mcp::notify::NotifyHub;
use rdy_mcp::server::{RdyServer, serve};
use rdy_qa::intake::{PERSON, batch_project};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

const V_NOV: &str = "2025-11-25";

/// A served engine; each client is a fresh rmcp server session on a duplex stream.
struct World {
    _dir: tempfile::TempDir,
    engine: Arc<dyn Engine>,
    hub: Arc<NotifyHub>,
}

impl World {
    fn stream(&self, sid: u64) -> DuplexStream {
        let (client, end) = tokio::io::duplex(1 << 20);
        let (r, w) = tokio::io::split(end);
        let server = RdyServer::new(Arc::clone(&self.engine), Some(sid), Arc::clone(&self.hub));
        tokio::spawn(async move {
            let _ = serve(server, r, w).await;
        });
        client
    }

    fn queue(&self, session_ref: Option<&str>) -> Vec<rdy_engine::pending::Item> {
        self.engine
            .read()
            .pending(&PendingQuery {
                person: PERSON.into(),
                session_ref: session_ref.map(str::to_string),
                max_tokens: rdy_engine::pending::CARD_MAX_TOKENS,
            })
            .unwrap()
            .items
    }
}

/// One JSON-RPC connection of a 2025-11-25 client with the form capability, as agent
/// `agent:relay` in run `r1`.
struct Client {
    r: BufReader<ReadHalf<DuplexStream>>,
    w: WriteHalf<DuplexStream>,
    next: u64,
}

impl Client {
    async fn new(w: &World, sid: u64) -> Client {
        let (r, wr) = tokio::io::split(w.stream(sid));
        let mut c = Client {
            r: BufReader::new(r),
            w: wr,
            next: 1,
        };
        c.send(
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": V_NOV, "capabilities": {"elicitation": {"form": {}}},
            "clientInfo": {"name": "mg9", "version": "0"}}}),
        )
        .await;
        let init = c.recv().await;
        assert_eq!(init["result"]["protocolVersion"], V_NOV, "{init}");
        c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        c
    }

    async fn send(&mut self, v: Value) {
        self.w.write_all(format!("{v}\n").as_bytes()).await.unwrap();
        self.w.flush().await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.r.read_line(&mut line).await.unwrap();
        assert!(n > 0, "the session closed");
        serde_json::from_str(&line).unwrap()
    }

    async fn start(&mut self, name: &str, args: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        let meta = json!({"rdy/actor": "agent:relay", "rdy/run": "r1"});
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": args, "_meta": meta}}))
            .await;
        id
    }

    async fn tool(&mut self, name: &str, args: Value) -> Value {
        let id = self.start(name, args).await;
        let msg = self.recv().await;
        assert_eq!(msg["id"], id, "{msg}");
        done(&msg["result"]).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    /// `rdy_answer` with `elicit: true` on `item`; the person picks `option` on the form.
    async fn elicited(&mut self, item: &rdy_engine::pending::Item, option: &str) -> Value {
        let id = self
            .start(
                "rdy_answer",
                json!({"ref": item.r#ref, "card_hash": item.card_hash, "elicit": true}),
            )
            .await;
        let req = self.recv().await;
        assert_eq!(req["method"], "elicitation/create", "{req}");
        self.send(json!({"jsonrpc": "2.0", "id": req["id"],
            "result": {"action": "accept", "content": {"choice": option}}}))
            .await;
        let msg = self.recv().await;
        assert_eq!(msg["id"], id, "{msg}");
        done(&msg["result"]).unwrap_or_else(|e| panic!("elicited {option}: {e}"))
    }
}

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

/// The ref an answer reply's `next` names, if any.
fn next_of(v: &Value) -> Option<String> {
    v["next"]
        .as_str()
        .or_else(|| v["next"]["ref"].as_str())
        .map(str::to_string)
}

fn refs(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap_or_else(|| panic!("rdy_pending items: {v:#}"))
        .iter()
        .map(|i| i["ref"].as_str().unwrap().to_string())
        .collect()
}

/// `batch_project` served, with its person `human:qa`.
fn world() -> World {
    let (dir, r, _) = batch_project();
    std::fs::write(dir.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    World {
        _dir: dir,
        engine: Arc::new(Local::new(r)),
        hub: NotifyHub::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mg9_elicited_later_is_honoured_by_pending_and_next() {
    let w = world();
    let confirms: Vec<_> = w
        .queue(None)
        .into_iter()
        .filter(|i| i.kind == ItemKind::Confirm)
        .collect();
    assert!(confirms.len() >= 2, "{confirms:#?}");
    let (relayed, elicited) = (&confirms[0], &confirms[1]);

    // control: a relayed Later names its chat session; that session's queue and the reply's
    // next skip the card, and another session still sees it
    let mut c = Client::new(&w, 1).await;
    let v = c
        .tool(
            "rdy_answer",
            json!({"ref": relayed.r#ref, "option": "later", "card_hash": relayed.card_hash,
                   "evidence": "Later.", "question": "Shall I ask again later?",
                   "session_ref": "chat-7"}),
        )
        .await;
    assert_eq!(v["act"], "later", "{v:#}");
    assert_ne!(
        next_of(&v).as_deref(),
        Some(relayed.r#ref.as_str()),
        "{v:#}"
    );
    let in_chat = c
        .tool("rdy_pending", json!({"session_ref": "chat-7"}))
        .await;
    assert!(!refs(&in_chat).contains(&relayed.r#ref), "{in_chat:#}");
    let elsewhere = c
        .tool("rdy_pending", json!({"session_ref": "chat-8"}))
        .await;
    assert!(refs(&elsewhere).contains(&relayed.r#ref), "{elsewhere:#}");

    // the contract: an elicited Later is the caller's run; neither its reply's next nor the same
    // caller's rdy_pending (no session_ref) gives the card back at once
    let v = c.elicited(elicited, "later").await;
    assert_eq!(v["act"], "later", "{v:#}");
    assert_eq!(v["assurance"], "local", "{v:#}");
    let next = next_of(&v);
    let mine = c.tool("rdy_pending", json!({})).await;
    let back_next = next.as_deref() == Some(elicited.r#ref.as_str());
    let back_queue = refs(&mine).contains(&elicited.r#ref);
    assert!(
        !back_next && !back_queue,
        "KNOWN-ISSUES row 139 reproduces: the elicited Later of {} (run r1) comes back at once: \
         next = {next:?} (back: {back_next}); the same caller's rdy_pending holds it: \
         {back_queue}; reply {v:#}",
        elicited.r#ref
    );
}
