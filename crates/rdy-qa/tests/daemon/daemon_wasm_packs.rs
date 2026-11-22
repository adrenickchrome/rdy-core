//! wck3: a loaded pack's rules compile once, with the pack, into one handle. Warning reads stay
//! structural and compile nothing; a reload that fails or panics keeps the old pack and its
//! compiled rules together; a holder has no compiled project rules and reads no project pack.
//!
//! The warning-read test re-runs itself as a child that runs it alone (`--exact`, one thread):
//! the compilation counter is per process, and the area binary runs other tests beside it. Its
//! pack is a public pack of module rules.
//!
//! wck6 (D-WV7ZS, D-QVTJ9): a daemon compiles its pack's rules once per loaded pack, never per
//! request, and a pack whose module does not compile keeps the previous pack and its rules
//! running; a holder never loads or runs a module.
use rdy_engine::check::module_run::module_runs_on_this_thread_for_tests;
use rdy_engine::check::rules::{
    module_compilations_on_this_thread_for_tests, rule_compilations_for_tests,
    rule_compilations_on_this_thread_for_tests,
};
use rdy_engine::{Rdy, ReloadOutcome, pack_source_reads_on_this_thread};
use rdy_qa::daemon::Hosted;
use serde_json::{Value, json};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;

const WARNING_READS: &str = "wck3_warning_reads_compile_no_module";

const RULE: &str = "  - name: r1\n    select: type = section\n    require: type = section\n    message: m\n    fix: f\n";
const OTHER_RULE: &str = "  - name: r2\n    select: type = section\n    require: type = section\n    message: m\n    fix: f\n";
const BAD_RULE: &str = "  - name: r1\n    select: type = = section\n    require: type = section\n    message: m\n    fix: f\n";

const PASS_WASM: &str = include_str!("../../fixtures/wasm/pass.b64");
const FAIL_WASM: &str = include_str!("../../fixtures/wasm/fail.b64");
const IMPORTS_WASM: &str = include_str!("../../fixtures/wasm/imports.b64");

/// A module rule `name` that runs `b64` on every claim.
fn module_rule(name: &str, b64: &str) -> String {
    format!(
        "  - name: {name}\n    select: type = claim\n    module: {}\n    message: {name} message\n    fix: {name} fix\n",
        b64.trim()
    )
}

/// Points the project at `dir` to the pack `proj`, which extends the built-in pack with `rules`.
fn set_pack(dir: &Path, rules: &str) {
    std::fs::write(
        dir.join(".rdy/packs/proj.yaml"),
        format!("name: proj\nversion: '1'\nextends: rdy-standard\nrules:\n{rules}"),
    )
    .unwrap();
    std::fs::write(dir.join(".rdy.yaml"), "pack: proj\n").unwrap();
}

#[test]
fn wck3_warning_reads_compile_no_module() {
    if std::env::var_os("RDY_QA_WCK3_CHILD").is_none() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &rdy_qa::test_name!(WARNING_READS),
                "--nocapture",
                "--test-threads=1",
            ])
            .env("RDY_QA_WCK3_CHILD", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stdout.contains("test result: ok. 1 passed"),
            "the child ran no test: {stdout}\n{stderr}"
        );
        return;
    }
    let dir = rdy_qa::daemon::short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    set_pack(
        dir.path(),
        &format!(
            "{}{}",
            module_rule("r1", PASS_WASM),
            module_rule("r2", FAIL_WASM)
        ),
    );
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "# Notes\n").unwrap();
    let h = Hosted::start_in(dir);
    let root = h.dir.path().to_path_buf();
    let rt = Hosted::rt();
    // warm the loaded handle: its one compilation is outside the counted span
    let mut a = rt.block_on(async {
        let mut a = h.client(None, None).await;
        assert_eq!(a.ctl("status", json!({})).await.unwrap()["pack"], "proj");
        a
    });
    let before = rule_compilations_for_tests();
    rt.block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        // the CLI's commands through the daemon: status reads the config warnings, check
        // resolves a named pack structurally, and a write runs the loaded rules
        let status = a.ctl("status", json!({})).await.unwrap();
        assert_eq!(status["pack"], "proj");
        a.ctl(
            "check",
            json!({"path": file.to_string_lossy(), "pack": "proj"}),
        )
        .await
        .unwrap();
        a.ctl("new_doc", json!({"path": "d1", "view": null}))
            .await
            .unwrap();
        // rdy_session reads the config warnings too
        let (r, mut w) = rdy_daemon::sys::split(h.mcp_stream().await);
        let mut r = BufReader::new(r);
        let mut line = String::new();
        let line_of = |v: serde_json::Value| format!("{v}\n");
        w.write_all(line_of(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": rdy_qa::mcp::PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}})).as_bytes()).await.unwrap();
        r.read_line(&mut line).await.unwrap();
        w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        w.write_all(line_of(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "rdy_session", "arguments": {"actor": "agent:m", "role": "writer"}}})).as_bytes()).await.unwrap();
        line.clear();
        r.read_line(&mut line).await.unwrap();
        assert!(line.contains("\"id\":2"), "{line}");
        assert!(!line.contains("\"isError\":true"), "{line}");
    });
    // the reads a CLI process makes on its own: every command's warning lines, `rdy config
    // --show-origin`'s pack warnings, and the working-copy commands' structural pack
    assert!(rdy_engine::config_levels::config_warnings_for_env(&root).is_empty());
    assert!(rdy_engine::config_levels::pack_warnings(&root).is_empty());
    assert_eq!(rdy_engine::project_pack(&root).unwrap().name, "proj");
    assert_eq!(
        rule_compilations_for_tests() - before,
        0,
        "warning reads and the loaded handle's reads compile no rules"
    );
    // `rdy pack check` is the one command that compiles
    let (pack, rules) = rdy_engine::project_pack_compiled(&root).unwrap();
    assert_eq!(pack.name, "proj");
    assert_eq!(rules.rules().len(), 2);
    assert_eq!(rule_compilations_for_tests() - before, 1);
}

#[test]
fn wck3_reload_panic_keeps_pack_and_compilation_together() {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    set_pack(dir.path(), RULE);
    let mut rdy = Rdy::open(dir.path()).unwrap();
    let pack: *const rdy_schema::Pack = rdy.pack();
    let rules = rdy.compiled_rules();
    assert_eq!(rules.rules().len(), 1);
    let state = rdy.read();
    assert!(Arc::ptr_eq(&state.compiled_rules().unwrap(), &rules));
    let together = |rdy: &Rdy, what: &str| {
        assert!(std::ptr::eq(rdy.pack(), pack), "{what}: the old pack stays");
        assert!(
            Arc::ptr_eq(&rdy.compiled_rules(), &rules),
            "{what}: its compiled rules and their cache stay"
        );
        let now = rdy.read();
        assert!(std::ptr::eq(now.pack(), pack), "{what}: the published pack");
        assert!(
            Arc::ptr_eq(&now.compiled_rules().unwrap(), &rules),
            "{what}: the published state's compiled rules"
        );
    };

    // a pack that does not compile: refused before anything is swapped
    set_pack(dir.path(), BAD_RULE);
    let (compiles, reads) = (
        rule_compilations_on_this_thread_for_tests(),
        pack_source_reads_on_this_thread(),
    );
    assert!(matches!(
        rdy.reload_pack().unwrap(),
        ReloadOutcome::Invalid { .. }
    ));
    assert_eq!(pack_source_reads_on_this_thread() - reads, 1);
    assert_eq!(
        rule_compilations_on_this_thread_for_tests() - compiles,
        0,
        "the bad rule is refused while the pack resolves"
    );
    together(&rdy, "an invalid reload");

    // a valid pack whose publish panics: the swap is undone, pack and rules together
    set_pack(dir.path(), &format!("{RULE}{OTHER_RULE}"));
    rdy.panic_full_publishes_for_tests(1);
    let compiles = rule_compilations_on_this_thread_for_tests();
    let out = catch_unwind(AssertUnwindSafe(|| rdy.reload_pack()));
    assert!(out.is_err(), "the reload's publish panicked");
    assert_eq!(
        rule_compilations_on_this_thread_for_tests() - compiles,
        1,
        "the new rules compiled before the swap"
    );
    together(&rdy, "a reload whose publish panicked");

    // the hook is spent: the same files load, and pack and rules move together
    assert!(matches!(
        rdy.reload_pack().unwrap(),
        ReloadOutcome::Reloaded
    ));
    let next = rdy.compiled_rules();
    assert!(!Arc::ptr_eq(&next, &rules));
    assert_eq!(next.rules().len(), 2);
    assert!(next.cache().is_empty(), "a new pack starts an empty cache");
    assert!(!std::ptr::eq(rdy.pack(), pack));
    assert!(Arc::ptr_eq(&rdy.read().compiled_rules().unwrap(), &next));
    // a state read before the reload keeps the rules it was published with
    assert!(Arc::ptr_eq(&state.compiled_rules().unwrap(), &rules));
}

#[test]
fn wck3_holder_has_no_compiled_project_rules() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("holder");
    let (compiles, reads) = (
        rule_compilations_on_this_thread_for_tests(),
        pack_source_reads_on_this_thread(),
    );
    let created = Rdy::create_holder(&root, rdy_model::RepoId([7; 16])).unwrap();
    assert!(created.compiled_rules().is_empty());
    assert!(created.read().compiled_rules().unwrap().is_empty());
    drop(created);
    // a project pack with a rule beside the holder's store is never read
    std::fs::create_dir_all(root.join(".rdy/packs")).unwrap();
    set_pack(&root, RULE);
    let mut held = Rdy::open_holder(&root).unwrap();
    assert!(held.compiled_rules().is_empty());
    assert!(held.read().compiled_rules().unwrap().is_empty());
    assert_ne!(held.pack().name, "proj");
    assert!(matches!(
        held.reload_pack().unwrap(),
        ReloadOutcome::Unchanged
    ));
    assert!(held.compiled_rules().is_empty());
    assert_eq!(
        pack_source_reads_on_this_thread() - reads,
        0,
        "a holder reads no project pack file"
    );
    assert_eq!(
        rule_compilations_on_this_thread_for_tests() - compiles,
        0,
        "a holder compiles no project rules"
    );
}

const COMPILES_ONCE: &str = "wck6_daemon_compiles_once_and_keeps_the_previous_pack";

/// One MCP tool call on a daemon stream: the call's parsed reply, notifications skipped.
async fn call<R, W>(r: &mut R, w: &mut W, id: u64, name: &str, args: Value) -> Value
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let msg = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}});
    w.write_all(format!("{msg}\n").as_bytes()).await.unwrap();
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            r.read_line(&mut line).await.unwrap() > 0,
            "the stream ended"
        );
        let v: Value = serde_json::from_str(&line).unwrap();
        if v["id"] == id {
            assert!(v["result"]["isError"] != true, "{v}");
            let text = v["result"]["content"][0]["text"].as_str().unwrap();
            return serde_json::from_str(text).unwrap();
        }
    }
}

/// The rule names of a write reply's check rows, distinct.
fn rule_names(v: &Value) -> std::collections::BTreeSet<String> {
    v["on_change"]["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("{v}"))
        .iter()
        .map(|r| r["rule"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn wck6_daemon_compiles_once_and_keeps_the_previous_pack() {
    // the compilation counter is per process: run alone in a child, as the warning-read test
    if std::env::var_os("RDY_QA_WCK6_CHILD").is_none() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &rdy_qa::test_name!(COMPILES_ONCE),
                "--nocapture",
                "--test-threads=1",
            ])
            .env("RDY_QA_WCK6_CHILD", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stdout.contains("test result: ok. 1 passed"),
            "the child ran no test: {stdout}\n{stderr}"
        );
        return;
    }
    let dir = rdy_qa::daemon::short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    set_pack(dir.path(), &module_rule("keep", FAIL_WASM));
    let h = Hosted::start_in(dir);
    let root = h.dir.path().to_path_buf();
    let rt = Hosted::rt();
    rt.block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let mut a = h.client(None, None).await;
        assert_eq!(a.ctl("status", json!({})).await.unwrap()["pack"], "proj");
        let before = rule_compilations_for_tests();
        let (r, mut w) = rdy_daemon::sys::split(h.mcp_stream().await);
        let mut r = BufReader::new(r);
        let mut line = String::new();
        w.write_all(format!("{}\n", json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": rdy_qa::mcp::PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}})).as_bytes()).await.unwrap();
        r.read_line(&mut line).await.unwrap();
        w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        call(&mut r, &mut w, 2, "rdy_session", json!({"actor": "agent:m", "role": "writer"})).await;
        // writes run the loaded rules and compile nothing
        let v = call(&mut r, &mut w, 3, "rdy_write", json!({"view": "v", "ops": [
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "parent": "$s", "body": "One."}
        ]})).await;
        assert_eq!(rule_names(&v), ["keep".to_string()].into(), "{v}");
        let s = v["created"]["$s"].as_str().unwrap().to_string();
        let claim = |n: u64| json!({"view": "v", "ops": [{"op": "create", "type": "claim", "parent": s, "body": format!("Claim {n}.")}]});
        for n in 4..7 {
            let v = call(&mut r, &mut w, n, "rdy_write", claim(n)).await;
            assert_eq!(rule_names(&v), ["keep".to_string()].into(), "{v}");
        }
        assert_eq!(rule_compilations_for_tests() - before, 0, "no request compiles");
        // a changed pack compiles once, when it loads
        set_pack(
            &root,
            &format!("{}{}", module_rule("keep", FAIL_WASM), module_rule("next", FAIL_WASM)),
        );
        a.ctl("new_doc", json!({"path": "d1", "view": null})).await.unwrap();
        assert!(a.take_notice().unwrap().starts_with("pack_changed"));
        assert_eq!(rule_compilations_for_tests() - before, 1);
        let v = call(&mut r, &mut w, 7, "rdy_write", claim(7)).await;
        assert_eq!(
            rule_names(&v),
            ["keep".to_string(), "next".to_string()].into(),
            "{v}"
        );
        // a pack whose module does not compile is refused: the previous pack and its compiled
        // rules stay, and later requests compile nothing more
        set_pack(&root, &module_rule("bad", IMPORTS_WASM));
        a.ctl("new_doc", json!({"path": "d2", "view": null})).await.unwrap();
        let n = a.take_notice().unwrap();
        assert!(
            n.starts_with("pack_error: ")
                && n.contains(
                    "invalid pack: rule bad module imports env.clock; a check module imports nothing"
                )
                && n.ends_with("the previous pack stays loaded until the file is fixed"),
            "{n}"
        );
        let tried = rule_compilations_for_tests() - before;
        assert_eq!(tried, 2, "the refused pack compiled once");
        assert_eq!(a.ctl("status", json!({})).await.unwrap()["pack"], "proj");
        for n in 8..10 {
            let v = call(&mut r, &mut w, n, "rdy_write", claim(n)).await;
            assert_eq!(
                rule_names(&v),
                ["keep".to_string(), "next".to_string()].into(),
                "{v}"
            );
        }
        assert_eq!(rule_compilations_for_tests() - before, tried);
    });
}

#[test]
fn wck6_holder_never_loads_or_runs_a_module() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("holder");
    let counts = || {
        (
            rule_compilations_on_this_thread_for_tests(),
            module_compilations_on_this_thread_for_tests(),
            module_runs_on_this_thread_for_tests(),
            pack_source_reads_on_this_thread(),
        )
    };
    let start = counts();
    drop(Rdy::create_holder(&root, rdy_model::RepoId([7; 16])).unwrap());
    // a project pack of module rules beside the holder's store
    std::fs::create_dir_all(root.join(".rdy/packs")).unwrap();
    set_pack(
        &root,
        &format!(
            "{}{}",
            module_rule("keep", FAIL_WASM),
            module_rule("bad", IMPORTS_WASM)
        ),
    );
    let mut held = Rdy::open_holder(&root).unwrap();
    assert!(held.compiled_rules().is_empty());
    assert!(held.read().compiled_rules().unwrap().is_empty());
    assert_ne!(held.pack().name, "proj");
    assert!(matches!(
        held.reload_pack().unwrap(),
        ReloadOutcome::Unchanged
    ));
    assert!(held.compiled_rules().is_empty());
    drop(held);
    assert_eq!(
        counts(),
        start,
        "a holder reads no pack file, compiles no rule or module and runs no module"
    );
}
