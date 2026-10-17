//! ec10: `rdy_overview` names the repo id and, for a clone or a fork, where it came from (plan
//! Export and clone; D-KQ4CA: the chat path shows what `rdy status` shows).
use rdy_engine::Rdy;
use rdy_engine::clone::{CloneSource, clone};
use rdy_qa::daemon::short_tempdir;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

fn overview(project: &std::path::Path) -> Value {
    let mut c = McpClient::start(project);
    let r = c.call("rdy_session", json!({"actor": "agent:o", "role": "writer"}));
    assert!(r.code().is_none(), "{:?}", r.ok());
    c.call("rdy_overview", json!({})).ok()
}

#[test]
fn ec10_rdy_overview_names_the_repo_id_and_origin() {
    let scratch = short_tempdir();
    let src = scratch.path().join("src");
    let r = Rdy::init(&src, "rdy-standard").unwrap();
    let id = r.read().repo_id().unwrap().to_string();
    let to = scratch.path().join("clone");
    clone(CloneSource::State(&r.read()), &to, false).unwrap();
    let fork = scratch.path().join("fork");
    clone(CloneSource::State(&r.read()), &fork, true).unwrap();
    r.close().unwrap();

    // a project that is not a clone: the id, no origin
    let o = overview(&src);
    assert_eq!(o["project"]["repo_id"], id, "{o}");
    assert!(o["project"].get("origin").is_none(), "{o}");
    assert!(o["project"].get("forked_from").is_none(), "{o}");

    // a clone: the same id, the origin names the source
    let o = overview(&to);
    assert_eq!(o["project"]["repo_id"], id, "{o}");
    assert_eq!(
        o["project"]["origin"],
        json!({"repo_id": id, "tag": null, "landing": null}),
        "{o}"
    );
    assert!(o["project"].get("forked_from").is_none(), "{o}");

    // a fork: a new id, `forked_from` names the source
    let o = overview(&fork);
    let fid = o["project"]["repo_id"].as_str().unwrap();
    assert_ne!(fid, id, "{o}");
    assert_eq!(o["project"]["forked_from"]["repo_id"], id, "{o}");
    assert!(o["project"].get("origin").is_none(), "{o}");
}
