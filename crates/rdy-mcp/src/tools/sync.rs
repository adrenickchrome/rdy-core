//! `rdy_push` and `rdy_pull` (D-47HVF, D-3RSCW, D-KQ4CA, D-VRH3Z): the chat surface of the sync
//! client, with `rdy push` and `rdy pull` as its command-line fallback. Both surfaces send the
//! same strict inputs to the daemon's sync ops through [`PushInputs`] and [`PullInputs`], which
//! check them once ([`PushInputs::check`], [`PullInputs::check`]). The daemon runs the one sync
//! coordinator, off its writer.
//!
//! - **Closed inputs.** `rdy_push` takes `{remote?, views?, force?, restore?}` and `rdy_pull`
//!   takes `{remote?, views?, take?, strict?}`, each with the relay inputs of `rdy_auth start`
//!   (`card_hash`, `answer`, `evidence`, `question`, `session_ref`). Unknown keys are refused
//!   with the present texts. A false bool is the same as none.
//! - **A person's card.** A forced push that moves hub canon, a restore into a hub repo that
//!   holds canon and a take of hub canon each remove landings from a canon. Each returns a card
//!   first (`status: confirmation_required`, with `card`, `card_hash` and the preview), and
//!   writes nothing. The agent shows it, then calls again with the person's answer, their
//!   quoted words and the card's `card_hash`. A card that changed since is `E_ITEM_MOVED`.
//! - **Dump fetch.** `rdy_pull {dump, out, remote?, url?}` fetches a hub dump to a new file
//!   (D-7CDEQ, `rdy_engine::sync_dump`). Its closed inputs come first ([`check_dump_inputs`]);
//!   it needs no project role, because it writes no store, ref or sync state.
//! - **Roles.** Push, pull and a view take need the writer role ([`require_writer`]). An agent
//!   never answers a card as the person: the relay check refuses an answer without a relayed
//!   quote.
//! - **Status.** [`sync_status`] is the sync state `rdy status` and the overview show.
use super::*;
use rdy_engine::engine::SyncCall;
use rdy_engine::hub_config::HubConfig;
use rdy_engine::hub_credentials::require_user_file;
use rdy_engine::hub_sync_wire::check_view_names;
use rdy_engine::*;
use rdy_engine::{ReadState, Session};
/// A pending or upload ref of a view write (`rdy_store::sync_state::KIND_VIEW`).
const KIND_VIEW: u8 = 1;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The most views one push or pull names (D-47HVF).
pub const MAX_SYNC_VIEWS: usize = 256;

/// The person's answer to a sync card, as `rdy_auth` takes it.
pub type SyncAnswer = AuthAnswerParam;

/// The relay inputs of a card answer, shared by push and pull.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelayFields {
    pub card_hash: Option<String>,
    pub answer: Option<SyncAnswer>,
    /// The command line's relay flag; over MCP the session is the relay.
    pub relayed_by: Option<String>,
    pub evidence: Option<String>,
    pub question: Option<String>,
    pub session_ref: Option<String>,
}

impl RelayFields {
    /// The first relay field given, in the order card_hash, answer, relayed_by, evidence,
    /// question, session_ref.
    fn first_given(&self) -> Option<&'static str> {
        [
            ("card_hash", self.card_hash.is_some()),
            ("answer", self.answer.is_some()),
            ("relayed_by", self.relayed_by.is_some()),
            ("evidence", self.evidence.is_some()),
            ("question", self.question.is_some()),
            ("session_ref", self.session_ref.is_some()),
        ]
        .into_iter()
        .find_map(|(f, given)| given.then_some(f))
    }

    /// The pairing rules of the relay inputs, as `rdy_auth start` states them: an answer needs
    /// its card_hash and the card_hash its answer; a question or session_ref needs evidence.
    fn check_pairs(&self, act: &str) -> Result<(), RdyError> {
        let answered = self.card_hash.is_some()
            || self.answer.is_some()
            || self.evidence.is_some()
            || self.relayed_by.is_some()
            || self.question.is_some()
            || self.session_ref.is_some();
        let needs = |field: &str| {
            RdyError::new(
                E_BAD_INPUT,
                format!("{act} needs {field}"),
                format!("pass {field}"),
            )
        };
        if answered && self.card_hash.is_none() {
            return Err(needs("card_hash"));
        }
        if answered && self.answer.is_none() {
            return Err(needs("answer"));
        }
        if (self.question.is_some() || self.session_ref.is_some()) && self.evidence.is_none() {
            return Err(needs("evidence"));
        }
        Ok(())
    }
}

/// `E_BAD_INPUT` for a field that an act does not take (D-09CJR: the field-rule form of
/// `rdy_auth`).
fn not_used(field: &str, act: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("{field} is not used by {act}"),
        format!("remove {field}"),
    )
}

/// The strict inputs of a push, as the sync op takes them.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PushInputs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub views: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
    /// A dump path to restore into the hub repo (relative to the project root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<SyncAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relayed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<String>,
}

impl PushInputs {
    /// The relay inputs.
    pub fn relay(&self) -> RelayFields {
        RelayFields {
            card_hash: self.card_hash.clone(),
            answer: self.answer,
            relayed_by: self.relayed_by.clone(),
            evidence: self.evidence.clone(),
            question: self.question.clone(),
            session_ref: self.session_ref.clone(),
        }
    }
    /// Checks the inputs that need no hub: a restore takes no views and no force; the relay
    /// inputs go only with force or restore; their pairing; and the view names.
    pub fn check(&self) -> Result<(), RdyError> {
        if self.restore.is_some() {
            if !self.views.is_empty() {
                return Err(not_used("views", "push with restore"));
            }
            if self.force {
                return Err(not_used("force", "push with restore"));
            }
        }
        if !self.force
            && self.restore.is_none()
            && let Some(field) = self.relay().first_given()
        {
            return Err(not_used(field, "push without force or restore"));
        }
        self.relay().check_pairs("push")?;
        check_view_names(&self.views)
    }

    /// The op's arguments.
    pub fn to_args(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// The strict inputs of a pull, as the sync op takes them.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PullInputs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub views: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub take: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
    /// The id of a hub dump to fetch (D-7CDEQ); with it the call takes only `out` and one of
    /// `remote` or `url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dump: Option<String>,
    /// The new file a dump fetch writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
    /// The HTTPS URL of the hub repo a dump fetch reads, in place of a remote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<SyncAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relayed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<String>,
}

impl PullInputs {
    /// The relay inputs.
    pub fn relay(&self) -> RelayFields {
        RelayFields {
            card_hash: self.card_hash.clone(),
            answer: self.answer,
            relayed_by: self.relayed_by.clone(),
            evidence: self.evidence.clone(),
            question: self.question.clone(),
            session_ref: self.session_ref.clone(),
        }
    }
    /// The names of the inputs this call gave: each set option, each true bool and each
    /// non-empty list.
    pub fn given(&self) -> Vec<&'static str> {
        [
            ("remote", self.remote.is_some()),
            ("views", !self.views.is_empty()),
            ("take", self.take),
            ("strict", self.strict),
            ("dump", self.dump.is_some()),
            ("out", self.out.is_some()),
            ("url", self.url.is_some()),
            ("card_hash", self.card_hash.is_some()),
            ("answer", self.answer.is_some()),
            ("relayed_by", self.relayed_by.is_some()),
            ("evidence", self.evidence.is_some()),
            ("question", self.question.is_some()),
            ("session_ref", self.session_ref.is_some()),
        ]
        .into_iter()
        .filter_map(|(k, given)| given.then_some(k))
        .collect()
    }

    /// Checks the inputs that need no hub: the closed inputs of a dump fetch and its id
    /// (D-7CDEQ); else a take without views takes no strict; the relay inputs go only with take;
    /// their pairing; and the view names.
    pub fn check(&self) -> Result<(), RdyError> {
        check_dump_inputs(&self.given())?;
        if let Some(id) = &self.dump {
            return rdy_engine::sync_dump::check_dump_id(id);
        }
        if self.take && self.strict && self.views.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "strict is not used by pull with take and no views",
                "remove strict, or name the views to take",
            ));
        }
        if !self.take
            && let Some(field) = self.relay().first_given()
        {
            return Err(not_used(field, "pull without take"));
        }
        self.relay().check_pairs("pull")?;
        check_view_names(&self.views)
    }

    /// The op's arguments.
    pub fn to_args(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// The closed inputs of a dump fetch (D-7CDEQ), from `given`, the names of every pull input a
/// call gave (a false bool or an empty list counts as given). With `dump`, the call takes only
/// `out` and one of `remote` or `url`; without it, `out` and `url` are refused.
pub fn check_dump_inputs(given: &[&str]) -> Result<(), RdyError> {
    let has = |k: &str| given.contains(&k);
    if !has("dump") {
        return match ["out", "url"].into_iter().find(|k| has(k)) {
            Some(k) => Err(RdyError::new(
                E_BAD_INPUT,
                format!("{k} goes only with dump"),
                format!("add --dump <id>, or remove {k}"),
            )),
            None => Ok(()),
        };
    }
    if given
        .iter()
        .any(|k| !matches!(*k, "dump" | "out" | "remote" | "url"))
    {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "a dump fetch takes only out and remote or url",
            "run the dump fetch on its own",
        ));
    }
    if !has("out") {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "a dump fetch needs out",
            "add --out <file>",
        ));
    }
    if has("remote") && has("url") {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "name remote or url, not both",
            "keep one of them",
        ));
    }
    Ok(())
}

/// The writer role guard of push, pull and a view take: a reader is refused before anything is
/// read or sent.
pub fn require_writer(session: &Session, act: &str) -> Result<(), RdyError> {
    if session.role.can_write() {
        return Ok(());
    }
    Err(RdyError::new(
        E_PERMISSION,
        format!("role Reader cannot {act}"),
        "start the session as a writer",
    ))
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PushParams {
    /// The share remote to push to, by its name. Default: the share remote, else the only remote.
    pub remote: Option<String>,
    /// Views to push after canon: at most 256 names, each once. No view travels unless named.
    pub views: Option<Vec<String>>,
    /// true forces hub canon to this project's head when local canon does not hold the hub tip. Canon only. Returns a card first.
    pub force: Option<bool>,
    /// The path of a dump to restore into the hub repo. Takes no views and no force. Returns a card first when the hub repo holds canon.
    pub restore: Option<String>,
    /// The card_hash of the card the person answered. With answer.
    pub card_hash: Option<String>,
    /// The person's answer to the card: confirm or cancel. With card_hash.
    pub answer: Option<SyncAnswer>,
    /// The person's words, quoted (1 to 2000 characters). With card_hash and answer.
    pub evidence: Option<String>,
    /// The question you asked the person. With evidence.
    pub question: Option<String>,
    /// The chat session the person spoke in (default: mcp). With evidence.
    pub session_ref: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PullParams {
    /// The share remote to pull from, by its name. Default: the share remote, else the only remote.
    pub remote: Option<String>,
    /// Views to pull after canon: at most 256 names, each once. No view is pulled unless named.
    pub views: Option<Vec<String>>,
    /// true takes hub canon with no replay (a card first); with views it takes only those views and canon pulls as usual.
    pub take: Option<bool>,
    /// true stops a replay at its first conflict. Not with take and no views.
    pub strict: Option<bool>,
    /// The id of a dump that the hub's dump job gave (1 to 256 printable ASCII bytes): fetch that dump to out instead of pulling. Takes only out and one of remote or url.
    pub dump: Option<String>,
    /// The new file a dump fetch writes (relative to the project root). Only with dump.
    pub out: Option<String>,
    /// The HTTPS URL of the hub repo a dump fetch reads, in place of remote. Only with dump.
    pub url: Option<String>,
    /// The card_hash of the card the person answered. With answer.
    pub card_hash: Option<String>,
    /// The person's answer to the card: confirm or cancel. With card_hash.
    pub answer: Option<SyncAnswer>,
    /// The person's words, quoted (1 to 2000 characters). With card_hash and answer.
    pub evidence: Option<String>,
    /// The question you asked the person. With evidence.
    pub question: Option<String>,
    /// The chat session the person spoke in (default: mcp). With evidence.
    pub session_ref: Option<String>,
}

impl PullParams {
    /// The names of the inputs the call gave, a false bool or an empty list included: a dump
    /// fetch refuses each of them but its own (D-7CDEQ).
    fn given(&self) -> Vec<&'static str> {
        [
            ("remote", self.remote.is_some()),
            ("views", self.views.is_some()),
            ("take", self.take.is_some()),
            ("strict", self.strict.is_some()),
            ("dump", self.dump.is_some()),
            ("out", self.out.is_some()),
            ("url", self.url.is_some()),
            ("card_hash", self.card_hash.is_some()),
            ("answer", self.answer.is_some()),
            ("evidence", self.evidence.is_some()),
            ("question", self.question.is_some()),
            ("session_ref", self.session_ref.is_some()),
        ]
        .into_iter()
        .filter_map(|(k, given)| given.then_some(k))
        .collect()
    }
}

impl From<PushParams> for PushInputs {
    fn from(p: PushParams) -> PushInputs {
        PushInputs {
            remote: p.remote,
            views: p.views.unwrap_or_default(),
            force: p.force.unwrap_or(false),
            restore: p.restore,
            card_hash: p.card_hash,
            answer: p.answer,
            relayed_by: None,
            evidence: p.evidence,
            question: p.question,
            session_ref: p.session_ref,
        }
    }
}

impl From<PullParams> for PullInputs {
    fn from(p: PullParams) -> PullInputs {
        PullInputs {
            remote: p.remote,
            views: p.views.unwrap_or_default(),
            take: p.take.unwrap_or(false),
            strict: p.strict.unwrap_or(false),
            dump: p.dump,
            out: p.out,
            url: p.url,
            card_hash: p.card_hash,
            answer: p.answer,
            relayed_by: None,
            evidence: p.evidence,
            question: p.question,
            session_ref: p.session_ref,
        }
    }
}

/// The person a card answer is checked against: the configured human actor, read only when the
/// call answers a card.
fn person_for(st: &McpState, answered: bool) -> Result<String, RdyError> {
    if !answered {
        return Ok(String::new());
    }
    let root = st.engine.root();
    let user_file = require_user_file(rdy_engine::config_levels::user_file_from_env())?;
    let settings = HubConfig::load(Some(&root), Some(&user_file))?;
    auth_person(Some(&settings), &user_file)
}

/// `rdy_push`: a push of canon and the named views to the share remote, or a restore of a dump
/// into its hub repo. A move that removes landings returns the person's card first.
pub fn rdy_push(st: &mut McpState, p: PushParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    require_writer(&s, "push")?;
    let inputs = PushInputs::from(p);
    inputs.check()?;
    let person = person_for(st, inputs.card_hash.is_some())?;
    st.engine.sync(
        SyncCall {
            op: "push",
            args: inputs.to_args(),
            person,
        },
        &s,
    )
}

/// `rdy_pull`: a pull of hub canon and the named views from the share remote. A take of hub
/// canon returns the person's card first. With `dump` it fetches that hub dump instead, with no
/// project role (D-7CDEQ).
pub fn rdy_pull(st: &mut McpState, p: PullParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    check_dump_inputs(&p.given())?;
    let inputs = PullInputs::from(p);
    if inputs.dump.is_none() {
        require_writer(&s, "pull")?;
    }
    inputs.check()?;
    let person = person_for(st, inputs.card_hash.is_some())?;
    st.engine.sync(
        SyncCall {
            op: "pull",
            args: inputs.to_args(),
            person,
        },
        &s,
    )
}

/// The sync state of every remote that has some, for `rdy status` and the overview: what was
/// fetched and applied, a held landing request, a write that has no reply yet, an upload that
/// was not sent, and the landings and view ops not sent. `None` when no remote has state.
pub fn sync_status(read: &ReadState) -> Result<Option<Value>, RdyError> {
    let reader = read.reader();
    let remotes = reader.sync_remotes()?;
    if remotes.is_empty() {
        return Ok(None);
    }
    let hex = |h: &Option<rdy_model::Hash>| h.map(|h| h.to_string());
    let mut rows = Vec::new();
    for name in remotes {
        let canon = reader.sync_canon(&name)?;
        let (unsent, held_count, views_unsent) = match reader.sync_unsent(&name)? {
            Some(u) => u.counts(),
            None => (0, 0, Default::default()),
        };
        let views: Vec<Value> = reader
            .sync_views(&name)?
            .into_iter()
            .map(|(view, v)| {
                json!({
                    "view": view,
                    "fetched": hex(&v.fetched),
                    "applied": hex(&v.applied),
                    "unsent": views_unsent.get(&view).copied().unwrap_or(0),
                })
            })
            .collect();
        rows.push(json!({
            "remote": name,
            "fetched": canon.as_ref().and_then(|c| hex(&c.fetched)),
            "applied": canon.as_ref().and_then(|c| hex(&c.applied)),
            "held": canon.as_ref().and_then(|c| c.held.as_ref()).map(|h| json!({
                "head": h.head.to_string(), "request": h.request, "push": h.push,
            })),
            "pending": reader.sync_pending(&name)?.map(|p| json!({
                "push": p.push,
                "kind": if p.kind == KIND_VIEW { "view" } else { "canon" },
                "view": p.view,
                "head": p.head.to_string(),
            })),
            "upload": reader.sync_upload(&name)?.map(|u| json!({
                "push": u.push,
                "kind": if u.kind == KIND_VIEW { "view" } else { "canon" },
                "view": u.view,
            })),
            "unsent": unsent,
            "held_landings": held_count,
            "views": views,
        }));
    }
    Ok(Some(Value::Array(rows)))
}
