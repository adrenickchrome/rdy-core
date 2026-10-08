//! Crash consistency end to end (kill1): engine writers and a live MCP server are killed with
//! SIGKILL at arbitrary moments; reopening keeps every acknowledged write, catches the index up
//! from the log instead of rebuilding it, and passes the invariant checker and fsck.
use rdy_engine::{Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_qa::invariants;
use rdy_qa::mcp::McpClient;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
#[ignore = "kill1_engine_writes… runs this in a child process"]
fn store_crash_child() {
    let Ok(project) = std::env::var("RDY_CRASH_PROJECT") else {
        return;
    };
    let section = std::env::var("RDY_CRASH_SECTION").unwrap();
    let base: usize = std::env::var("RDY_CRASH_BASE").unwrap().parse().unwrap();
    let mut r = Rdy::open(std::path::Path::new(&project)).unwrap();
    let mut out = std::io::stdout().lock();
    for i in base..usize::MAX {
        let res = write(
            &mut r,
            "work",
            json!([{"op": "create", "type": "claim", "id": "$c", "parent": section, "body": format!("Claim {i}.")}]),
        )
        .unwrap();
        writeln!(out, "ACK {}", res.created["$c"]).unwrap();
        out.flush().unwrap();
    }
}

/// Review Focus 3.
#[test]
fn kill1_engine_writes_killed_at_random_keep_every_acknowledged_write() {
    let (dir, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Crash", "doc_type": "constraints"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Claims"}}
    ]))
    .unwrap();
    let section = res.created["$s"].clone();
    drop(r);
    let rounds = if deep() { 30 } else { 6 };
    let mut acked: Vec<String> = vec![];
    let mut seed: u64 = 0x5eed;
    for round in 0..rounds {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let want = 1 + ((seed >> 33) as usize) % 40;
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                &rdy_qa::test_name!("store_crash_child"),
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env("RDY_CRASH_PROJECT", dir.path())
            .env("RDY_CRASH_SECTION", &section)
            .env("RDY_CRASH_BASE", (round * 1000).to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut got = 0;
        while got < want {
            let line = lines.next().expect("the child stopped early").unwrap();
            if let Some(id) = line.strip_prefix("ACK ") {
                acked.push(id.to_string());
                got += 1;
            }
        }
        child.kill().unwrap(); // SIGKILL, somewhere inside the next write
        child.wait().unwrap();
        // writes acknowledged between `want` and the kill count too
        for line in lines.map_while(Result::ok) {
            if let Some(id) = line.strip_prefix("ACK ") {
                acked.push(id.to_string());
            }
        }
        let r = Rdy::open(dir.path()).unwrap_or_else(|e| panic!("round {round}: {e}"));
        let report = r.store().open_report().clone();
        assert!(
            !report.index_rebuilt,
            "round {round}: a kill is not damage: {report:?}"
        );
        assert!(
            report.caught_up_frames > 0,
            "round {round}: the index lagged: {report:?}"
        );
        let work = Scope::View("work".into());
        let m = r.read().materialized(&work).unwrap().clone();
        for id in &acked {
            assert!(
                m.get(&BlockId::parse(id).unwrap()).is_some(),
                "round {round}: lost {id}"
            );
        }
        invariants::check(&r, &work).unwrap_or_else(|v| panic!("round {round}: {v:?}"));
        r.fsck().unwrap();
    }
}

#[test]
fn kill1_a_killed_mcp_server_keeps_every_acknowledged_write() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let session = c
        .call(
            "rdy_session",
            json!({"actor": "agent:qa", "role": "writer"}),
        )
        .ok();
    let view = session["view"].as_str().unwrap().to_string();
    let mut ids = vec![];
    for i in 0..10 {
        let w = c
            .call("rdy_write", json!({"ops": [{"op": "create", "type": "doc", "id": "$d", "fields": {"title": format!("Killed {i}"), "doc_type": "glossary"}}]}))
            .ok();
        ids.push(w["created"]["$d"].as_str().unwrap().to_string());
    }
    drop(c); // SIGKILL: the server never closes the store
    let r = Rdy::open(dir.path()).unwrap();
    assert!(
        !r.store().open_report().index_rebuilt,
        "{:?}",
        r.store().open_report()
    );
    let m = r.read().materialized(&Scope::View(view)).unwrap().clone();
    for id in &ids {
        assert!(m.get(&BlockId::parse(id).unwrap()).is_some(), "lost {id}");
    }
    r.fsck().unwrap();
}
