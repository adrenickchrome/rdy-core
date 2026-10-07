//! erow1: rdy_schema's error rows come from one table that covers every view, briefing and daemon
//! code, and the E_VIEW_OWNER row says what M2c3 built; FW-LP5's instruction line.
use rdy_engine::*;
use rdy_mcp::tools::{EDIT_LOOP_DOC, ERROR_ROWS};

fn row(code: &str) -> (&'static str, &'static str) {
    ERROR_ROWS
        .iter()
        .find(|(c, _, _)| *c == code)
        .map(|(_, w, f)| (*w, *f))
        .unwrap_or_else(|| panic!("no row for {code}"))
}

#[test]
fn erow1_every_view_brief_and_daemon_code_has_a_row() {
    for code in [
        E_VIEW_OWNER,
        E_VIEW_EXISTS,
        E_VIEW_CLOSED,
        E_HAS_DEPENDENTS,
        E_DEPS_MISSING,
        E_CONFLICT_UNRESOLVED,
        E_PROPOSAL_MOVED,
        E_UNDO_BLOCKED,
        E_UNDO_LANDING,
        E_NOTHING_TO_UNDO,
        E_BRIEF_REQUIRED,
        E_BRIEF_STALE,
        W_BRIEF_STALE,
        E_PAGE_STALE,
        "E_DAEMON_START",
        "E_DAEMON_VERSION",
        "E_DAEMON_GONE",
    ] {
        let (when, fix) = row(code);
        assert!(!when.is_empty() && !fix.is_empty(), "{code}");
    }
    let codes: Vec<&str> = ERROR_ROWS.iter().map(|(c, _, _)| *c).collect();
    let mut unique = codes.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), codes.len(), "one row per code");
}

#[test]
fn erow1_the_e_view_owner_row_names_every_owned_action_and_the_humans_view() {
    let (when, fix) = row(E_VIEW_OWNER);
    for word in [
        "commits", "includes", "excludes", "undoes", "closes", "proposes", "work", "claims",
    ] {
        assert!(when.contains(word), "{word}: {when}");
    }
    assert!(
        fix.contains("fork") && fix.contains("work-<actor>"),
        "{fix}"
    );
}

#[test]
fn erow1_the_edit_loop_doc_and_instructions_carry_the_m2c3_lines() {
    for word in [
        "inflight",
        "page",
        "already_written",
        "lineage",
        "conflict entries",
        "rdy_propose",
    ] {
        assert!(EDIT_LOOP_DOC.contains(word), "{word}: {EDIT_LOOP_DOC}");
    }
    let i = rdy_mcp::server::INSTRUCTIONS;
    assert!(
        i.contains("a second time in a session") && i.contains("change plan"),
        "{i}"
    );
}

#[test]
fn erow1_the_views_doc_names_codes_but_defines_none() {
    // the errors table is the one source of each code's when and fix
    let doc = rdy_mcp::view_tools::SCHEMA_DOC;
    for (code, _, _) in ERROR_ROWS {
        assert!(
            !doc.contains(&format!("{code}:")),
            "{code} is defined in the views doc: {doc}"
        );
    }
    assert!(doc.contains("E_VIEW_OWNER"), "{doc}");
}

#[test]
fn erow1_the_edit_loop_doc_says_every_page_of_a_cut_briefing_is_truncated() {
    assert!(
        EDIT_LOOP_DOC.contains("Every page of a cut briefing has truncated: true and docs"),
        "{EDIT_LOOP_DOC}"
    );
}
