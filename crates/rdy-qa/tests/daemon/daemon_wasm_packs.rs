//! wck3: a loaded pack's rules compile once, with the pack, into one handle. Warning reads stay
//! structural and compile nothing; a reload that fails or panics keeps the old pack and its
//! compiled rules together; a holder has no compiled project rules and reads no project pack.
//!
//! The warning-read test re-runs itself as a child that runs it alone (`--exact`, one thread):
//! the compilation counter is per process, and the area binary runs other tests beside it. The
//! pack is a test-only prepared pack of filter rules: module keys open in pack files later, and
//! the counter counts every rule-set compilation, whatever its rules' kind.
use rdy_engine::check::rules::{
    rule_compilations_for_tests, rule_compilations_on_this_thread_for_tests,
};
use rdy_engine::{Rdy, ReloadOutcome, pack_source_reads_on_this_thread};
use rdy_qa::daemon::Hosted;
use serde_json::json;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;

const WARNING_READS: &str = "wck3_warning_reads_compile_no_module";

const RULE: &str = "  - name: r1\n    select: type = section\n    require: type = section\n    message: m\n    fix: f\n";
const OTHER_RULE: &str = "  - name: r2\n    select: type = section\n    require: type = section\n    message: m\n    fix: f\n";
const BAD_RULE: &str = "  - name: r1\n    select: type = = section\n    require: type = section\n    message: m\n    fix: f\n";

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
    set_pack(dir.path(), &format!("{RULE}{OTHER_RULE}"));
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
