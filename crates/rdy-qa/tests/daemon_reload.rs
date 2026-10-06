//! reload1: a long-lived daemon follows .rdy/config.yaml and .rdy/packs; an invalid pack is kept
//! out and reported on every reply until fixed.
use rdy_qa::daemon::Hosted;
use serde_json::json;

const PACK_A: &str = "name: proj\nversion: '1'\nextends: rdy-standard\n";
const PACK_BAD: &str = "name: proj\nversion: '1'\nextends: no-such-pack\n";

fn switch_to(h: &Hosted, pack: &str) {
    std::fs::write(h.dir.path().join(".rdy/packs/proj.yaml"), pack).unwrap();
    std::fs::write(h.dir.path().join(".rdy/config.yaml"), "pack: proj\n").unwrap();
}

#[test]
fn reload1_a_write_after_an_edit_reloads_and_every_session_hears_pack_changed_once() {
    let h = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut a = h.client(None, None).await;
        let mut b = h.client(None, None).await;
        assert_eq!(
            a.ctl("status", json!({})).await.unwrap()["pack"],
            "rdy-standard"
        );
        switch_to(&h, PACK_A);
        // a read on an existing connection does not reload
        assert_eq!(
            a.ctl("status", json!({})).await.unwrap()["pack"],
            "rdy-standard"
        );
        assert!(a.take_notice().is_none());
        // a write does
        a.ctl("new_doc", json!({"path": "d", "view": null}))
            .await
            .unwrap();
        assert_eq!(
            a.take_notice().as_deref(),
            Some("pack_changed: the pack was reloaded (generation 1)")
        );
        assert_eq!(a.ctl("status", json!({})).await.unwrap()["pack"], "proj");
        assert!(a.take_notice().is_none(), "once per session");
        assert_eq!(b.ctl("status", json!({})).await.unwrap()["pack"], "proj");
        assert_eq!(
            b.take_notice().as_deref(),
            Some("pack_changed: the pack was reloaded (generation 1)")
        );
        assert!(b.take_notice().is_none());
    });
}

#[test]
fn reload1_a_new_connection_reloads_before_its_first_reply() {
    let h = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut a = h.client(None, None).await;
        assert_eq!(
            a.ctl("status", json!({})).await.unwrap()["pack"],
            "rdy-standard"
        );
        switch_to(&h, PACK_A);
        let mut c = h.client(None, None).await;
        assert_eq!(c.ctl("status", json!({})).await.unwrap()["pack"], "proj");
        assert!(
            c.take_notice().is_none(),
            "a session that connected after the reload has nothing to hear"
        );
        assert_eq!(a.ctl("status", json!({})).await.unwrap()["pack"], "proj");
        assert!(a.take_notice().unwrap().starts_with("pack_changed"));
    });
}

#[test]
fn reload1_an_invalid_pack_keeps_the_previous_one_and_reports_on_every_reply() {
    let h = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut a = h.client(None, None).await;
        switch_to(&h, PACK_A);
        a.ctl("new_doc", json!({"path": "d1", "view": null}))
            .await
            .unwrap();
        assert!(a.take_notice().unwrap().starts_with("pack_changed"));
        std::fs::write(h.dir.path().join(".rdy/packs/proj.yaml"), PACK_BAD).unwrap();
        a.ctl("new_doc", json!({"path": "d2", "view": null}))
            .await
            .unwrap();
        let n = a.take_notice().unwrap();
        assert!(
            n.starts_with("pack_error: ")
                && n.contains("no-such-pack")
                && n.ends_with("the previous pack stays loaded until the file is fixed"),
            "{n}"
        );
        assert_eq!(
            a.ctl("status", json!({})).await.unwrap()["pack"],
            "proj",
            "the previous pack stays"
        );
        assert!(
            a.take_notice().unwrap().starts_with("pack_error"),
            "every reply, reads included"
        );
        a.ctl("ping", json!({})).await.unwrap();
        assert!(
            a.take_notice().unwrap().starts_with("pack_error"),
            "built-ins too"
        );
        // a new session hears the standing error too
        let mut b = h.client(None, None).await;
        b.ctl("status", json!({})).await.unwrap();
        assert!(b.take_notice().unwrap().starts_with("pack_error"));
        // the fix clears it with one pack_changed
        std::fs::write(
            h.dir.path().join(".rdy/packs/proj.yaml"),
            format!("{PACK_A}# fixed\n"),
        )
        .unwrap();
        a.ctl("new_doc", json!({"path": "d3", "view": null}))
            .await
            .unwrap();
        assert_eq!(
            a.take_notice().as_deref(),
            Some("pack_changed: the pack was reloaded (generation 3)")
        );
        a.ctl("status", json!({})).await.unwrap();
        assert!(a.take_notice().is_none());
    });
}

#[test]
fn reload1_the_watch_reads_bytes_only_when_a_stat_changes() {
    let h = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut a = h.client(None, None).await;
        for i in 0..20 {
            a.ctl("new_doc", json!({"path": format!("d{i}"), "view": null}))
                .await
                .unwrap();
        }
    });
    let hashed = h.run.as_ref().unwrap().shared().watch.stats();
    assert!(
        hashed <= 1,
        "twenty writes with no edit hashed the pack files {hashed} times"
    );
}

#[test]
fn reload1_mcp_results_carry_the_notice_once() {
    let h = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (r, mut w) = h.mcp_stream().await.into_split();
        let mut r = BufReader::new(r);
        let mut line = String::new();
        let line_of = |v: serde_json::Value| format!("{v}\n");
        w.write_all(line_of(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": rdy_qa::mcp::PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}})).as_bytes()).await.unwrap();
        r.read_line(&mut line).await.unwrap();
        w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n").await.unwrap();
        w.write_all(line_of(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "rdy_session", "arguments": {"actor": "agent:m", "role": "writer"}}})).as_bytes()).await.unwrap();
        line.clear();
        r.read_line(&mut line).await.unwrap();
        switch_to(&h, PACK_A);
        w.write_all(line_of(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "rdy_write", "arguments": {"ops": [{"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "glossary"}}]}}})).as_bytes()).await.unwrap();
        line.clear();
        r.read_line(&mut line).await.unwrap();
        assert!(line.contains("pack_changed"), "{line}");
        w.write_all(line_of(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "rdy_overview", "arguments": {}}})).as_bytes()).await.unwrap();
        line.clear();
        r.read_line(&mut line).await.unwrap();
        assert!(!line.contains("pack_changed"), "once: {line}");
        assert!(line.contains("proj"), "{line}");
    });
}

#[test]
fn reload1_the_cli_prints_a_standing_pack_error_on_stderr() {
    let s = rdy_qa::daemon::Served::new();
    assert!(s.json(&["--json", "status"]).0);
    std::fs::write(s.dir.path().join(".rdy/packs/proj.yaml"), PACK_BAD).unwrap();
    std::fs::write(
        s.dir.path().join(".rdy/config.yaml"),
        "pack: proj\nserve:\n  idle_timeout: 60\n",
    )
    .unwrap();
    let out = s.rdy(&["status"]);
    assert!(out.status.success(), "the previous pack stays loaded");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("notice: pack_error: "), "{err}");
}

#[test]
fn reload1_reverting_an_invalid_pack_to_the_loaded_bytes_clears_the_error() {
    let h = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut a = h.client(None, None).await;
        switch_to(&h, PACK_A);
        a.ctl("new_doc", json!({"path": "d1", "view": null}))
            .await
            .unwrap();
        assert!(a.take_notice().unwrap().starts_with("pack_changed"));
        std::fs::write(h.dir.path().join(".rdy/packs/proj.yaml"), PACK_BAD).unwrap();
        a.ctl("new_doc", json!({"path": "d2", "view": null}))
            .await
            .unwrap();
        assert!(a.take_notice().unwrap().starts_with("pack_error"));
        // back to the bytes that are loaded: the pack never changed, and nothing stands in error
        std::fs::write(h.dir.path().join(".rdy/packs/proj.yaml"), PACK_A).unwrap();
        a.ctl("new_doc", json!({"path": "d3", "view": null}))
            .await
            .unwrap();
        assert!(
            a.take_notice().is_none(),
            "the error clears with the revert"
        );
        assert_eq!(a.ctl("status", json!({})).await.unwrap()["pack"], "proj");
        assert!(a.take_notice().is_none());
    });
}
