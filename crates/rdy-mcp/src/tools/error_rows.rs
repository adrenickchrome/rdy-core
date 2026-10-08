//! ERROR_ROWS: the error code table that rdy_schema reports.

use rdy_engine::*;

/// `rdy_schema`'s error rows: `(code, when, fix)`.
pub const ERROR_ROWS: &[(&str, &str, &str)] = &[
    (
        "E_VIEW_OWNER",
        "an agent session commits, includes, excludes, undoes, closes or proposes in a view another session owns, or acts in work, the human's view; a view with no owner takes a commit, which claims it, and refuses the rest",
        "fork the view, or include its changes into your own view; an agent with no view writes to work-<actor>, never to the human's work",
    ),
    (
        "E_VIEW_EXISTS",
        "a view of that name exists",
        "pick a free name, or fork the view under another name",
    ),
    (
        "E_VIEW_CLOSED",
        "the view is closed and takes no write, include or exclude",
        "reopen it with rdy_view {op: \"undo\", view, target: <the close op>}, or fork it",
    ),
    (
        "E_HAS_DEPENDENTS",
        "changes in the view depend on a change you exclude (each is an issue)",
        "exclude them too, or pass cascade: true",
    ),
    (
        "E_DEPS_MISSING",
        "a proposal subset leaves out changes its members depend on (each is an issue)",
        "add the missing changes to rdy_propose's changes",
    ),
    (
        "E_CONFLICT_UNRESOLVED",
        "the proposal version carries a conflict it does not resolve (each is an issue)",
        "write the block from the version you want, then propose again",
    ),
    (
        "E_BLOCKING_TENSION",
        "a landing would put a blocking tension into canon: a side of the tension is in the proposal, every side would be in canon after it, and the tension is not dismissed (each is an issue)",
        "resolve each with a change (edit or repin a side), or dismiss it with rdy_resolve {tension, dismiss: {reason}} (a reviewer of another actor, or a human); rdy_tensions {involving: <change>} lists them",
    ),
    (
        "E_SELF_REVIEW",
        "rdy_resolve dismisses a blocking tension, and the session's actor wrote a side of it (another run of the same actor is not independent)",
        "a reviewer of another actor, or a human, dismisses it",
    ),
    (
        "E_PROPOSAL_MOVED",
        "a newer version exists, or a landing since the version changed blocks it writes, or the project requires proposals on the current canon",
        "propose again; a human approves the latest P-x@n",
    ),
    (
        "E_UNDO_BLOCKED",
        "an undo would leave the view inconsistent (each blocking op is an issue)",
        "undo with cascade: true, or undo the dependent ops first",
    ),
    (
        "E_UNDO_LANDING",
        "an undo would bring back or remove work that has landed",
        "a landing is undone by a revoking decision (rdy_revoke)",
    ),
    (
        "E_NOTHING_TO_UNDO",
        "you have no undoable op of your own on the view",
        "name an op with target (rdy_view show lists them)",
    ),
    (
        "E_INTENT_EXPIRED",
        "renewing or withdrawing an expired intent",
        "post a new one",
    ),
    (
        "E_ALT_UNCHOSEN",
        "a member view proposed without naming its set and member",
        "pass alternatives: {set, chosen} with your own member",
    ),
    (
        "E_ALT_MEMBER",
        "a view in two open sets, a member that is not an open view, a chosen member that is not the proposing view's, or a set that is no longer open",
        "propose from the chosen member's view",
    ),
    (
        "E_DAEMON_START",
        "the daemon couldn't be started or reached",
        "read the last lines of .rdy/run/daemon.log it cites; run rdy serve in the foreground to see why",
    ),
    (
        "E_DAEMON_VERSION",
        "another binary version's daemon has clients",
        "close them, or rdy serve --stop",
    ),
    (
        "E_DAEMON_GONE",
        "the daemon closed mid-request",
        "check rdy_overview or rdy status; a write may or may not have committed; it is not retried",
    ),
    (
        "E_BRIEF_REQUIRED",
        "a write touches stable content and cites no briefing, or one that does not cover the block",
        "retry with the token of the briefing the error carries",
    ),
    (
        "E_BRIEF_STALE",
        "the cited briefing is unknown, or a block it recorded moved in the write's view",
        "read what moved, then retry with the token of the briefing the error carries",
    ),
    (
        "W_BRIEF_STALE",
        "a write to drafts cited a stale or unknown briefing (it went ahead)",
        "brief again before the next write if the moved blocks matter",
    ),
    (
        "E_PAGE_STALE",
        "a page cursor (the next of a cut rdy_brief, rdy_read or rdy_search) is unknown to this session, or what it pages (a briefing, a read or a search) moved since its first page, or the search query differs",
        PAGE_STALE_FIX,
    ),
    (
        "W_CONFIG_UNKNOWN",
        "a config file (.rdy/local.yaml, the project config or the user config) holds a key rdy does not know, such as a typo; rdy ignores the key; reported by rdy config --show-origin, rdy status, every CLI command and rdy_session",
        "fix the key's spelling (the fix names the nearest known key), or remove it",
    ),
    (
        "W_SKELETON_MISSING",
        "a doc lacks a required section of its doc_type; reported once per doc per session, and later writes count the repeats in warnings_repeated",
        "add the section, or give the doc a doc_type without it (page)",
    ),
];
