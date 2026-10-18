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
        "the view is closed and takes no write, include, exclude, undo or propose, except the undo of its close; or an approval names a proposal version that a close of its view withdrew",
        "reopen it with rdy_view {op: \"undo\", view, target: <the close op>}, or fork it; a withdrawn proposal stays withdrawn, so propose again after the reopen",
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
        "resolve each as the fix names for its kind (a change to a side; a repin helps a stale citation, never a semantic tension), or dismiss it with rdy_resolve {tension, dismiss: {reason}} (a reviewer of another actor, or a human); rdy_tensions {involving: <change>} lists them, a form only this landing gives with its proposal",
    ),
    (
        "E_SELF_REVIEW",
        "rdy_resolve dismisses a blocking tension or one a reviewer advised to block, or rdy_review_submit judges a pair, and the session's actor wrote a side of it (another run of the same actor is not independent)",
        "a reviewer of another actor, or a human, dismisses the tension; a reviewer of another actor judges the pair",
    ),
    (
        "E_REVIEW_INVALID",
        "rdy_review_submit: the verdict fails its schema (a verdict other than tension or no_tension, a finding on another block, an empty or over-long note, a tension with no finding, more than 20 findings, a severity other than advisory or blocking, advice on a no_tension, advice to block without a reason)",
        "send the corrected payload",
    ),
    (
        "E_REVIEW_STALE",
        "rdy_review_submit: the case hash is no longer current (a side changed, landed or left every view), or a verdict already settled the request (the message names it)",
        "call rdy_review_next again",
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
        "a landing is undone by a revoking decision (rdy_revoke); when only a dep of the change landed, include the change directly with rdy_view include, which adds the landed dep",
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
        "a view in two open sets, a member that is not an open view, a chosen member that is not the proposing view's, a set that is no longer open, a change bound to another member (it alone held it when it joined the set), or a change bound in two open sets",
        "propose from the bound member's view with its pick; for a change bound in two open sets, exclude it and propose without it",
    ),
    (
        "E_ALT_CHOSEN",
        "a proposal carries a change of a member that lost a chosen set, and records no decision that supersedes or refines the choice",
        "land it with a decision that supersedes or refines the choice decision (rdy_supersede, or rdy_decide with refines)",
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
        "a config file (.rdy/local.yaml, the project config or the user config) holds a key rdy does not know (such as a typo), or a known key in a file where rdy does not read it; the warning names the file it belongs in; rdy ignores the key; reported by rdy config --show-origin, rdy status, every CLI command and rdy_session",
        "fix the key's spelling (the fix names the nearest known key), move it to the file the warning names, or remove it",
    ),
    (
        "W_UNAPPLIED_DECISION",
        "a proposal creates a decision that no content applies in its view, and the decision does not set needs_content: false; rdy_propose and rdy propose report it, and it never refuses the proposal",
        "write the content that carries it with implements: <decision>, or link that content to it with decided_by; for a process ruling, set needs_content: false",
    ),
    (
        "E_SITE",
        "rdy_site or rdy site cannot give the live site's link: this session runs without a daemon (--no-daemon), or the daemon could not bind a loopback port for the site",
        "drop --no-daemon; for a static site, run rdy render --out <dir>; or set serve.site_port to a free port from 1024 to 65535 in .rdy/local.yaml, then run rdy serve --stop",
    ),
    (
        "W_SITE_LAN",
        "the live site is open to the network (serve.site_lan is on): anyone who can reach the address can read the docs; or the LAN address could not be bound, and the site stays on this machine; rdy_site and rdy site carry it with the link",
        "set serve.site_lan to false to keep the site on this machine; when the site already stays on it, follow the warning's own fix",
    ),
    (
        "E_DUMP",
        "the file is not a dump this rdy reads, the dump is damaged, the loaded project fails fsck, or a clone's dump is older than store format 12",
        "pass a file rdy dump wrote; a damaged dump is never repaired: dump the project again; for an older dump, load it, then clone the loaded project",
    ),
    (
        "W_SKELETON_MISSING",
        "a doc lacks a required section of its doc_type; reported once per doc per session, and later writes count the repeats in warnings_repeated",
        "add the section, or give the doc a doc_type without it (page)",
    ),
    (
        "E_EXCEPTION_OPEN",
        "an approval names a version whose exceptions have no answer",
        "answer each exception card first",
    ),
    (
        "E_ITEM_MOVED",
        "the item changed since its card was shown",
        "show the fresh card and ask again",
    ),
    (
        "W_CARD_WORDS",
        "a decision or question lacks its card words, or they are too long",
        "add a short title, a one-line why and, for a question, two to four options",
    ),
    // plan Redaction: the shared texts, the message as when; the warnings have empty fixes
    (
        E_REDACTION_TARGET,
        "Cannot redact {target}: {cause}.",
        REDACTION_TARGET_FIX,
    ),
    (
        E_REDACTION_LIMIT,
        "The redaction exceeds {limit}.",
        REDACTION_LIMIT_FIX,
    ),
    (
        E_REDACTION_REASON,
        REDACTION_REASON_MESSAGE,
        REDACTION_REASON_FIX,
    ),
    (
        E_REDACTION_STALE,
        "The redaction report changed after the request was recorded.",
        REDACTION_STALE_FIX,
    ),
    (
        E_REDACTION_HUMAN,
        "Content removal needs a person's approval.",
        REDACTION_HUMAN_FIX,
    ),
    (
        E_REDACTION_PENDING,
        "Content removal was authorized but has not finished.",
        REDACTION_PENDING_FIX,
    ),
    (
        E_REDACTION_CORRUPT,
        "The redaction record does not authorize the missing payload at {segment}:{offset}.",
        REDACTION_CORRUPT_FIX,
    ),
    (
        E_REDACTED_STATE,
        "This read state predates content removal.",
        REDACTED_STATE_FIX,
    ),
    (
        E_REDACTED,
        "Content removed by redaction {hash}.",
        REDACTED_FIX,
    ),
    (
        E_REDACTION_BUDGET,
        REDACTION_BUDGET_MESSAGE,
        REDACTION_BUDGET_FIX,
    ),
    (W_REDACTION_COPIES, W_REDACTION_COPIES_MESSAGE, ""),
    (W_REDACTION_INVENTORY, W_REDACTION_INVENTORY_MESSAGE, ""),
    (W_REDACTION_QUOTES, W_REDACTION_QUOTES_MESSAGE, ""),
    (W_REDACTION_REUSE, W_REDACTION_REUSE_MESSAGE, ""),
    (W_REDACTION_QUARANTINE, W_REDACTION_QUARANTINE_MESSAGE, ""),
    (W_REDACTION_SECRET, W_REDACTION_SECRET_MESSAGE, ""),
    (
        E_SIGNING_KEY_MISSING,
        "{setting} is not set",
        "set {setting} in the user config or .rdy/local.yaml",
    ),
    (
        E_SIGNING_KEY_TYPE,
        "{key} has unsupported signing key type {type}",
        SIGNING_KEY_TYPE_FIX,
    ),
    (
        E_NO_AGENT,
        "the SSH agent could not sign: {cause}",
        NO_AGENT_FIX,
    ),
    (
        E_SIG_INVALID,
        "signature {hash} is invalid",
        SIG_INVALID_FIX,
    ),
    (
        E_SIG_OBJECT_MISMATCH,
        "signature {hash} does not match its subject",
        SIG_OBJECT_MISMATCH_FIX,
    ),
    (
        W_SIG_UNKNOWN_KEY,
        "key {key} is not trusted for {principal} in {namespace}",
        SIG_UNKNOWN_KEY_FIX,
    ),
    (
        W_SIGNING_FAILED,
        "the act was saved without signatures: {code}: {message}",
        SIGNING_FAILED_FIX,
    ),
    (
        E_PUBLICATION_REFERENCE,
        "a public profile refers to blocks outside its selection",
        "Include the target docs or remove these references. Run the publication check again.",
    ),
    (
        W_PUBLICATION_REFERENCE,
        "an org profile refers to blocks outside its selection",
        "Review these references before you share the output.",
    ),
];
