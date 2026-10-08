//! cmcp1: rdy_brief {code_path} over MCP, through the daemon.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::*;
use rdy_qa::git_repo::GitRepo;
use rdy_qa::mcp::McpClient;
use serde_json::json;

fn served_with_code() -> (Served, Spec, GitRepo) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let spec = stable_spec(&mut r);
    r.close().unwrap();
    let g = GitRepo::new();
    g.link(s.dir.path());
    g.write("src/a.rs", "a\n");
    g.commit(&format!("feat: a\n\nImplements: {}", spec.a));
    (s, spec, g)
}

#[test]
fn cmcp1_rdy_brief_with_code_path_returns_the_code_section_and_a_citable_token() {
    let (s, sp, _g) = served_with_code();
    let mut c = McpClient::start_proxy(&s);
    let r = c.call("rdy_session", json!({"actor": "agent:m", "role": "writer"}));
    assert!(r.code().is_none(), "{:?}", r.ok());
    let b = c.call("rdy_brief", json!({"code_path": "src/a.rs"})).ok();
    assert_eq!(b["code"]["path"], "src/a.rs", "{b}");
    assert_eq!(b["code"]["implements"][0]["block"], sp.a.to_string(), "{b}");
    assert_eq!(b["covers"], 1, "{b}");
    let token = b["base"]["token"].as_str().unwrap().to_string();
    let w = c.call(
        "rdy_write",
        json!({"brief": token, "ops": [{"op": "update", "id": sp.a.to_string(), "body": "A, by the coder."}]}),
    );
    assert!(
        w.code().is_none(),
        "the code briefing's token lands a stable write: {w:?}"
    );
}

#[test]
fn cmcp1_code_path_with_ids_is_e_bad_input() {
    let (s, sp, _g) = served_with_code();
    let mut c = McpClient::start_proxy(&s);
    let r = c.call("rdy_session", json!({"actor": "agent:m", "role": "writer"}));
    assert!(r.code().is_none(), "{:?}", r.ok());
    let e = c.call(
        "rdy_brief",
        json!({"code_path": "src/a.rs", "ids": [sp.a.to_string()]}),
    );
    assert_eq!(e.code(), Some("E_BAD_INPUT"), "{e:?}");
    let bad = c.call("rdy_brief", json!({"code_path": "../a.rs"}));
    assert_eq!(bad.code(), Some("E_BAD_INPUT"), "{bad:?}");
}
