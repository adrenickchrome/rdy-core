//! ap9 (D-ZWP7Q, D-P3YD5, D-X3136, O2, O20): `rdy_policy init` and `rdy policy init` start the
//! authority policy. In the session's view they write one proposed decision with no parent and
//! the engine's template block under the doc, which implements the decision, then propose the
//! view. A person approves it like any proposal. The `Policy` fact of the card is Task 8's and is
//! pinned by `ap8_with_no_policy_the_fact_reads_no_authority_policy_is_set`.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::engine::{Local, write_typed};
use rdy_engine::policy::init::PolicyInitInfo;
use rdy_engine::policy::live_policy;
use rdy_engine::{Rdy, RdyError, Scope, Session};
use rdy_mcp::tools::{McpState, rdy_policy, rdy_session};
use rdy_model::{BlockId, Hash, Justification};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::intake::confirm_before_landing;
use serde_json::{Value, json};
use std::sync::Arc;

const TITLE: &str = "Adopt the rdy-standard authority policy";

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

/// A project with an empty doc `policy` in canon.
fn with_policy_doc() -> (tempfile::TempDir, Rdy) {
    let (d, mut r) = project();
    add_doc(&mut r);
    (d, r)
}

fn add_doc(r: &mut Rdy) {
    write(
        r,
        "docs",
        json!([{"op": "create", "type": "doc", "fields": {"name": "policy", "title": "Policy", "doc_type": "page"}}]),
    )
    .unwrap();
    land(r, "docs").unwrap();
}

fn a1() -> Session {
    agent("agent:a1", Some("r1"))
}

/// An MCP state on `r` with the agent session `agent:a1`, run `r1` (its view is `r1`).
fn agent_state(r: Rdy) -> McpState {
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        p(json!({"actor": "agent:a1", "run": "r1", "role": "writer"})),
    )
    .unwrap();
    st
}

fn on<T: serde::Serialize + 'static>(
    st: &McpState,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Value {
    write_typed(&*st.engine, f).unwrap()
}

fn init(st: &mut McpState, args: Value) -> Result<Value, RdyError> {
    rdy_policy(st, p(args))
}

fn id(v: &Value) -> BlockId {
    BlockId::parse(v.as_str().unwrap()).unwrap()
}

/// The shape of `v`: its structure with every string and number reduced to its kind.
fn shape(v: &Value) -> Value {
    match v {
        Value::String(_) => json!("s"),
        Value::Number(_) => json!("n"),
        Value::Array(a) => Value::Array(a.iter().map(shape).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
        other => other.clone(),
    }
}

#[test]
fn ap9_init_writes_a_decision_and_the_template_and_proposes() {
    let (_d, r) = with_policy_doc();
    let mut st = agent_state(r);
    let v = init(&mut st, json!({"op": "init", "doc": "policy"})).unwrap();
    let (decision, policy) = (id(&v["decision"]), id(&v["policy"]));
    assert!(decision.to_string().starts_with("D-"), "{v}");
    assert!(policy.to_string().starts_with("ap-"), "{v}");
    // the proposal is the rdy_propose reply, and the version is ready
    let pr = &v["proposal"];
    assert_eq!(pr["title"], TITLE, "{v}");
    assert_eq!(
        pr["summary"],
        "Adopts the rdy-standard authority policy. It accepts nothing by itself until a person adds a rule.",
        "{v}"
    );
    assert_eq!(pr["changes"], 2, "{v}");
    assert_eq!(pr["ready"], true, "{v}");
    assert_eq!(pr["issues"], json!([]), "{v}");
    assert!(pr["ref"].as_str().unwrap().starts_with("P-"), "{v}");
    assert!(pr.get("consequences").is_some(), "{v}");
    // two changes in the view: the decision, then the policy, which implements it
    let (d2, p2) = (decision.clone(), policy.clone());
    let seen = on(&st, move |r| {
        let rs = r.read();
        let changes: Vec<_> = rs
            .included("r1")
            .iter()
            .map(|h| rs.reader().change(h).unwrap())
            .collect();
        let view = rs.materialized(&Scope::View("r1".into())).unwrap();
        let b = view.get(&p2).unwrap();
        let doc = view.get(b.parent.as_ref().unwrap()).unwrap();
        Ok(json!({
            "changes": changes.len(),
            "first": changes[0].justification.is_none(),
            "second": changes[1].justification == Some(Justification::Implements(vec![d2])),
            "policy_type": b.block_type,
            "policy_doc": doc.field_str("name"),
            "policy_fields": b.fields.iter().map(|(k, v)| (k.clone(), v.to_yaml())).map(|(k, y)| (k, serde_json::to_value(y).unwrap())).collect::<serde_json::Map<_, _>>(),
        }))
    });
    assert_eq!(seen["changes"], 2, "{seen}");
    assert_eq!(seen["first"], true, "{seen}");
    assert_eq!(seen["second"], true, "{seen}");
    assert_eq!(seen["policy_type"], "authority_policy");
    assert_eq!(seen["policy_doc"], "policy");
    // O2: the engine's template, the same for every pack
    assert_eq!(
        seen["policy_fields"],
        json!({"key": "authority-policy", "agent_may_accept": [], "roles": {}, "require": [], "owners": []}),
        "{seen}"
    );
}

#[test]
fn ap9_init_writes_the_decision_with_no_parent() {
    let (_d, mut r) = with_policy_doc();
    let info = r.policy_init(&a1(), Some("policy"), None).unwrap();
    let d = BlockId::parse(&info.decision).unwrap();
    let st = r.read();
    let view = st.materialized(&Scope::View("r1".into())).unwrap();
    let b = view.get(&d).expect("the decision is in the view");
    assert_eq!(b.parent, None, "D-X3136: a decision has no parent");
    assert_eq!(b.field_str("statement"), Some(TITLE));
    assert_eq!(b.field_str("title"), Some(TITLE));
    assert_eq!(
        b.field_str("why"),
        Some("The policy says what an agent may land without a person, and who approves the rest.")
    );
    assert_eq!(b.field_str("status"), Some("proposed"));
    assert_eq!(b.field_str("reversibility"), Some("two_way"));
    assert_eq!(b.field_str("authority_basis"), None, "no basis");
    // only the policy block sits in the doc
    let doc = view
        .iter()
        .find(|x| x.field_str("name") == Some("policy"))
        .unwrap();
    let kids: Vec<_> = view
        .iter()
        .filter(|x| x.parent.as_ref() == Some(&doc.id))
        .map(|x| x.block_type.clone())
        .collect();
    assert_eq!(kids, ["authority_policy"]);
}

#[test]
fn ap9_approving_it_lands_the_policy() {
    let (_d, mut r) = with_policy_doc();
    {
        let st = r.read();
        let canon = st.materialized(&Scope::Canon).unwrap();
        assert!(live_policy(&st, &canon).is_none(), "no policy before");
    }
    let info = r.policy_init(&a1(), Some("policy"), None).unwrap();
    assert!(info.proposal.ready, "{:?}", info.proposal.issues);
    confirm_before_landing(&mut r, &approver(), &info.proposal);
    r.land(&approver(), &info.proposal.id).unwrap();
    let st = r.read();
    let canon = st.materialized(&Scope::Canon).unwrap();
    let live = live_policy(&st, &canon).expect("canon holds the policy after the landing");
    assert_eq!(live.block.to_string(), info.policy);
    let fields = live.fields.expect("the template validates");
    assert!(fields.rules.is_empty() && fields.roles.is_empty());
    assert!(fields.require.is_empty() && fields.owners.is_empty());
    // the decision landed with it
    let d = BlockId::parse(&info.decision).unwrap();
    assert!(canon.get(&d).is_some());
}

#[test]
fn ap9_init_with_a_live_policy_is_e_bad_input() {
    let (_d, mut r) = with_policy_doc();
    let info = r.policy_init(&a1(), Some("policy"), None).unwrap();
    // the view holds a policy already
    let e = r.policy_init(&a1(), Some("policy"), None).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
    assert_eq!(
        e.message,
        format!("the project has an authority policy: {}", info.policy)
    );
    assert_eq!(
        e.fix,
        "change it with rdy_write / rdy edit, with implements a decision"
    );
    // canon holds one: another doc, another session
    confirm_before_landing(&mut r, &approver(), &info.proposal);
    r.land(&approver(), &info.proposal.id).unwrap();
    add_other_doc(&mut r);
    let b2 = agent("agent:b2", Some("r2"));
    let e = r.policy_init(&b2, Some("other"), None).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
    assert_eq!(
        e.message,
        format!("the project has an authority policy: {}", info.policy)
    );
    // nothing was written in the second session's view
    assert!(r.read().included("r2").is_empty());
}

fn add_other_doc(r: &mut Rdy) {
    write(
        r,
        "docs2",
        json!([{"op": "create", "type": "doc", "fields": {"name": "other", "title": "Other", "doc_type": "page"}}]),
    )
    .unwrap();
    land(r, "docs2").unwrap();
}

#[test]
fn ap9_init_with_no_doc_or_a_folder_is_e_bad_input() {
    let (_d, mut r) = with_policy_doc();
    write(
        &mut r,
        "dirs",
        json!([{"op": "create", "type": "folder", "fields": {"name": "areas"}}]),
    )
    .unwrap();
    land(&mut r, "dirs").unwrap();
    let want = |e: RdyError| {
        assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
        assert_eq!(e.message, "name the doc that holds the authority policy");
        assert_eq!(
            e.fix,
            "pass doc: rdy_policy {op: \"init\", doc} / rdy policy init --doc <doc>"
        );
    };
    want(r.policy_init(&a1(), None, None).unwrap_err());
    want(r.policy_init(&a1(), Some("  "), None).unwrap_err());
    want(r.policy_init(&a1(), Some("areas"), None).unwrap_err());
    // a name that is no block is the read's own refusal
    let e = r.policy_init(&a1(), Some("nothing"), None).unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_BLOCK", "{e:?}");
    assert!(r.read().included("r1").is_empty(), "nothing was written");
}

#[test]
fn ap9_init_under_a_stable_doc_briefs_itself() {
    let (_d, mut r) = project();
    write(
        &mut r,
        "docs",
        json!([{"op": "create", "type": "doc", "fields": {"name": "policy", "title": "Policy", "doc_type": "page", "lifecycle": "stable"}}]),
    )
    .unwrap();
    land(&mut r, "docs").unwrap();
    let info: PolicyInitInfo = r.policy_init(&a1(), Some("policy"), None).unwrap();
    assert_eq!(info.proposal.changes, 2);
    let _: Hash = info.proposal.id.parse().unwrap();
}

#[test]
fn ap9_cli_policy_init_matches_mcp() {
    // the tool, on a project of its own
    let (_d, r) = with_policy_doc();
    let mut st = agent_state(r);
    let mcp = init(&mut st, json!({"op": "init", "doc": "policy"})).unwrap();
    // the command, through the daemon
    let s = Served::new();
    s.stop();
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        add_doc(&mut r);
        r.close().unwrap();
    }
    let (ok, cli) = s.json(&["--json", "policy", "init", "--doc", "policy"]);
    assert!(ok, "{cli}");
    assert_eq!(shape(&cli), shape(&mcp), "cli {cli}\nmcp {mcp}");
    assert_eq!(cli["proposal"]["title"], TITLE);
    assert_eq!(cli["proposal"]["summary"], mcp["proposal"]["summary"]);
    assert_eq!(cli["proposal"]["changes"], 2);
    assert_eq!(cli["proposal"]["ready"], true, "{cli}");
    // the text names the records and the next step
    let s2 = Served::new();
    s2.stop();
    {
        let mut r = Rdy::open(s2.dir.path()).unwrap();
        add_doc(&mut r);
        r.close().unwrap();
    }
    let o = s2.rdy(&["policy", "init", "--doc", "policy", "--view", "w1"]);
    let text = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(o.status.success(), "{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("decision D-"), "{text}");
    assert!(lines[1].starts_with("policy ap-"), "{text}");
    assert!(lines[2].starts_with("proposal P-"), "{text}");
    assert_eq!(lines[3], "ready", "{text}");
    assert!(
        lines[4].starts_with("approve with: rdy approve P-"),
        "{text}"
    );
}

#[test]
fn ap9_an_unknown_op_is_refused_with_did_you_mean() {
    let (d, r) = with_policy_doc();
    let mut st = agent_state(r);
    let e = init(&mut st, json!({"op": "inti", "doc": "policy"})).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
    assert_eq!(e.message, "unknown policy op \"inti\"");
    assert!(e.fix.starts_with("did you mean \"init\"?"), "{e:?}");
    let e = init(&mut st, json!({"op": "list"})).unwrap_err();
    assert_eq!(e.fix, "use init", "{e:?}");
    drop(st);
    // a key the op does not take, through the daemon op
    let r = Rdy::open(d.path()).unwrap();
    let engine = Local::new(r);
    let s = as_agent("agent:a1", "r1");
    let cx = OpContext {
        engine: &engine,
        session: &s,
        human: "human:qa",
        cwd: d.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };
    let e = dispatch(&cx, "policy", json!({"op": "init", "dco": "policy"})).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
    assert!(e.fix.starts_with("did you mean \"doc\"?"), "{e:?}");
    engine.close().unwrap();
}
