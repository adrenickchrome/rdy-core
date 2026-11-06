//! rtc4 (FW-MH.rules, D-ZSRBN): the host's and the ctl op's own contract. A host that cannot write
//! another project refuses by default, and a request crosses a connection unchanged. The ctl op
//! `rule` runs only in the project it is asked of, and a request for another project is refused,
//! because a ctl op holds no signer it can hand on: a caller of a person rule connects to meta.
use rdy_daemon::instructions::PersonReader;
use rdy_daemon::{OPS, OpContext, dispatch};
use rdy_engine::engine::{Engine, Local};
use rdy_engine::instructions::{Caller, Contribution, ForwardedRule, LoadRequest, PersonHost};
use rdy_engine::instructions_config::{PathEnv, Platform};
use rdy_engine::rule_types::parse_request;
use rdy_engine::rules::{RuleOrigin, route_rule};
use rdy_engine::signing_crypto::UnavailableAgent;
use rdy_engine::{E_BAD_INPUT, E_NO_PROJECT, E_PERMISSION, Rdy, RdyError, Scope, Session};
use rdy_model::Role;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

const ME: &str = "agent:a";
const PERSON: &str = "human:qa";

/// A person's isolated config: the human, and the meta project at `meta`.
struct Home {
    cfg: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Home {
    fn new(meta: Option<&Path>) -> Home {
        let (cfg, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::create_dir_all(cfg.path().join("rdy")).unwrap();
        let mut text = format!("actor: {PERSON}\n");
        if let Some(m) = meta {
            text.push_str(&format!(
                "instructions:\n  person: {}\n",
                serde_json::to_string(&m.to_string_lossy()).unwrap()
            ));
        }
        std::fs::write(cfg.path().join("rdy/config.yaml"), text).unwrap();
        Home { cfg, home }
    }

    fn reader(&self) -> PersonReader {
        let env = PathEnv {
            xdg_config_home: Some(self.cfg.path().into()),
            home: Some(self.home.path().into()),
            user_profile: Some(self.home.path().into()),
            home_drive: None,
            home_path: None,
            app_data: Some(self.home.path().join("AppData").into()),
        };
        PersonReader::daemon(env!("CARGO_PKG_VERSION")).with_env(Platform::HOST, env)
    }
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(
        dir.path().join(".rdy/local.yaml"),
        "signing:\n  key: null\n  agent_key: null\n  sign: never\n  allowed_signers: null\n",
    )
    .unwrap();
    dir
}

fn me() -> Session {
    Session::agent(ME.into(), Some("r1".into()), Role::Writer).unwrap()
}

fn add(scope: &str, text: &str) -> Value {
    json!({"scope": scope, "op": "add", "text": text, "why": "They asked.",
           "evidence": "yes, keep it", "session_ref": "chat-1", "relayed_by": ME})
}

fn op(e: &dyn Engine, op_name: &str, args: Value) -> Result<Value, RdyError> {
    let s = me();
    let root = e.root();
    let cx = OpContext {
        engine: e,
        session: &s,
        human: PERSON,
        cwd: &root,
        agent: &UnavailableAgent,
    };
    dispatch(&cx, op_name, args)
}

/// A host that only reads.
struct ReadOnly;

impl PersonHost for ReadOnly {
    fn fetch(&self, _: &Path, _: &LoadRequest, _: &Caller) -> Result<Contribution, RdyError> {
        panic!("not asked to read")
    }
}

#[test]
fn rtc4_default_host_refuses_a_rule_and_opens_nothing() {
    let v = json!({"scope": "person", "op": "add", "text": "Rule.", "why": "Why.",
                   "evidence": "yes"});
    let request = parse_request(&v).unwrap();
    let s = me();
    let e = ReadOnly
        .write_rule(
            Path::new("/no/such/meta"),
            &ForwardedRule {
                session: &s,
                relayed_by: None,
                request: &request,
                agent: Arc::new(UnavailableAgent),
            },
        )
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_BAD_INPUT,
            "rdy_rule needs a host that can write the meta project",
            "use rdy mcp or rdy rule with the daemon",
        ),
        "{e}"
    );
}

#[test]
fn rtc4_no_host_and_no_project_keep_their_refusals() {
    let meta = project();
    let h = Home::new(Some(meta.path()));
    let host = h.reader();
    let s = me();
    let origin = RuleOrigin {
        session: &s,
        relayed_by: None,
    };
    let person = parse_request(&json!({"scope": "person", "op": "add", "text": "Rule.",
                                       "why": "Why.", "evidence": "yes"}))
    .unwrap();
    let scoped = parse_request(&json!({"scope": "project", "op": "add", "text": "Rule.",
                                       "why": "Why.", "evidence": "yes"}))
    .unwrap();
    // a projectless call of a project rule
    let e = route_rule(
        None,
        Some(&host),
        &origin,
        &scoped,
        Arc::new(UnavailableAgent),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_NO_PROJECT,
            "this session has no project",
            "use scope person, or run rdy init in a folder and reconnect",
        ),
        "{e}"
    );
    // a host with no writer for the meta project: its default refusal, and meta stays closed
    struct LocatesOnly(PersonReader);
    impl PersonHost for LocatesOnly {
        fn locate(&self) -> Result<rdy_engine::instructions_config::PersonSource, RdyError> {
            self.0.locate()
        }
        fn fetch(&self, _: &Path, _: &LoadRequest, _: &Caller) -> Result<Contribution, RdyError> {
            panic!("not asked to read")
        }
    }
    let locates = LocatesOnly(h.reader());
    let e = route_rule(
        None,
        Some(&locates),
        &origin,
        &person,
        Arc::new(UnavailableAgent),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(
        e.message,
        "rdy_rule needs a host that can write the meta project"
    );
    Rdy::open(meta.path())
        .expect("nothing holds the meta store")
        .close()
        .unwrap();
}

#[test]
fn rtc4_error_reply_keeps_every_diagnostic_member() {
    let mut e = RdyError::new(E_BAD_INPUT, "m", "f").with_corrected(json!({"state": "x"}));
    e.briefing = Some(json!({"ids": ["a"]}));
    e.warnings = Some(Box::new(vec![json!({"code": "W_ONE"})]));
    let r = rdy_daemon::error::ErrorReply::from(e);
    assert_eq!(
        (r.code.as_str(), r.message.as_str(), r.fix.as_str()),
        (E_BAD_INPUT, "m", "f")
    );
    assert_eq!(r.extra["corrected"], json!({"state": "x"}));
    assert_eq!(r.extra["briefing"], json!({"ids": ["a"]}));
    assert_eq!(r.extra["warnings"], json!([{"code": "W_ONE"}]));
}

#[test]
fn rtc4_request_json_is_the_request() {
    let id = "ai-ABCDE";
    let cases = [
        json!({"scope": "person", "op": "add", "text": "  Exact\n\nwords.  ", "why": "Why.",
               "evidence": "  their words\n", "question": " Save? ", "session_ref": "chat-1",
               "ruleset": "person:person:odd", "applies_to": ["all", "act:write"]}),
        json!({"scope": "project", "op": "add", "text": "Rule.", "why": "Why.",
               "evidence": "yes", "ruleset": "named"}),
        json!({"scope": "person", "op": "edit", "id": id, "text": "New.", "why": "Why.",
               "evidence": "yes", "applies_to": ["all"]}),
        json!({"scope": "person", "op": "remove", "id": id, "why": "Why.", "evidence": "yes",
               "question": "Q?", "session_ref": "s"}),
        json!({"scope": "person", "answer": {"ref": "P-000000000000@1", "option": "approve",
               "card_hash": "0".repeat(64), "evidence": "yes", "question": "Q?"}}),
        json!({"scope": "project", "answer": {"ref": "P-000000000000@1", "option": "later",
               "card_hash": "0".repeat(64), "evidence": "yes"}}),
    ];
    for v in cases {
        let q = parse_request(&v).unwrap_or_else(|e| panic!("{v}: {e}"));
        let back = parse_request(&q.to_json()).unwrap_or_else(|e| panic!("{v}: {e}"));
        assert_eq!(back, q, "{v}");
    }
}

#[test]
fn rtc4_ctl_rule_runs_only_in_its_own_project() {
    assert!(OPS.contains(&"rule"));
    let (meta, other) = (project(), project());
    let h = Home::new(Some(meta.path()));

    // on another project: a project rule runs here; a person rule cannot be carried from here
    let e = Local::open(other.path())
        .unwrap()
        .with_person_host(Arc::new(h.reader()));
    let v = op(&e, "rule", add("project", "Lands here.")).unwrap();
    assert_eq!(v["scope"], "project", "{v}");
    let refused = op(&e, "rule", add("person", "Goes to meta.")).unwrap_err();
    assert_eq!(
        (refused.code, refused.message.as_str(), refused.fix.as_str()),
        (
            E_BAD_INPUT,
            "rdy_rule needs a host that can write the meta project",
            "use rdy mcp or rdy rule with the daemon",
        ),
        "{refused}"
    );
    // the private form says this store is the destination, whatever this process's config says
    let mut forwarded = add("person", "Here, as asked.");
    forwarded["part"] = json!(true);
    let v = op(&e, "rule", forwarded).unwrap();
    assert_eq!(v["scope"], "person", "{v}");
    let mut wrong = add("project", "Not for the private form.");
    wrong["part"] = json!(true);
    let refused = op(&e, "rule", wrong).unwrap_err();
    assert_eq!(refused.code, E_BAD_INPUT, "{refused}");
    assert!(refused.message.contains("\"part\""), "{refused}");
    // a session that names no relayer is the relay only in the private form
    let mut unnamed = add("project", "Whose words?");
    unnamed.as_object_mut().unwrap().remove("relayed_by");
    let refused = op(&e, "rule", unnamed).unwrap_err();
    assert_eq!(refused.code, E_BAD_INPUT, "{refused}");
    assert_eq!(refused.message, "relayed_by and evidence go together");
    // a relayer that is not the session
    let mut other_actor = add("project", "Another relayer.");
    other_actor["relayed_by"] = json!("agent:b");
    let refused = op(&e, "rule", other_actor).unwrap_err();
    assert_eq!(refused.code, E_PERMISSION, "{refused}");
    // closed arguments, with the nearest key
    let mut typo = add("project", "A typo.");
    typo["relayed_bye"] = json!(ME);
    let refused = op(&e, "rule", typo).unwrap_err();
    assert_eq!(refused.code, E_BAD_INPUT, "{refused}");
    assert!(refused.message.contains("relayed_bye"), "{refused}");
    let mut wrong_kind = add("project", "A wrong kind.");
    wrong_kind["part"] = json!("yes");
    let refused = op(&e, "rule", wrong_kind).unwrap_err();
    assert_eq!(refused.code, E_BAD_INPUT, "{refused}");
    e.close().unwrap();

    // on meta itself: both spellings run here, in one doc
    let e = Local::open(meta.path())
        .unwrap()
        .with_person_host(Arc::new(h.reader()));
    let a = op(&e, "rule", add("project", "One.")).unwrap();
    let b = op(&e, "rule", add("person", "Two.")).unwrap();
    assert_eq!(a["rule"]["ruleset"], b["rule"]["ruleset"], "{a} {b}");
    let st = e.read();
    let m = st.materialized(&Scope::Canon).unwrap();
    let doc = rdy_model::BlockId::parse(a["rule"]["ruleset"].as_str().unwrap()).unwrap();
    assert_eq!(m.get(&doc).unwrap().field_str("name"), Some("my-rules"));
    drop(m);
    drop(st);
    e.close().unwrap();

    // meta missing: project scope still runs, and person scope is the setup refusal
    let gone = Home::new(Some(&meta.path().join("no-such")));
    let e = Local::open(other.path())
        .unwrap()
        .with_person_host(Arc::new(gone.reader()));
    let v = op(&e, "rule", add("project", "Still here.")).unwrap();
    assert_eq!(v["scope"], "project", "{v}");
    let refused = op(&e, "rule", add("person", "Nowhere.")).unwrap_err();
    assert_eq!(
        (refused.code, refused.message.as_str(), refused.fix.as_str()),
        (
            E_NO_PROJECT,
            "the meta project does not exist",
            "run rdy setup"
        ),
        "{refused}"
    );
    e.close().unwrap();
}
