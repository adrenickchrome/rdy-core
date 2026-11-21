//! vxs7 (D-76Y9H, D-5VGAS, D-RY7B5, D-ZDM0G, D-5BM47, D-0ZPAP): `like` and `like_text` through
//! `rdy_search` over stdio and HTTP and through the daemon's `search` op. One engine serves the
//! three, in process; each person lives in an isolated home. The three give the same results and
//! the same eight refusals, in the same order, with the same codes, messages and fixes.
use rdy_engine::engine::{Engine, Local};
use rdy_engine::instructions::{
    self as instructions, Caller, Contribution, Layer, LoadRequest, PersonHost,
};
use rdy_engine::instructions_config::{PathEnv, PersonSource, Platform, resolve_person};
use rdy_engine::vectors::WeightScope;
use rdy_engine::{Rdy, RdyError, Scope, Session};
use rdy_mcp::http::HttpMcp;
use rdy_mcp::notify::NotifyHub;
use rdy_mcp::server::{RdyServer, serve};
use rdy_qa::fixtures::*;
use rdy_qa::mcp::supply_kungfu;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

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

const ACTOR: &str = "agent:v";
const RUN: &str = "v1";

fn claim() -> Value {
    json!({"rdy/actor": ACTOR, "rdy/run": RUN})
}

/// One stdio connection to the world's engine, in process.
struct Stdio {
    r: BufReader<ReadHalf<DuplexStream>>,
    w: WriteHalf<DuplexStream>,
    next: u64,
}

impl Stdio {
    async fn open(engine: &Arc<dyn Engine>) -> Stdio {
        let (client, end) = tokio::io::duplex(1 << 22);
        let (r, w) = tokio::io::split(end);
        let server = RdyServer::new(Arc::clone(engine), None, NotifyHub::new());
        tokio::spawn(async move {
            let _ = serve(server, r, w).await;
        });
        let (r, w) = tokio::io::split(client);
        let mut c = Stdio {
            r: BufReader::new(r),
            w,
            next: 0,
        };
        c.send(
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "vxs7", "version": "0"}}}),
        )
        .await;
        c.recv(json!(0)).await;
        c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        c
    }

    async fn send(&mut self, v: Value) {
        self.w.write_all(format!("{v}\n").as_bytes()).await.unwrap();
        self.w.flush().await.unwrap();
    }

    async fn recv(&mut self, id: Value) -> Value {
        loop {
            let mut line = String::new();
            assert!(self.r.read_line(&mut line).await.unwrap() > 0, "closed");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    }

    async fn call(&mut self, tool: &str, args: Value) -> Reply {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": args, "_meta": claim()}}))
            .await;
        decode(&self.recv(json!(id)).await)
    }
}

/// A tool reply: the result, or the refusal's code, message and fix.
#[derive(Clone, Debug, PartialEq)]
enum Reply {
    Ok(Value),
    No(String, String, String),
}

impl Reply {
    fn ok(self) -> Value {
        match self {
            Reply::Ok(v) => v,
            Reply::No(c, m, f) => panic!("refused: {c}: {m} ({f})"),
        }
    }

    fn refused(c: &str, m: &str, f: &str) -> Reply {
        Reply::No(c.into(), m.into(), f.into())
    }

    fn of_error(e: &RdyError) -> Reply {
        Reply::No(e.code.to_string(), e.message.clone(), e.fix.clone())
    }
}

fn decode(v: &Value) -> Reply {
    let text = v["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no tool reply in {v}"));
    let body: Value = serde_json::from_str(text).unwrap();
    if v["result"]["isError"] == json!(true) {
        let s = |k: &str| body[k].as_str().unwrap_or_default().to_string();
        Reply::No(s("code"), s("message"), s("fix"))
    } else {
        Reply::Ok(body)
    }
}

/// One project served three ways from one engine: stdio, HTTP and the daemon op.
struct World {
    _dir: tempfile::TempDir,
    _home: tempfile::TempDir,
    engine: Arc<dyn Engine>,
    http: Arc<HttpMcp>,
    stdio: Stdio,
    next: AtomicU64,
    session: Session,
    cwd: PathBuf,
}

impl World {
    async fn new<T>(seed: impl FnOnce(&mut Rdy) -> T) -> (World, T) {
        let (dir, mut r) = project();
        let t = seed(&mut r);
        r.close().unwrap();
        let home = tempfile::tempdir().unwrap();
        let engine: Arc<dyn Engine> = Arc::new(
            Local::open(dir.path())
                .unwrap()
                .with_person_host(Arc::new(Home(home.path().to_path_buf()))),
        );
        let http = Arc::new(HttpMcp::new(Arc::clone(&engine), NotifyHub::new()));
        let stdio = Stdio::open(&engine).await;
        let cwd = dir.path().to_path_buf();
        let w = World {
            _dir: dir,
            _home: home,
            engine,
            http,
            stdio,
            next: AtomicU64::new(0),
            session: as_agent(ACTOR, RUN),
            cwd,
        };
        (w, t)
    }

    async fn stdio(&mut self, args: Value) -> Reply {
        let mut args = args;
        supply_kungfu("rdy_search", &mut args);
        self.stdio.call("rdy_search", args).await
    }

    async fn http(&self, args: Value) -> Reply {
        let mut args = args;
        supply_kungfu("rdy_search", &mut args);
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let body = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": "rdy_search", "arguments": args, "_meta": claim()}});
        let p = self
            .http
            .post(
                &[
                    ("host", "localhost"),
                    ("accept", "application/json, text/event-stream"),
                    ("content-type", "application/json"),
                    ("mcp-protocol-version", "2025-11-25"),
                ],
                body.to_string().into_bytes(),
            )
            .await;
        assert_eq!(p.status, 200, "{p:?}");
        decode(&p.reply())
    }

    fn daemon(&self, args: Value) -> Reply {
        let cx = rdy_daemon::OpContext {
            engine: &*self.engine,
            session: &self.session,
            human: "human:t",
            cwd: &self.cwd,
            agent: &rdy_engine::signing_crypto::UnavailableAgent,
        };
        match rdy_daemon::dispatch(&cx, "search", args) {
            Ok(v) => Reply::Ok(v),
            Err(e) => Reply::of_error(&e),
        }
    }

    /// The reply of each of the three, which must agree; the agreed reply.
    async fn all(&mut self, args: Value) -> Reply {
        let s = self.stdio(args.clone()).await;
        let h = self.http(args.clone()).await;
        let d = self.daemon(args.clone());
        assert_eq!(s, h, "stdio and HTTP: {args}");
        assert_eq!(s, d, "MCP and the daemon: {args}");
        s
    }
}

fn ids(res: &Value) -> Vec<String> {
    res["matches"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| {
            std::iter::once(m["id"].as_str().unwrap().to_string()).chain(
                m["also"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| a["id"].as_str().unwrap().to_string()),
            )
        })
        .collect()
}

/// Whether any object in `v` has member `key`.
fn has(v: &Value, key: &str) -> bool {
    match v {
        Value::Object(o) => o.contains_key(key) || o.values().any(|x| has(x, key)),
        Value::Array(a) => a.iter().any(|x| has(x, key)),
        _ => false,
    }
}

/// The scopes and ids of [`reef`].
struct Reef {
    ids: BTreeMap<String, String>,
    /// The first landing, tagged v1.0.0; the head, tagged v1.1.0.
    past: String,
    head: String,
    /// A proposal version, and its short form.
    proposal: String,
    short: String,
}

/// Doc `reef`: a term (alias Kardex), the source claim `$src` and claims that share its words, its
/// link or both, then a later landing and an open proposal.
fn reef(r: &mut Rdy) -> Reef {
    let w = write(
        r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "reef", "title": "Reef", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Shore"}},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "General ledger", "aliases": ["Kardex"]}, "body": "Holds balances."},
            {"op": "create", "type": "claim", "id": "$src", "parent": "$s", "body": "Kelp forests shelter otters at dawn.", "links": {"describes": [{"target": "$t"}]}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "Kelp forests shelter otters.\nMany more words live here in a long second line."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "Kelp forests shelter otters.\nShort."},
            {"op": "create", "type": "claim", "id": "$z", "parent": "$s", "body": "Zebra stripes.", "links": {"describes": [{"target": "$t"}]}},
            {"op": "create", "type": "claim", "id": "$y", "parent": "$s", "body": "Desert sand.", "links": {"describes": [{"target": "$t"}]}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Kelp grows in cold water."}
        ]),
    )
    .unwrap();
    land(r, "work").unwrap();
    let past = r.read().canon_head().unwrap().to_string();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(
        r,
        "more",
        json!([{"op": "create", "type": "claim", "parent": w.created["$s"], "body": "Kelp forests at noon."}]),
    )
    .unwrap();
    land(r, "more").unwrap();
    let head = r.read().canon_head().unwrap().to_string();
    r.tag(&approver(), "v1.1.0", None, None).unwrap();
    write(
        r,
        "p",
        json!([{"op": "create", "type": "claim", "parent": w.created["$s"], "body": "Kelp forests at dusk."}]),
    )
    .unwrap();
    let p = r.propose(&writer(), "p", "p", Some("Add a claim")).unwrap();
    let pref = r.resolve_proposal(&p.id).unwrap();
    let proposal = format!("proposal:{pref}");
    let Scope::Proposal(v) = r.read().resolve_scope(&proposal).unwrap() else {
        panic!("a proposal version");
    };
    Reef {
        ids: w.created,
        past,
        head,
        proposal,
        short: v.short(),
    }
}

/// `n` bytes of text with one known word, `kelp`, and two-byte characters.
fn sized(n: usize) -> String {
    let mut s = "kelp ".to_string();
    while s.len() + 2 <= n {
        s.push('é');
    }
    while s.len() < n {
        s.push('a');
    }
    assert_eq!(s.len(), n);
    s
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vxs7_eight_refusals_pin_codes_messages_fixes_and_precedence() {
    let (mut w, f) = World::new(reef).await;
    let src = f.ids["$src"].clone();
    let one = Reply::refused(
        "E_BAD_INPUT",
        "give exactly one of query, like or like_text",
        "send one of query, like or like_text",
    );
    let regex = Reply::refused("E_BAD_INPUT", "regex applies to query only", "drop regex");
    let sort = Reply::refused(
        "E_BAD_INPUT",
        "sort applies to query only",
        "drop sort; like and like_text hits come in score order",
    );
    let literal = Reply::refused(
        "E_BAD_INPUT",
        "sort relevance needs a literal query",
        "drop regex, or drop sort",
    );
    let long = Reply::refused(
        "E_LIMIT",
        "like_text is over 16384 bytes",
        "send a shorter like_text",
    );
    let unknown_word = Reply::refused(
        "E_BAD_INPUT",
        "like_text has no word that a block in this scope holds",
        "use words that the project uses, or search with query",
    );
    let unknown_block = Reply::refused(
        "E_UNKNOWN_BLOCK",
        "no block c-ZZZZZ",
        "check the id with rdy_read",
    );
    let at_past = |p: &str| {
        Reply::refused(
            "E_BAD_INPUT",
            &format!("{p} ranks the current state only, not landing:{}", f.past),
            "ask for canon or a view; ranking over time is not part of rdy core",
        )
    };
    let at_proposal = |p: &str| {
        Reply::refused(
            "E_BAD_INPUT",
            &format!("{p} ranks canon and views only, not proposal:{}", f.short),
            "ask for the proposal's view, or canon",
        )
    };
    let past = "tag:v1.0.0";
    let cases: Vec<(Value, Reply)> = vec![
        // 1: zero, two or three sources; a blank value is a supplied value
        (json!({}), one.clone()),
        (json!({"scope": "canon"}), one.clone()),
        (json!({"query": "kelp", "like": src}), one.clone()),
        (json!({"query": "kelp", "like_text": "kelp"}), one.clone()),
        (json!({"like": src, "like_text": "kelp"}), one.clone()),
        (
            json!({"query": "kelp", "like": src, "like_text": "kelp"}),
            one.clone(),
        ),
        (json!({"query": "", "like_text": ""}), one.clone()),
        (
            json!({"query": "kelp", "like": src, "regex": true, "sort": "relevance"}),
            one.clone(),
        ),
        // 2: regex with a like mode, before the sort refusal
        (json!({"like": src, "regex": true}), regex.clone()),
        (json!({"like_text": "kelp", "regex": true}), regex.clone()),
        (
            json!({"like": src, "regex": true, "sort": "relevance"}),
            regex.clone(),
        ),
        (
            json!({"like_text": sized(16_385), "regex": true}),
            regex.clone(),
        ),
        // 3: any sort with a like mode, valid or not
        (json!({"like": src, "sort": "relevance"}), sort.clone()),
        (json!({"like": src, "sort": "reading"}), sort.clone()),
        (
            json!({"like_text": "kelp", "sort": "coverage"}),
            sort.clone(),
        ),
        (json!({"like_text": "kelp", "sort": "score"}), sort.clone()),
        (
            json!({"like_text": sized(16_385), "sort": "reading"}),
            sort.clone(),
        ),
        (json!({"like": "c-ZZZZZ", "sort": "reading"}), sort.clone()),
        // 4: relevance needs a literal, before the scope refusal
        (
            json!({"query": "kelp", "regex": true, "sort": "relevance"}),
            literal.clone(),
        ),
        (
            json!({"query": "kelp", "regex": true, "sort": "relevance", "scope": past}),
            literal.clone(),
        ),
        // 5: the like_text limit, in bytes, before the word check and the scope refusal
        (json!({"like_text": sized(16_385)}), long.clone()),
        (
            json!({"like_text": format!("zzqq{}", "é".repeat(8191))}),
            long.clone(),
        ),
        (
            json!({"like_text": sized(16_385), "scope": past}),
            long.clone(),
        ),
        // 6: no word a block holds, before the scope refusal
        (json!({"like_text": "zzqq wwvv"}), unknown_word.clone()),
        (json!({"like_text": ""}), unknown_word.clone()),
        (
            json!({"like_text": "zzqq", "scope": past}),
            unknown_word.clone(),
        ),
        (
            json!({"like_text": "zzqq", "scope": f.proposal}),
            unknown_word.clone(),
        ),
        // 7: an unknown like id, before the scope refusal
        (json!({"like": "c-ZZZZZ"}), unknown_block.clone()),
        (
            json!({"like": "c-ZZZZZ", "scope": past}),
            unknown_block.clone(),
        ),
        (
            json!({"like": "c-ZZZZZ", "scope": f.proposal}),
            unknown_block.clone(),
        ),
        // 8: ranked modes at a past landing and in a proposal version
        (json!({"like": src, "scope": past}), at_past("like")),
        (
            json!({"like": src, "scope": format!("landing:{}", f.past)}),
            at_past("like"),
        ),
        (
            json!({"like_text": "kelp", "scope": past}),
            at_past("like_text"),
        ),
        (
            json!({"query": "kelp", "sort": "relevance", "scope": past}),
            at_past("sort relevance"),
        ),
        (
            json!({"like": src, "scope": f.proposal}),
            at_proposal("like"),
        ),
        (
            json!({"like_text": "kelp", "scope": f.proposal}),
            at_proposal("like_text"),
        ),
        (
            json!({"query": "kelp", "sort": "relevance", "scope": f.proposal}),
            at_proposal("sort relevance"),
        ),
    ];
    for (args, want) in cases {
        let what = args.to_string();
        let got = w.all(args).await;
        assert_eq!(got, want, "{}", &what[..what.len().min(200)]);
    }
    // the limit is inclusive, counted in bytes of multibyte text
    for n in [16_383, 16_384] {
        let res = w
            .all(json!({"like_text": sized(n), "scope": "canon"}))
            .await
            .ok();
        assert_eq!(res["like_text"].as_str().unwrap().len(), n);
    }
    // every sort value still works with a query, and an unknown one names the three
    for s in ["relevance", "reading", "coverage"] {
        w.all(json!({"query": "kelp", "sort": s, "scope": "canon"}))
            .await
            .ok();
    }
    assert_eq!(
        w.all(json!({"query": "kelp", "sort": "score"})).await,
        Reply::refused(
            "E_BAD_INPUT",
            "sort takes coverage, relevance or reading, not score",
            "pass coverage, relevance or reading, or omit sort",
        )
    );
    // an unknown key is refused with the nearest key on each transport
    for args in [
        json!({"liek": src}),
        json!({"like": src, "like_txt": "kelp"}),
    ] {
        for got in [
            w.stdio(args.clone()).await,
            w.http(args.clone()).await,
            w.daemon(args.clone()),
        ] {
            let Reply::No(code, message, fix) = got else {
                panic!("an unknown key is refused: {args}");
            };
            assert_eq!(code, "E_BAD_INPUT", "{args}");
            assert!(
                format!("{message} {fix}").contains("like"),
                "{message} {fix}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vxs7_ranked_echoes_members_and_empty_lines_follow_modes() {
    let (mut w, f) = World::new(reef).await;
    let id = |k: &str| f.ids[k].clone();
    let res = w
        .all(json!({"like": id("$src"), "scope": "canon"}))
        .await
        .ok();
    assert_eq!(res["query"], "");
    assert_eq!(res["like"], id("$src"));
    assert!(res.get("like_text").is_none(), "{res}");
    let m = res["matches"].as_array().unwrap();
    // `$a` and `$b` share their one matching line: one entry, in its best member's place, the
    // shorter `$b` representing it; the member keeps its own score, words and links
    let entry = m
        .iter()
        .find(|e| e["id"] == id("$b"))
        .unwrap_or_else(|| panic!("{res}"));
    let also = entry["also"].as_array().unwrap();
    assert_eq!(also.len(), 1, "{res}");
    assert_eq!(also[0]["id"], id("$a"));
    assert!(entry["score"].as_f64() > also[0]["score"].as_f64(), "{res}");
    assert!(
        also[0]["words"]
            .as_array()
            .unwrap()
            .contains(&json!("kelp"))
    );
    assert_eq!(also[0]["links"], json!([]));
    assert_eq!(entry["lines"][0]["text"], "Kelp forests shelter otters.");
    // the link-only hits have no line, and never collapse
    for k in ["$z", "$y"] {
        let e = m
            .iter()
            .find(|e| e["id"] == id(k))
            .unwrap_or_else(|| panic!("{k}: {res}"));
        assert_eq!(e["lines"], json!([]));
        assert!(e.get("also").is_none(), "{e}");
        assert_eq!(e["links"], json!([id("$t")]));
        assert_eq!(e["words"], json!([]));
    }
    // like_text echoes its text
    let text = w
        .all(json!({"like_text": "the Kardex kelp otters", "scope": "canon"}))
        .await
        .ok();
    assert_eq!(text["query"], "");
    assert_eq!(text["like_text"], "the Kardex kelp otters");
    assert!(text.get("like").is_none());
    for k in ["$z", "$y"] {
        assert!(
            ids(&text).contains(&id(k)),
            "the alias names the term: {text}"
        );
    }
    // ignore_case changes nothing: vector words are lower-cased
    let upper = w
        .all(json!({"like_text": "THE kardex KELP Otters", "ignore_case": true, "scope": "canon"}))
        .await
        .ok();
    assert_eq!(upper["matches"], text["matches"]);
    assert_eq!(upper["total"], text["total"]);
    // relevance members carry their own score and no words or links
    let ranked = w
        .all(json!({"query": "Kelp forests shelter otters", "sort": "relevance", "scope": "canon"}))
        .await
        .ok();
    let rm = &ranked["matches"][0];
    assert!(rm["also"][0]["score"].is_number(), "{ranked}");
    assert!(!has(&ranked, "words") && !has(&ranked, "links"), "{ranked}");
    assert!(ranked.get("like").is_none() && ranked.get("like_text").is_none());
    // the default mode carries none of the ranked members
    let plain = w
        .all(json!({"query": "Kelp forests shelter otters", "scope": "canon"}))
        .await
        .ok();
    for key in ["score", "words", "links", "like", "like_text"] {
        assert!(!has(&plain, key), "{key}: {plain}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vxs7_stdio_http_and_daemon_search_results_agree() {
    let (mut w, f) = World::new(reef).await;
    let id = |k: &str| f.ids[k].clone();
    for args in [
        json!({"query": "Kelp", "scope": "canon"}),
        json!({"query": "Kelp forests", "sort": "relevance", "scope": "canon"}),
        json!({"like": id("$src"), "scope": "canon"}),
        json!({"like": id("$src"), "types": ["claim"], "scope": "canon"}),
        json!({"like_text": "the Kardex kelp otters", "scope": "canon"}),
        json!({"like_text": "kelp otters", "path": "reef", "scope": "tag:v1.1.0"}),
    ] {
        let res = w.all(args.clone()).await.ok();
        assert!(res["total"].as_u64().unwrap() > 0, "{args}: {res}");
    }
    // a cursor from one transport pages on the others
    let first = w
        .all(json!({"like": id("$src"), "scope": "canon", "max_tokens": 100}))
        .await
        .ok();
    let next = first["next"]
        .as_str()
        .unwrap_or_else(|| panic!("{first}"))
        .to_string();
    let second = w
        .all(json!({"like": id("$src"), "scope": "canon", "max_tokens": 100, "page": next}))
        .await
        .ok();
    assert!(!second["matches"].as_array().unwrap().is_empty());
}

/// Doc `a` with 20 claims and doc `b` with one, each holding `walrus pups haul` and its own
/// number, and doc `c` with 40 other blocks, so the shared words are not common.
fn walrus(r: &mut Rdy) -> BTreeMap<String, String> {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "Alpha", "doc_type": "page"}}),
        json!({"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "Beta", "doc_type": "page"}}),
        json!({"op": "create", "type": "doc", "id": "$c", "fields": {"name": "c", "title": "Gamma", "doc_type": "page"}}),
        json!({"op": "create", "type": "claim", "id": "$b0", "parent": "$b", "body": "Walrus pups haul out near rock 100."}),
    ];
    for i in 0..20 {
        ops.push(
            json!({"op": "create", "type": "claim", "id": format!("$a{i}"), "parent": "$a",
            "body": format!("Walrus pups haul out near rock {i}.")}),
        );
    }
    for i in 0..40 {
        ops.push(json!({"op": "create", "type": "prose", "parent": "$c", "body": format!("Filler block {i}.")}));
    }
    let w = write(r, "work", Value::Array(ops)).unwrap();
    land(r, "work").unwrap();
    w.created
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vxs7_each_mode_pages_and_briefs_only_its_own_results() {
    let (mut w, c) = World::new(walrus).await;
    let modes = [
        json!({"query": "Walrus pups"}),
        json!({"query": "Walrus pups", "sort": "relevance"}),
        json!({"like": c["$b0"]}),
        json!({"like_text": "walrus pups haul"}),
    ];
    let mut cursors = vec![];
    for mode in &modes {
        let mut whole = mode.clone();
        whole["scope"] = json!("canon");
        let all = ids(&w.all(whole.clone()).await.ok());
        assert!(all.len() >= 20, "{mode}: {all:?}");
        // one small page after another gives the whole list, in order
        let mut walked = vec![];
        let mut page: Option<String> = None;
        loop {
            let mut args = whole.clone();
            args["max_tokens"] = json!(250);
            if let Some(p) = &page {
                args["page"] = json!(p);
            }
            let res = w.all(args).await.ok();
            walked.extend(ids(&res));
            match res["next"].as_str() {
                Some(n) => {
                    if page.is_none() {
                        cursors.push(n.to_string());
                    }
                    page = Some(n.to_string());
                }
                None => break,
            }
        }
        assert_eq!(walked, all, "{mode}");
        // brief: true briefs the page's hits, and only them
        let mut args = whole.clone();
        args["max_tokens"] = json!(1200);
        args["brief"] = json!(true);
        let res = w.stdio(args).await.ok();
        let hits = ids(&res);
        assert!(!hits.is_empty() && res["next"].is_string(), "{mode}: {res}");
        assert_eq!(
            res["briefing"]["covers"],
            json!(hits.len()),
            "{mode}: {res}"
        );
    }
    // a cursor pages its own mode only
    for (i, cursor) in cursors.iter().enumerate() {
        for (j, mode) in modes.iter().enumerate() {
            if i == j {
                continue;
            }
            let mut args = mode.clone();
            args["scope"] = json!("canon");
            args["max_tokens"] = json!(250);
            args["page"] = json!(cursor);
            let Reply::No(code, ..) = w.all(args).await else {
                panic!("{mode} took the cursor of mode {i}");
            };
            assert_eq!(code, "E_PAGE_STALE", "{mode} {i}");
        }
    }
    // D-5BM47 and D-0ZPAP in a search-added briefing: 20 covered content blocks score their
    // vector neighbors, 21 score none and count all of them; an empty list and a zero count
    // stay absent
    let mut twenty =
        json!({"like_text": "walrus pups haul", "path": "a", "brief": true, "max_tokens": 40000});
    let res = w.stdio(twenty.clone()).await.ok();
    assert_eq!(ids(&res).len(), 20, "{res}");
    assert!(res.get("next").is_none());
    let b = &res["briefing"];
    assert_eq!(b["covers"], 20, "{b}");
    assert!(b.get("vector_neighbors_skipped").is_none(), "{b}");
    if let Some(rows) = b.get("vector_neighbors") {
        assert!(!rows.as_array().unwrap().is_empty(), "{b}");
    }
    twenty.as_object_mut().unwrap().remove("path");
    let res = w.stdio(twenty).await.ok();
    assert_eq!(ids(&res).len(), 21, "{res}");
    let b = &res["briefing"];
    assert_eq!(b["covers"], 21, "{b}");
    assert_eq!(b["vector_neighbors_skipped"], 21, "{b}");
    assert!(b.get("vector_neighbors").is_none(), "{b}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vxs7_past_scopes_refuse_before_weights_and_head_tags_read_canon() {
    let (mut w, f) = World::new(reef).await;
    let src = f.ids["$src"].clone();
    let past = Scope::At(f.past.parse().unwrap());
    let scopes = ["tag:v1.0.0".to_string(), format!("landing:{}", f.past)];
    for scope in &scopes {
        let got = w.all(json!({"like": src, "scope": scope})).await;
        assert!(
            matches!(&got, Reply::No(c, m, _) if c == "E_BAD_INPUT" && m.starts_with("like ranks the current state only")),
            "{got:?}"
        );
        // refused before the past state's lexicon or weights are read
        assert!(!w.engine.read().lexicon_built(&past), "{scope}");
    }
    for scope in &scopes {
        let got = w
            .all(json!({"like_text": "kelp otters", "scope": scope}))
            .await;
        assert!(
            matches!(&got, Reply::No(_, m, _) if m.starts_with("like_text ranks the current state only")),
            "{got:?}"
        );
    }
    let weights = w
        .engine
        .read()
        .vector_weights(WeightScope::Read(&past))
        .unwrap()
        .stats();
    assert_eq!(weights, Default::default(), "no weight at the past landing");
    for args in [json!({"like": src}), json!({"like_text": "kelp otters"})] {
        let got = w.all(with(&args, "scope", json!(f.proposal))).await;
        assert!(
            matches!(&got, Reply::No(_, m, _) if m.contains("ranks canon and views only, not proposal:")),
            "{got:?}"
        );
        // canon's head, by its tag and by its landing, reads canon
        let canon = w.all(with(&args, "scope", json!("canon"))).await.ok();
        for scope in ["tag:v1.1.0".to_string(), format!("landing:{}", f.head)] {
            let at = w.all(with(&args, "scope", json!(scope))).await.ok();
            assert_eq!(at, canon, "{scope}");
        }
    }
}

fn with(args: &Value, k: &str, v: Value) -> Value {
    let mut a = args.clone();
    a[k] = v;
    a
}
