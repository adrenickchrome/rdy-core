//! mfb2 (final review I4): over MCP, a reach entry (FW-LP6) never sends the agent to a repin rdy
//! refuses. The refined decision did not change, so the governed block already pins it: the
//! entry's fix says to check it, and that no repin is needed; the edit-loop doc says so too.
use rdy_engine::consequence::LINEAGE_FIX;
use rdy_mcp::tools::EDIT_LOOP_DOC;
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::json;

#[test]
fn mfb2_a_reach_entry_needs_no_repin() {
    let (dir, mut r) = project();
    let s = spec_doc(&mut r);
    r.close().unwrap();
    let mut c = McpClient::start(dir.path());
    let ses = c.call(
        "rdy_session",
        json!({"actor": "agent:r", "run": "r1", "role": "writer"}),
    );
    assert!(ses.code().is_none(), "{ses:?}");
    // the agent's view rewrites A (decided_by D) first: the reach entry is already_written
    let w = c.call(
        "rdy_write",
        json!({"ops": [{"op": "update", "id": s.a.to_string(), "body": "A holds, as written."}]}),
    );
    assert!(w.code().is_none(), "{w:?}");
    let d = c
        .call(
            "rdy_decide",
            json!({"parent": s.section.to_string(), "statement": "Keep A, in short form",
                   "refines": [s.decision.to_string()]}),
        )
        .ok();
    let entry = d["consequences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["block"] == s.a.to_string())
        .unwrap_or_else(|| panic!("no entry for A: {d}"))
        .clone();
    assert_eq!(entry["kind"], "dependent", "{entry}");
    assert_eq!(entry["follow_up"], "confirm", "{entry}");
    assert_eq!(entry["already_written"], true, "{entry}");
    assert_eq!(entry["fix"], LINEAGE_FIX, "{entry}");
    assert_eq!(
        LINEAGE_FIX,
        "a decision that governs it was refined or partly superseded: check that it still holds, and update it if not; no repin is needed"
    );
    // what the entry says is true: rdy has nothing to repin
    let repin = c.call(
        "rdy_write",
        json!({"ops": [{"op": "repin", "id": s.a.to_string()}]}),
    );
    assert_eq!(repin.code(), Some("E_BAD_INPUT"), "{repin:?}");
    assert!(
        EDIT_LOOP_DOC.contains(
            "A reach entry (FW-LP6) needs no repin: check it and update it only if it no longer holds"
        ),
        "{EDIT_LOOP_DOC}"
    );
    c.alive();
}

/// Final review M4: the edit-loop doc sends an already-written entry to its own follow_up and
/// fix (a kept entry whose target is deleted, superseded or revoked cannot be cleared by a
/// repin, and a live decision gets reaffirm), and says what a conflicted propose returns.
#[test]
fn df4_the_edit_loop_doc_sends_a_kept_entry_to_its_follow_up() {
    assert!(
        EDIT_LOOP_DOC.contains(
            "An entry with already_written: true is a block your view already rewrote. Follow its follow_up and fix. For confirm, check that the block still holds. If its fix gives no other instruction, repin it unless its meaning changed. For update, rewrite it to fit the change. A repin cannot clear a target that is gone, superseded or revoked. Point the block at another target or remove the link, as its fix says. For reaffirm, re-affirm the decision with a reaffirm op if it still holds; otherwise supersede it. A reach entry (FW-LP6) needs no repin: check it and update it only if it no longer holds."
        ),
        "{EDIT_LOOP_DOC}"
    );
    assert!(
        EDIT_LOOP_DOC.contains(
            "When the version has a blocking conflict, rdy_propose returns no consequences and a consequences_error (E_CONFLICT_UNRESOLVED) that names the blocks in conflict: write each from the version you want, then propose again."
        ),
        "{EDIT_LOOP_DOC}"
    );
    assert!(!EDIT_LOOP_DOC.contains("confirm it (repin) unless its meaning changed"));
    // an outside entry also carries already_written; its update means rewrite, not unlink
    assert!(!EDIT_LOOP_DOC.contains("update means its target is deleted"));
}
