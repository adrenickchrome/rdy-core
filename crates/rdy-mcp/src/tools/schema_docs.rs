//! The schema text constants that rdy_schema and the tool descriptions quote.

use serde_json::Value;

pub(super) const NAME_RULES: &str = "names: 1-64 characters from A-Z a-z 0-9 . _ -; not starting with . or _; not ending .yaml or .yml; not index; not shaped like a block id; not a Windows device name (con, prn, aux, nul, com1-9, lpt1-9); unique among siblings ignoring case. Omit name on create to have one assigned from the title.";

/// Every write op and its JSON shape (`?` marks optional keys). rdy_schema lists them under
/// "ops", and rdy_write's `ops` description repeats them; a QA test keeps the two in step.
pub const WRITE_OPS: &[(&str, &str)] = &[
    (
        "create",
        r#"{"op":"create","type":<type>,"id":"$name"?,"parent":<id or path>?,"after":<sibling>?,"before":<sibling>?,"fields":{..}?,"body":".."?,"links":{..}?}"#,
    ),
    (
        "update",
        r#"{"op":"update","id":<id or path>,"base":<version>?,"fields":{..}?,"unset":[<field>]?,"body":".."?,"links":{..}?,"type":<new type>?,"move_to":{"parent":<id or path>?,"after":<sibling>?,"before":<sibling>?}?} type changes a content block's type in place (same id); move_to moves it in the same op."#,
    ),
    (
        "move",
        r#"{"op":"move","id":<id or path>,"base":<version>?,"parent":<id or path>?,"after":<sibling>?,"before":<sibling>?}"#,
    ),
    (
        "delete",
        r#"{"op":"delete","id":<id or path>,"base":<version>?}"#,
    ),
    ("rebalance", r#"{"op":"rebalance","parent":<id or path>?}"#),
    (
        "repin",
        r#"{"op":"repin","id":<id or path>,"base":<version>?,"targets":[<id or path>]?,"code":<true>?} code: true also records the code state the block's reflects globs match now, and clears its code suspect; the block must declare reflects."#,
    ),
    (
        "reaffirm",
        r#"{"op":"reaffirm","id":<decision id>?,"because":<id or path>?,"base":<version>?,"targets":[<id or path>]?,"reason":<1 to 2000 characters>} Give id or because, not both: id re-affirms one suspect accepted decision, and because re-affirms every decision in the view that is suspect on that block. reason is required. targets limits the op to the suspects on those blocks (default: every current suspect). base goes with id only: it is the version of the decision you read, and the op is refused if the decision has moved. The decision keeps its ruling, body and links."#,
    ),
];

/// rf8 (D-4WC6R, D-XC9B9): how a write is justified, in rdy_write's description and its
/// `reflects` parameter.
pub const JUSTIFICATION_GUIDE: &str = "Pass one of implements, editorial or reflects. reflects is {sources?: [source id, key or path], code?: true}. A source created in this write counts if the final state holds it. code: true records the code state of each written block with reflects globs.";

/// The change plan section of `rdy_schema` (D-4JMPH; reference/mcp-tools p-RW856, p-6YBR7;
/// agents/working-with-agents n-EVMGG). Its first paragraph is the write guidance, word for word.
pub const CHANGE_PLAN_DOC: &str = "Before proposing, read rdy_plan and close or defer each item. Give every deferral a reason. Re-affirm a suspect decision that still holds with a reaffirm op, and supersede one that does not.\n\nItem kinds: update (change the subject to follow the decision), rejustify (give the subject a new justification), reaffirm (re-affirm the subject against the decision), confirm (confirm the subject as it is) and update_implementation (change the code that implements the decision). A tension item (kind tension, id tn- and 12 hex digits) asks the author to answer an advisory tension of the version: confirm accepts it as it is, and dismiss says it does not apply. Each needs a reason. Each item has an id (pi- and 16 hex digits), a kind, a subject, a cause, an edge and a state: open, closed or deferred.\n\nActs: confirm, defer, done and dismiss. update, rejustify and reaffirm take defer. confirm takes confirm or defer. update_implementation takes done or defer. confirm and defer need a reason (1 to 2000 characters); done keeps none. tension takes confirm or dismiss; dismiss answers only a tension item. On stable content an open item refuses the approval (E_PLAN_OPEN); on drafts it warns (W_PLAN_OPEN). An unanswered tension item refuses on drafts too. In rdy_plan an answer may name group: {kind?, follow_up?, doc?, edge?} in place of item. The group answers every open item that matches each field it gives, with one act. repin: true with a confirm group also repins each matched item in one change. On stable content the repin needs a justification: implements, editorial or reflects, as rdy_write takes. rdy_plan {answers: [{item, act, reason?}]} records answers in the session's view, and rdy_propose {plan_answers: [...]} records them in the same batch as the proposal. item is a pi- id (a unique prefix is enough) or a cq- consequence id from the last write or proposal. All the answers are checked first, and either all are recorded or none.\n\nrdy_plan takes one selector or none (the session's view). Every reply is {scope, plan, items, counts, code, notes, next?}. view: scope view:<name>, items with state. proposal (P-<id>@<n>): scope proposal:P-<id>@<n>, items with state. landing (the full hash): scope landing:<hash>, the items the landing sealed, each with an outcome. decision (an id): scope decision:<id>, one row per seal that names it: {landing, version, items, outcomes}. counts cover every item, not only the page. A cut reply carries next: pass page: <next> alone to continue it.\n\nTwo queue cards belong to a plan. A defer card (ref defer:P-x@n) asks the person to accept or refuse the deferrals of a proposal. A reaffirm card (ref reaffirm:<cause>@<version>) asks the person whether the decisions that a changed block made suspect still hold. Each needs the person's own choice and carries no recommended option. Show its options word for word and relay the choice with rdy_answer. all_recommended refuses both kinds with <ref> has no recommended option; answer each on its own. Keep them suspect needs a note of 1 to 2000 characters.\n\nThe reaffirm op records the reaffirmed field and a code repin records reflects_at; rdy sets both, and a write that sets or unsets either is refused. reaffirmed maps each target block id to {against, reason}: against lists the decisions that retire the target (empty when the target only changed) or is \"deleted\" when the target is gone, and reason is the re-affirmation reason. reflects_at is {files, commits}: files is a hash of the committed files the block's reflects globs match, and commits maps each linked repository to its HEAD commit (null when it has none). Neither changes the block's content hash or adds a reference.";

/// Examples of the change plan inputs, as JSON (`rdy_schema` change_plan.examples).
pub const CHANGE_PLAN_EXAMPLES: [&str; 4] = [
    r#"{"op":"reaffirm","id":"D-12345","reason":"The decision still holds after the premise changed."}"#,
    r#"{"op":"reaffirm","because":"p-12345","reason":"These decisions still hold after the premise changed."}"#,
    r#"{"op":"repin","id":"p-12345","code":true}"#,
    r#"{"view":"edit","answers":[{"item":"pi-0123456789abcdef","act":"defer","reason":"The follow-up needs a separate proposal."}]}"#,
];

pub(super) fn change_plan_examples() -> Value {
    Value::Array(
        CHANGE_PLAN_EXAMPLES
            .iter()
            .map(|e| serde_json::from_str(e).expect("CHANGE_PLAN_EXAMPLES are JSON"))
            .collect(),
    )
}

/// doc1: the shape of a block's `links`.
pub const LINK_SHAPE: &str = r#"links: {"<kind>": "<id, key or path>" | ["<target>", …] | [{"target": "<id, key or path>", "part": "<optional: the words of the target this link is about>"}]}"#;

pub const LINK_EXAMPLE: &str =
    r#"{"decided_by": [{"target": "D-W2HXR"}], "describes": [{"target": "wallets/transactions"}]}"#;

/// link1: how an update treats links.
pub const LINK_UPDATE: &str = r#"an update replaces only the link kinds it names; "<kind>": [] removes that kind; kinds it doesn't name are kept"#;

pub(super) fn link_example() -> Value {
    serde_json::from_str(LINK_EXAMPLE).expect("LINK_EXAMPLE is JSON")
}

/// What bodies and text fields accept (md1, dtext1, key1).
pub(super) const TEXT_RULES: &str = "Bodies and text fields: **bold**, *italic*, `code`, [link](https://… or a relative path), and in bodies - bullets and 1. numbered lists (indent two spaces to nest). No raw HTML. {{id}} or {{id|words}} references a block; \\{{ is a literal {{ and a leading @ in words is written \\@. fields.key: optional short human id, unique per project ignoring case, letters/digits/-_. , not shaped like a block id; a doc or folder created without one gets one from its path and fields.key_auto: true (rdy sets key_auto; a write that sends it is refused, and a key you write is yours). If another block holds a key rdy derived when the doc is proposed or landed, rdy gives it a new one (rdy_propose and the landing list it under keys); when a doc or folder is renamed or moved, a key rdy derived for it follows its new path (the docs and folders under it keep their keys); a key rdy derived yields to a new or renamed top-level doc or folder name equal to it and gets a suffix (the write lists both under keys); a key you set is never changed, and a clash on it is E_KEY_CLASH. fields.reflects: optional list of code path globs (e.g. crates/*/src/error.rs) this block follows; rdy_brief {code_path} lists the blocks whose globs match a file.\n\nA finding records a statement and evidence. Each evidence entry is {quote, session_ref?, question?} or {source: \"{{S-…}}\"}. Each string has 1 to 2000 characters. A source names a release, commit or spec with title, kind and locator. Both types live only in the store. With no parent, they use the decision home, or the project root when the project names none.";

/// The decision home's rule, in `rdy_schema` (FW-PR2), with the basis and time rules of a
/// decision's input (D-2WR0G).
pub const DECISIONS_RULE: &str = "Record new decisions in the decision home (rdy_decide without parent records there) and link the content they govern to them with decided_by; don't put decisions inside content docs. With no home configured, a decision with no parent sits at the project root. A decision whose authority_basis is delegated needs attested_by: name who ruled and under whose mandate in attested_by. decided_at, recorded_at, effective_from and review_by take a UTC time or a date: write an RFC 3339 UTC time, such as 2026-10-21T15:10:58Z, or a date, such as 2026-10-05.";

/// rf5 (D-Z8P09): the fields of `rdy_schema`'s example create of a finding and of a source.
/// Each is a valid write as it stands; it names no parent, so it goes to the decision home, or the
/// project root.
pub(super) fn supporting_example(block_type: &str) -> Option<Value> {
    match block_type {
        "finding" => Some(serde_json::json!({
            "statement": "The release changed retention.",
            "evidence": [{"quote": "Retention is now seven days."}]
        })),
        "source" => {
            Some(serde_json::json!({"title": "Release 2", "kind": "release", "locator": "v2"}))
        }
        _ => None,
    }
}

/// What an entity is and how to link facts to it (FW-EN1, FW-EN3), shown by `rdy_schema`.
pub const ENTITY_RULE: &str = "Entities are the things the docs talk about: blocks of a type that defines something (term, component, actor, interface, value, entity). Give each a name and, if it helps, aliases. rdy_search on a key, name or alias returns its fact sheet. Link facts to the entities they describe (describes; affects for a decision): write responses suggest these links, and rdy_accept takes the right ones in one call. A value renders as its value and unit wherever it is referenced; a value whose value is blank renders as its name, so give every value a value.";

/// How to edit with briefings, in `rdy_schema`.
pub const EDIT_LOOP_DOC: &str = "Brief before editing: rdy_brief {ids or path} returns governs, dependents, lineage (the refines, supersedes and revokes edges of the covered blocks, in both directions: {block, kind, direction (in or out), on, summary}; the incoming refines and supersedes are dependents too), will_go_stale, suspects, inflight (other views and open proposals that change the blocks) and base (each covered block's version and the briefing token). Pass brief: <token> on rdy_write, rdy_decide, rdy_supersede (inside decision) and rdy_revoke. A write that touches stable content (a block whose own or inherited lifecycle is stable before or after the op; for a create, its parent's) needs a current briefing that covers it: brief the block, or its parent to create under it. rdy_decide, rdy_supersede and rdy_revoke create a decision under a parent. Brief that parent; a briefing of the old decision does not cover it. A briefing is current while no block it recorded has moved in the write's view. Drafts need none; a stale one there only warns (W_BRIEF_STALE). E_BRIEF_REQUIRED and E_BRIEF_STALE carry a fresh briefing for exactly the blocks the write touches: retry with its base.token. Every write response lists consequences: {id, block, kind (conflict, suspect or dependent; tension (a tension the write opened: resolve it, or ask a reviewer to dismiss it)), because {edge, changed, edges?, values?, values_omitted?}, follow_up (update, confirm, reaffirm or resolve), matched?, matched_omitted?, fix?, already_written?}; max_tokens bounds them. A write that records a decision with refines, or a supersedes with a part, also lists each block the refined or partly superseded decision governs (decided_by, implements, its affects): kind dependent, follow_up confirm (a governed decision gets confirm too, or update while proposed, and no entry when it asserts nothing live), because.edge refines or supersedes. A full supersede reaches only through the refines passed to rdy_supersede, not through the ones it copies (a refines list equal to the old decision's counts as copied). A briefing of the new decision lists them in reaches: {decision, via, target, blocks}; reaches_omitted counts the blocks past 50. A cut briefing carries next and per-doc counts (docs): pass page: <next> alone, in the scope of the first page, to read the rest. Every page of a cut briefing has truncated: true and docs; the last page has no next. A page is stale (E_PAGE_STALE) when a paged entry other than inflight came, went or changed; inflight is advice and may shift between pages. rdy_view include, exclude and undo return consequences too, with conflict entries (follow_up resolve) for conflicts the op exposed; rdy_propose returns the consequences for canon of landing it (advice). When the version has a blocking conflict, rdy_propose returns no consequences and a consequences_error (E_CONFLICT_UNRESOLVED) that names the blocks in conflict: write each from the version you want, then propose again. An entry with already_written: true is a block your view already rewrote. Follow its follow_up and fix. For confirm, check that the block still holds. If its fix gives no other instruction, repin it unless its meaning changed. For update, rewrite it to fit the change. A repin cannot clear a target that is gone, superseded or revoked. Point the block at another target or remove the link, as its fix says. For reaffirm, re-affirm the decision with a reaffirm op if it still holds; otherwise supersede it. A reach entry (FW-LP6) needs no repin: check it and update it only if it no longer holds. Every write response also lists suggestions: {kind: link, block, link (describes or affects), target, mention, field} or {kind: entity, literal, blocks, count}. They never block; pass the ones you want to rdy_accept. A write that records a decision lists related_decisions: decisions it refines, supersedes or revokes, or that refine, supersede or revoke it, and live or proposed decisions on the same entities. Every write response carries on_change: the tensions it opened and the ones it resolved; max_tokens cuts their detail first, then counts advisory ones per kind in collapsed, and never leaves out a blocking one. The schema topics brief and consequences state the token rules and the consequence fields in short.\n\nA nondecision's follow-up comes from changed literals, not its type. The consequences topic gives the matching rules and bounds. A will_go_stale row adds states: up to eight covered literals the dependent states. It asks update when states is nonempty or rdy cut the dependent scan. It asks confirm only when states is empty after a complete scan.\n\nA decision keeps its status-based follow-up. A deleted target asks update. A full supersede lists every actionable link kind to the selected old target in because.edges. A fix write that keeps such a link reports it again in consequences with already_written: true. Change every listed link kind; an update keeps the kinds it does not name.";

/// What `rdy_brief` returns and what its token does, in `rdy_schema` (sg10).
pub const BRIEF_DOC: &str = "rdy_brief {ids?, path?, code_path?, page?, scope?, max_tokens?}: what governs the blocks, what depends on them, what will go stale, open tensions and other in-flight changes, and a token. Pass the token as brief on every write that touches stable content. A token is current while none of the briefed versions moved. Codes: E_BRIEF_REQUIRED (a stable write with no token; the reply carries a fresh briefing), E_BRIEF_STALE (a briefed version moved; brief again). Governs includes proposed decisions and decisions that affect a described entity; through names that entity. Sources follow suspects. Findings follow related_decisions. Trace explains why. A briefing guards a write and stays one level deep.";

/// What a consequence entry means and what to do with it, in `rdy_schema` (sg10).
pub const CONSEQUENCES_DOC: &str = "Every write, rdy_view include/exclude/undo and propose reply carries consequences. Each entry names a block, a kind, a cause and a follow_up. Kinds are suspect, dependent, conflict and tension. Follow-ups are update, confirm, reaffirm and resolve.\n\nbecause.edge is the representative cause. because.changed names its target. because.edges lists the link kinds from the block to that target, including implements. A reference is listed only when it is because.edge.\n\nA nondecision gets update when it states a literal removed by the change, and confirm when it states none. Literals are numbers with units, code literals and entity names or aliases. Numbers use the value grammar. Units match exactly, including case. Code matches exact text between non-alphanumeric boundaries. Entity names match by entity identity.\n\nReferences are blanked, except for their explicit display words. A plain value reference does not match its rendered value.\n\nFor a write, compare the final text with both the view's base and the write's start. A literal removed against either baseline counts once. For propose, compare canon with the candidate. For a view operation, compare its before and after states. For supersede, compare the old and new decision's title and statement. For revoke, compare the old title and statement with no text. Read at most 16384 dependent bytes and 32 distinct changed literals. A cut dependent with no match gets update.\n\nbecause.values lists up to eight inspected literals; values_omitted counts the rest inspected. matched lists up to eight rows as {value, field, line, span, text}; matched_omitted counts the rest found. Lines start at one. span is [start, end] in bytes of the original line. A matched entity phrase across lines gives one row per nonempty line fragment. text uses the search excerpt.\n\nDecisions keep their status-based follow-up. A deleted target asks update. Kept suspects keep their existing follow-up and fix. Refine and part-supersede reach keeps confirm for nondecisions and can carry matched evidence.\n\nFor supersede or revoke, follow the fix even when the follow-up is confirm. A repin cannot clear a link to a retired decision. A fix write reports a retained link in consequences, even before the replacement lands. on_change remains the tension delta.\n\nAnswer each consequence before proposing: change the block, or say why it stays. Consequences are advice; the gate does not check the answers yet.";

/// The signing topic of `rdy_schema` (D-1Z951, D-QVH4N).
pub const SIGNING_DOC: &str = "Signing uses only the caller's SSH agent. Accepted key types are ssh-ed25519 and sk-ssh-ed25519@openssh.com. signing.sign is never, auto or always; the default is auto. Presence is an authenticated flag, not proof that a person read the act.";
