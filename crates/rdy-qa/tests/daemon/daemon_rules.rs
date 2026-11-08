//! rtc4 (FW-MH.rules, D-ZSRBN): a person-scoped rule reaches the meta project through the host, as
//! the caller's actor and run. Meta's own daemon holds its store and its writer; the source project
//! is not touched; a reader or a person never gets as far as a connection; the signer that answers
//! is the caller's own, and nothing waits on a writer while it does. A lost reply is reported, and
//! only an explicit repeat reads the receipt.
//!
//! The caller's side runs in this process, on a `Local` of the source project with a host from
//! `rdy-daemon`. Meta's daemon is the real `rdy`, in the world's isolated environment. Every person
//! lives in an isolated home.
use ed25519_dalek::Signer as _;
use rdy_daemon::error::ErrorReply;
use rdy_daemon::instructions::PersonReader;
use rdy_daemon::{Proto, RunDir, StartOptions, connect_or_start, serve_command, spawn_serve};
use rdy_engine::engine::{Engine, Local, with_writer_result};
use rdy_engine::instructions::{ForwardedRule, PersonHost};
use rdy_engine::rule_types::parse_request;
use rdy_engine::rules::{RuleFault, RuleOrigin, route_rule, with_rule_fault_for_tests};
use rdy_engine::signing_crypto::{AgentIdentity, SigningAgent, blob_fingerprint};
use rdy_engine::{
    E_BAD_INPUT, E_EXCEPTION_OPEN, E_INTERNAL, E_ITEM_MOVED, E_LOCKED, E_NO_AGENT, E_NO_PROJECT,
    E_NOT_FOUND, E_PERMISSION, E_PROPOSAL_MOVED, E_PROPOSAL_NOT_READY, E_STALE_BASE, E_VALIDATION,
    NoAgentCause, Rdy, RdyError, Scope, Session, WriteRequest, no_agent,
};
use rdy_model::{EvidenceKind, Hash, Justification, Role};
use rdy_qa::fixtures::{land, parse_test_ops, rdy_bin, write, writer};
use rdy_qa::instructions::World;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

const ME: &str = "agent:a";
const PERSON: &str = "human:qa";
const GONE: &str = "E_DAEMON_GONE";
const GONE_FIX: &str = "check rdy status; a write may or may not have committed; it is not retried";
/// How long a test waits for a thread that should end, so a deadlock fails instead of hanging. No
/// assertion depends on how long anything took.
const HANG_GUARD: Duration = Duration::from_secs(120);

// ---- a fixed in-memory key and an agent ----

const SEED: [u8; 32] = [7; 32];

fn ssh_string(bytes: &[u8]) -> Vec<u8> {
    let mut out = (bytes.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(bytes);
    out
}

fn key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&SEED)
}

fn blob() -> Vec<u8> {
    [
        ssh_string(b"ssh-ed25519"),
        ssh_string(&key().verifying_key().to_bytes()),
    ]
    .concat()
}

type Hook = Box<dyn FnOnce() + Send>;

/// The caller's agent, holding the one key. `hook` runs before it answers its first sign request.
#[derive(Default)]
struct FakeAgent {
    signs: AtomicUsize,
    refuse: bool,
    hook: Mutex<Option<Hook>>,
}

impl FakeAgent {
    fn plain() -> Arc<FakeAgent> {
        Arc::new(FakeAgent::default())
    }

    fn hooked(hook: impl FnOnce() + Send + 'static) -> Arc<FakeAgent> {
        Arc::new(FakeAgent {
            hook: Mutex::new(Some(Box::new(hook))),
            ..FakeAgent::default()
        })
    }

    fn refusing() -> Arc<FakeAgent> {
        Arc::new(FakeAgent {
            refuse: true,
            ..FakeAgent::default()
        })
    }

    fn signs(&self) -> usize {
        self.signs.load(Ordering::SeqCst)
    }
}

impl SigningAgent for FakeAgent {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        Ok(vec![AgentIdentity { public_key: blob() }])
    }

    fn sign(&self, _public_key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        self.signs.fetch_add(1, Ordering::SeqCst);
        let hook = self.hook.lock().unwrap().take();
        if let Some(h) = hook {
            h();
        }
        if self.refuse {
            return Err(no_agent(NoAgentCause::Refused));
        }
        let raw = key().sign(payload).to_bytes();
        Ok([ssh_string(b"ssh-ed25519"), ssh_string(&raw)].concat())
    }
}

/// The signing settings of the project at `dir`, with this file's key when `agent_key`.
fn signing(dir: &Path, sign: &str, agent_key: bool) {
    let agent_key = if agent_key {
        format!("\"{}\"", blob_fingerprint(&blob()))
    } else {
        "null".to_string()
    };
    std::fs::write(
        dir.join(".rdy/local.yaml"),
        format!(
            "signing:\n  key: null\n  agent_key: {agent_key}\n  sign: {sign}\n  allowed_signers: null\n"
        ),
    )
    .expect("local config");
}

// ---- the host and the caller ----

/// A host of world `w` and how many daemons it started: each one is the real `rdy`, in `w`'s
/// environment.
fn reader(w: &World, direct: bool) -> (Arc<PersonReader>, Arc<AtomicUsize>) {
    let (platform, env) = w.path_env();
    let version = env!("CARGO_PKG_VERSION");
    let r = if direct {
        PersonReader::direct(version)
    } else {
        PersonReader::daemon(version)
    };
    let starts = Arc::new(AtomicUsize::new(0));
    let (n, vars) = (Arc::clone(&starts), w.env());
    let r =
        r.with_env(platform, env)
            .with_starter(Arc::new(move |project: &Path, run: &RunDir| {
                n.fetch_add(1, Ordering::SeqCst);
                let mut cmd = serve_command(&rdy_bin(), project, run)?;
                cmd.envs(vars.iter().map(|(k, v)| (k, v)));
                spawn_serve(&mut cmd)
            }));
    (Arc::new(r), starts)
}

fn source(dir: &Path, host: Arc<PersonReader>) -> Local {
    signing(dir, "never", false);
    Local::open(dir)
        .expect("open the source project")
        .with_person_host(host)
}

fn me() -> Session {
    session_of(ME, "r1", Role::Writer)
}

fn session_of(actor: &str, run: &str, role: Role) -> Session {
    Session::agent(actor.into(), Some(run.into()), role).unwrap()
}

/// One `rdy_rule` request from `e`'s session `s`, signing with `agent`.
fn call_with(
    e: &dyn Engine,
    s: &Session,
    agent: Arc<dyn SigningAgent>,
    v: Value,
) -> Result<Value, RdyError> {
    let q = parse_request(&v).unwrap_or_else(|e| panic!("{v}: {e}"));
    let host = e.person_host();
    route_rule(
        Some(e),
        host.as_deref(),
        &RuleOrigin {
            session: s,
            relayed_by: None,
        },
        &q,
        agent,
    )
}

fn call(e: &dyn Engine, s: &Session, v: Value) -> Result<Value, RdyError> {
    call_with(e, s, FakeAgent::plain(), v)
}

fn add(scope: &str, text: &str) -> Value {
    json!({"scope": scope, "op": "add", "text": text, "why": "They asked.",
           "evidence": "yes, keep it", "question": "Save this?", "session_ref": "chat-1"})
}

fn edit(scope: &str, id: &str, text: &str) -> Value {
    json!({"scope": scope, "op": "edit", "id": id, "text": text, "why": "Wider.",
           "evidence": "change it", "session_ref": "chat-1"})
}

fn remove(scope: &str, id: &str) -> Value {
    json!({"scope": scope, "op": "remove", "id": id, "why": "No longer wanted.",
           "evidence": "drop it", "session_ref": "chat-1"})
}

fn answer(scope: &str, item: &str, option: &str, hash: &str) -> Value {
    json!({"scope": scope, "answer": {"ref": item, "option": option, "card_hash": hash,
           "evidence": "Yes, as the card says.", "question": "This card?"}})
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_else(|| panic!("{key} in {v}"))
}

fn landed_hash(v: &Value) -> Hash {
    v["landing"]["landing"]
        .as_str()
        .unwrap_or_else(|| panic!("a landing in {v}"))
        .parse()
        .unwrap()
}

/// What a project's state shows: its canon head, its views with their changes, and its landings.
fn state_of(st: &rdy_engine::ReadState) -> (Option<Hash>, Vec<(String, usize)>, usize) {
    (
        st.canon_head(),
        st.views_with_changes(),
        st.log_rows().unwrap().len(),
    )
}

fn state(e: &Local) -> (Option<Hash>, Vec<(String, usize)>, usize) {
    state_of(&e.read())
}

/// The state of the closed project at `dir`: its store opened and closed here.
fn closed_state(dir: &Path) -> (Option<Hash>, Vec<(String, usize)>, usize) {
    let r = Rdy::open(dir).expect("open the closed project");
    let out = state_of(&r.read());
    r.close().expect("close");
    out
}

/// Runs `f` on the store of the project at `dir`, its daemon stopped, then closes it.
fn with_closed<T>(w: &World, dir: &Path, f: impl FnOnce(&mut Rdy) -> T) -> T {
    w.stop(dir);
    let mut r = Rdy::open(dir).expect("open the closed project");
    let out = f(&mut r);
    r.close().expect("close");
    out
}

/// One ctl request to the daemon of `dir`, as `actor` and `run`; starts that daemon when none runs.
fn ctl(
    w: &World,
    dir: &Path,
    actor: &str,
    run: &str,
    op: &str,
    args: Value,
) -> Result<Value, ErrorReply> {
    let (project, env) = (dir.to_path_buf(), w.env());
    let opts = StartOptions::new(
        env!("CARGO_PKG_VERSION").to_string(),
        Proto::Ctl,
        dir.to_path_buf(),
        Box::new(move |r| {
            let mut cmd = serve_command(&rdy_bin(), &project, r)?;
            cmd.envs(env.iter().map(|(k, v)| (k, v)));
            spawn_serve(&mut cmd)
        }),
    );
    let run_dir = RunDir::of_project(dir);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = connect_or_start(&run_dir, opts).await?;
        c.ctl("session", json!({"actor": actor, "run": run}))
            .await?;
        let v = c.ctl(op, args).await;
        c.close();
        v
    })
}

/// Lands in the closed store `r` a doc with the policy `fields` and the decision that adopts it.
fn land_policy(r: &mut Rdy, fields: Value) {
    let w = write(
        r,
        "pol",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
    )
    .unwrap_or_else(|e| panic!("write the rules: {e}"));
    r.write(
        &writer(),
        WriteRequest {
            view: "pol".into(),
            ops: parse_test_ops(json!([
                {"op": "create", "type": "authority_policy", "id": "$p", "parent": w.created["$d"], "fields": fields}
            ]))
            .unwrap(),
            justification: Some(Justification::Implements(vec![
                rdy_model::BlockId::parse(&w.created["$dec"]).unwrap(),
            ])),
            message: String::new(),
        },
    )
    .unwrap_or_else(|e| panic!("write the policy: {e}"));
    land(r, "pol").unwrap_or_else(|e| panic!("land the policy: {e}"));
}

fn policy(extra: Value) -> Value {
    let mut base = json!({
        "key": "authority-policy",
        "agent_may_accept": [],
        "roles": {},
        "require": [],
        "owners": []
    });
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    base
}

/// The ids and bodies of the live instructions of the closed project at `dir`.
fn rules_in(dir: &Path) -> Vec<(String, String)> {
    let r = Rdy::open(dir).expect("open the closed project");
    let st = r.read();
    let m = st.materialized(&Scope::Canon).unwrap();
    let mut out: Vec<(String, String)> = m
        .iter()
        .filter(|b| b.block_type == "instruction")
        .map(|b| (b.id.to_string(), b.body.clone()))
        .collect();
    out.sort();
    drop(m);
    drop(st);
    r.close().expect("close");
    out
}

/// The names of the root docs of the closed project at `dir`.
fn docs_in(dir: &Path) -> Vec<String> {
    let r = Rdy::open(dir).expect("open the closed project");
    let st = r.read();
    let m = st.materialized(&Scope::Canon).unwrap();
    let mut out: Vec<String> = m
        .iter()
        .filter(|b| b.block_type == "doc" && b.parent.is_none())
        .filter_map(|b| b.field_str("name").map(ToString::to_string))
        .collect();
    out.sort();
    drop(m);
    drop(st);
    r.close().expect("close");
    out
}

/// Kills the daemon whose pid file is `pid_file`, and returns once it is gone.
fn kill_daemon(pid_file: &Path) {
    // the record is the pid alone on Unix and the pid with a creation time on Windows
    let pid = rdy_daemon::sys::parse_pid_record(&std::fs::read(pid_file).unwrap())
        .expect("a pid record")
        .pid();
    rdy_qa::platform::kill_hard(pid);
    while !rdy_qa::platform::process_exited(pid) {
        std::thread::yield_now();
    }
}

// ---- tests ----

#[test]
fn rtc4_person_rule_uses_meta_writer_and_caller_identity() {
    let w = World::new();
    let (md, pd) = (w.project(), w.project());
    w.person(md.path());
    let (host, starts) = reader(&w, false);
    let src = source(pd.path(), host);
    // the caller has work of its own in its view in the source project
    with_writer_result(&src, move |r| {
        write(
            r,
            &me().default_view(),
            json!([{"op": "create", "type": "doc", "fields": {"name": "wip", "title": "Wip", "doc_type": "page"}}]),
        )
        .map(drop)
    })
    .unwrap();
    let before = state(&src);
    let meta_repo = {
        let r = Rdy::open(md.path()).unwrap();
        let id = r.read().repo_id().unwrap().to_string();
        r.close().unwrap();
        id
    };

    let s = me();
    let v = call(&src, &s, add("person", "Say what you changed.")).unwrap();
    assert_eq!(v["scope"], "person", "{v}");
    assert_eq!(v["project"]["repo_id"], meta_repo.as_str(), "{v}");
    assert_eq!(v["rule"]["text"], "Say what you changed.", "{v}");
    let landing = landed_hash(&v);
    assert_eq!(
        starts.load(Ordering::SeqCst),
        1,
        "one start, then its daemon"
    );
    assert_eq!(state(&src), before, "the source project is not touched");

    let pid = w.daemon_pid(md.path()).expect("meta's daemon stays");
    let held = Rdy::open(md.path()).map(drop).unwrap_err();
    assert_eq!(
        held.code, E_LOCKED,
        "meta's daemon holds its store: {held:?}"
    );
    assert!(rdy_qa::platform::process_running(pid));

    // in meta: the landing records the caller as the relayer and the configured person
    with_closed(&w, md.path(), |r| {
        let st = r.read();
        let rec = st.reader().landing(&landing).unwrap();
        let by = rec.relayed_by.as_ref().expect("relayed");
        assert_eq!(by.actor, ME);
        assert_eq!(by.run.as_deref(), Some("r1"));
        assert_eq!(rec.approved_by.actor, PERSON);
        assert_eq!(rec.evidence.as_ref().unwrap().kind, EvidenceKind::Text);
        let quote = rec.relay_evidence.as_ref().unwrap();
        assert_eq!(quote.quoted, "yes, keep it");
        assert_eq!(quote.session_ref, "chat-1");
        // only the request's own view changed, and the caller's view in meta was never made
        assert!(st.view_head(&s.default_view()).is_none());
        let views = st.views_with_changes();
        assert!(
            views.iter().all(|(n, _)| n.starts_with("rule-")),
            "{views:?}"
        );
        let m = st.materialized(&Scope::Canon).unwrap();
        let rule = m
            .get(&rdy_model::BlockId::parse(str_of(&v["rule"], "id")).unwrap())
            .expect("the rule is in meta's canon");
        assert_eq!(rule.body, "Say what you changed.");
    });
    assert_eq!(state(&src), before);
    src.close().unwrap();
}

/// A host that fails the test if anything asks it for another project.
struct NoSelfCall(Arc<PersonReader>);

impl PersonHost for NoSelfCall {
    fn locate(&self) -> Result<rdy_engine::instructions_config::PersonSource, RdyError> {
        self.0.locate()
    }

    fn person_actor(&self, project: &Path) -> Result<String, RdyError> {
        self.0.person_actor(project)
    }

    fn fetch(
        &self,
        _: &Path,
        _: &rdy_engine::instructions::LoadRequest,
        _: &rdy_engine::instructions::Caller,
    ) -> Result<rdy_engine::instructions::Contribution, RdyError> {
        panic!("a session on meta asked another project to read")
    }

    fn write_rule(&self, _: &Path, _: &ForwardedRule<'_>) -> Result<Value, RdyError> {
        panic!("a session on meta asked another project to write")
    }
}

#[test]
fn rtc4_meta_session_scopes_share_one_destination() {
    let w = World::new();
    let (md, pd) = (w.project(), w.project());
    // the configured path is another spelling of the same directory
    let link = w.home.path().join("meta-link");
    rdy_qa::platform::symlink(md.path(), &link).unwrap();
    // A link made by a standard user on a Windows build box can be refused on traversal
    // (ERROR_UNTRUSTED_MOUNT_POINT, 448; ruled at Task 7). The second spelling is then a
    // path with a `.` step, which names the same directory without a link.
    let link = match std::fs::symlink_metadata(link.join(".rdy.yaml")) {
        Err(e) if e.raw_os_error() == Some(448) => md.path().join("."),
        _ => link,
    };
    w.person(&link);
    signing(md.path(), "never", false);
    let (host, starts) = reader(&w, false);
    let on_meta = Local::open(md.path())
        .unwrap()
        .with_person_host(Arc::new(NoSelfCall(Arc::clone(&host))));
    let s = me();

    let first = call(&on_meta, &s, add("person", "One default doc.")).unwrap();
    assert_eq!(first["scope"], "person");
    // the other spelling of the same request is the same request: its landing, no second one
    let same = call(&on_meta, &s, add("project", "One default doc.")).unwrap();
    assert_eq!(same["scope"], "project");
    assert_eq!(landed_hash(&same), landed_hash(&first), "{same}");
    assert_eq!(state(&on_meta).2, 1, "one landing");
    // new words under the other spelling share its default doc
    let more = call(&on_meta, &s, add("project", "Still one default doc.")).unwrap();
    assert_eq!(more["rule"]["ruleset"], first["rule"]["ruleset"], "{more}");
    on_meta.close().unwrap();
    assert_eq!(docs_in(md.path()), ["my-rules"], "My rules for both scopes");
    assert_eq!(starts.load(Ordering::SeqCst), 0, "no daemon was asked");

    // meta's daemon, on its own ctl op: both spellings run here, in the same doc
    let relayed = |mut v: Value| {
        v["relayed_by"] = json!(ME);
        v
    };
    let v = ctl(
        &w,
        md.path(),
        ME,
        "r1",
        "rule",
        relayed(add("project", "One default doc.")),
    )
    .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(landed_hash(&v), landed_hash(&first), "{v}");
    let v = ctl(
        &w,
        md.path(),
        ME,
        "r1",
        "rule",
        relayed(add("person", "And from the daemon.")),
    )
    .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(v["rule"]["ruleset"], first["rule"]["ruleset"], "{v}");

    // from another project, the forwarded form finds the same destination and the same request
    let src = source(pd.path(), Arc::clone(&host));
    let v = call(&src, &s, add("person", "One default doc.")).unwrap();
    assert_eq!(landed_hash(&v), landed_hash(&first), "{v}");
    assert_eq!(
        starts.load(Ordering::SeqCst),
        0,
        "meta's daemon was running"
    );
    src.close().unwrap();
    w.stop(md.path());
    assert_eq!(docs_in(md.path()), ["my-rules"]);
}

#[test]
fn rtc4_missing_damaged_and_locked_meta_keep_their_refusals() {
    let w = World::new();
    let pd = w.project();
    let (host, starts) = reader(&w, false);
    let src = source(pd.path(), Arc::clone(&host));
    let before = state(&src);
    let s = me();
    let setup = |e: &RdyError| {
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_NO_PROJECT,
                "the meta project does not exist",
                "run rdy setup"
            ),
            "{e}"
        );
    };

    // the default place holds nothing
    setup(&call(&src, &s, add("person", "Nowhere to go.")).unwrap_err());
    // an explicit override that names no project
    let gone = w.home.path().join("no-such-meta");
    w.person(&gone);
    setup(&call(&src, &s, add("person", "Nowhere to go.")).unwrap_err());
    // an answer has nowhere to go either
    setup(
        &call(
            &src,
            &s,
            answer("person", "P-000000000000@1", "approve", &"0".repeat(64)),
        )
        .unwrap_err(),
    );
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert_eq!(state(&src), before);

    // no home at all: the same refusal
    let nh = World::without_home();
    let nd = nh.project();
    let (nh_host, _) = reader(&nh, false);
    let nh_src = source(nd.path(), nh_host);
    setup(&call(&nh_src, &s, add("person", "No home.")).unwrap_err());
    // project scope needs no meta
    let v = call(&nh_src, &s, add("project", "Lands here.")).unwrap();
    assert_eq!(v["scope"], "project", "{v}");
    nh_src.close().unwrap();
    assert_eq!(docs_in(nd.path()), ["project-rules"]);

    // a partial project is its own refusal, never the setup fix
    let partial = w.home.path().join("partial");
    std::fs::create_dir_all(partial.join(".rdy")).unwrap();
    w.person(&partial);
    let e = call(&src, &s, add("person", "Partial.")).unwrap_err();
    assert_eq!(e.code, E_NO_PROJECT, "{e}");
    assert_eq!(
        e.message,
        format!("{} has no .rdy.yaml, the project config", partial.display()),
        "{e}"
    );
    assert_ne!(e.fix, "run rdy setup", "{e}");

    // a meta project whose pack does not load: the read's refusal, in the same words
    let broken = w.project();
    std::fs::write(broken.path().join(".rdy.yaml"), "pack: no-such-pack\n").unwrap();
    w.person(broken.path());
    let read = host
        .fetch(
            broken.path(),
            &rdy_engine::instructions::LoadRequest::default(),
            &rdy_engine::instructions::Caller {
                actor: ME.into(),
                run: Some("r1".into()),
            },
        )
        .unwrap_err();
    let e = call(&src, &s, add("person", "Broken.")).unwrap_err();
    assert_eq!(e.code, read.code, "{e}");
    assert_ne!(e.fix, "run rdy setup", "{e}");
    assert_eq!(state(&src), before);

    // a held store with no daemon to serve it: the direct host keeps the lock's refusal
    let held = w.project();
    signing(held.path(), "never", false);
    w.person(held.path());
    let (direct, direct_starts) = reader(&w, true);
    let d_src = Local::open(w.project().path())
        .unwrap()
        .with_person_host(direct);
    let holder = Rdy::open(held.path()).unwrap();
    let e = call(&d_src, &s, add("person", "Held.")).unwrap_err();
    assert_eq!(e.code, E_LOCKED, "{e}");
    assert_eq!(
        direct_starts.load(Ordering::SeqCst),
        0,
        "no daemon could open it"
    );
    drop(holder);
    d_src.close().unwrap();

    // project scope works with any of those meta projects in place
    w.person(broken.path());
    let v = call(&src, &s, add("project", "Stays here.")).unwrap();
    assert_eq!(v["scope"], "project", "{v}");
    src.close().unwrap();
    assert_eq!(docs_in(pd.path()), ["project-rules"]);
}

/// Whether `e` is a normal refusal of a request another session raced: its existing code, with its
/// message and fix.
fn normal_refusal(e: &RdyError) {
    assert!(
        [
            E_STALE_BASE,
            E_PROPOSAL_MOVED,
            E_ITEM_MOVED,
            E_VALIDATION,
            E_EXCEPTION_OPEN,
            E_BAD_INPUT,
        ]
        .contains(&e.code),
        "not a normal refusal: {e} {:?}",
        e.issues
    );
    assert!(!e.message.is_empty() && !e.fix.is_empty(), "{e}");
}

/// Two requests at once, from the two sources.
fn race(a: (&Local, Session, Value), b: (&Local, Session, Value)) -> [Result<Value, RdyError>; 2] {
    let barrier = Barrier::new(2);
    std::thread::scope(|sc| {
        let barrier = &barrier;
        let (ae, asession, av) = a;
        let (be, bsession, bv) = b;
        let ha = sc.spawn(move || {
            barrier.wait();
            call(ae, &asession, av)
        });
        let hb = sc.spawn(move || {
            barrier.wait();
            call(be, &bsession, bv)
        });
        [ha.join().unwrap(), hb.join().unwrap()]
    })
}

#[test]
fn rtc4_two_sessions_land_or_receive_normal_refusal() {
    let w = World::new();
    let (md, p1, p2) = (w.project(), w.project(), w.project());
    w.person(md.path());
    signing(md.path(), "always", true);
    let (host, starts) = reader(&w, false);
    let (a, b) = (
        source(p1.path(), Arc::clone(&host)),
        source(p2.path(), Arc::clone(&host)),
    );
    let (s1, s2) = (
        session_of("agent:one", "r1", Role::Writer),
        session_of("agent:two", "r2", Role::Writer),
    );
    // every rule acknowledged, and what it should say now; a stop is no acknowledgement
    let mut acked: Vec<(String, String)> = vec![];
    let keep = |acked: &mut Vec<(String, String)>, v: &Value| {
        if v.get("landing").is_none() {
            return;
        }
        let id = str_of(&v["rule"], "id").to_string();
        let text = str_of(&v["rule"], "text").to_string();
        acked.retain(|(i, _)| *i != id);
        acked.push((id, text));
    };

    // first use: both find no default doc; one doc comes of it, never a numbered second
    let [r1, r2] = race(
        (&a, s1.clone(), add("person", "First of two.")),
        (&b, s2.clone(), add("person", "Second of two.")),
    );
    assert!(r1.is_ok() || r2.is_ok(), "{r1:?} {r2:?}");
    for (r, s, text) in [(&r1, &s1, "First of two."), (&r2, &s2, "Second of two.")] {
        match r {
            Ok(v) => keep(&mut acked, v),
            Err(e) => {
                normal_refusal(e);
                // its view stays, and says where the words are. Closed through the normal view
                // operation, the same words under a new chat session are a new request
                let view = str_of(e.corrected.as_ref().expect("the view"), "view");
                ctl(
                    &w,
                    md.path(),
                    &s.actor,
                    s.run.as_deref().unwrap(),
                    "view",
                    json!({"op": "close", "name": view, "reason": "lost the first use"}),
                )
                .unwrap_or_else(|e| panic!("close {view}: {e:?}"));
                let mut again = add("person", text);
                again["session_ref"] = json!("chat-2");
                let src = if s.actor == "agent:one" { &a } else { &b };
                let v = call(src, s, again).unwrap_or_else(|e| panic!("{text}: {e}"));
                assert!(v.get("landing").is_some(), "{v}");
                keep(&mut acked, &v);
            }
        }
    }
    assert_eq!(acked.len(), 2, "{acked:?}");

    // identical retries of one request: one landing
    let [r1, r2] = race(
        (&a, s1.clone(), add("person", "Only once.")),
        (&b, s1.clone(), add("person", "Only once.")),
    );
    let mut hashes = vec![];
    for r in [&r1, &r2] {
        match r {
            Ok(v) => {
                keep(&mut acked, v);
                hashes.push(landed_hash(v));
            }
            Err(e) => normal_refusal(e),
        }
    }
    assert!(!hashes.is_empty(), "{r1:?} {r2:?}");
    hashes.dedup();
    assert_eq!(hashes.len(), 1, "one request lands once");

    // two sessions edit one rule: whatever each hears, canon ends as an acknowledged edit
    let target = acked[0].0.clone();
    let [r1, r2] = race(
        (&a, s1.clone(), edit("person", &target, "Edited by one.")),
        (&b, s2.clone(), edit("person", &target, "Edited by two.")),
    );
    let mut edits: Vec<Value> = vec![];
    for r in [r1, r2] {
        match r {
            Ok(v) if v.get("landing").is_some() => edits.push(v),
            Ok(v) => assert!(v["state"].is_string(), "an explicit stop: {v}"),
            Err(e) => normal_refusal(&e),
        }
    }
    // two open supersedes of one decision are a tension, so both may stop at their cards
    let mut edited: Vec<String> = edits
        .iter()
        .map(|v| str_of(&v["rule"], "text").to_string())
        .collect();
    edited.push(acked[0].1.clone());

    // a signing pause: another session lands while the first one's signer waits
    let (paused_tx, paused_rx) = channel();
    let (go_tx, go_rx) = channel::<()>();
    let agent = FakeAgent::hooked(move || {
        paused_tx.send(()).unwrap();
        go_rx.recv().unwrap();
    });
    let paused = std::thread::scope(|sc| {
        let s = s1.clone();
        let a = &a;
        let agent = Arc::clone(&agent);
        let h = sc.spawn(move || call_with(a, &s, agent, add("person", "Signed slowly.")));
        paused_rx.recv_timeout(HANG_GUARD).expect("the signer asks");
        let v = call(&b, &s2, add("person", "Landed meanwhile.")).unwrap();
        go_tx.send(()).unwrap();
        (v, h.join().unwrap())
    });
    keep(&mut acked, &paused.0);
    match &paused.1 {
        Ok(v) => keep(&mut acked, v),
        Err(e) => normal_refusal(e),
    }

    // every acknowledged rule is there after the store reopens, and no default doc was doubled
    assert!(starts.load(Ordering::SeqCst) >= 1);
    w.stop(md.path());
    let rules = rules_in(md.path());
    for (id, text) in &acked {
        if *id == target {
            continue;
        }
        assert!(
            rules.iter().any(|(i, t)| i == id && t == text),
            "{id} {text:?} in {rules:#?}"
        );
    }
    let now = &rules
        .iter()
        .find(|(i, _)| *i == target)
        .expect("the rule")
        .1;
    assert!(edited.contains(now), "{now:?} is one of {edited:?}");
    assert_eq!(docs_in(md.path()), ["my-rules"]);
    a.close().unwrap();
    b.close().unwrap();
    assert_eq!(closed_state(p1.path()).2, 0, "no landing in a source");
    assert_eq!(closed_state(p2.path()).2, 0, "no landing in a source");
}

#[test]
fn rtc4_destination_failure_leaves_other_project_untouched() {
    let w = World::new();
    let (md, pd) = (w.project(), w.project());
    w.person(md.path());
    let (host, _) = reader(&w, false);
    let src = source(pd.path(), Arc::clone(&host));
    let s = me();
    let before = state(&src);

    // ---- a person rule that fails in meta ----

    // draft: the daemon refuses before any write
    call(&src, &s, add("person", "A rule to stand there.")).unwrap();
    let e = call(&src, &s, edit("person", "ai-ZZZZZ", "No such rule.")).unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
    assert_eq!(state(&src), before);

    // role: the policy needs someone else; nothing lands, and the view closes
    with_closed(&w, md.path(), |r| {
        land_policy(
            r,
            policy(json!({
                "roles": {"approvers": ["human:boss"]},
                "require": [{"when": "any", "role": "approvers"}],
            })),
        );
    });
    let landings = closed_state(md.path()).2;
    let e = call(&src, &s, add("person", "Needs the boss.")).unwrap_err();
    assert_eq!(e.code, E_PERMISSION, "{e}");
    assert_eq!(state(&src), before);
    w.stop(md.path());
    assert_eq!(closed_state(md.path()).2, landings, "nothing landed");
    assert!(
        closed_state(md.path())
            .1
            .iter()
            .all(|(n, _)| !n.starts_with("rule-")),
        "the failed view closed"
    );

    // open: a meta project that does not open
    let broken = w.project();
    std::fs::write(broken.path().join(".rdy.yaml"), "pack: no-such-pack\n").unwrap();
    w.person(broken.path());
    call(&src, &s, add("person", "Cannot open.")).unwrap_err();
    assert_eq!(state(&src), before);

    // signing: a meta project that must sign, and a caller whose agent refuses
    let signed = w.project();
    signing(signed.path(), "always", true);
    w.person(signed.path());
    let refusing = FakeAgent::refusing();
    let e = call_with(
        &src,
        &s,
        Arc::clone(&refusing) as Arc<dyn SigningAgent>,
        add("person", "Must sign."),
    )
    .unwrap_err();
    assert_eq!(e.code, E_NO_AGENT, "{e}");
    assert!(
        refusing.signs() >= 1,
        "the caller's agent was the one asked"
    );
    assert_eq!(state(&src), before);

    // durability: meta's daemon dies while the signer waits
    let dying = w.project();
    signing(dying.path(), "always", true);
    w.person(dying.path());
    let pid_file = RunDir::of_project(dying.path()).pid_file();
    let kill = FakeAgent::hooked(move || kill_daemon(&pid_file));
    let v = call_with(&src, &s, kill, add("person", "Dies in flight."));
    let e = v.unwrap_err();
    assert_eq!(e.code, GONE, "{e}");
    assert_eq!(e.fix, GONE_FIX, "{e}");
    assert_eq!(state(&src), before);
    src.close().unwrap();

    // propose: the direct host runs meta's store in this process, where a fault can be injected
    let direct_meta = w.project();
    signing(direct_meta.path(), "never", false);
    w.person(direct_meta.path());
    let (direct, _) = reader(&w, true);
    let src = source(pd.path(), direct);
    let meta_before = closed_state(direct_meta.path());
    let e = with_rule_fault_for_tests(RuleFault::Propose, || {
        call(&src, &s, add("person", "Fails at propose."))
    })
    .unwrap_err();
    assert_eq!(e.code, E_INTERNAL, "{e}");
    assert_eq!(state(&src), before);
    assert_eq!(closed_state(direct_meta.path()).2, meta_before.2);
    assert_eq!(closed_state(direct_meta.path()).0, meta_before.0);

    // ---- a project rule that fails in the source: meta is never opened or started ----

    let e = call(&src, &s, edit("project", "ai-ZZZZZ", "No such rule.")).unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
    let e = with_rule_fault_for_tests(RuleFault::Propose, || {
        call(&src, &s, add("project", "Fails at propose here."))
    })
    .unwrap_err();
    assert_eq!(e.code, E_INTERNAL, "{e}");
    let refusing = FakeAgent::refusing();
    src.close().unwrap();
    signing(pd.path(), "always", true);
    let (direct, _) = reader(&w, true);
    let src = Local::open(pd.path()).unwrap().with_person_host(direct);
    let e = call_with(
        &src,
        &s,
        refusing as Arc<dyn SigningAgent>,
        add("project", "Must sign here."),
    )
    .unwrap_err();
    assert_eq!(e.code, E_NO_AGENT, "{e}");
    assert_eq!(state(&src).2, before.2, "nothing landed in the source");
    src.close().unwrap();
    assert!(w.daemon_pid(direct_meta.path()).is_none(), "never started");
    assert_eq!(closed_state(direct_meta.path()).0, meta_before.0);
    assert_eq!(closed_state(direct_meta.path()).1, meta_before.1);
}

#[test]
fn rtc4_forwarded_errors_keep_issues_and_corrected() {
    let w = World::new();
    let (md, pd) = (w.project(), w.project());
    w.person(md.path());
    let (host, _) = reader(&w, false);
    let src = source(pd.path(), Arc::clone(&host));
    let s = me();

    // a rule, and a block that also rests on its decision: the shared refusal's corrected
    let v = call(&src, &s, add("person", "Shared reason.")).unwrap();
    let (id, decision, doc) = (
        str_of(&v["rule"], "id").to_string(),
        str_of(&v, "decision").to_string(),
        str_of(&v["rule"], "ruleset").to_string(),
    );
    let other = with_closed(&w, md.path(), |r| {
        let c = write(
            r,
            "also",
            json!([{"op": "create", "type": "instruction", "id": "$i", "parent": doc,
                    "fields": {"applies_to": ["all"]}, "body": "Also rests.",
                    "links": {"decided_by": [{"target": decision}]}}]),
        )
        .unwrap_or_else(|e| panic!("write the dependent: {e}"));
        land(r, "also").unwrap_or_else(|e| panic!("land the dependent: {e}"));
        c.created["$i"].clone()
    });
    let e = call(&src, &s, edit("person", &id, "Changed reason.")).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(
        e.message,
        format!("decision {decision} also governs other blocks")
    );
    assert_eq!(
        e.fix,
        "brief the returned blocks and use the normal decision workflow"
    );
    let c = e.corrected.as_ref().expect("corrected crossed the daemon");
    assert_eq!(c["state"], "normal_path");
    assert_eq!(c["scope"], "person");
    assert_eq!(c["blocks"], json!([other]));

    // card mode: a moved card's fresh diagnostics
    with_closed(&w, md.path(), |r| {
        land_policy(r, policy(json!({"rules_from_chat": "card"})));
    });
    let stop = call(&src, &s, add("person", "On a card.")).unwrap();
    assert_eq!(stop["state"], "card", "{stop}");
    let (r#ref, hash) = (str_of(&stop, "ref"), str_of(&stop, "card_hash"));
    let wrong = "0".repeat(64);
    let e = call(&src, &s, answer("person", r#ref, "approve", &wrong)).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e}");
    assert_eq!(
        e.corrected.as_ref().expect("the fresh card")["card_hash"].as_str(),
        Some(hash),
        "{e}"
    );

    // readiness: the same words again are a real tension, and its issues cross as they are
    let again = json!({"scope": "person", "op": "add", "text": "Shared reason.",
                       "why": "Again.", "evidence": "again", "session_ref": "chat-2"});
    let stop = call(&src, &s, again).unwrap();
    assert_eq!(stop["state"], "not_ready", "{stop}");
    let want = stop["issues"].as_array().unwrap().clone();
    assert!(!want.is_empty());
    let r#ref = str_of(&stop, "ref");
    let e = call(&src, &s, answer("person", r#ref, "approve", &wrong)).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e}");
    let got: Vec<Value> = e
        .issues
        .iter()
        .map(|i| serde_json::to_value(i).unwrap())
        .collect();
    assert_eq!(got, want, "the issues, whole and in order");
    let c = e.corrected.as_ref().expect("corrected");
    assert_eq!(c["ref"].as_str(), Some(r#ref));
    assert_eq!(c["scope"], "person");
    src.close().unwrap();
}

#[test]
fn rtc4_cross_project_card_answers_stay_in_meta() {
    let w = World::new();
    let (md, pd) = (w.project(), w.project());
    w.person(md.path());
    let (host, _) = reader(&w, false);
    let src = source(pd.path(), Arc::clone(&host));
    let s = me();
    let before = state(&src);

    // two rules that stand before the policy
    let to_edit = call(&src, &s, add("person", "Rule to edit.")).unwrap();
    let to_remove = call(&src, &s, add("person", "Rule to remove.")).unwrap();
    with_closed(&w, md.path(), |r| {
        land_policy(r, policy(json!({"rules_from_chat": "card"})));
    });

    // add: the card comes back through the source; its answer goes to meta
    let stop = call(&src, &s, add("person", "A rule on a card.")).unwrap();
    assert_eq!(stop["state"], "card", "{stop}");
    assert_eq!(stop["card"]["options"][0]["id"], "approve", "{stop}");
    let (r#ref, hash) = (str_of(&stop, "ref"), str_of(&stop, "card_hash"));
    let done = call(&src, &s, answer("person", r#ref, "approve", hash)).unwrap();
    assert_eq!(done["rule"]["text"], "A rule on a card.", "{done}");
    let added = landed_hash(&done);

    // edit: confirm, then approve, each on the same route
    let stop = call(
        &src,
        &s,
        edit("person", str_of(&to_edit["rule"], "id"), "Rule edited."),
    )
    .unwrap();
    assert_eq!(stop["state"], "card", "{stop}");
    let (r#ref, cards) = (str_of(&stop, "ref"), stop["cards"].as_array().unwrap());
    assert_eq!(cards.len(), 1, "{stop}");
    assert_eq!(cards[0]["card"]["kind"], "confirm", "{stop}");
    let confirmed = call(
        &src,
        &s,
        answer(
            "person",
            str_of(&cards[0], "ref"),
            "confirm",
            str_of(&cards[0], "card_hash"),
        ),
    )
    .unwrap();
    assert_eq!(confirmed["state"], "answered", "{confirmed}");
    let remaining = confirmed["remaining_cards"].as_array().unwrap();
    assert_eq!(remaining.len(), 1, "{confirmed}");
    let done = call(
        &src,
        &s,
        answer(
            "person",
            r#ref,
            "approve",
            str_of(&remaining[0], "card_hash"),
        ),
    )
    .unwrap();
    assert_eq!(done["rule"]["text"], "Rule edited.", "{done}");
    let edited = landed_hash(&done);

    // remove: the same
    let stop = call(&src, &s, remove("person", str_of(&to_remove["rule"], "id"))).unwrap();
    assert_eq!(stop["state"], "card", "{stop}");
    assert_eq!(stop["rule"]["text"], "Rule to remove.", "{stop}");
    let (r#ref, cards) = (str_of(&stop, "ref"), stop["cards"].as_array().unwrap());
    let confirmed = call(
        &src,
        &s,
        answer(
            "person",
            str_of(&cards[0], "ref"),
            "confirm",
            str_of(&cards[0], "card_hash"),
        ),
    )
    .unwrap();
    let remaining = confirmed["remaining_cards"].as_array().unwrap();
    let done = call(
        &src,
        &s,
        answer(
            "person",
            r#ref,
            "approve",
            str_of(&remaining[0], "card_hash"),
        ),
    )
    .unwrap();
    assert_eq!(done["rule"]["retired"], true, "{done}");

    // the source holds none of it; meta records the card evidence and the caller as the relayer
    assert_eq!(state(&src), before);
    with_closed(&w, md.path(), |r| {
        let st = r.read();
        for h in [added, edited] {
            let rec = st.reader().landing(&h).unwrap();
            assert_eq!(rec.evidence.as_ref().unwrap().kind, EvidenceKind::Card);
            let by = rec.relayed_by.as_ref().expect("relayed");
            assert_eq!(by.actor, ME);
            assert_eq!(by.run.as_deref(), Some("r1"));
            assert_eq!(
                rec.relay_evidence.as_ref().unwrap().quoted,
                "Yes, as the card says."
            );
        }
    });
    src.close().unwrap();
}

#[test]
fn rtc4_warm_meta_call_does_not_reopen_store() {
    let w = World::new();
    let (md, pd) = (w.project(), w.project());
    w.person(md.path());
    signing(md.path(), "never", false);

    // an unused store, a direct host: open once, close, and no daemon is ever started
    let (direct, direct_starts) = reader(&w, true);
    let src = source(pd.path(), direct);
    let s = me();
    let v = call(&src, &s, add("person", "Direct and closed.")).unwrap();
    assert_eq!(v["rule"]["text"], "Direct and closed.", "{v}");
    assert_eq!(direct_starts.load(Ordering::SeqCst), 0);
    assert!(w.daemon_pid(md.path()).is_none(), "no daemon");
    let reopened = Rdy::open(md.path()).expect("the host closed the store");
    reopened.close().unwrap();
    src.close().unwrap();

    // a daemon host: a cold call starts the daemon once, and warm calls reuse it
    let (host, starts) = reader(&w, false);
    let src = source(pd.path(), Arc::clone(&host));
    call(&src, &s, add("person", "Cold.")).unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    let pid = w.daemon_pid(md.path()).expect("meta's daemon");
    for text in ["Warm one.", "Warm two."] {
        call(&src, &s, add("person", text)).unwrap();
        assert_eq!(starts.load(Ordering::SeqCst), 1, "no second start");
        assert_eq!(w.daemon_pid(md.path()), Some(pid), "the same daemon");
        let held = Rdy::open(md.path()).map(drop).unwrap_err();
        assert_eq!(held.code, E_LOCKED, "it holds its store: {held:?}");
    }
    src.close().unwrap();

    // a direct host with the store held asks that daemon, and starts nothing
    let (direct, direct_starts) = reader(&w, true);
    let src = source(pd.path(), direct);
    call(&src, &s, add("person", "Direct, held.")).unwrap();
    assert_eq!(direct_starts.load(Ordering::SeqCst), 0);
    assert_eq!(w.daemon_pid(md.path()), Some(pid));
    src.close().unwrap();

    // the caller is gone; the daemon lives on its own idle policy
    assert!(rdy_qa::platform::process_running(pid));
    let v = ctl(&w, md.path(), "agent:b", "b1", "status", json!({}));
    assert!(v.is_ok(), "{v:?}");
    w.stop(md.path());
    assert_eq!(rules_in(md.path()).len(), 5);
}

#[test]
fn rtc4_signing_wait_holds_neither_writer() {
    let w = World::new();
    let (md, pd) = (w.project(), w.project());
    w.person(md.path());
    signing(md.path(), "always", true);
    let (host, _) = reader(&w, false);
    let src = source(pd.path(), host);
    let s = me();

    let (paused_tx, paused_rx) = channel();
    let (go_tx, go_rx) = channel::<()>();
    let agent = FakeAgent::hooked(move || {
        paused_tx.send(()).unwrap();
        go_rx.recv().unwrap();
    });
    let landed = std::thread::scope(|sc| {
        let (src, s) = (&src, &s);
        let waiter = Arc::clone(&agent);
        let h = sc.spawn(move || call_with(src, s, waiter, add("person", "Signed at leisure.")));
        paused_rx.recv_timeout(HANG_GUARD).expect("the signer asks");

        // the source's reads and its writer answer while the caller's signer waits
        src.read().canon_head();
        with_writer_result(src, |r| {
            write(
                r,
                "meanwhile",
                json!([{"op": "create", "type": "doc", "fields": {"name": "meanwhile", "title": "Meanwhile", "doc_type": "page"}}]),
            )
            .map(drop)
        })
        .expect("the source's writer is free");

        // so do the destination's, for another session
        let status = ctl(&w, md.path(), "agent:b", "b1", "status", json!({}));
        assert!(status.is_ok(), "{status:?}");
        let doc = ctl(
            &w,
            md.path(),
            "agent:b",
            "b1",
            "new_doc",
            json!({"path": "meanwhile"}),
        );
        assert!(doc.is_ok(), "the destination's writer is free: {doc:?}");

        go_tx.send(()).unwrap();
        h.join().unwrap()
    });
    let v = landed.unwrap_or_else(|e| panic!("{e} {:?}", e.issues));
    assert!(agent.signs() >= 1, "{v}");
    let h = landed_hash(&v);
    with_closed(&w, md.path(), |r| {
        let st = r.read();
        assert!(st.reader().landing(&h).is_ok());
    });
    src.close().unwrap();
}

#[test]
fn rtc4_disconnect_does_not_retry_mutation() {
    let w = World::new();
    let (md, pd) = (w.project(), w.project());
    w.person(md.path());
    signing(md.path(), "always", true);
    let (host, starts) = reader(&w, false);
    let src = source(pd.path(), host);
    let s = me();
    let before = state(&src);
    let request = add("person", "Lost in flight.");

    // the daemon dies with the request in flight, while the caller's signer holds the request
    let pid_file = RunDir::of_project(md.path()).pid_file();
    let agent = FakeAgent::hooked(move || kill_daemon(&pid_file));
    let e = call_with(&src, &s, agent, request.clone()).unwrap_err();
    assert_eq!(e.code, GONE, "{e}");
    assert_eq!(e.fix, GONE_FIX, "the ambiguity is said: {e}");
    assert_eq!(starts.load(Ordering::SeqCst), 1, "no transport retry");
    assert!(w.daemon_pid(md.path()).is_none(), "nothing started another");
    assert_eq!(state(&src), before);
    assert_eq!(closed_state(md.path()).2, 0, "nothing landed");

    // only the caller's explicit repeat goes on, from the receipt
    let v = call(&src, &s, request.clone()).unwrap_or_else(|e| panic!("{e} {:?}", e.issues));
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    let first = landed_hash(&v);
    // the reply of this one is the lost one: the next repeat reads it, and lands nothing
    let again = call(&src, &s, request).unwrap();
    assert_eq!(landed_hash(&again), first);
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    w.stop(md.path());
    assert_eq!(closed_state(md.path()).2, 1, "one landing");
    assert_eq!(state(&src), before);
    src.close().unwrap();
}

#[test]
fn rtc4_reader_and_person_sessions_never_connect() {
    let w = World::new();
    let (md, pd) = (w.project(), w.project());
    w.person(md.path());
    let (host, starts) = reader(&w, false);
    let src = source(pd.path(), host);

    // a reader: its role is checked here, because the ctl session's default role is a writer's
    let reader_session = session_of(ME, "r1", Role::Reader);
    for v in [
        add("person", "From a reader."),
        answer("person", "P-000000000000@1", "approve", &"0".repeat(64)),
    ] {
        let e = call(&src, &reader_session, v).unwrap_err();
        assert_eq!(e.code, E_PERMISSION, "{e}");
    }
    // a person: no daemon connection claims the person's actor
    let person = Session::human(PERSON.into(), Role::Writer).unwrap();
    let e = call(&src, &person, add("person", "From the person.")).unwrap_err();
    assert_eq!(e.code, E_PERMISSION, "{e}");

    assert_eq!(starts.load(Ordering::SeqCst), 0, "nothing was started");
    assert!(w.daemon_pid(md.path()).is_none());
    assert_eq!(closed_state(md.path()).2, 0);
    src.close().unwrap();
}
