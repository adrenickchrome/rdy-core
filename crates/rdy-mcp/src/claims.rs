//! D-4PHYQ: the claim of a stdio connection, and the session settings saved per actor and run.
//!
//! A request states who acts in its `_meta`, with the keys `rdy/actor`, `rdy/run`, `rdy/role`
//! and `rdy/dedupe`, or the connection keeps the claim of an earlier call. The first valid
//! explicit claim, by `rdy_session` or by `_meta`, binds the connection for its life; a later
//! call that states another actor or run is refused. `rdy_session` saves its role and view for
//! the actor and run in the [`Registry`] its server's hub owns; a call's `rdy/role` holds for that
//! call only. Saved settings expire one hour after the last call that used them.
use crate::notify::NotifyHub;
use crate::params::parse_params_strict as parse_params;
use crate::tools::{McpState, no_session};
use rdy_engine::{E_BAD_INPUT, E_LIMIT, E_PERMISSION, RdyError, Session};
use rdy_model::Role;
use rmcp::model::JsonObject;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The `_meta` key of the claimed actor.
pub const ACTOR: &str = "rdy/actor";
/// The `_meta` key of the claimed run: a string, or null for no run.
pub const RUN: &str = "rdy/run";
/// The `_meta` key of this call's role.
pub const ROLE: &str = "rdy/role";
/// The `_meta` key of the saved dedupe setting.
pub const DEDUPE: &str = "rdy/dedupe";

/// The most saved session settings one registry holds.
pub const MAX_SETTINGS: usize = 1024;

/// How long saved settings live after the last call that used them; at this age they are gone.
pub const SETTINGS_IDLE: Duration = Duration::from_secs(3600);

/// The tool whose arguments are a claim of their own.
const SESSION_TOOL: &str = "rdy_session";

/// The name refusals of `_meta` give as their place.
const META: &str = "_meta";

/// An explicit claim: the actor and run a request states, with its options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub actor: String,
    pub run: Option<String>,
    pub role: Option<Role>,
    pub dedupe: Option<bool>,
}

/// What one request's `_meta` states about who acts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    /// An actor and a run, with options.
    Claim(Claim),
    /// Options only: the connection's claim acts.
    Options {
        role: Option<Role>,
        dedupe: Option<bool>,
    },
}

impl Asked {
    /// The actor and run stated, if any.
    pub fn identity(&self) -> Option<Key> {
        match self {
            Asked::Claim(c) => Some((c.actor.clone(), c.run.clone())),
            Asked::Options { .. } => None,
        }
    }

    /// This call's role, if stated.
    pub fn role(&self) -> Option<Role> {
        match self {
            Asked::Claim(c) => c.role,
            Asked::Options { role, .. } => *role,
        }
    }

    /// The dedupe setting stated, if any.
    pub fn dedupe(&self) -> Option<bool> {
        match self {
            Asked::Claim(c) => c.dedupe,
            Asked::Options { dedupe, .. } => *dedupe,
        }
    }
}

/// An actor and its run: the key of saved settings and of a connection's claim.
pub type Key = (String, Option<String>);

/// The settings saved for one actor and run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// `None` on an entry that only records a build warning.
    pub view: Option<String>,
    /// `None` on an entry that only records a build warning.
    pub role: Option<Role>,
    pub dedupe: bool,
    /// W_DAEMON_BUILD went out for this actor and run (D-CCC6E). `rdy_session` keeps it.
    pub daemon_build_warned: bool,
    /// ain4: a complete `rdy_instructions` reply went out for this actor and run, so its
    /// overview and briefings carry no reminder. In memory only; `rdy_session` keeps it.
    pub instructions_read: bool,
    /// The owning rulesets a complete instructions reply delivered ([`crate::kungfu::bit_of`]).
    /// In memory only; every save keeps it.
    pub kungfu_received: u16,
    /// The owning rulesets whose `W_KUNGFU` went out since a correct value or a reload.
    /// In memory only; every save keeps it.
    pub kungfu_warned: u16,
    /// The last call that used them.
    pub touched: Instant,
}

impl Settings {
    /// Settings that hold kungfu state and nothing else. A new functional entry may take their
    /// place when the registry is full: they exist for the advisory alone.
    fn kungfu_only(&self) -> bool {
        self.view.is_none()
            && self.role.is_none()
            && !self.dedupe
            && !self.daemon_build_warned
            && !self.instructions_read
    }
}

/// What the kungfu check reads for one actor and run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KungfuMarks {
    pub received: u16,
    pub warned: u16,
    /// A complete instructions reply went out: the reminder is not due.
    pub read: bool,
}

/// A monotonic clock: the registry reads the time through it, so a test can set the time.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// The session settings of one daemon or one stdio server, by actor and run. In memory only: a
/// restart drops them. At most [`MAX_SETTINGS`] entries; an entry idle for [`SETTINGS_IDLE`]
/// is purged before any capacity check.
pub struct Registry {
    clock: Clock,
    entries: Mutex<BTreeMap<Key, Settings>>,
}

impl Default for Registry {
    fn default() -> Registry {
        Registry::with_clock(Arc::new(Instant::now))
    }
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    /// A registry that reads the time from `clock`.
    pub fn with_clock(clock: Clock) -> Registry {
        Registry {
            clock,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    /// The entries, with every expired one purged, and the time now.
    fn live(&self) -> (MutexGuard<'_, BTreeMap<Key, Settings>>, Instant) {
        let now = (self.clock)();
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries.retain(|_, s| now.saturating_duration_since(s.touched) < SETTINGS_IDLE);
        (entries, now)
    }

    /// The settings saved for `key`, unless they expired.
    pub fn get(&self, key: &Key) -> Option<Settings> {
        self.live().0.get(key).cloned()
    }

    /// The number of live entries.
    pub fn len(&self) -> usize {
        self.live().0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Refreshes the settings of `key`, if they live: a call used them now.
    pub fn touch(&self, key: &Key) {
        let (mut entries, now) = self.live();
        if let Some(s) = entries.get_mut(key) {
            s.touched = now;
        }
    }

    /// `E_LIMIT` when `key` has no settings and the registry is full of functional entries. A
    /// kungfu-only entry gives way first.
    pub fn check_room(&self, key: &Key) -> Result<(), RdyError> {
        let (mut entries, _) = self.live();
        room(&mut entries, key)
    }

    /// Saves `view` and `role` for `key`. The dedupe setting is `dedupe`, else the one saved, else
    /// false. `E_LIMIT` when `key` is new and the registry is full.
    pub fn save(
        &self,
        key: &Key,
        view: String,
        role: Role,
        dedupe: Option<bool>,
    ) -> Result<(), RdyError> {
        let (mut entries, now) = self.live();
        room(&mut entries, key)?;
        let dedupe = dedupe
            .or_else(|| entries.get(key).map(|s| s.dedupe))
            .unwrap_or(false);
        let daemon_build_warned = entries.get(key).is_some_and(|s| s.daemon_build_warned);
        let instructions_read = entries.get(key).is_some_and(|s| s.instructions_read);
        let kungfu_received = entries.get(key).map_or(0, |s| s.kungfu_received);
        let kungfu_warned = entries.get(key).map_or(0, |s| s.kungfu_warned);
        entries.insert(
            key.clone(),
            Settings {
                view: Some(view),
                role: Some(role),
                dedupe,
                daemon_build_warned,
                instructions_read,
                kungfu_received,
                kungfu_warned,
                touched: now,
            },
        );
        Ok(())
    }

    /// Saves the dedupe setting of `key`. Settings that do not exist yet, or hold kungfu state
    /// alone, start with the writer role and `view()`, the session's default view; the kungfu sets
    /// stay. `E_LIMIT` when `key` is new and the registry is full of functional entries.
    pub fn save_dedupe(
        &self,
        key: &Key,
        dedupe: bool,
        view: impl FnOnce() -> String,
    ) -> Result<(), RdyError> {
        let (mut entries, now) = self.live();
        if let Some(s) = entries.get_mut(key) {
            if s.kungfu_only() {
                s.view = Some(view());
                s.role = Some(Role::Writer);
            }
            s.dedupe = dedupe;
            s.touched = now;
            return Ok(());
        }
        room(&mut entries, key)?;
        entries.insert(
            key.clone(),
            Settings {
                view: Some(view()),
                role: Some(Role::Writer),
                dedupe,
                daemon_build_warned: false,
                instructions_read: false,
                kungfu_received: 0,
                kungfu_warned: 0,
                touched: now,
            },
        );
        Ok(())
    }

    /// Whether this call's reply carries W_DAEMON_BUILD for `key`, taken under the registry's
    /// lock, so concurrent connections emit once. The first take records it, with no role or
    /// view when `key` has no settings. A full registry with no entry for `key` records nothing:
    /// the warning then repeats, and it never refuses.
    pub fn take_build_warning(&self, key: &Key) -> bool {
        let (mut entries, now) = self.live();
        if let Some(s) = entries.get_mut(key) {
            s.touched = now;
            return !std::mem::replace(&mut s.daemon_build_warned, true);
        }
        if room(&mut entries, key).is_ok() {
            entries.insert(
                key.clone(),
                Settings {
                    view: None,
                    role: None,
                    dedupe: false,
                    daemon_build_warned: true,
                    instructions_read: false,
                    kungfu_received: 0,
                    kungfu_warned: 0,
                    touched: now,
                },
            );
        }
        true
    }

    /// Whether a complete instructions reply went out for `key` while its settings live.
    pub fn instructions_read(&self, key: &Key) -> bool {
        self.live().0.get(key).is_some_and(|s| s.instructions_read)
    }

    /// Records that a complete instructions reply went out for `key`, with no role or view when
    /// `key` has no settings. A full registry with no entry for `key` records nothing: the
    /// reminder then stays, and the reply is never refused for it.
    pub fn mark_instructions_read(&self, key: &Key) {
        let (mut entries, now) = self.live();
        if let Some(s) = entries.get_mut(key) {
            s.touched = now;
            s.instructions_read = true;
            return;
        }
        if room(&mut entries, key).is_ok() {
            entries.insert(
                key.clone(),
                Settings {
                    view: None,
                    role: None,
                    dedupe: false,
                    daemon_build_warned: false,
                    instructions_read: true,
                    kungfu_received: 0,
                    kungfu_warned: 0,
                    touched: now,
                },
            );
        }
    }

    /// The entries as they stand, with no purge: the kungfu check runs after the call's claim, which
    /// purged them and refreshed this key's, so it adds no scan of the registry.
    fn entries(&self) -> MutexGuard<'_, BTreeMap<Key, Settings>> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// What the kungfu check reads for `key`.
    pub fn kungfu_marks(&self, key: &Key) -> KungfuMarks {
        self.entries()
            .get(key)
            .map_or_else(Default::default, |s| KungfuMarks {
                received: s.kungfu_received,
                warned: s.kungfu_warned,
                read: s.instructions_read,
            })
    }

    /// Clears `bit` from `key`'s warned set: a correct value rearms that owner. It makes no entry.
    pub fn clear_kungfu_warned(&self, key: &Key, bit: u16) {
        if let Some(s) = self.entries().get_mut(key) {
            s.kungfu_warned &= !bit;
        }
    }

    /// Marks `bit`'s warning as sent for `key`: whether this reply is the one to carry it. Another
    /// connection of the same actor and run may have carried it first. With no room for a new
    /// entry the mark is skipped and the reply carries the warning: a later reply may repeat it,
    /// and no call is refused. A new mark takes no other entry's place.
    pub fn mark_kungfu_warned(&self, key: &Key, bit: u16) -> bool {
        let now = (self.clock)();
        let mut entries = self.entries();
        if let Some(s) = entries.get_mut(key) {
            let fresh = s.kungfu_warned & bit == 0;
            s.kungfu_warned |= bit;
            return fresh;
        }
        if entries.len() < MAX_SETTINGS {
            entries.insert(
                key.clone(),
                Settings {
                    view: None,
                    role: None,
                    dedupe: false,
                    daemon_build_warned: false,
                    instructions_read: false,
                    kungfu_received: 0,
                    kungfu_warned: bit,
                    touched: now,
                },
            );
        }
        true
    }

    /// Adds `bits` to `key`'s received set and clears them from its warned set. An actor and run
    /// with no entry have no room for one: the bits are skipped.
    pub fn mark_kungfu_received(&self, key: &Key, bits: u16) {
        if let Some(s) = self.entries().get_mut(key) {
            s.kungfu_received |= bits;
            s.kungfu_warned &= !bits;
        }
    }
}

/// Room for `key`: it has an entry, or the registry has a free slot, or the first kungfu-only entry
/// in map order gives way. Else the registry's own refusal.
fn room(entries: &mut BTreeMap<Key, Settings>, key: &Key) -> Result<(), RdyError> {
    if entries.contains_key(key) || entries.len() < MAX_SETTINGS {
        return Ok(());
    }
    let reclaim = entries
        .iter()
        .find(|(k, s)| *k != key && s.kungfu_only())
        .map(|(k, _)| k.clone());
    if let Some(k) = reclaim {
        entries.remove(&k);
        return Ok(());
    }
    Err(RdyError::new(
        E_LIMIT,
        format!("saved session settings exceed {MAX_SETTINGS} sessions"),
        "reuse an actor and run, or try again later: idle settings expire after one hour",
    ))
}

/// The refusal of a call whose actor or run is not the connection's claim.
pub fn mismatch() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "this stdio connection is bound to another actor or run",
        "use the connection's actor and run, or open another stdio connection",
    )
}

/// The role a session or a claim names: reader, writer or reviewer. Approver is refused, as any
/// other word is.
pub fn parse_role(text: &str) -> Result<Role, RdyError> {
    match text {
        "reader" => Ok(Role::Reader),
        "writer" => Ok(Role::Writer),
        "reviewer" => Ok(Role::Reviewer),
        "approver" => Err(RdyError::new(
            E_PERMISSION,
            "approver sessions are not available over MCP",
            "relay the person's answer with rdy_answer",
        )),
        other => Err(RdyError::new(
            E_BAD_INPUT,
            format!("unknown role {other:?}"),
            "use reader, writer or reviewer",
        )),
    }
}

/// An agent's session over MCP (D-ME2C0): [`Session::agent`], with MCP's fix for a human claim.
pub fn agent_session(actor: String, run: Option<String>, role: Role) -> Result<Session, RdyError> {
    Session::agent(actor, run, role).map_err(|mut e| {
        if Session::is_human_claim(&e) {
            e.fix = crate::tools::MCP_HUMAN_CLAIM_FIX.to_string();
        }
        e
    })
}

/// The closed set of claim keys, for the unknown-key refusal and its nearest key.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct MetaKeys {
    #[serde(rename = "rdy/actor")]
    actor: Option<Value>,
    #[serde(rename = "rdy/dedupe")]
    dedupe: Option<Value>,
    #[serde(rename = "rdy/role")]
    role: Option<Value>,
    #[serde(rename = "rdy/run")]
    run: Option<Value>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ActorValue {
    #[serde(rename = "rdy/actor")]
    v: String,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct RunValue {
    #[serde(rename = "rdy/run")]
    v: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct RoleValue {
    #[serde(rename = "rdy/role")]
    v: String,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct DedupeValue {
    #[serde(rename = "rdy/dedupe")]
    v: bool,
}

/// The refusal for the value of `key`, in the parameter vocabulary, if its type is wrong.
fn check_type(key: &str, value: &Value) -> Result<(), RdyError> {
    let one: JsonObject = [(key.to_string(), value.clone())].into_iter().collect();
    match key {
        ACTOR => parse_params::<ActorValue>(META, one).map(drop),
        RUN => parse_params::<RunValue>(META, one).map(drop),
        ROLE => parse_params::<RoleValue>(META, one).map(drop),
        _ => parse_params::<DedupeValue>(META, one).map(drop),
    }
}

/// What a request's `_meta` states about who acts: `None` when it has no `rdy/` key. Metadata
/// outside that namespace is ignored. The checks, in order: unknown `rdy/` keys (the first in
/// bytewise order, with the nearest key), value types (actor, run, role, dedupe), both of actor
/// and run or neither, the role, then the actor and run forms and the human claim.
pub fn parse(meta: Option<&JsonObject>) -> Result<Option<Asked>, RdyError> {
    let ours: JsonObject = meta
        .into_iter()
        .flatten()
        .filter(|(k, _)| k.starts_with("rdy/"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if ours.is_empty() {
        return Ok(None);
    }
    parse_params::<MetaKeys>(META, ours.clone())?;
    for key in [ACTOR, RUN, ROLE, DEDUPE] {
        if let Some(v) = ours.get(key) {
            check_type(key, v)?;
        }
    }
    if ours.contains_key(ACTOR) != ours.contains_key(RUN) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "a _meta claim needs both rdy/actor and rdy/run",
            "pass both keys, or omit both and use the connection's claim",
        ));
    }
    let role = ours
        .get(ROLE)
        .and_then(Value::as_str)
        .map(parse_role)
        .transpose()?;
    let dedupe = ours.get(DEDUPE).and_then(Value::as_bool);
    let Some(actor) = ours.get(ACTOR).and_then(Value::as_str) else {
        return Ok(Some(Asked::Options { role, dedupe }));
    };
    let run = ours.get(RUN).and_then(Value::as_str).map(str::to_string);
    let s = agent_session(actor.to_string(), run, role.unwrap_or(Role::Writer))?;
    Ok(Some(Asked::Claim(Claim {
        actor: s.actor,
        run: s.run,
        role,
        dedupe,
    })))
}

/// The hub's registry, held apart from the state it is reached through.
fn hub(st: &McpState) -> Arc<NotifyHub> {
    Arc::clone(&st.hub)
}

/// Starts one claimed call (a tool call or a resource read) under the state lock: resolves who
/// acts from `asked` and the connection's claim, and installs the call's effective session and
/// view. The first explicit claim binds the connection. `rdy_session` resolves its own claim
/// from its arguments, so here it only receives `asked`. A refusal changes nothing.
pub fn enter(st: &mut McpState, asked: Option<&Asked>, tool: &str) -> Result<(), RdyError> {
    st.claim = asked.cloned();
    if tool == SESSION_TOOL {
        return Ok(());
    }
    let key = claimed_key(asked, st.bound.as_ref())?;
    let hub = hub(st);
    let saved = hub.claims().get(&key);
    let role = asked
        .and_then(Asked::role)
        .or(saved.as_ref().and_then(|s| s.role))
        .unwrap_or(Role::Writer);
    let session = agent_session(key.0.clone(), key.1.clone(), role)?;
    if let Some(d) = asked.and_then(Asked::dedupe) {
        hub.claims()
            .save_dedupe(&key, d, || session.default_view())?;
    }
    hub.claims().touch(&key);
    if st.bound.is_none() {
        hub.identify(st.hub_sid(), &key.0, key.1.as_deref());
        st.bound = Some(key);
    }
    st.view = saved
        .and_then(|s| s.view)
        .unwrap_or_else(|| session.default_view());
    st.session = Some(session);
    Ok(())
}

/// The actor and run a call acts as: its stated claim, which must be the connection's `bound`
/// one when the connection has one, else the bound one. Neither is `E_NO_SESSION`.
pub fn claimed_key(asked: Option<&Asked>, bound: Option<&Key>) -> Result<Key, RdyError> {
    match (asked.and_then(Asked::identity), bound) {
        (Some(k), Some(b)) if k != *b => Err(mismatch()),
        (Some(k), _) => Ok(k),
        (None, Some(b)) => Ok(b.clone()),
        (None, None) => Err(no_session()),
    }
}

/// The role saved for the actor and run of `s` (writer when none is saved): the role a call that
/// states no `rdy/role` acts with. A one-call override never reaches what is persisted.
pub fn saved_role(st: &McpState, s: &Session) -> Role {
    hub(st)
        .claims()
        .get(&(s.actor.clone(), s.run.clone()))
        .and_then(|x| x.role)
        .unwrap_or(Role::Writer)
}

/// `rdy_session`'s claim check, once its session is valid: the actor and run of a `_meta` claim
/// on the same call, and the connection's claim, are those of `key`.
pub fn check_session_claim(st: &McpState, key: &Key) -> Result<(), RdyError> {
    check_claim(st.bound.as_ref(), st.claim.as_ref(), key)
}

/// [`check_session_claim`] over a connection's `bound` claim and a call's `stated` one.
pub fn check_claim(bound: Option<&Key>, stated: Option<&Asked>, key: &Key) -> Result<(), RdyError> {
    let stated = stated.and_then(Asked::identity);
    if stated.is_some_and(|k| k != *key) || bound.is_some_and(|b| b != key) {
        return Err(mismatch());
    }
    Ok(())
}

/// Ends a call: the state holds the connection's claim with its saved settings, and no call's
/// override. An unbound connection holds no session.
pub fn rest(st: &mut McpState) {
    st.claim = None;
    let Some((actor, run)) = st.bound.clone() else {
        st.session = None;
        return;
    };
    let saved = hub(st).claims().get(&(actor.clone(), run.clone()));
    let role = saved.as_ref().and_then(|s| s.role).unwrap_or(Role::Writer);
    match agent_session(actor, run, role) {
        Ok(s) => {
            st.view = saved
                .and_then(|x| x.view)
                .unwrap_or_else(|| s.default_view());
            st.session = Some(s);
        }
        Err(_) => st.session = None,
    }
}

/// Runs `f` as one claimed call: [`enter`], then `f`, then [`rest`]. A refused claim is the
/// call's reply. A successful `rdy_session` binds an unbound connection to its actor and run.
/// This is the protocol's door; the server runs it under the state lock. A tool function called
/// directly, outside this door, binds nothing.
pub fn call<T>(
    st: &mut McpState,
    asked: Option<&Asked>,
    tool: &str,
    f: impl FnOnce(&mut McpState) -> Result<T, RdyError>,
) -> Result<T, RdyError> {
    let out = enter(st, asked, tool).and_then(|()| f(st));
    if tool == SESSION_TOOL
        && out.is_ok()
        && st.bound.is_none()
        && let Some(s) = &st.session
    {
        st.bound = Some((s.actor.clone(), s.run.clone()));
    }
    rest(st);
    out
}

/// Takes the build warning for this call's actor and run, once a claim is valid: the connection's
/// bound one. Nothing for an unbound connection or one with no warning to give.
pub fn take_build_warning(st: &mut McpState) {
    if st.build_warning_due.is_some() {
        return;
    }
    let (Some(warning), Some(key)) = (&st.build_warning, &st.bound) else {
        return;
    };
    if st.hub.claims().take_build_warning(key) {
        st.build_warning_due = Some(warning.to_json());
    }
}

/// Puts a taken warning on a tool's result. The old keys, the refusal or not, and the warnings
/// already there (strings or objects, in their order) stay; it goes last.
pub fn deliver_build_warning(
    st: &mut McpState,
    out: Result<Value, RdyError>,
) -> Result<Value, RdyError> {
    let Some(warning) = st.build_warning_due.take() else {
        return out;
    };
    match out {
        Ok(mut v) => {
            append_warning(&mut v, warning);
            Ok(v)
        }
        Err(mut e) => {
            e.warnings.get_or_insert_default().push(warning);
            Err(e)
        }
    }
}

/// Appends to `v`'s `warnings` array, or starts one. A reply that is not an object stays as it is.
pub(crate) fn append_warning(v: &mut Value, warning: Value) {
    let Some(obj) = v.as_object_mut() else {
        return;
    };
    match obj.get_mut("warnings").and_then(Value::as_array_mut) {
        Some(all) => all.push(warning),
        None => {
            obj.insert("warnings".into(), Value::Array(vec![warning]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mh5_a_warning_follows_existing_warning_strings() {
        let w = json!({"code": "W_DAEMON_BUILD"});
        let mut v = json!({"a": 1, "warnings": ["one", {"code": "W_X"}]});
        append_warning(&mut v, w.clone());
        assert_eq!(v["warnings"], json!(["one", {"code": "W_X"}, w.clone()]));
        assert_eq!(v["a"], 1);
        let mut v = json!({"a": 1});
        append_warning(&mut v, w.clone());
        assert_eq!(v["warnings"], json!([w]));
    }

    fn meta(v: Value) -> JsonObject {
        v.as_object().unwrap().clone()
    }

    fn err(v: Value) -> RdyError {
        parse(Some(&meta(v))).unwrap_err()
    }

    #[test]
    fn ui5_meta_parse_follows_the_closed_order() {
        assert_eq!(parse(None).unwrap(), None);
        assert_eq!(
            parse(Some(&meta(json!({"progressToken": 1})))).unwrap(),
            None
        );
        let e = err(json!({"rdy/zz": 1, "rdy/actr": 2, "rdy/actor": 3}));
        assert_eq!(e.message, "_meta: unknown parameter \"rdy/actr\"");
        assert_eq!(
            e.fix,
            "did you mean \"rdy/actor\"? the parameters here are rdy/actor, rdy/dedupe, rdy/role, rdy/run"
        );
        let e = err(json!({"rdy/zzzzzz": 1}));
        assert_eq!(
            e.fix,
            "remove \"rdy/zzzzzz\"; the parameters here are rdy/actor, rdy/dedupe, rdy/role, rdy/run"
        );
        // types in actor, run, role, dedupe order
        let e = err(json!({"rdy/dedupe": "x", "rdy/run": 1, "rdy/actor": "agent:a"}));
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_BAD_INPUT,
                "_meta: parameter \"rdy/run\" must be a string, not a number",
                "pass \"rdy/run\" as a string"
            )
        );
        let e = err(json!({"rdy/actor": null, "rdy/run": null}));
        assert_eq!(
            e.message,
            "_meta: parameter \"rdy/actor\" must be a string, not null"
        );
        let e = err(json!({"rdy/dedupe": null}));
        assert_eq!(
            e.message,
            "_meta: parameter \"rdy/dedupe\" must be a boolean, not null"
        );
        let e = err(json!({"rdy/actor": "agent:a"}));
        assert_eq!(e.message, "a _meta claim needs both rdy/actor and rdy/run");
        let ok = parse(Some(&meta(
            json!({"rdy/actor": "ci:0123456789abcdef0123456789abcdef/nightly", "rdy/run": null}),
        )))
        .unwrap()
        .unwrap();
        assert_eq!(
            ok.identity(),
            Some((
                "ci:0123456789abcdef0123456789abcdef/nightly".to_string(),
                None
            ))
        );
    }

    #[test]
    fn ui5_the_registry_expires_inclusively_and_caps_new_entries() {
        let base = Instant::now();
        let at = Arc::new(Mutex::new(base));
        let clock = Arc::clone(&at);
        let r = Registry::with_clock(Arc::new(move || *clock.lock().unwrap()));
        let k = |n: usize| (format!("agent:a{n}"), None);
        r.save(&k(0), "v".into(), Role::Reader, None).unwrap();
        *at.lock().unwrap() = base + SETTINGS_IDLE - Duration::from_secs(1);
        assert!(r.get(&k(0)).is_some());
        *at.lock().unwrap() = base + SETTINGS_IDLE;
        assert!(r.get(&k(0)).is_none(), "gone at one hour");
        for n in 0..MAX_SETTINGS {
            r.save(&k(n), "v".into(), Role::Writer, None).unwrap();
        }
        let e = r.save(&k(MAX_SETTINGS), "v".into(), Role::Writer, None);
        assert_eq!(e.unwrap_err().code, E_LIMIT);
        r.save(&k(5), "w".into(), Role::Reader, Some(true)).unwrap();
        assert_eq!(r.len(), MAX_SETTINGS);
    }
}
