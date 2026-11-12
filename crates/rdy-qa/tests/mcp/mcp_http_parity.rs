//! mht8 (D-6M0AF, D-4PHYQ, D-QG54Z, D-44NVC): MCP over HTTP is the stdio tool contract on another
//! wire. One scenario table runs through four equivalent isolated worlds: stdio, and HTTP on each
//! of the three served revisions. Every tool of the live inventory has a result scenario and a
//! refusal scenario; the full replies are compared. The only designed transport differences are
//! the two named in [`DESIGNED_DIFFERENCES`]. In memory, through the production admission layer
//! and rmcp's `StreamableHttpService`; no socket. Every person lives in an isolated home.
use rdy_daemon::instructions::PersonReader;
use rdy_engine::engine::{Engine, Local};
use rdy_engine::instructions_config::{PathEnv, Platform};
use rdy_engine::{Rdy, Scope};
use rdy_mcp::http::{HTTP_REVISIONS, HttpMcp};
use rdy_mcp::notify::NotifyHub;
use rdy_mcp::server::{DaemonBuildWarning, RdyServer, serve};
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::instructions::{Dir, Set, World, author};
use rdy_qa::mcp::supply_kungfu;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

/// The transport differences the design names, and nothing else (D-6M0AF, D-44NVC): stdio
/// pushes subscriptions over its one connection and may ask the client for a card, and HTTP
/// answers a modern `subscriptions/listen` POST with a stream, gives the older revisions no push
/// channel, and gives them no callback elicitation. The tool replies below do not depend on
/// either: no scenario here subscribes, listens or elicits. mht4 owns both behaviours.
const DESIGNED_DIFFERENCES: [&str; 2] = [
    "subscriptions: stdio subscribes on its connection; HTTP listens on a modern POST stream",
    "elicitation: stdio asks over its connection; older HTTP revisions have no callback",
];

/// What the comparison masks, and nothing more. Error codes, messages, fixes, `isError`, warning
/// counts, token budgets, sizes, claims and the order of every list are never masked. A generated
/// identity reads `<kind:n>`, where `n` counts the distinct values of that kind in order of first
/// appearance in one world: two replies agree only if the same identity recurs in the same
/// places, so a swapped or merged identity is a difference.
const NORMALIZATIONS: [&str; 9] = [
    "protocol envelope: the modern revision's own `result.resultType: \"complete\"` member is dropped; it is not part of the tool's reply",
    "request ids: the JSON-RPC `id` and `jsonrpc` members are dropped",
    "fixture paths: each world's project, scratch, home and config folders read `<root>`",
    "generated identities: a block, landing or proposal id (`d-XXXXX`) reads `<id:n>`; a hash reads `<hex:n>` only where a named member holds it whole (`HASH_MEMBERS`, 64 or 32 hex digits) or where a text anchors it (`P-<hex>@n`, `D-XXXXX@<hex>`, `cq-`, `pi-`, `in-` and `rule-` ids, `canon at <hex>`, `for` or `fit` and a 64-digit hash, a redaction record line); a number of any length is never a hash; a temporary folder's own name (`rdyd...`), which is also the project's name, reads `<tmp:n>`",
    "the order of the records of one kind in a redaction dry run's `text`: the engine lists them by hash, so each run of one kind compares in the order of the names its JSON part gave them",
    "the pending card an answer leads to (`next` in an `rdy_answer` reply): the engine picks it by a generated id, so it reads `<next>`",
    "the allocation of the log file in an `rdy_compact` reply (`on_disk_bytes`): the filesystem rounds it to its block size, so it reads `<disk>`; `log_bytes` and every other size are kept",
    "ports: the digits after a loopback host and `:` read `<port>`",
    "known timestamps: `YYYY-MM-DDTHH:MM:SS...` in text reads `<time>`, and a number in a member whose name ends `_ms`, over a billion reads `<time>`",
];

const ACTOR: &str = "agent:m";
const RUN: &str = "m1";

/// Which wire a world is served on.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Via {
    Stdio,
    Http(&'static str),
}

const WIRES: [Via; 4] = [
    Via::Stdio,
    Via::Http(HTTP_REVISIONS[0]),
    Via::Http(HTTP_REVISIONS[1]),
    Via::Http(HTTP_REVISIONS[2]),
];

const MODERN: &str = "2026-07-28";

/// One stdio connection to the world's engine, in process.
struct Stdio {
    r: BufReader<ReadHalf<DuplexStream>>,
    w: WriteHalf<DuplexStream>,
    next: u64,
}

impl Stdio {
    async fn open(server: RdyServer) -> Stdio {
        let (client, end) = tokio::io::duplex(1 << 22);
        let (r, w) = tokio::io::split(end);
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
            "clientInfo": {"name": "mht8", "version": "0"}}}),
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

    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        self.recv(json!(id)).await
    }
}

/// An isolated person host: a [`World`]'s home and config, read in process through the
/// daemon's real reader in its direct mode.
fn person_host(w: &World) -> Arc<PersonReader> {
    let (platform, env): (Platform, PathEnv) = w.path_env();
    Arc::new(PersonReader::direct(env!("CARGO_PKG_VERSION")).with_env(platform, env))
}

/// How a world is built: its person layer, and the build warning its replies may owe.
#[derive(Default)]
struct Setup {
    /// A meta project with this rule text in a required ruleset, named by the user config.
    person_rule: Option<&'static str>,
    build: Option<DaemonBuildWarning>,
}

/// One equivalent world on one wire: its own isolated home, project, scratch folder and engine.
struct Sut {
    via: Via,
    world: World,
    dir: Dir,
    scratch: tempfile::TempDir,
    spec: Spec,
    mcp: Option<Arc<HttpMcp>>,
    stdio: Option<Stdio>,
    next: u64,
    /// HTTP requests this world sent, for the activity check.
    sent: usize,
    /// Generated identities this world's replies held so far.
    seen: Seen,
}

impl Sut {
    async fn build(via: Via, setup: Setup) -> Sut {
        let world = World::new();
        let dir = world.project();
        if let Some(text) = setup.person_rule {
            let meta = world.default_meta();
            world.project_at(&meta);
            author(
                &meta,
                "A person keeps these rules",
                &[Set {
                    name: "person-base",
                    title: "Person base",
                    required: true,
                    lead: Some("Rules of one person."),
                    rules: &[(&["all"], text)],
                }],
            );
            world.person(&meta);
        }
        let mut r = Rdy::open(dir.path()).expect("open the project");
        let spec = spec_doc(&mut r);
        r.close().expect("close the project");
        let engine: Arc<dyn Engine> = Arc::new(
            Local::open(dir.path())
                .expect("open the engine")
                .with_person_host(person_host(&world)),
        );
        let scratch = tempfile::tempdir().unwrap();
        let hub = NotifyHub::new();
        let (mcp, stdio) = match via {
            Via::Stdio => {
                let server = RdyServer::new(Arc::clone(&engine), None, hub)
                    .with_build_warning(setup.build.clone());
                (None, Some(Stdio::open(server).await))
            }
            Via::Http(_) => {
                let mut h = HttpMcp::new(Arc::clone(&engine), hub);
                if let Some(b) = setup.build.clone() {
                    h = h.with_build_warning(Arc::new(move || Some(b.clone())));
                }
                (Some(Arc::new(h)), None)
            }
        };
        let seen = name_fixture(&spec);
        Sut {
            via,
            world,
            dir,
            scratch,
            spec,
            mcp,
            stdio,
            next: 0,
            sent: 0,
            seen,
        }
    }

    /// One JSON-RPC request's whole reply. `meta` is the request's `_meta` as stated; the
    /// modern revision's own protocol keys are added on HTTP.
    async fn rpc(&mut self, method: &str, mut params: Value) -> Value {
        match self.via {
            Via::Stdio => self.stdio.as_mut().unwrap().request(method, params).await,
            Via::Http(rev) => {
                self.next += 1;
                self.sent += 1;
                let id = self.next;
                if rev == MODERN {
                    let mut meta = rdy_qa::mcp_http::modern_meta(json!({}));
                    if let Some(own) = params.get("_meta").and_then(Value::as_object) {
                        for (k, v) in own {
                            meta[k] = v.clone();
                        }
                    }
                    params["_meta"] = meta;
                }
                let mut h: Vec<(&str, String)> = vec![
                    ("host", "localhost".into()),
                    ("accept", "application/json, text/event-stream".into()),
                    ("content-type", "application/json".into()),
                    ("mcp-protocol-version", rev.into()),
                ];
                if rev == MODERN {
                    h.push(("mcp-method", method.into()));
                    if let Some(name) = params["name"].as_str() {
                        h.push(("mcp-name", name.into()));
                    }
                }
                let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
                let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
                let p = self
                    .mcp
                    .as_ref()
                    .unwrap()
                    .post(&h, body.to_string().into_bytes())
                    .await;
                assert_eq!(p.status, 200, "{method} on {rev}: {p:?}");
                let reply = p.reply();
                assert_eq!(reply["id"], json!(id), "{reply}");
                reply
            }
        }
    }

    /// A `tools/call` as this wire sends it. With no `meta`, a stdio call states no claim (the
    /// connection holds it) and an HTTP call states the world's claim; with `Some`, both send
    /// exactly that `_meta`. The kungfu value is supplied unless the arguments name one.
    async fn call(&mut self, tool: &str, args: Value, meta: Option<Value>) -> Value {
        let mut args = args;
        supply_kungfu(tool, &mut args);
        self.call_raw(tool, args, meta).await
    }

    async fn call_raw(&mut self, tool: &str, args: Value, meta: Option<Value>) -> Value {
        let meta = match (meta, self.via) {
            (Some(m), _) => m,
            (None, Via::Http(_)) => json!({"rdy/actor": ACTOR, "rdy/run": RUN}),
            (None, Via::Stdio) => json!({}),
        };
        let v = self
            .rpc(
                "tools/call",
                json!({"name": tool, "arguments": args, "_meta": meta}),
            )
            .await;
        decoded(&v)
    }

    /// The masks of this world: its folders.
    fn roots(&self) -> Vec<String> {
        let (platform, env) = self.world.path_env();
        let _ = platform;
        let mut all: Vec<PathBuf> = vec![
            self.dir.path().to_path_buf(),
            self.scratch.path().to_path_buf(),
            self.world.home.path().to_path_buf(),
            self.world.config_dir(),
        ];
        if let Some(c) = env.xdg_config_home {
            all.push(PathBuf::from(c));
        }
        all.iter()
            .flat_map(|p| {
                let mut v = vec![p.to_string_lossy().into_owned()];
                if let Ok(real) = p.canonicalize() {
                    v.push(real.to_string_lossy().into_owned());
                }
                v
            })
            .collect()
    }

    fn norm(&self, v: &Value) -> Value {
        normalized(v, &self.roots(), &masks(), &self.seen)
    }
}

/// The reply as a value: the whole `result` with the tool text parsed, or the protocol error,
/// without the request id.
fn decoded(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("id");
        o.remove("jsonrpc");
    }
    if v["result"]["resultType"] == json!("complete")
        && let Some(o) = v["result"].as_object_mut()
    {
        o.remove("resultType");
    }
    if let Some(text) = v["result"]["content"][0]["text"].as_str() {
        let parsed = serde_json::from_str::<Value>(text)
            .unwrap_or_else(|_| panic!("a tool reply that is not JSON: {text}"));
        v["result"]["content"][0]["text"] = parsed;
    }
    v
}

fn is_error(v: &Value) -> bool {
    v["result"]["isError"] == json!(true) || v.get("error").is_some()
}

/// The tool's own body: the parsed text, or the protocol error.
fn body(v: &Value) -> Value {
    match v["result"]["content"][0]["text"].clone() {
        Value::Null => v.clone(),
        b => b,
    }
}

/// A generated-identity pattern: the kind it reads as, and the capture group that holds the
/// generated value (the rest of the match is kept as written).
struct Mask {
    kind: &'static str,
    re: regex::Regex,
    group: usize,
}

/// The generated-identity patterns found inside texts. Each hash is named by the text that
/// carries it (a proposal ref, a card line, a `canon at`, a signing refusal), never by its
/// digits alone: a number of eight or more digits is not a hash. Order matters: the folder name
/// first, since it contains digits and letters of the other kinds; the hashes before the block
/// ids, since a card line holds both (`D-XXXXX@<hash>`).
fn masks() -> Vec<Mask> {
    [
        ("tmp", r"(?i)[^\s\x22']*rdyd[0-9a-z]+", 0),
        // a proposal ref: `P-<12 hex>@n`
        ("hex", r"\bP-([0-9a-f]{12})@\d+", 1),
        // a pinned block ref: `D-XXXXX@<hash>`
        ("hex", r"\b[A-Za-z]{1,3}-[0-9A-Z]{5}@([0-9a-f]{12,64})\b", 1),
        // a consequence, plan item, intent or rule id
        ("hex", r"\b(?:cq|pi|in|rule)-([0-9a-f]{12,64})\b", 1),
        ("hex", r"\bcanon at ([0-9a-f]{12})\b", 1),
        ("hex", r"\b(?:for|fit) ([0-9a-f]{64})\b", 1),
        // a record line of a redaction dry run: `<kind>: <hash> (<relation>)`
        ("hex", r"(?m)^[a-z_]+: ([0-9a-f]{64}) \([a-z]+\)$", 1),
        ("id", r"\b[A-Za-z]{1,3}-[0-9A-Z]{5}\b", 0),
    ]
    .iter()
    .map(|(kind, m, group)| Mask {
        kind,
        re: regex::Regex::new(m).unwrap(),
        group: *group,
    })
    .collect()
}

/// The members that hold one hash or repository id as their whole text (64 or 32 lowercase hex
/// digits). A value of that shape at any other member is kept as it is.
const HASH_MEMBERS: [&str; 31] = [
    "answer",
    "approved_version",
    "assessment",
    "base",
    "canon_at",
    "card_hash",
    "change",
    "changes",
    "digest",
    "effective_base",
    "from",
    "hash",
    "head",
    "id",
    "identity",
    "ids",
    "intent",
    "landing",
    "pin",
    "prev",
    "proposal",
    "record",
    "report_hash",
    "repo_id",
    "stable",
    "tag",
    "to",
    "to_version",
    "token",
    "version",
    "versions",
];

/// `s` with the `mask.group` span of every match of `mask` replaced by `f(value)`.
fn replace_group(mask: &Mask, s: &str, f: &dyn Fn(&str) -> String) -> String {
    let mut out = String::new();
    let mut at = 0;
    for c in mask.re.captures_iter(s) {
        let g = c.get(mask.group).unwrap();
        out.push_str(&s[at..g.start()]);
        out.push_str(&f(g.as_str()));
        at = g.end();
    }
    out.push_str(&s[at..]);
    out
}

fn is_hash(s: &str) -> bool {
    matches!(s.len(), 32 | 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Identities of one world: the names of its fixture blocks, and the numbers of the other values
/// seen so far, kind by kind.
#[derive(Default)]
struct Names {
    of: std::collections::BTreeMap<(&'static str, String), String>,
    count: std::collections::BTreeMap<&'static str, usize>,
}

type Seen = std::cell::RefCell<Names>;

fn identity(seen: &Seen, kind: &'static str, value: &str) -> String {
    let mut n = seen.borrow_mut();
    if let Some(name) = n.of.get(&(kind, value.to_string())) {
        return format!("<{kind}:{name}>");
    }
    let c = n.count.entry(kind).or_insert(0);
    *c += 1;
    let name = c.to_string();
    n.of.insert((kind, value.to_string()), name.clone());
    format!("<{kind}:{name}>")
}

/// The fixture's blocks, named: their generated ids read the same in every world.
fn name_fixture(spec: &Spec) -> Seen {
    let n = Names {
        of: [
            ("doc", &spec.doc),
            ("section", &spec.section),
            ("a", &spec.a),
            ("b", &spec.b),
            ("c", &spec.c),
            ("decision", &spec.decision),
        ]
        .iter()
        .map(|(name, id)| (("id", id.to_string()), name.to_string()))
        .collect(),
        count: Default::default(),
    };
    Seen::new(n)
}

/// The lists the engine sorts by a generated id, as the last member names of their path. Their
/// entries compare in the order of their fixture names. Every other list keeps its order.
const ID_ORDERED: [&str; 13] = [
    "/neighbors",
    "/plan/items",
    "/consequences",
    "/incoming",
    "/base/blocks",
    "/coverage/issues",
    "/coverage/reasons",
    "/neighbors/of",
    "/items",
    "/text/issues",
    "/corrected/exceptions",
    "/selection/entries",
    "/report/inside",
];

/// One world's normalizer.
struct Norm<'a> {
    roots: &'a [String],
    masks: &'a [Mask],
    seen: &'a Seen,
    time: regex::Regex,
    port: regex::Regex,
    list_start: regex::Regex,
    whole_hash: regex::Regex,
}

/// [`NORMALIZATIONS`], applied. Nothing else is masked.
fn normalized(v: &Value, roots: &[String], masks: &[Mask], seen: &Seen) -> Value {
    let n = Norm {
        roots,
        masks,
        seen,
        time: regex::Regex::new(r"\d{4}-\d{2}-\d{2}[T ][0-9:.]+Z?").unwrap(),
        list_start: regex::Regex::new(r"(?:, |; )[A-Za-z]{1,3}-[0-9A-Z]{5}\b").unwrap(),
        whole_hash: regex::Regex::new(r#""([0-9a-f]{64}|[0-9a-f]{32})""#).unwrap(),
        port: regex::Regex::new(r"((?:127\.0\.0\.1|localhost|\[::1\]):)\d+").unwrap(),
    };
    n.walk(v, "")
}

impl Norm<'_> {
    /// A text with its roots, generated identities, times and ports masked. `hashes` is false
    /// where the text is documentation, whose example values are the same in every world.
    fn text(&self, s: &str, hashes: bool) -> String {
        let mut s = s.to_string();
        for r in self.roots {
            s = s.replace(r, "<root>");
        }
        for m in self.masks.iter().filter(|m| hashes || m.kind != "hex") {
            s = replace_group(m, &s, &|v| identity(self.seen, m.kind, v));
        }
        s = self.time.replace_all(&s, "<time>").into_owned();
        self.port.replace_all(&s, "${1}<port>").into_owned()
    }

    /// A card lists its decisions, and each one's related decisions, in the order of their
    /// generated ids, and a refusal lists the cards to answer so. Only those lists are put in the order of their fixture names; the rest
    /// of the text, and the entries themselves, are kept.
    fn listed(&self, s: &str) -> String {
        const MARKERS: [(&str, &str); 3] = [
            (" new: ", ", "),
            (" omitted: ", "; "),
            (" card first: ", ", "),
        ];
        s.split('\n')
            .map(|line| {
                let Some((marker, sep)) = MARKERS.iter().find(|(m, _)| line.contains(m)) else {
                    return line.to_string();
                };
                let at = line.find(marker).unwrap() + marker.len();
                let (head, list) = line.split_at(at);
                let starts: Vec<usize> = std::iter::once(0)
                    .chain(self.list_start.captures_iter(list).filter_map(|c| {
                        let m = c.get(0).unwrap();
                        (m.as_str().starts_with(sep)).then(|| m.start() + sep.len())
                    }))
                    .collect();
                let mut entries: Vec<&str> = starts
                    .iter()
                    .enumerate()
                    .map(|(i, &a)| {
                        let end = starts.get(i + 1).map_or(list.len(), |&b| b - sep.len());
                        &list[a..end]
                    })
                    .collect();
                entries.sort_by_cached_key(|e| self.sort_key(e));
                format!("{head}{}", entries.join(sep))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A redaction report's text lists the records of one kind in the order of their ids (hashes
    /// of generated content). Each run of lines of one kind is put in the order of the names the
    /// JSON part of the same reply gave those records; the order of the kinds is kept.
    fn records(&self, s: &str) -> String {
        let kind = |l: &str| l.split_once(": ").map(|(k, _)| k.to_string());
        let lines: Vec<&str> = s.split('\n').collect();
        let mut out: Vec<&str> = vec![];
        let mut i = 0;
        while i < lines.len() {
            let k = kind(lines[i]).filter(|_| lines[i].ends_with(')'));
            let mut j = i + 1;
            while k.is_some() && j < lines.len() && kind(lines[j]) == k && lines[j].ends_with(')') {
                j += 1;
            }
            let mut run: Vec<&str> = lines[i..j].to_vec();
            run.sort_by_cached_key(|l| self.sort_key(l));
            out.extend(run);
            i = j;
        }
        out.join("\n")
    }

    /// A text with the fixture's blocks named and every other generated value reduced to its kind:
    /// the order of a list the engine sorts by generated id.
    fn sort_key(&self, s: &str) -> String {
        let mut s = s.to_string();
        for r in self.roots {
            s = s.replace(r, "<root>");
        }
        for m in self.masks {
            s = replace_group(
                m,
                &s,
                &|v| match self.seen.borrow().of.get(&(m.kind, v.to_string())) {
                    Some(name) => format!("<{}:{name}>", m.kind),
                    None => format!("<{}>", m.kind),
                },
            );
        }
        // a hash held whole by a member has no anchor in the text of its list: only the order of
        // a list the engine sorts by it is read through this, so any hash shape serves
        self.whole_hash
            .replace_all(&s, |c: &regex::Captures| {
                match self.seen.borrow().of.get(&("hex", c[1].to_string())) {
                    Some(name) => format!("\"<hex:{name}>\""),
                    None => "\"<hex>\"".to_string(),
                }
            })
            .into_owned()
    }

    /// `path` is the member names from the root, joined by `/`.
    fn walk(&self, v: &Value, path: &str) -> Value {
        let key = path.rsplit('/').next().unwrap_or("");
        match v {
            // the card an answer leads to is the first of the pending ones, which the engine
            // orders by a generated id
            Value::String(_) if path.ends_with("/text/next") => json!("<next>"),
            Value::String(s)
                if path.ends_with("/text/text") && s.starts_with("Redaction dry run") =>
            {
                Value::String(self.text(&self.records(s), true))
            }
            // a hash held whole by an identity member
            Value::String(s) if HASH_MEMBERS.contains(&key) && is_hash(s) => {
                Value::String(identity(self.seen, "hex", s))
            }
            // the schema's examples are documentation: the same text in every world
            Value::String(s) => {
                Value::String(self.text(&self.listed(s), !path.contains("/examples/")))
            }
            // the filesystem's allocation for the log file, which rounds up to its block size
            Value::Number(_) if path.ends_with("/text/on_disk_bytes") => json!("<disk>"),
            Value::Number(n)
                if key.ends_with("_ms") && n.as_u64().is_some_and(|n| n > 1_000_000_000) =>
            {
                json!("<time>")
            }
            Value::Array(a) => {
                let mut a: Vec<&Value> = a.iter().collect();
                if ID_ORDERED.iter().any(|p| path.ends_with(p)) {
                    a.sort_by_cached_key(|x| self.sort_key(&x.to_string()));
                }
                Value::Array(a.into_iter().map(|x| self.walk(x, path)).collect())
            }
            // an object has no order; its members are visited by name so that numbers are stable
            Value::Object(m) => {
                let mut m: Vec<(&String, &Value)> = m.iter().collect();
                m.sort_by_cached_key(|(k, _)| self.sort_key(k));
                Value::Object(
                    m.into_iter()
                        .map(|(k, x)| (self.text(k, true), self.walk(x, &format!("{path}/{k}"))))
                        .collect(),
                )
            }
            other => other.clone(),
        }
    }
}

/// The first place `a` and `b` differ, as a path and the two values there.
fn first_difference(a: &Value, b: &Value, at: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            keys.into_iter().find_map(|k| {
                let null = Value::Null;
                first_difference(
                    x.get(k).unwrap_or(&null),
                    y.get(k).unwrap_or(&null),
                    &format!("{at}/{k}"),
                )
            })
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => x
            .iter()
            .zip(y)
            .enumerate()
            .find_map(|(i, (p, q))| first_difference(p, q, &format!("{at}/{i}"))),
        _ => (a != b).then(|| format!("{at}: {a} != {b}")),
    }
}

/// A format-3 export tree at `out`, for `rdy_import`.
fn import_tree(out: &Path) {
    std::fs::create_dir_all(out.join("docs")).unwrap();
    std::fs::write(
        out.join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: work\n",
    )
    .unwrap();
    std::fs::write(
        out.join("docs/imported.yaml"),
        "- id: d-QM001\n  type: doc\n  order: a1\n  fields:\n    doc_type: overview\n    key: imported\n    title: Imported\n",
    )
    .unwrap();
    std::fs::write(
        out.join("authors.yaml"),
        "- id: d-QM001\n  actor: agent:a\n  run: r1\n  role: writer\n  at_ms: 100\n",
    )
    .unwrap();
}

/// Which scenario of a tool a row is.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    /// The tool does its work, or gives its ordinary refusal when the world cannot ([`REFUSED`]).
    Result,
    /// The tool refuses an input it does not accept.
    Refusal,
    /// A read of the project after the script.
    Effect,
}

struct Row {
    tool: String,
    kind: Kind,
    err: bool,
    reply: Value,
}

/// One run of the table on one wire.
struct Run<'a> {
    sut: &'a mut Sut,
    rows: Vec<Row>,
}

impl Run<'_> {
    /// One call: its normalized reply is recorded; its raw body comes back.
    async fn step(&mut self, tool: &str, kind: Kind, args: Value) -> Value {
        let v = self.sut.call(tool, args, None).await;
        self.record(tool, kind, v)
    }

    fn record(&mut self, tool: &str, kind: Kind, v: Value) -> Value {
        let raw = body(&v);
        self.rows.push(Row {
            tool: tool.into(),
            kind,
            err: is_error(&v),
            reply: self.sut.norm(&v),
        });
        raw
    }

    async fn ok(&mut self, tool: &str, args: Value) -> Value {
        self.step(tool, Kind::Result, args).await
    }
}

/// The steps whose result scenario ends in the tool's ordinary refusal, with each one's code: each
/// needs what these worlds do not hold (a tension, a person's answer, a hub, a key, a review
/// request, a stream, a daemon or an SSH agent). Every other result scenario succeeds.
const REFUSED: &[(&str, &str)] = &[
    ("rdy_resolve", "E_NOT_FOUND"),
    ("rdy_land", "E_EXCEPTION_OPEN"),
    // an engine without a daemon refuses the four tools that run in one (E_SITE, E_BAD_INPUT)
    ("rdy_site", "E_SITE"),
    ("rdy_publish", "E_BAD_INPUT"),
    ("rdy_render", "E_BAD_INPUT"),
    ("rdy_export", "E_BAD_INPUT"),
    ("rdy_auth", "E_BAD_INPUT"),
    ("rdy_key_bind", "E_NOT_FOUND"),
    ("rdy_key_retire", "E_BAD_INPUT"),
    ("rdy_key_revoke", "E_BAD_INPUT"),
    ("rdy_review_submit", "E_BAD_INPUT"),
    ("rdy_rule", "E_NOT_FOUND"),
    ("rdy_stream_check", "E_BAD_RECORD"),
    ("rdy_sign", "E_PERMISSION"),
];

/// The scenario table. Each tool has its result scenario here, in order, on the seeded spec; the
/// refusal scenarios follow for every tool of the inventory.
async fn script(x: &mut Run<'_>, tools: &[String]) {
    let spec = x.sut.spec.clone();
    let scratch = x.sut.scratch.path().to_path_buf();
    let s = |p: &str| scratch.join(p).to_string_lossy().into_owned();
    let (a, c, section, decision) = (
        spec.a.to_string(),
        spec.c.to_string(),
        spec.section.to_string(),
        spec.decision.to_string(),
    );
    x.ok(
        "rdy_session",
        json!({"actor": ACTOR, "run": RUN, "role": "writer"}),
    )
    .await;
    x.ok("rdy_instructions", json!({"full": true})).await;
    x.ok("rdy_overview", json!({})).await;
    x.ok("rdy_schema", json!({})).await;
    x.ok("rdy_read", json!({"id": "spec", "scope": "canon"}))
        .await;
    x.ok("rdy_context", json!({"id": a, "scope": "canon"}))
        .await;
    x.ok("rdy_search", json!({"query": "holds", "scope": "canon"}))
        .await;
    x.ok(
        "rdy_query",
        json!({"filter": "type = decision", "scope": "canon"}),
    )
    .await;
    let brief = x.ok("rdy_brief", json!({"path": "spec"})).await;
    x.ok("rdy_suspects", json!({"scope": "canon"})).await;
    x.ok("rdy_tensions", json!({"scope": "canon"})).await;
    x.ok("rdy_pending", json!({})).await;
    x.ok("rdy_site", json!({"scope": "canon"})).await;

    let token = brief["base"]["token"]
        .as_str()
        .unwrap_or("none")
        .to_string();
    x.ok(
        "rdy_write",
        json!({"ops": [{"op": "update", "id": a, "body": "A holds, by m."}],
            "brief": token, "message": "m edits A"}),
    )
    .await;
    let made = x
        .ok(
            "rdy_decide",
            json!({"statement": "Cache the reads", "title": "Cache the reads",
                "why": "A person decides how reads are cached", "parent": section}),
        )
        .await;
    let made = made["created"]["$decision"]
        .as_str()
        .unwrap_or("none")
        .to_string();
    x.ok(
        "rdy_supersede",
        json!({"old": decision, "decision": {"statement": "Keep A always", "title": "Keep A always",
            "why": "A person keeps A", "parent": section}}),
    )
    .await;
    x.ok(
        "rdy_revoke",
        json!({"target": made, "reason": "the reads stay uncached", "parent": section}),
    )
    .await;
    x.ok(
        "rdy_accept",
        json!({"suggestions": [{"kind": "entity", "literal": "Fee", "name": "Fee", "type": "term",
            "body": "Fee is what a payment costs."}], "parent": section}),
    )
    .await;
    x.ok(
        "rdy_intent",
        json!({"op": "post", "blocks": [c], "angle": "fees", "ttl": "30m"}),
    )
    .await;
    x.ok("rdy_asset", json!({"data_base64": "aGVsbG8="})).await;
    x.ok("rdy_plan", json!({})).await;
    let proposed = x
        .ok(
            "rdy_propose",
            json!({"title": "Fix A", "summary": "Rewords A."}),
        )
        .await;
    let pref = proposed["ref"].as_str().unwrap_or("none").to_string();
    x.ok(
        "rdy_resolve",
        json!({"tension": "none", "dismiss": {"reason": "checked"}}),
    )
    .await;
    let queue = x.ok("rdy_pending", json!({"max_tokens": 100_000})).await;
    let (item, card_hash) = queue["items"]
        .as_array()
        .and_then(|all| all.first())
        .map(|i| {
            (
                i["ref"].as_str().unwrap().to_string(),
                i["card_hash"].as_str().unwrap().to_string(),
            )
        })
        .unwrap_or(("none".into(), "none".into()));
    x.ok(
        "rdy_answer",
        json!({"ref": item, "option": "later", "card_hash": card_hash,
            "evidence": "Not yet.", "question": "Land it?", "session_ref": "chat-1"}),
    )
    .await;
    x.ok("rdy_land", json!({"proposal": pref})).await;
    x.ok("rdy_dispute", json!({"op": "list"})).await;
    x.ok(
        "rdy_tag",
        json!({"version": "0.0.1", "evidence": "Tag it.", "question": "Tag?"}),
    )
    .await;

    x.ok("rdy_trace", json!({"id": a, "scope": "canon"})).await;
    x.ok("rdy_diff", json!({"from": "canon"})).await;
    let log = x.ok("rdy_log", json!({"limit": 5})).await;
    let landing = log[0]["landing"].as_str().unwrap_or("none").to_string();
    x.ok("rdy_impact", json!({"id": a, "scope": "canon"})).await;
    x.ok("rdy_inbox", json!({})).await;
    x.ok("rdy_coverage", json!({"scope": "canon"})).await;
    x.ok("rdy_tags", json!({})).await;
    x.ok("rdy_links", json!({})).await;

    x.ok(
        "rdy_publication_check",
        json!({"profile": "implicit:org", "scope": "canon"}),
    )
    .await;
    x.ok(
        "rdy_publish",
        json!({"tag": "none", "to": "none", "site": "none"}),
    )
    .await;
    x.ok("rdy_render", json!({"out": s("render"), "scope": "canon"}))
        .await;
    x.ok(
        "rdy_export",
        json!({"out": s("export"), "format": "md", "scope": "canon"}),
    )
    .await;
    x.ok("rdy_remote", json!({"op": "list"})).await;
    x.ok("rdy_auth", json!({"op": "state"})).await;
    x.ok("rdy_key_show", json!({})).await;
    x.ok("rdy_key_bind", json!({"kind": "agent", "remote": "none"}))
        .await;
    x.ok("rdy_key_retire", json!({"key": "none", "remote": "none"}))
        .await;
    x.ok("rdy_key_revoke", json!({"key": "none", "remote": "none"}))
        .await;

    x.ok("rdy_review", json!({"op": "list"})).await;
    // the reviewer's tools need that role, in the same run
    x.ok(
        "rdy_session",
        json!({"actor": ACTOR, "run": RUN, "role": "reviewer"}),
    )
    .await;
    x.ok("rdy_review_next", json!({"kind": "tension"})).await;
    x.ok(
        "rdy_review_submit",
        json!({"request": "none", "case_hash": "none", "verdict": "pass"}),
    )
    .await;
    x.ok(
        "rdy_session",
        json!({"actor": ACTOR, "run": RUN, "role": "writer"}),
    )
    .await;

    x.ok("rdy_view", json!({"op": "create", "name": "fork-a"}))
        .await;
    x.ok("rdy_alternatives", json!({"op": "list"})).await;
    x.ok(
        "rdy_rule",
        json!({"scope": "project", "op": "remove", "id": "ai-00000", "why": "No longer wanted.",
            "evidence": "Drop it.", "session_ref": "chat-1"}),
    )
    .await;

    // m4a4: a check outside the project and its worktrees, the same on every wire
    x.ok(
        "rdy_hook",
        json!({"op": "check", "kind": "edit", "paths": [s("outside.txt")]}),
    )
    .await;
    x.ok("rdy_dump", json!({"out": s("dump.rdy")})).await;
    x.ok("rdy_stream_check", json!({"file": s("dump.rdy")}))
        .await;
    x.ok(
        "rdy_load",
        json!({"file": s("dump.rdy"), "dir": s("loaded")}),
    )
    .await;
    x.ok("rdy_clone", json!({"dir": s("cloned")})).await;
    import_tree(&scratch.join("tree"));
    x.ok("rdy_import", json!({"dir": s("tree"), "view": "imported"}))
        .await;
    x.ok(
        "rdy_redact",
        json!({"targets": [c], "reason_class": "secret", "reason": "a leaked code", "dry_run": true}),
    )
    .await;
    x.ok("rdy_sign", json!({"target": landing})).await;
    x.ok("rdy_upgrade", json!({"dry_run": true})).await;
    x.ok("rdy_compact", json!({"dry_run": true})).await;
    // last: a policy changes what later acts need
    x.ok("rdy_policy", json!({"op": "init", "doc": "spec"}))
        .await;

    // the refusal scenario of every tool: an input it does not accept, on the session's claim
    for tool in tools {
        x.step(tool, Kind::Refusal, json!({"bogus": true})).await;
    }

    // the effects: what the project holds now, read through the same wire
    for (tool, args) in [
        ("rdy_log", json!({"limit": 1000, "max_tokens": 100_000})),
        ("rdy_view", json!({"op": "show", "max_tokens": 100_000})),
        ("rdy_overview", json!({"max_tokens": 100_000})),
    ] {
        x.step(tool, Kind::Effect, args).await;
    }
}

/// Every row of `got` is the row of `want`, in order: same tool, scenario, error flag and reply.
fn assert_same(via: Via, got: &[Row], want: &[Row]) {
    assert_eq!(got.len(), want.len(), "{via:?}: row count");
    for (i, (got, want)) in got.iter().zip(want).enumerate() {
        assert_eq!(
            (&got.tool, got.kind, got.err),
            (&want.tool, want.kind, want.err),
            "{via:?}: step {i}"
        );
        if let Some(d) = first_difference(&got.reply, &want.reply, "") {
            panic!(
                "{via:?}: step {i} ({} {:?}) differs from stdio at {d}\nHTTP:  {}\nstdio: {}",
                got.tool, got.kind, got.reply, want.reply
            );
        }
    }
}

/// The tools a world lists, with each one's whole listing.
async fn inventory(sut: &mut Sut) -> Vec<Value> {
    let v = sut.rpc("tools/list", json!({})).await;
    let mut tools = v["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("a tool list: {v}"))
        .clone();
    tools.sort_by_key(|t| t["name"].as_str().unwrap_or_default().to_string());
    tools
}

fn names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht8_every_tool_has_stdio_http_parity() {
    let mut reference: Option<(Vec<Value>, Vec<Row>)> = None;
    for via in WIRES {
        let mut sut = Sut::build(via, Setup::default()).await;
        let listed = inventory(&mut sut).await;
        let tools = names(&listed);
        // the live inventory, Repo links' tool included, is the same on every wire
        assert!(tools.contains(&"rdy_links".to_string()), "{tools:?}");
        assert!(tools.contains(&"rdy_sign".to_string()));
        assert!(tools.contains(&"rdy_key_bind".to_string()));
        let mut run = Run {
            sut: &mut sut,
            rows: vec![],
        };
        script(&mut run, &tools).await;
        let rows = std::mem::take(&mut run.rows);
        // every tool has a result scenario and an applicable refusal scenario, and none passes
        // by method-not-found
        for t in &tools {
            for kind in [Kind::Result, Kind::Refusal] {
                let mine: Vec<&Row> = rows
                    .iter()
                    .filter(|r| &r.tool == t && r.kind == kind)
                    .collect();
                assert!(!mine.is_empty(), "{via:?}: {t} has no {kind:?} scenario");
                for r in mine {
                    assert_ne!(
                        r.reply["error"]["code"],
                        json!(-32601),
                        "{via:?}: {t} is method-not-found"
                    );
                    if kind == Kind::Refusal {
                        assert!(r.err, "{via:?}: {t} accepted an unknown parameter");
                        assert_eq!(
                            r.reply["result"]["content"][0]["text"]["code"], "E_BAD_INPUT",
                            "{via:?}: {t}: {}",
                            r.reply
                        );
                    }
                }
            }
        }
        // a result scenario succeeds unless the world cannot hold what it needs
        let mut off = vec![];
        for r in rows.iter().filter(|r| r.kind == Kind::Result) {
            let code = r.reply["result"]["content"][0]["text"]["code"]
                .as_str()
                .map(str::to_string);
            let want = REFUSED.iter().find(|(t, _)| *t == r.tool).map(|(_, c)| *c);
            if (r.err, code.as_deref()) != (want.is_some(), want) {
                off.push(format!(
                    "{}: err {} code {code:?}, want {want:?}: {}",
                    r.tool, r.err, r.reply
                ));
            }
        }
        assert!(off.is_empty(), "{via:?}: {off:#?}");
        // the tools the script names are all listed
        for r in &rows {
            assert!(tools.contains(&r.tool), "{via:?}: {} is not listed", r.tool);
        }
        // the runtime HTTP activity is counted apart from the replies: every admitted request
        // made its transport service, and none outlives its request
        if let Some(mcp) = &sut.mcp {
            let stats = mcp.transport_stats();
            assert_eq!(
                stats.live(),
                0,
                "{via:?}: a transport service outlived its call"
            );
            assert!(
                stats.made() >= sut.sent,
                "{via:?}: {} made for {} admitted",
                stats.made(),
                sut.sent
            );
            assert_eq!(mcp.listen_streams(), 0, "no scenario listens");
        }
        match &reference {
            None => reference = Some((listed, rows)),
            Some((want_tools, want_rows)) => {
                assert_eq!(&listed, want_tools, "{via:?}: the tool listing differs");
                assert_same(via, &rows, want_rows);
            }
        }
    }
    assert_eq!(DESIGNED_DIFFERENCES.len(), 2);
    assert_eq!(NORMALIZATIONS.len(), 9);
}

// ------------------------------------------------------------------------ shared drivers

/// The scenarios that share the driver below.
#[derive(Clone, Copy)]
enum Scenario {
    Claims,
    Budgets,
    Person,
    Signing,
}

/// The rows of `scenario` on `via`, in a fresh world.
async fn run_on(via: Via, setup: Setup, scenario: Scenario) -> (Vec<Row>, Sut) {
    let mut sut = Sut::build(via, setup).await;
    if matches!(scenario, Scenario::Signing) {
        write_signing_config(sut.dir.path());
    }
    let mut run = Run {
        sut: &mut sut,
        rows: vec![],
    };
    match scenario {
        Scenario::Claims => claims(&mut run).await,
        Scenario::Budgets => budgets(&mut run).await,
        Scenario::Person => person(&mut run).await,
        Scenario::Signing => signing(&mut run).await,
    }
    let rows = std::mem::take(&mut run.rows);
    if let Some(mcp) = &sut.mcp {
        assert_eq!(
            mcp.transport_stats().live(),
            0,
            "{via:?}: a service outlived its call"
        );
    }
    (rows, sut)
}

/// `scenario` on every wire; each HTTP revision's rows equal stdio's. The rows come back.
async fn same_on_every_wire(setup: impl Fn() -> Setup, scenario: Scenario) -> Vec<Row> {
    let (want, _) = run_on(Via::Stdio, setup(), scenario).await;
    for via in &WIRES[1..] {
        let (got, _) = run_on(*via, setup(), scenario).await;
        assert_same(*via, &got, &want);
    }
    want
}

impl Run<'_> {
    /// One call that states exactly `meta` on both wires.
    async fn with(&mut self, tool: &str, kind: Kind, args: Value, meta: Value) -> Value {
        let v = self.sut.call(tool, args, Some(meta)).await;
        self.record(tool, kind, v)
    }

    /// One call with exactly `args`: no kungfu value is supplied.
    async fn bare(&mut self, tool: &str, kind: Kind, args: Value, meta: Option<Value>) -> Value {
        let v = self.sut.call_raw(tool, args, meta).await;
        self.record(tool, kind, v)
    }
}

fn agent_claim(actor: &str, run: &str) -> Value {
    json!({"rdy/actor": actor, "rdy/run": run})
}

fn with_key(mut meta: Value, k: &str, v: Value) -> Value {
    meta[k] = v;
    meta
}

fn folder(name: &str) -> Value {
    json!({"ops": [{"op": "create", "type": "folder", "id": "$f", "fields": {"name": name, "title": name}}]})
}

// ------------------------------------------------------------------------ claims and errors

/// The refusals in the claim parser's closed order, the top-level and nested kungfu keys, unknown
/// fields, wrong roles, missing views and stale briefing tokens. Each call states its own
/// `_meta`, so stdio and HTTP read the same request.
async fn claims(x: &mut Run<'_>) {
    // malformed claims: refused before the tool reads its arguments
    let bad_args = json!({"id": "nope", "bogus": true});
    let malformed = [
        json!({"rdy/zz": 1, "rdy/actr": 2, "rdy/actor": 3}),
        json!({"rdy/zzzzzz": 1}),
        json!({"rdy/dedupe": "x", "rdy/run": 1, "rdy/actor": "agent:a"}),
        json!({"rdy/actor": null, "rdy/run": null}),
        json!({"rdy/dedupe": null}),
        json!({"rdy/actor": "agent:a"}),
        json!({"rdy/actor": "agent:a", "rdy/run": "a1", "rdy/role": "approver"}),
        json!({"rdy/actor": "agent:a", "rdy/run": "a1", "rdy/role": "boss"}),
        json!({"rdy/actor": "agent:a", "rdy/run": "a1", "rdy/role": 3}),
        json!({"rdy/actor": "human:a", "rdy/run": "a1"}),
        json!({"rdy/actor": "not an actor", "rdy/run": "a1"}),
        json!({"rdy/actor": "agent:a", "rdy/run": "adopt-1"}),
        json!({"progressToken": 1}),
        json!({}),
    ];
    for meta in malformed {
        x.with("rdy_read", Kind::Refusal, bad_args.clone(), meta)
            .await;
        assert!(x.rows.last().unwrap().err);
    }
    // a valid claim: the tool's own parameter checks, unknown fields with the nearest key
    let a = agent_claim("agent:a", "a1");
    for args in [
        bad_args.clone(),
        json!({"scop": "canon"}),
        json!({"id": "nope", "kungfu": "x", "scope": {"kungfu": "y"}}),
        json!({"id": "spec", "kungfu": null}),
        json!({"id": "spec", "kungfu": 5}),
        json!({"max_tokens": "many"}),
        json!({"id": ["spec"]}),
    ] {
        x.with("rdy_read", Kind::Refusal, args, a.clone()).await;
    }
    // top-level kungfu leaves the arguments: right, stale, wrong type; a nested one is an
    // unknown input of its own place
    x.with(
        "rdy_tags",
        Kind::Result,
        with_kungfu("rdy_tags", json!({})),
        a.clone(),
    )
    .await;
    for k in [json!("kf-00000000"), json!(null), json!(7), json!({})] {
        x.with("rdy_tags", Kind::Refusal, json!({"kungfu": k}), a.clone())
            .await;
    }
    x.with(
        "rdy_search",
        Kind::Refusal,
        with_kungfu(
            "rdy_search",
            json!({"query": "holds", "scope": {"kungfu": "y"}}),
        ),
        a.clone(),
    )
    .await;
    // wrong roles: an override holds for its call; approver and reviewer cannot write
    for role in ["reader", "reviewer", "writer"] {
        x.with(
            "rdy_write",
            Kind::Refusal,
            with_kungfu("rdy_write", folder(&format!("by-{role}"))),
            with_key(a.clone(), "rdy/role", json!(role)),
        )
        .await;
    }
    for role in ["reader", "reviewer"] {
        x.with(
            "rdy_decide",
            Kind::Refusal,
            with_kungfu(
                "rdy_decide",
                json!({"statement": "No", "title": "No", "why": "no", "parent": "spec"}),
            ),
            with_key(a.clone(), "rdy/role", json!(role)),
        )
        .await;
        x.with(
            "rdy_sign",
            Kind::Refusal,
            with_kungfu("rdy_sign", json!({"target": "tag:0.0.1"})),
            with_key(a.clone(), "rdy/role", json!(role)),
        )
        .await;
    }
    // missing views and scopes
    for (tool, args) in [
        ("rdy_read", json!({"id": "spec", "scope": "view/ghost"})),
        ("rdy_overview", json!({"scope": "view/ghost"})),
        ("rdy_log", json!({"scope": "view/ghost"})),
        ("rdy_view", json!({"op": "show", "name": "ghost"})),
        ("rdy_land", json!({"view": "ghost"})),
    ] {
        x.with(tool, Kind::Refusal, with_kungfu(tool, args), a.clone())
            .await;
    }
    x.with(
        "rdy_session",
        Kind::Refusal,
        json!({"actor": "agent:a", "run": "a1", "view": "ghost"}),
        a.clone(),
    )
    .await;
    // stale briefing tokens: one never issued, one that a later write made stale
    let brief = x
        .with(
            "rdy_brief",
            Kind::Result,
            with_kungfu("rdy_brief", json!({"path": "spec"})),
            a.clone(),
        )
        .await;
    let token = brief["base"]["token"].as_str().unwrap().to_string();
    let target = x.sut.spec.a.to_string();
    let write = |t: &str, body: &str| {
        with_kungfu(
            "rdy_write",
            json!({"ops": [{"op": "update", "id": target, "body": body}], "brief": t}),
        )
    };
    x.with(
        "rdy_write",
        Kind::Refusal,
        write("not-a-token", "A zero."),
        a.clone(),
    )
    .await;
    x.with(
        "rdy_write",
        Kind::Result,
        write(&token, "A one."),
        a.clone(),
    )
    .await;
    x.with(
        "rdy_write",
        Kind::Result,
        write(&token, "A two."),
        a.clone(),
    )
    .await;
    // a draft write with a stale token goes ahead and says so
    let stale = &x.rows.last().unwrap().reply["result"]["content"][0]["text"]["warnings"];
    assert_eq!(stale[0]["code"], "W_BRIEF_STALE", "{stale}");
    // the claim that was refused left nothing: its own writes are one view's
    x.with(
        "rdy_overview",
        Kind::Effect,
        with_kungfu("rdy_overview", json!({})),
        a,
    )
    .await;
}

/// `args` with this source's kungfu value for `tool`.
fn with_kungfu(tool: &str, mut args: Value) -> Value {
    supply_kungfu(tool, &mut args);
    args
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht8_claim_parse_kungfu_and_error_order_match_stdio() {
    let rows = same_on_every_wire(Setup::default, Scenario::Claims).await;
    // the table is not all one outcome: refusals and successes both ran
    let refused = rows.iter().filter(|r| r.err).count();
    assert!(refused >= 25, "{refused} refusals");
    assert!(rows.iter().any(|r| !r.err));
    // the parser's order is the stdio parser's: an unknown key is named with its nearest
    let first = &rows[0].reply["result"]["content"][0]["text"];
    assert_eq!(first["code"], "E_BAD_INPUT", "{first}");
    // the missing-claim refusal is the shared one on each wire
    let last_malformed = &rows[13].reply["result"]["content"][0]["text"];
    assert_eq!(last_malformed["code"], "E_NO_SESSION", "{last_malformed}");
}

// ------------------------------------------------------------------------ budgets and marks

/// A doc of `n` claims, as one write.
fn many(n: usize) -> Value {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "many", "title": "Many", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Claims"}}),
    ];
    for i in 0..n {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s", "body": format!("Claim number {i} holds.")}));
    }
    json!({ "ops": ops })
}

/// Token budgets, cut and paged reads, the instructions reminder, and the once-only warnings.
async fn budgets(x: &mut Run<'_>) {
    x.ok(
        "rdy_session",
        json!({"actor": ACTOR, "run": RUN, "role": "writer"}),
    )
    .await;
    let made = x.ok("rdy_write", many(120)).await;
    let doc = made["created"]["$d"].as_str().unwrap().to_string();
    // the reminder is due until this claim reads its instructions, then it is not
    let key = rdy_engine::instructions::REMINDER_KEY;
    let ov = x.ok("rdy_overview", json!({})).await;
    assert!(ov.get(key).is_some(), "{ov}");
    // budgets: the default, a small one and a tiny one cut the same way on each wire
    for budget in [50, 120, 400, 100_000] {
        x.ok("rdy_overview", json!({"max_tokens": budget})).await;
        x.ok(
            "rdy_search",
            json!({"query": "Claim", "max_tokens": budget}),
        )
        .await;
        x.ok("rdy_log", json!({"limit": 50, "max_tokens": budget}))
            .await;
        x.ok("rdy_pending", json!({"max_tokens": budget})).await;
    }
    // a read cut into pages continues on the same claim's later request, to the end
    let mut next = None;
    for _ in 0..4 {
        let args = match &next {
            None => json!({"id": doc, "max_tokens": 300}),
            Some(p) => json!({"page": p, "max_tokens": 300}),
        };
        let page = x.ok("rdy_read", args).await;
        match page["next"].as_str() {
            Some(p) => next = Some(p.to_string()),
            None => break,
        }
    }
    assert!(next.is_some(), "the doc is cut into pages");
    // a page that is not this claim's: the same refusal
    x.step("rdy_read", Kind::Refusal, json!({"page": "nonsense"}))
        .await;
    // the reminder follows the instructions
    x.ok("rdy_instructions", json!({"load": ["rdy:read"]}))
        .await;
    let ov = x.ok("rdy_overview", json!({})).await;
    assert!(ov.get(key).is_none(), "{ov}");
    // kungfu warnings: missing and stale, once per owner; the warning count is compared
    for args in [json!({}), json!({}), json!({"kungfu": "kf-00000000"})] {
        x.bare("rdy_tags", Kind::Result, args, None).await;
    }
    for args in [json!({"query": "x"}), json!({"query": "x"})] {
        x.bare("rdy_search", Kind::Result, args, None).await;
    }
    let warned = |r: &Row| {
        r.reply["result"]["content"][0]["text"]["warnings"]
            .as_array()
            .map_or(0, Vec::len)
    };
    let counts: Vec<usize> = x.rows.iter().rev().take(5).map(warned).collect();
    assert!(
        counts.iter().any(|n| *n > 0),
        "a kungfu warning went out: {counts:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht8_budgets_reminders_and_warnings_match_stdio() {
    let rows = same_on_every_wire(Setup::default, Scenario::Budgets).await;
    // the budgets cut: a tiny budget gives a shorter reply than the large one
    let size = |tool: &str, n: usize| -> usize {
        rows.iter()
            .filter(|r| r.tool == tool && r.kind == Kind::Result)
            .nth(n)
            .map_or(0, |r| r.reply.to_string().len())
    };
    assert!(size("rdy_overview", 1) < size("rdy_overview", 4) + 1);
    // the daemon's build warning goes out once per actor and run, on each wire alike
    let build = || Setup {
        build: Some(DaemonBuildWarning {
            version: "1.2.3".into(),
            daemon_build: "0123456789abcdef".into(),
            client_build: "fedcba9876543210".into(),
        }),
        ..Setup::default()
    };
    let rows = same_on_every_wire(build, Scenario::Budgets).await;
    let owed: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r.reply["result"]["content"][0]["text"]["warnings"]
                .as_array()
                .is_some_and(|w| w.iter().any(|w| w["code"] == "W_DAEMON_BUILD"))
        })
        .map(|(i, _)| i)
        .collect();
    assert_eq!(owed.len(), 1, "once per actor and run: {owed:?}");
}

// ------------------------------------------------------------------------ person layer

const PERSON_RULE: &str = "Keep every answer short.";

fn add_rule(scope: &str, text: &str) -> Value {
    json!({"scope": scope, "op": "add", "text": text, "why": "The person asked for it.",
        "evidence": "Please keep this rule.", "question": "Shall I save this rule?",
        "session_ref": "chat-1"})
}

/// Instructions, rule forwarding and Repo links, on a world whose person layer is its own meta
/// project in its own home.
async fn person(x: &mut Run<'_>) {
    x.ok(
        "rdy_session",
        json!({"actor": ACTOR, "run": RUN, "role": "writer"}),
    )
    .await;
    let full = x.ok("rdy_instructions", json!({"full": true})).await;
    let shown = full.to_string();
    assert!(
        shown.contains(PERSON_RULE),
        "the person's rule is read: {shown}"
    );
    x.ok("rdy_instructions", json!({"load": ["rdy:bootstrap"]}))
        .await;
    x.ok("rdy_overview", json!({})).await;
    // a project rule and a person rule: the second one goes through the host to the meta project
    x.ok("rdy_rule", add_rule("project", "Say what you changed."))
        .await;
    assert!(
        x.rows.last().is_some_and(|r| !r.err),
        "{:?}",
        x.rows.last().map(|r| &r.reply)
    );
    x.ok("rdy_rule", add_rule("person", "Answer in British English."))
        .await;
    let forwarded = x.rows.last().is_some_and(|r| !r.err);
    assert!(forwarded, "{:?}", x.rows.last().map(|r| &r.reply));
    let again = x.ok("rdy_instructions", json!({"full": true})).await;
    assert!(
        again.to_string().contains("Answer in British English."),
        "the person rule reaches the person layer: {again}"
    );
    x.step("rdy_rule", Kind::Refusal, add_rule("team", "No scope."))
        .await;
    x.step(
        "rdy_rule",
        Kind::Refusal,
        json!({"scope": "project", "op": "remove", "id": "ai-00000", "why": "Gone.",
            "evidence": "Drop it.", "session_ref": "chat-1"}),
    )
    .await;
    // a reader cannot write a rule
    let reader = with_key(agent_claim(ACTOR, RUN), "rdy/role", json!("reader"));
    x.with(
        "rdy_rule",
        Kind::Refusal,
        with_kungfu("rdy_rule", add_rule("person", "Not by a reader.")),
        reader,
    )
    .await;
    // Repo links: no link is configured in the fixture, and every form answers the same
    x.ok("rdy_links", json!({})).await;
    x.ok("rdy_links", json!({"op": "refs"})).await;
    x.step(
        "rdy_links",
        Kind::Refusal,
        json!({"op": "show", "alias": "nope"}),
    )
    .await;
    x.step("rdy_links", Kind::Refusal, json!({"op": "show"}))
        .await;
    x.step(
        "rdy_links",
        Kind::Refusal,
        json!({"op": "list", "alias": "core"}),
    )
    .await;
    x.step("rdy_links", Kind::Refusal, json!({"op": "export"}))
        .await;
    x.ok("rdy_policy", json!({"op": "show"})).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht8_instructions_rules_and_repo_links_use_isolated_person_host() {
    let setup = || Setup {
        person_rule: Some(PERSON_RULE),
        ..Setup::default()
    };
    let mut reference: Option<Vec<Row>> = None;
    for via in WIRES {
        let (rows, sut) = run_on(via, setup(), Scenario::Person).await;
        assert!(rows.iter().any(|r| r.tool == "rdy_rule" && !r.err));
        assert!(rows.iter().any(|r| r.tool == "rdy_links" && !r.err));
        assert!(rows.iter().any(|r| r.tool == "rdy_links" && r.err));
        // the world reads and writes its own meta project under its own isolated home
        let (_, env) = sut.world.path_env();
        let home = sut.world.home.path().to_path_buf();
        assert_eq!(env.home.as_deref().map(Path::new), Some(home.as_path()));
        let meta = sut.world.default_meta();
        assert!(meta.starts_with(&home), "{}", meta.display());
        // the forwarded rule landed in that project, and the person's own rule is there too
        let r = Rdy::open(&meta).expect("the world's meta project");
        let st = r.read();
        let held: Vec<String> = st
            .materialized(&Scope::Canon)
            .unwrap()
            .iter()
            .map(|b| b.body.clone())
            .collect();
        drop(st);
        r.close().unwrap();
        for text in [PERSON_RULE, "Answer in British English."] {
            assert!(
                held.iter().any(|b| b.contains(text)),
                "{via:?}: the meta project holds {text:?}: {held:?}"
            );
        }
        // the project's own rule stays in the project, and the person's goes nowhere near it
        let mut sut = sut;
        let mine = sut
            .call(
                "rdy_search",
                json!({"query": "Say what you changed", "scope": "canon"}),
                None,
            )
            .await;
        assert!(body(&mine)["total"].as_u64() > Some(0), "{mine}");
        let theirs = sut
            .call(
                "rdy_search",
                json!({"query": "British English", "scope": "canon"}),
                None,
            )
            .await;
        assert_eq!(body(&theirs)["total"], 0, "{theirs}");
        match &reference {
            None => reference = Some(rows),
            Some(want) => assert_same(via, &rows, want),
        }
    }
}

// ------------------------------------------------------------------------ signing

/// The keys of the fixture, by fingerprint, and `sign: never`: an agent key is configured and
/// the world has no agent to sign with.
fn write_signing_config(dir: &Path) {
    use rdy_qa::signing::{QaAgent, TestKey};
    std::fs::write(
        dir.join(".rdy/local.yaml"),
        format!(
            "actor: human:qa\nsigning:\n  key: {:?}\n  agent_key: {:?}\n  sign: never\n  allowed_signers: null\n",
            QaAgent::fingerprint(TestKey::Person),
            QaAgent::fingerprint(TestKey::Agent)
        ),
    )
    .unwrap();
}

/// `rdy_sign` and `rdy_key_bind` with no caller agent: the ordinary refusal, never the daemon's.
async fn signing(x: &mut Run<'_>) {
    x.ok(
        "rdy_session",
        json!({"actor": ACTOR, "run": RUN, "role": "writer"}),
    )
    .await;
    // the agent writes, proposes and relays the person's approval: a landing it may sign later
    let section = x.sut.spec.section.to_string();
    x.ok(
        "rdy_write",
        json!({"ops": [{"op": "create", "type": "claim", "parent": section, "body": "S signs later."}]}),
    )
    .await;
    x.ok(
        "rdy_propose",
        json!({"title": "S signs later", "summary": "Adds one claim."}),
    )
    .await;
    let queue = x.ok("rdy_pending", json!({"max_tokens": 100_000})).await;
    let item = queue["items"]
        .as_array()
        .and_then(|all| all.iter().find(|i| i["kind"] == "approve"))
        .unwrap_or_else(|| panic!("an approval card: {queue}"))
        .clone();
    x.ok(
        "rdy_answer",
        json!({"ref": item["ref"], "option": "approve", "card_hash": item["card_hash"],
            "evidence": "Yes, land it.", "question": "Shall I land it?"}),
    )
    .await;
    assert!(
        x.rows.last().is_some_and(|r| !r.err),
        "{:?}",
        x.rows.last().map(|r| &r.reply)
    );
    let log = x.ok("rdy_log", json!({"limit": 1})).await;
    let landing = log[0]["landing"].as_str().unwrap_or("none").to_string();
    for kind in [json!(null), json!("relay"), json!("approval"), json!("tag")] {
        let mut args = json!({"target": landing});
        if !kind.is_null() {
            args["kind"] = kind;
        }
        x.step("rdy_sign", Kind::Result, args).await;
    }
    x.step("rdy_sign", Kind::Refusal, json!({"target": "tag:9.9.9"}))
        .await;
    x.step(
        "rdy_sign",
        Kind::Refusal,
        json!({"target": landing, "kind": "bogus"}),
    )
    .await;
    for kind in ["agent", "person"] {
        x.step("rdy_key_bind", Kind::Result, json!({"kind": kind}))
            .await;
    }
    x.step("rdy_key_bind", Kind::Refusal, json!({"kind": "robot"}))
        .await;
    x.step("rdy_key_show", Kind::Result, json!({})).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht8_http_signing_uses_no_daemon_agent_fallback() {
    let rows = same_on_every_wire(Setup::default, Scenario::Signing).await;
    let sign: Vec<&Row> = rows
        .iter()
        .filter(|r| r.tool == "rdy_sign" && r.kind == Kind::Result)
        .collect();
    assert_eq!(sign.len(), 4);
    for r in sign.iter().chain(
        rows.iter()
            .filter(|r| r.tool == "rdy_key_bind" && r.kind == Kind::Result)
            .collect::<Vec<_>>()
            .iter(),
    ) {
        assert!(r.err, "no caller agent: {}", r.reply);
    }
    // the default kind for an agent is a relay, which needs the caller's own agent: there is
    // none, on any wire, and no daemon agent stands in for it
    for r in sign.iter().take(2) {
        let e = &r.reply["result"]["content"][0]["text"];
        assert_eq!(e["code"], "E_NO_AGENT", "{e}");
        assert!(
            e["message"].as_str().unwrap().contains("SSH_AUTH_SOCK"),
            "{e}"
        );
    }
}

// ------------------------------------------------------------ the normalizer itself

fn norm_with(v: Value, known: &[(&str, &str)]) -> Value {
    let seen: Seen = Seen::default();
    for (id, name) in known {
        seen.borrow_mut()
            .of
            .insert(("id", id.to_string()), name.to_string());
    }
    normalized(&v, &[], &masks(), &seen)
}

#[test]
fn mht8_normalization_keeps_what_is_not_generated() {
    let wrap = |t: Value| json!({"result": {"content": [{"text": t}]}});
    // generated values of the same shape agree; a changed meaning does not
    let same = |a: Value, b: Value| norm_with(wrap(a), &[]) == norm_with(wrap(b), &[]);
    assert!(same(
        json!({"id": "d-AAAAA", "at": "2026-01-01T00:00:00Z"}),
        json!({"id": "d-BBBBB", "at": "2027-02-02T01:01:01Z"})
    ));
    // sizes, counts, other digit strings and claims are kept
    assert!(!same(
        json!({"drop_bytes": 393}),
        json!({"drop_bytes": 394})
    ));
    assert!(!same(json!({"total": 123456}), json!({"total": 123457})));
    assert!(!same(
        json!({"message": "over 16777216 bytes"}),
        json!({"message": "over 4194304 bytes"})
    ));
    assert!(!same(
        json!({"claim": "A holds 12345"}),
        json!({"claim": "A holds 12346"})
    ));
    // two numbers of eight or more digits are not hashes: in a message, a claim, a fix, or on
    // their own, they stay different
    assert!(!same(
        json!({"message": "over 16777216 bytes"}),
        json!({"message": "over 33554432 bytes"})
    ));
    assert!(!same(
        json!({"claim": "A holds 16777216"}),
        json!({"claim": "A holds 33554432"})
    ));
    assert!(!same(
        json!({"fix": "send at most 16777216 bytes"}),
        json!({"fix": "send at most 33554432 bytes"})
    ));
    assert!(!same(
        json!({"hash": "1234567890123456"}),
        json!({"hash": "6543210987654321"})
    ));
    assert!(!same(json!({"code": "E_A"}), json!({"code": "E_B"})));
    // hashes and proposal refs at the members and in the texts that carry them read as names
    let h = |c: char, n: usize| c.to_string().repeat(n);
    assert!(same(
        json!({"hash": h('a', 64), "repo_id": h('b', 32),
            "message": format!("this session cannot sign relay for {}", h('c', 64)),
            "line": format!("P-{}@1 waits", h('d', 12)),
            "card": format!("D-AAAAA@{}", h('e', 12)),
            "id": format!("cq-{}", h('f', 16))}),
        json!({"hash": h('1', 64), "repo_id": h('2', 32),
            "message": format!("this session cannot sign relay for {}", h('3', 64)),
            "line": format!("P-{}@1 waits", h('4', 12)),
            "card": format!("D-BBBBB@{}", h('5', 12)),
            "id": format!("cq-{}", h('9', 16))})
    ));
    // a hash is one value: two members that held one hash, and then two, differ
    assert!(!same(
        json!({"hash": h('a', 64), "to": h('a', 64)}),
        json!({"hash": h('a', 64), "to": h('b', 64)})
    ));
    // a hash at a member that is not an identity member, or in an unanchored text, is kept
    assert!(!same(
        json!({"note": h('a', 64)}),
        json!({"note": h('b', 64)})
    ));
    assert!(!same(
        json!({"message": format!("see {}", h('a', 64))}),
        json!({"message": format!("see {}", h('b', 64))})
    ));
    // an identity is one value: two ids that swap places, or merge, differ
    assert!(!same(
        json!({"x": "d-AAAAA", "y": "d-BBBBB", "z": "d-AAAAA"}),
        json!({"x": "d-AAAAA", "y": "d-BBBBB", "z": "d-BBBBB"})
    ));
    // a list that is not named as id-ordered keeps its order, even with ids in it
    assert!(!same(
        json!({"rows": ["d-AAAAA x", "d-BBBBB y"]}),
        json!({"rows": ["d-BBBBB y", "d-AAAAA x"]})
    ));
    assert!(!same(
        json!({"steps": ["b", "a"]}),
        json!({"steps": ["a", "b"]})
    ));
    // a list the engine orders by id compares in the order of the fixture's names
    let known = [("d-AAAAA", "a"), ("d-BBBBB", "b")];
    assert_eq!(
        norm_with(
            wrap(json!({"consequences": [{"block": "d-BBBBB"}, {"block": "d-AAAAA"}]})),
            &known
        ),
        norm_with(
            wrap(json!({"consequences": [{"block": "d-AAAAA"}, {"block": "d-BBBBB"}]})),
            &known
        )
    );
    // a port is masked, a size next to it is not
    assert!(same(
        json!({"url": "http://127.0.0.1:41234/x", "n": 7}),
        json!({"url": "http://127.0.0.1:51234/x", "n": 7})
    ));
    assert!(!same(
        json!({"url": "http://127.0.0.1:41234/x", "n": 7}),
        json!({"url": "http://127.0.0.1:51234/x", "n": 8})
    ));
}
