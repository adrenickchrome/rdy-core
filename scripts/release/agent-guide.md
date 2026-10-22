# rdy agent guide (rdy 0.17.0, preview)

## Using this file

Save this file as `AGENTS.md` or `CLAUDE.md` at the project root, or wherever your agent reads its instructions. A later rdy will ship this guide inside the tool.

It is for an agent that works on an rdy project for a person. It adds to the instructions that `rdy mcp` sends, and does not repeat them. rdy computes. You write. The person decides.

Until Release 1, any tool, field, code or file format may change in the next preview. After an update, read `rdy_schema` again.

## Ground rules

- The person talks to you in chat. You do the work through the rdy MCP tools, and you relay the person's decisions.
- Don't ask the person to run routine rdy commands. The CLI is a fallback.
- Use the tools only. Never edit files under `.rdy/`, the working copy, an export or the rendered site.
- When you run the `rdy` CLI, set `RDY_ACTOR` to your own `agent:` actor on every call. Never run it as the person.
- Never approve, tag or answer on your own judgment. Every one of those acts relays the person's own words.
- rdy refuses a tag or an import from an agent session. Give the person the exact command.
- When an error says a store is open or locked, read the code. `E_LOCKED` means another process holds the store: stop that process or use the daemon. `E_HELD_BY_READER` means an earlier read state in this process holds it: release that state and try again.
- "No suspects" means no stale pins. It does not mean that the content is correct.

## Structure first

A flat list of pages is the most common failure. The server tells you to plan the tree first. These details make the plan work:

- Create a folder with `{"op":"create","type":"folder","fields":{"name":"billing","title":"Billing","description":"…"}}`. Don't use a name prefix such as `billing-overview` in place of a folder.
- A doc or folder name uses only `A-Z`, `a-z`, `0-9`, `.`, `_` and `-`. A name that reads as a block id fails with `E_BAD_NAME`. For `d-` and `fd-` ids, rdy ignores case. The refusal carries a fixed name in `corrected`.
- Give every folder a one-sentence `description`, and open every doc with a plain lead paragraph.
- Use the `page` doc type only when no other type fits. One topic per doc: split a long doc into sections, not into sibling docs.
- A question's `prompt` states the product question and its options. Put your own advice in chat.
- Close a question with a decision that has an `answers` link to it. Never write the answer into the question.
- Define each term as a `term` block in the doc that explains it. The site's glossary page collects them.
- To fix a flat project, create the folders, then move each doc with a `move` op. A move keeps block ids and history.

## The loop, for every change

1. **Start.** Call `rdy_session` with your `actor`, a `run` named for the task, and role `writer`. Keep the run name when you resume.
2. **Brief.** Call `rdy_brief` on the blocks you will change, or on a doc with `path`. To brief one section, pass `<doc>#<heading-slug>`. To create a block, brief its parent.
3. **Write.** Pass the briefing's `base.token` as `brief`. An `E_BRIEF_REQUIRED` or `E_BRIEF_STALE` reply carries a fresh briefing: retry with its `base.token`.
4. **Answer the consequences** of each write, as the next section says. Brief again before each follow-up write.
5. **Check.** After your last write, run `rdy_suspects` and `rdy_tensions` with `scope` set to your view. Clear what your changes caused.
6. **Propose.** Call `rdy_propose` with a plain-words `title` and a `summary`. A version without a summary gets no card: `rdy_pending` lists it under `not_ready`.

Propose once per iteration, at the break that `propose.cadence` sets. Propose again only when the view changed since the last version.

## What each consequence asks

Each entry names a `block`, a `kind`, `because` and a `follow_up`. Follow its `fix` when it has one.

- `update`: rewrite the block so that it agrees with the change.
- `confirm`: read the block. If it still holds, repin it.
- `reaffirm`: a premise of an accepted decision changed. Supersede the decision if it no longer holds. Otherwise leave it suspect, and name it in the summary.
- `resolve`: for a tension, see "Tensions". For a conflict, write the block from the version you want.

`already_written: true` names a block that your view already wrote. Check it, then act on its `follow_up`.

A supersede or revoke lists each dependent block, also one that reaches the old decision through `implements`. `because.edges` names every link. When the dependent states a literal that the change removed, the follow-up is `update`. `because.values` lists the removed literals, and `matched` lists the lines that state one. Edit those lines. With no match the follow-up is `confirm`, and a refine or a part supersede is always `confirm`. rdy reads only the first 16 KiB of a dependent, so a cut text gets `update`: read the whole block yourself. rdy matches literals exactly, so search for copies in other forms yourself.

**Repins.** Read every suspect block before you repin it: a repin claims that the block still holds. Repin in a separate write, after the write that changes the target. A repin cannot clear a suspect on a deleted, superseded or revoked target. Point the link at the decision in force instead.

A repin of an accepted or rejected decision fails with `E_FROZEN`. In a repin batch, skip those decisions.

**Suspects in a view.** A changed target shows in `rdy_suspects` for your view. A supersede or revoke in a view raises no `superseded` suspect there. It comes only as a write consequence, so act on it then.

## Decisions and their links

- `decided_by` is a link you write on a content block: this decision governs the block.
- `implements` on `rdy_write` says that the change carries out these decisions. rdy adds an `implements` link to every block that the write creates or edits. It skips decisions, questions, docs, folders and sections, but not notes.
- So group writes by each block's own governing decisions. Never pass a whole list of decisions on one write. Write scaffolding and planned notes in their own write.
- A write takes `implements` or `editorial: {class, reason}`, never both. The class is `typo`, `restructure` or `clarify`.
- An accepted decision is frozen (`E_FROZEN`). Only its tracking fields change, such as a disposition, and each such edit needs `editorial`.
- An update replaces only the link kinds it names, and `"<kind>": []` removes that kind. Read the block first, and send the full list for each kind you name.
- `$name` placeholders work only inside one write, also as `{{$name}}` in text. The create must come before every op that names it.
- Keep a chain of creates that name each other in one write. When you must split creates across writes, replace each placeholder with the real id of the block already written. Group by each block's own decisions even in a recovery write.
- New blocks that name each other in text (a cycle) need two writes. Create them with the references left out, then update the text with the real ids.
- Give each decision a `title` (80 characters or fewer) and a `why` (160 or fewer). Card option labels take 60 or fewer.
- A question's `recommended` is the exact text of one option, never `o1`. A wrong form only warns (`W_CARD_WORDS`). `rdy_answer` still takes option ids such as `o1`.
- Give every recommendation a reason: `recommend_reason` on a question, and `recommend` with `recommend_reason` on a decision. Write it in one line of 200 characters or fewer. rdy warns when it is missing or long.
- Add a new block's `affects` and `describes` links in the write that creates it. A later link-only edit makes every pin on the block go stale.
- Before you supersede, count the blocks in the briefing's `dependents`. If only the reason changes, refine instead, or supersede with `part`.
- After a supersede, move each citing block's links to the new decision yourself.
- For a process ruling that no content carries, pass `fields: {needs_content: false}`. Otherwise the card shows the decision as an exception.
- Before you choose a decision key, look it up with `rdy_search` and check for an exact match. Never pick one from an export.

**Briefing a new decision.** `rdy_decide`, `rdy_supersede` and `rdy_revoke` each create a decision under a parent. The parent is the doc or section in `parent`, or the project's decision home when you leave `parent` out. `rdy_schema` names the decision home.

- Brief that parent. A briefing of the old decision does not cover it.
- `rdy_decide` and `rdy_revoke` take the token as `brief`. `rdy_supersede` takes it inside `decision`, as `decision.brief`.
- The `E_BRIEF_REQUIRED` refusal names the parent, as `<id> (the parent of the new decision)`. It carries a fresh briefing: retry with its token.

## Tensions

- Resolve a tension with a change: edit or repin a side. A `semantic` tension resolves only when a side's text changes.
- For an `unjustified_change`, rewrite the block with `implements` or `editorial`.
- Dismiss a tension only when it needs no action: `rdy_resolve {tension, dismiss: {reason}}`. A blocking one needs the reviewer role or the person, as another actor than every side's author.
- To relay the person's dismissal, add their `evidence` and your `question` to `dismiss`.
- Sibling views forked from one parent see each other's edits as blocking `premise_shift` tensions. They clear when the views are included into one view and repinned.

## The inbox

Show the person their inbox with `rdy_inbox {person: true}`. Ack it only after they have seen it. Your own cursor belongs to your actor and run, and a new run starts at the log head.

## Semantic review

1. Start a session with role `reviewer`, under an actor that wrote neither block. Call `rdy_review_next {kind: "tension"}` at once.
2. Answer with `rdy_review_submit {request, case_hash, verdict}`. A `tension` verdict needs `findings` (`{block, note}`).
3. To advise that a tension should block, add `severity: "blocking"` and a `reason`. On `E_REVIEW_STALE`, take the next request.

Advice to block gates nothing until the person accepts its `resolve` card. Relay their answer like any other card.

## Approvals and relays

Before you ask for an approval, check the propose reply: `blocking` is empty, and no tension has `refuses: true`. Give the person the live link: `rdy_site {scope: "proposal:P-x@n"}`. When the proposal changes a publication profile, add `profile` to that call. Then the person reviews the selection they will publish. Pass on every warning in the reply.

Relay each card in this order:

1. Call `rdy_pending` with your `session_ref`. Find the approve card for your version, and the `confirm` cards beside it.
2. Show each card to the person word for word, with the option labels as rdy gives them. Keep the `recommended` mark and the line `Why recommended: <reason>`. The approve card counts exceptions but does not show them, so show each exception card too.
3. Relay each `confirm` answer first. An approval waits until every exception has one (`E_EXCEPTION_OPEN`).
4. Call `rdy_answer {ref, option, card_hash, evidence, question, session_ref, kind: "card"}`. `evidence` holds their exact words, such as the label they chose.

- Relay only the person's exact words. Never approve, confirm or reject for them, and never fill a gap with your guess.
- `option` is the option id from the card (`approve`, `confirm`, `reject`, `o1`), not its label.
- Every card kind marks its recommended option the same way. In `rdy_pending`, that option has `recommended: true` and a `reason` (200 characters or fewer). No other option has either.
- An approve card's reason is one of two fixed texts. Don't rewrite it.
- Branch on the card kind. Send `confirm` only to a `confirm` card. A tension card offers `dismiss` and `later`. Never send one option name to every card.
- A decision that no content applies is an exception, and it needs its own `confirm`.
- A changed card is refused with the fresh card in `corrected`, as are `E_PROPOSAL_MOVED` and `E_ITEM_MOVED`. Show it and ask again.
- Pass `all_recommended` (a list of `{ref, card_hash}`) only when the person asks you to accept every recommendation.
- A relay checks the question in your view. A view that moved a question cannot relay its answer: relay from a fresh view.
- Relay only from the session the person talks to. A subagent never relays.
- A tag is the person's act too: `rdy_tag {version, evidence, question}`, on their words.
- The CLI fallbacks are `rdy answer <ref> <option> --card <hash>` and `rdy approve P-x@n`. Add `--relayed-by <your actor> --evidence "<their words>" --question "<what you asked>"`.

## Subagents and swarms

- Give every subagent its own `run`, and so its own view and inbox. A write to another session's view fails with `E_VIEW_OWNER`.
- The main agent reads each subagent's changes with `rdy_view {op: "show", view: "<run>"}`. It includes them with `rdy_view {op: "include", changes: [...]}`.
- The main agent alone runs the checks, proposes, shows the cards and relays.
- A subagent never attests a ruling it did not see. The main agent records the person's rulings itself.
- A reviewer subagent uses its own actor, such as `agent:claude-review`. rdy reads independence from the actor, not the run.
- Closing a view withdraws its open proposal for good.

## Sharing and the hub

rdy can connect to a hub service for sharing and publication. This preview has no push, pull or hub clone yet.

- `rdy_remote` lists and edits remotes offline. A remote has a `name`, an HTTPS `url` and a mode, `share` or `publish`. At most one remote is `share`.
- Sign-in: `rdy_auth {op: "start", install, machine_name}` returns a card and sends nothing. Show the card.
- Relay the answer: `start` again with `card_hash`, `answer` (`confirm` or `cancel`), `evidence` and `question`. Give the person the `user_code` and `verification_uri`, and poll `state`.
- You never see, ask for or handle a token. `rdy auth token` is for credential programs, not for agents.
- `rdy_key_bind {kind}` returns a `confirm_url` and state `pending`. Give the person the link: the key binds only when they confirm it on the hub's page.
- Retire or revoke a key only on the person's word. Revoke is for a key that may be compromised.
- On `E_UPSTREAM_UNCERTAIN`, the request may have completed. Check its state on the hub before you try again.

## Signing

Signing goes through the SSH agent, and rdy never reads a private key. Leave the `signing.*` settings to the person.

- Relay a `W_SIGNING_FAILED` warning to the person: the act landed unsigned. Under `always`, rdy refuses the act instead.
- After the person fixes signing, `rdy_sign {target}` signs your relay.
- Sign with the person's key (`kind: "approval"` or `"tag"`) only when they ask.
- A signature shows that a key signed, not that the person read the act. Never claim more.

## Redaction

The server gives the steps. Add these:

- Write the `reason` without the content itself: rdy keeps it as audit text.
- Show every warning in the dry run report. For the class `secret`, tell the person to rotate the secret.
- A removal lands only with the person's answer on its own card. `all_recommended` and an agent's own approval fail with `E_REDACTION_HUMAN`.
- On `E_REDACTION_STALE`, run the dry run again and show the new report.
- A redaction cannot erase dumps, exports, renders, clones or other copies outside the store. Say so.
- On `W_STOP_BEFORE_REDACTION`, tell the person. Never record a removal again on your own.

## Dumps and moving a project

- `rdy_dump {out}` writes the whole project, with every relay's evidence, to one new file. It is unsigned, and as private as the store.
- `rdy_dump {out, after}` writes an increment after `after`, the newest earlier dump or increment of this project. CLI: `rdy dump --after <earlier> <file>`.
- When nothing is newer, `out` is null. rdy refuses an `out` that exists even then, so pick a new path each time.
- `rdy_load` and `rdy_clone` write only into a directory with no store.
- To load a chain, pass the increments in order: `rdy_load {file, dir, then: [...]}`. CLI: `rdy load <file> <dir> --then <inc>`, once for each increment. A wrong order fails with `E_DUMP_CHAIN`.
- To load part of a chain, pass `stop_at` (`landing:<hash>`, `tag:<name>` or `pos:<segment>.<offset>`). CLI: `--stop-at`.
- A stop before a content removal still loads, with `W_STOP_BEFORE_REDACTION`: the removed content is back in that store. Tell the person. Never propose the removal again yourself.
- Every later rdy reads every released dump format. rdy 0.17.0 writes dump format 3 (store format 16).
- A move in place across a store format change is a CLI act. On `E_STORE_FORMAT`, give the person the steps from the error. Don't run them yourself.
- `rdy_export` writes local files for people. It is not a publication. Relay its warnings, and read the project through the tools.

## Publications

- A publication profile is a `publication_profile` block under a doc. It has a `key`, a `title`, an `audience` (`org` or `public`) and a `select` list of doc and folder ids. `rdy new profile` creates one.
- Propose, land and tag a profile like any block.
- `rdy_publication_check {profile, scope?, against?}` checks the selection and the references that leave it. Pass `against` to see what the output newly discloses.
- Outside references block a `public` profile and warn on an `org` one. Show the person every row and warning. The check authorizes nothing.

**Profiles on the live site.** Call `rdy_site {profile}` with an optional `scope`. `profile` is an id, a key or `implicit:org`. CLI: `rdy site --profile <profile>`.

- For a separate site that shows only that profile, call `rdy_site {profile, only: true}`. The scope must be canon: otherwise `E_BAD_INPUT`. CLI: `rdy serve --profile <profile>`.
- On `E_SITE`, follow the fix in the message.
- Pass on `W_SITE_PROFILE`. It says a profile-only site is open, or its profile is no longer in canon.
- A `public` profile that has blocked references still serves on the live site, with a warning. It is not publishable. Run the check before any publish.

**Publish.** Call `rdy_publish {tag, to, site, keep_latest?}`. CLI: `rdy publish <tag> --to <remote> --site <site>`.

- `to` names a remote in `publish` mode. The hub `site` selects the profile: don't pass one.
- Sign in first. Tag the profile and the content first.
- The call creates a candidate on the hub and publishes nothing. Show the person the warnings, the candidate link and the disclosure review. Never claim that the content is public.
- Pass `keep_latest` only when the person says so. Without it the hub's default applies.
- On `E_UPSTREAM_UNCERTAIN`, check the candidate on the hub before you try again.

## The daemon and the MCP server

- `E_DAEMON_START`: tell the person, and follow the fix in the message.
- `E_DAEMON_GONE`: the daemon closed the connection before it answered. A write may or may not have committed, and rdy does not retry it.
- When the daemon goes away, `rdy mcp` answers each open call with `E_DAEMON_GONE` and exits. Your host must start the rdy MCP server again. Then read your view before you retry.
- `E_DAEMON_VERSION`, or stale replies after an rdy update: ask the person, then run `rdy serve --stop`. It ends every session.
- A daemon that rdy started stops after 10 idle minutes, and the live site goes with it. For a long review, ask the person to start `rdy serve` in a terminal first.

## Hygiene

- Stop at the first failed write. Read the view before you run anything again.
- Change each block at most once per write. To edit and move a block, use one `update` op with `move_to`.
- When a decision narrows or reverses a rule, find every statement of the old rule. Search its words, and search with `governed_by`.
- Before you propose, read each section that you changed from top to bottom. Two blocks on one page must not contradict.
- Before you propose a decision, read its related decisions and the neighbours in its section. Search for closed lists that it changes.
- When a block states an exception, link it to the block that states the rule.
- Search the terms (`rdy_search`, `types` set to `term`) before you introduce a term or a field name.
- Before you remove or move a doc, brief it and read each block in `dependents`.
- Don't write decisions that sit in a sibling view's open proposal.
- A list of ids in a hand-off names the project that holds each id. Read an id only in its own project.
- Quote decisions from `rdy_read`, not from the site or an export.
- When you retitle a doc, decide its `name` too, because the name sets its page path.
