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
        "an issue names a conflict the proposal version does not resolve",
        "write the block from the version you want, then propose again",
    ),
    (
        "E_BLOCKING_TENSION",
        "an issue names an undismissed blocking tension that the landing would put into canon",
        "resolve each as the fix names for its kind (a change to a side; a repin helps a stale citation, never a semantic tension), or dismiss it with rdy_resolve {tension, dismiss: {reason}} (a reviewer of another actor, or a human); rdy_tensions {involving: <change>} lists them, a form only this landing gives with its proposal",
    ),
    (
        "E_SELF_REVIEW",
        "rdy_resolve dismisses a blocking tension or one a reviewer advised to block, or rdy_review_submit judges a pair, and the session's actor wrote a side of it (another run of the same actor is not independent)",
        "a reviewer of another actor, or a human, dismisses the tension; a reviewer of another actor judges the pair",
    ),
    (
        "E_REVIEW_INVALID",
        "rdy_review_submit: the verdict fails its schema (a verdict other than tension or no_tension, a citation on another block, an empty or over-long note, a tension with no citation, more than 20 citations, a severity other than advisory or blocking, advice on a no_tension, advice to block without a reason)",
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
        "the live site has no daemon, cannot bind its port, has reached its profile-site limit, or is stopping",
        "follow the error's fix",
    ),
    (
        "W_SITE_LAN",
        "the live site is open to the network (serve.site_lan is on): anyone who can reach the address can read the docs; or the LAN address could not be bound, and the site stays on this machine; rdy_site and rdy site carry it with the link",
        "set serve.site_lan to false to keep the site on this machine; when the site already stays on it, follow the warning's own fix",
    ),
    (
        "W_SITE_PROFILE",
        "a profile-only site is open, or its profile is no longer in canon",
        "relay the warning; use rdy site for the full source link, or restore the profile",
    ),
    (
        "W_DIAGRAM_INVALID",
        "A Mermaid diagram does not parse, or is over one of rdy's size bounds, so the render shows it as its code block. The render never fails for it. The bounds limit the source's bytes, its statements, its nesting depth, the length of one arrow, and its arrows and groups. A longer or labelled arrow counts as more than one. A state note and each boundary between concurrent regions also count as one arrow. A smaller diagram, split into parts, or shorter arrows fix a bound.",
        "correct the diagram source",
    ),
    (
        "W_DIAGRAM_UNSUPPORTED",
        "A Mermaid diagram whose header keyword names a type that rdy does not draw shows its source as a code block.",
        "use a flowchart, class, sequence or state diagram, or keep it as source",
    ),
    (
        "W_DIAGRAM_FAILED",
        "The drawn SVG of a diagram fails rdy's checks, so the render shows its source as a code block.",
        "simplify the diagram, or report it as a bug in rdy, with the diagram's source",
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
        "E_PROPOSAL_NOT_READY",
        "the proposal version fails readiness; issues name each failed check",
        "fix each issue and propose again: rdy_propose / rdy propose",
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
    // Redaction and load texts; E_REDACTION_LIMIT describes both messages. Warnings have empty fixes.
    (
        E_REDACTION_TARGET,
        "Cannot redact {target}: {cause}.",
        REDACTION_TARGET_FIX,
    ),
    (
        E_REDACTION_LIMIT,
        "A redaction or dump-chain load exceeds {limit}.",
        REDACTION_LIMIT_ROW_FIX,
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
    (E_ACTOR_UNBOUND, ACTOR_UNBOUND_MESSAGE, ACTOR_UNBOUND_FIX),
    (
        E_REMOTE_REQUIRED,
        REMOTE_REQUIRED_MESSAGE,
        REMOTE_REQUIRED_FIX,
    ),
    (E_REMOTE_EXISTS, REMOTE_EXISTS_MESSAGE, REMOTE_EXISTS_FIX),
    (
        E_UPSTREAM_ORIGIN,
        "the request destination is not trusted for {install}",
        "use the install origin; for a publish upload, add its destination to the pinned origins of this install (hub.installs.<install>.pinned_origins)",
    ),
    (E_UPSTREAM_TOKEN, UPSTREAM_TOKEN_MESSAGE, UPSTREAM_TOKEN_FIX),
    (
        E_UPSTREAM_AUTH,
        "sign-in is required for {install}",
        UPSTREAM_AUTH_FIX,
    ),
    (
        E_UPSTREAM_PROTOCOL,
        UPSTREAM_PROTOCOL_MESSAGE,
        UPSTREAM_PROTOCOL_FIX,
    ),
    (
        E_UPSTREAM_TRANSPORT,
        UPSTREAM_TRANSPORT_MESSAGE,
        UPSTREAM_TRANSPORT_FIX,
    ),
    (
        E_UPSTREAM_REFUSED,
        UPSTREAM_REFUSED_MESSAGE,
        UPSTREAM_REFUSED_FIX,
    ),
    (
        E_UPSTREAM_UNCERTAIN,
        UPSTREAM_UNCERTAIN_MESSAGE,
        "check its state on the hub before trying again; for a publish upload, check push {push} on the hub",
    ),
    (
        E_UPSTREAM_CONFIRM_HOST,
        UPSTREAM_CONFIRM_HOST_MESSAGE,
        UPSTREAM_CONFIRM_HOST_FIX,
    ),
    (E_UPSTREAM_CA, UPSTREAM_CA_MESSAGE, UPSTREAM_CA_FIX),
    (
        E_UPSTREAM_KEY_PROOF,
        UPSTREAM_KEY_PROOF_MESSAGE,
        UPSTREAM_KEY_PROOF_FIX,
    ),
    (
        E_UPSTREAM_ACTOR_CLAIM,
        UPSTREAM_ACTOR_CLAIM_MESSAGE,
        UPSTREAM_ACTOR_CLAIM_FIX,
    ),
    (
        W_UPSTREAM_HANDLE_CHANGED,
        "the hub handle changed from {old} to {new}",
        "use human:{new} for this install",
    ),
    (
        W_UPSTREAM_SIGNOUT_LOCAL,
        UPSTREAM_SIGNOUT_LOCAL_MESSAGE,
        UPSTREAM_SIGNOUT_LOCAL_FIX,
    ),
    (
        E_BAD_RECORD,
        "a sync batch has an invalid object, frame or header, an unlisted dependency, an unneeded prerequisite, a root tag that is not in the batch or its prerequisites, a dependency cycle or a repeated object",
        "request this stream again; run rdy fsck at its source",
    ),
    (
        E_UNRESOLVED_REF,
        "an upload needs an object that was not supplied",
        "send the missing object with its dependencies",
    ),
    (
        E_STREAM_TRUNCATED,
        "a sync batch's trailer is missing or does not match",
        "request the complete stream again",
    ),
    (
        E_STREAM_PURPOSE,
        "a sync batch has a purpose this call did not request",
        "request a tag_history stream",
    ),
    (
        E_REPO_MISMATCH,
        "a sync batch names a different repo from its holder",
        "use the holder for the stream's repo",
    ),
    (
        E_DUMP_CHAIN,
        "The dump files do not form a chain, or the predecessor does not match this store.",
        "Pass a full dump and its increments in order, or start a new chain.",
    ),
    (
        E_LOAD_STOP,
        "The requested stop does not name one batch end in the dump chain.",
        "Name a landing, a tag, or a batch end that the chain holds.",
    ),
    (
        rdy_engine::carried_warning::W_STOP_BEFORE_REDACTION,
        "This store stops before redaction apply {apply} of its source, approved by {actor} at landing {landing}. It holds {held} removed payloads again, and {removed} stay removed.",
        "Propose the redaction of the listed targets again, and have a person approve it.",
    ),
    (
        E_PLAN_ITEM,
        "<id> is not an item of the change plan for <scope>",
        "read the plan with rdy_plan / rdy plan and pass an item id it lists",
    ),
    (
        E_DEFERRAL_OPEN,
        "<n> deferrals of P-x@n have no answer",
        "answer the deferral card first: defer:P-x@n",
    ),
    (
        rdy_engine::plan::W_PLAN_OPEN,
        "<n> change-plan items are open",
        "close or defer each item: rdy_plan / rdy plan",
    ),
    (
        rdy_engine::plan::W_PLAN_ANSWER_LAPSED,
        "the answer to <id> lapsed: <block> changed",
        "answer the item again",
    ),
    (
        rdy_engine::reflects::W_REFLECTS_NO_CODE,
        "no linked checkout; rdy records no code state for <id>",
        "set code_paths, then repin it with code: true",
    ),
    (
        E_FROZEN,
        "a write changes an accepted or rejected decision beyond its tracking fields, or beyond its pins and reaffirmed field through a reaffirm op, or a conflict on one cannot be resolved by an edit",
        "take the landed side of a conflict (rdy view resolve <view> <id> --landed; rdy_view {op: \"resolve\", block, landed: true}), or write a new decision that supersedes or revokes it. For a suspect accepted decision that still holds, re-affirm it with a reaffirm op instead.",
    ),
    (
        E_HELD_BY_READER,
        "the project store is still open in this process through a read state of an earlier open",
        HELD_BY_READER_FIX,
    ),
    (
        E_PUBLICATION_REFERENCE,
        "a public profile refers to blocks outside its selection",
        "Include the target docs or remove these references. Run the publication check again.",
    ),
    (
        W_PUBLICATION_REFERENCE,
        "an org profile has references outside its selection, or a live public profile has blocked references",
        "follow the warning's own fix",
    ),
];
