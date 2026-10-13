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
        r#"{"op":"repin","id":<id or path>,"base":<version>?,"targets":[<id or path>]?}"#,
    ),
];

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
pub(super) const TEXT_RULES: &str = "Bodies and text fields: **bold**, *italic*, `code`, [link](https://… or a relative path), and in bodies - bullets and 1. numbered lists (indent two spaces to nest). No raw HTML. {{id}} or {{id|words}} references a block; \\{{ is a literal {{ and a leading @ in words is written \\@. fields.key: optional short human id, unique per project ignoring case, letters/digits/-_. , not shaped like a block id; a doc or folder created without one gets one from its path and fields.key_auto: true (rdy sets key_auto; a write that sends it is refused, and a key you write is yours). If another block holds a key rdy derived when the doc is proposed or landed, rdy gives it a new one (rdy_propose and the landing list it under keys); when a doc or folder is renamed or moved, a key rdy derived for it follows its new path (the docs and folders under it keep their keys); a key rdy derived yields to a new or renamed top-level doc or folder name equal to it and gets a suffix (the write lists both under keys); a key you set is never changed, and a clash on it is E_KEY_CLASH. fields.reflects: optional list of code path globs (e.g. crates/*/src/error.rs) this block follows; rdy_brief {code_path} lists the blocks whose globs match a file.";

/// The decision home's rule, in `rdy_schema` (FW-PR2).
pub const DECISIONS_RULE: &str = "Record new decisions in the decision home (rdy_decide without parent records there) and link the content they govern to them with decided_by; don't put decisions inside content docs. With no home configured, pass parent, or set decisions.home in .rdy.yaml.";

/// What an entity is and how to link facts to it (FW-EN1, FW-EN3), shown by `rdy_schema`.
pub const ENTITY_RULE: &str = "Entities are the things the docs talk about: blocks of a type that defines something (term, component, actor, interface, value, entity). Give each a name and, if it helps, aliases. rdy_search on a key, name or alias returns its fact sheet. Link facts to the entities they describe (describes; affects for a decision): write responses suggest these links, and rdy_accept takes the right ones in one call. A value renders as its value and unit wherever it is referenced; a value whose value is blank renders as its name, so give every value a value.";

/// How to edit with briefings, in `rdy_schema`.
pub const EDIT_LOOP_DOC: &str = "Brief before editing: rdy_brief {ids or path} returns governs, dependents, lineage (the refines, supersedes and revokes edges of the covered blocks, in both directions: {block, kind, direction (in or out), on, summary}; the incoming refines and supersedes are dependents too), will_go_stale, suspects, inflight (other views and open proposals that change the blocks) and base (each covered block's version and the briefing token). Pass brief: <token> on rdy_write, rdy_decide, rdy_supersede (inside decision) and rdy_revoke. A write that touches stable content (a block whose own or inherited lifecycle is stable before or after the op; for a create, its parent's) needs a current briefing that covers it: brief the block, or its parent to create under it. A briefing is current while no block it recorded has moved in the write's view. Drafts need none; a stale one there only warns (W_BRIEF_STALE). E_BRIEF_REQUIRED and E_BRIEF_STALE carry a fresh briefing for exactly the blocks the write touches: retry with its base.token. Every write response lists consequences: {id, block, kind (conflict, suspect or dependent; tension (a tension the write opened: resolve it, or ask a reviewer to dismiss it)), because {edge, changed}, follow_up (update, confirm, reaffirm or resolve)}; max_tokens bounds them. A write that records a decision with refines, or a supersedes with a part, also lists each block the refined or partly superseded decision governs (decided_by, implements, its affects): kind dependent, follow_up confirm (a governed decision gets reaffirm, or update while proposed, and no entry when it asserts nothing live), because.edge refines or supersedes. A full supersede reaches only through the refines passed to rdy_supersede, not through the ones it copies (a refines list equal to the old decision's counts as copied). A briefing of the new decision lists them in reaches: {decision, via, target, blocks}; reaches_omitted counts the blocks past 50. A cut briefing carries next and per-doc counts (docs): pass page: <next> alone, in the scope of the first page, to read the rest. Every page of a cut briefing has truncated: true and docs; the last page has no next. A page is stale (E_PAGE_STALE) when a paged entry other than inflight came, went or changed; inflight is advice and may shift between pages. rdy_view include, exclude and undo return consequences too, with conflict entries (follow_up resolve) for conflicts the op exposed; rdy_propose returns the consequences for canon of landing it (advice). An entry with already_written: true is a block your view already rewrote: confirm it (repin) unless its meaning changed; a reach entry (FW-LP6) needs no repin: check it and update it only if it no longer holds. Every write response also lists suggestions: {kind: link, block, link (describes or affects), target, mention, field} or {kind: entity, literal, blocks, count}. They never block; pass the ones you want to rdy_accept. A write that records a decision lists related_decisions: decisions it refines, supersedes or revokes, or that refine, supersede or revoke it, and live or proposed decisions on the same entities. Every write response carries on_change: the tensions it opened and the ones it resolved; max_tokens cuts their detail first, then counts advisory ones per kind in collapsed, and never leaves out a blocking one. The schema topics brief and consequences state the token rules and the consequence fields in short.";

/// What `rdy_brief` returns and what its token does, in `rdy_schema` (sg10).
pub const BRIEF_DOC: &str = "rdy_brief {ids?, path?, code_path?, page?, scope?, max_tokens?}: what governs the blocks, what depends on them, what will go stale, open tensions and other in-flight changes, and a token. Pass the token as brief on every write that touches stable content. A token is current while none of the briefed versions moved. Codes: E_BRIEF_REQUIRED (a stable write with no token; the reply carries a fresh briefing), E_BRIEF_STALE (a briefed version moved; brief again).";

/// What a consequence entry means and what to do with it, in `rdy_schema` (sg10).
pub const CONSEQUENCES_DOC: &str = "Every write, view op and proposal reply carries consequences. Each entry names a block, its kind (suspect, dependent, conflict or tension), because (the edge and the changed block) and a follow_up (update, confirm, reaffirm or resolve). Answer each one before you propose: change the block, or say why it stays. Consequences are advice; the gate does not check the answers yet.";
