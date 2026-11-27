//! r59011's profile driver (ignored): a seed push of a frozen project dump to the fixture hub,
//! then a first URL clone of it, both through the release `rdy` command line. It reports wall
//! times, each hub request's time and bytes, and whether the clone holds the source's history,
//! as one JSON object for the orchestrator's measurement step. It asserts no time.
//!
//! Inputs, all required: `RDY_PROFILE_DUMP` (the dump file, at most 536,870,912 bytes),
//! `RDY_PROFILE_DUMP_SHA256` (its SHA-256, hex) and `RDY_PROFILE_REPORT` (where the report goes;
//! no file may be there). The dump is checked against its digest before it is loaded. The report
//! names no token, URL, title, block text or path.

use super::mock::{Hub, Served};
use super::{Machine, held_at, rdy_bin, run_json};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Instant;

/// The most bytes of a profile dump.
const MAX_DUMP_BYTES: u64 = 536_870_912;

const INVALID: &str = "profile inputs are not valid";
const UNWRITTEN: &str = "profile report could not be written";

/// The checked inputs: the dump's verified bytes, their digest, and the report path.
struct Inputs {
    dump: Vec<u8>,
    sha256: String,
    report: PathBuf,
}

fn inputs() -> Option<Inputs> {
    let dump = PathBuf::from(std::env::var_os("RDY_PROFILE_DUMP")?);
    let want = std::env::var("RDY_PROFILE_DUMP_SHA256")
        .ok()?
        .to_ascii_lowercase();
    let report = PathBuf::from(std::env::var_os("RDY_PROFILE_REPORT")?);
    if want.len() != 64 || !want.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // the report path must be free, a dangling link included
    if std::fs::symlink_metadata(&report).is_ok() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&dump)
        .ok()?
        .take(MAX_DUMP_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_DUMP_BYTES {
        return None;
    }
    let got: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    (got == want).then_some(Inputs {
        dump: bytes,
        sha256: got,
        report,
    })
}

/// The report's route of a served request, or None for a route the report has no name for.
fn route_of(s: &Served) -> Option<&'static str> {
    Some(match s.route.as_str() {
        "discovery" => "discovery",
        "state" => "state",
        "settings" => "settings",
        "known" => "known",
        "objects/get" => "get",
        "objects/put" => "put",
        "write" => "write",
        _ => return None,
    })
}

/// The `rdy --version` line of the binary under test.
fn client_revision(m: &Machine) -> String {
    let o = m
        .command(m.path(), &["--version"])
        .output()
        .unwrap_or_else(|_| panic!("{INVALID}"));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
#[ignore = "the orchestrator's profile driver: needs a frozen dump and its digest"]
fn r59011_profile_seed_push_and_first_clone() {
    let Some(inputs) = inputs() else {
        panic!("{INVALID}");
    };
    let _ = rdy_bin();
    let hub = Hub::start();
    let a = Machine::new("a", &hub);
    // load exactly the verified bytes
    let scratch = tempfile::tempdir().unwrap_or_else(|_| panic!("{INVALID}"));
    let dump = scratch.path().join("profile.rdydump");
    std::fs::write(&dump, &inputs.dump).unwrap_or_else(|_| panic!("{INVALID}"));
    drop(inputs.dump);
    rdy_engine::load::load(&dump, a.path()).unwrap_or_else(|_| panic!("{INVALID}"));
    std::fs::remove_file(&dump).unwrap_or_else(|_| panic!("{INVALID}"));
    a.remote(&hub.url());
    let source = held_at(a.path());

    let start = Instant::now();
    let (ok, _) = {
        let _scope = hub.scope("push");
        a.cli(&["push"])
    };
    let push_ms = start.elapsed().as_secs_f64() * 1000.0;
    assert!(ok, "the profile push");

    let c = Machine::new("c", &hub);
    let target = c.path().join("clone");
    let start = Instant::now();
    let (ok, _) = {
        let _scope = hub.scope("clone");
        let mut cmd = c.command(
            c.path(),
            &["--json", "clone", &hub.url(), target.to_str().unwrap()],
        );
        cmd.stdin(Stdio::null());
        run_json(cmd)
    };
    let clone_ms = start.elapsed().as_secs_f64() * 1000.0;
    assert!(ok, "the profile clone");
    let history_equal = held_at(&target) == source;

    let mut requests = Vec::new();
    for operation in ["push", "clone"] {
        let rows = hub
            .served()
            .into_iter()
            .filter(|s| s.operation == operation);
        for (sequence, s) in rows.enumerate() {
            let route = route_of(&s).unwrap_or_else(|| panic!("{UNWRITTEN}"));
            requests.push(
                json!({"operation": operation, "sequence": sequence, "route": route,
                "wire_bytes": s.wire_bytes, "raw_bytes": s.raw_bytes,
                "server_ms": s.server_ms}),
            );
        }
    }
    let report: Value = json!({"schema": 1, "input_sha256": inputs.sha256,
        "client_revision": client_revision(&a),
        "mock_revision": env!("CARGO_PKG_VERSION"), "push_ms": push_ms,
        "clone_ms": clone_ms, "requests": requests, "history_equal": history_equal});
    let text = serde_json::to_vec_pretty(&report).unwrap_or_else(|_| panic!("{UNWRITTEN}"));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&inputs.report)
        .unwrap_or_else(|_| panic!("{UNWRITTEN}"));
    f.write_all(&text)
        .and_then(|()| f.sync_all())
        .unwrap_or_else(|_| panic!("{UNWRITTEN}"));
    hub.finish();
}
