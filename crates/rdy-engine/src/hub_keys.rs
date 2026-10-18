//! Key bind, retire and revoke over the hub's key API (D-XXA9D, D-17AA8, D-MRQB6).
//!
//! - **Bind proves possession and returns a link.** The configured key of the kind asked for
//!   (`signing.key` for a person, `signing.agent_key` for an agent) is resolved through the
//!   caller's agent, the hub issues one challenge, SIG builds the keybind statement and the agent
//!   signs it under `rdy-keybind-v1`. The proof is checked once, then sent once. The reply is the
//!   pending key and the hub's confirmation link: confirmation is a person's act on the hub, so
//!   it is never called, polled or claimed here. Nothing is stored: no proof, statement or
//!   signature record.
//! - **Retire and revoke look the hub id up first.** The key argument is normalized to its
//!   SHA256 fingerprint with SIG's reader (no agent is contacted), the hub lists that one key,
//!   and its hub id is retired or revoked once. The hub owns the key's state, its times and the
//!   meaning of a compromise time; nothing local changes.
//! - **No lock spans the agent.** Access comes from [`HubCredentials`], which releases the
//!   credential lock before it returns; settings are read without locks; no project store is
//!   opened. An agent or security key may wait for the person for as long as it needs.
//! - **One request each, never retried.** A lost FindKey reply changed nothing
//!   (`E_UPSTREAM_TRANSPORT`); a lost challenge, Bind, Retire or Revoke reply may have
//!   completed (`E_UPSTREAM_UNCERTAIN`). A refused actor claim reads `/me` once through
//!   [`HubCredentials::refresh_handle`] and reports `E_UPSTREAM_ACTOR_CLAIM` with any handle
//!   change; the refused request is not sent again.
use crate::config_levels::{SIGNING_AGENT_KEY, SIGNING_KEY, effective};
use crate::error::{
    E_BAD_INPUT, RdyError, signing_key_missing, upstream_actor_claim, upstream_auth,
    upstream_key_proof, upstream_protocol, upstream_refused, upstream_transport,
};
use crate::hub_config::{HubConfig, Remote};
use crate::hub_credentials::{AccessGrant, HubCredentials};
use crate::hub_http::route;
use crate::hub_types::{
    BindResult, HubCall, HubClock, HubFailure, HubKey, HubOperation, HubReply, HubTransport,
    KEYBIND_NAMESPACE, KeyChallenge, KeyKind, KeyState, LimitUnit, MAX_ID_BYTES, MAX_SIG_KEY_BYTES,
    MAX_SIG_PROOF_BYTES, MAX_TIMESTAMP_BYTES, Origin, OriginFence, RequestClaim, check_count,
    check_label, parse_compromised_since,
};
use crate::session::Session;
use crate::signing_config::SigningConfig;
use crate::signing_crypto::{
    KEY_TYPES, KeyInfo, SigningAgent, key_fingerprint, resolve_key, sign_sshsig, verify_sshsig,
};
use rdy_model::signature::{StatementSource, statement};
use rdy_schema::Issue;
use serde::Serialize;
use ssh_encoding::base64::{Base64, Encoding};
use std::path::{Path, PathBuf};

/// How long a key-binding challenge lasts at most, in milliseconds (v-36TCV).
pub const CHALLENGE_LIFE_MS: i64 = 600_000;

/// The message of an `E_BAD_INPUT` key argument.
pub const BAD_KEY_MESSAGE: &str = "key is not a public key or SHA256 fingerprint";
/// The fix of [`BAD_KEY_MESSAGE`].
pub const BAD_KEY_FIX: &str = "pass a public key line or its SHA256 fingerprint";
/// The message of an `E_BAD_INPUT` expired challenge.
pub const CHALLENGE_EXPIRED_MESSAGE: &str = "the key binding challenge expired";
/// The fix of [`CHALLENGE_EXPIRED_MESSAGE`].
pub const CHALLENGE_EXPIRED_FIX: &str = "start key binding again";

fn bad_key() -> RdyError {
    RdyError::new(E_BAD_INPUT, BAD_KEY_MESSAGE, BAD_KEY_FIX)
}

fn challenge_expired() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        CHALLENGE_EXPIRED_MESSAGE,
        CHALLENGE_EXPIRED_FIX,
    )
}

// ---------------------------------------------------------------------------------------------
// Replies
// ---------------------------------------------------------------------------------------------

/// A bind reply: the key's SHA256 fingerprint, its kind, the hub's state (always `pending`: the
/// binding waits for a person's confirmation on the hub), the link that person opens, the
/// remote used and the warnings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BindReply {
    pub key: String,
    pub kind: KeyKind,
    pub state: KeyState,
    pub confirm_url: String,
    pub remote: String,
    pub warnings: Vec<Issue>,
}

/// A retire reply: the key's SHA256 fingerprint and the hub's state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KeyStateReply {
    pub key: String,
    pub state: KeyState,
    pub remote: String,
    pub warnings: Vec<Issue>,
}

/// A revoke reply: as [`KeyStateReply`], with the hub's revoke time and the start of the
/// compromise, both exactly as the hub returned them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RevokeReply {
    pub key: String,
    pub state: KeyState,
    pub revoked_at: String,
    pub compromised_from: String,
    pub remote: String,
    pub warnings: Vec<Issue>,
}

// ---------------------------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------------------------

/// The canonical SHA256 fingerprint of a key argument: a public-key line or a fingerprint, as
/// SIG reads them. Over the key bound is `E_LIMIT`; anything else is `E_BAD_INPUT`.
fn normalize_key(key: &str) -> Result<String, RdyError> {
    check_count(key.len(), MAX_SIG_KEY_BYTES, LimitUnit::Bytes)?;
    key_fingerprint(key).map_err(|()| bad_key())
}

/// Nonempty text within the id bound with no control character.
fn bounded_value(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_ID_BYTES && !text.chars().any(char::is_control)
}

/// An RFC3339 time, within the timestamp bound, in milliseconds since the epoch.
fn time_ms(text: &str) -> Option<i64> {
    if text.len() > MAX_TIMESTAMP_BYTES {
        return None;
    }
    let t =
        time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()?;
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok()
}

/// Checks a challenge against the request and the signed-in account: its account, kind and
/// label (the given one, or the kind's default the hub stores), a bounded id and nonce, the
/// keybind namespace exactly, and an end within the next ten minutes on `now`. Returns that end.
/// Anything else is `E_UPSTREAM_PROTOCOL`.
fn check_challenge(
    c: &KeyChallenge,
    account: &str,
    kind: KeyKind,
    label: Option<&str>,
    now: i64,
) -> Result<i64, RdyError> {
    let want_label = label.unwrap_or(kind.default_label());
    let ends = time_ms(&c.expires_at).ok_or_else(upstream_protocol)?;
    let latest = now
        .checked_add(CHALLENGE_LIFE_MS)
        .ok_or_else(upstream_protocol)?;
    let ok = c.account == account
        && c.kind == kind
        && c.label == want_label
        && bounded_value(&c.id)
        && bounded_value(&c.nonce)
        && c.namespace == KEYBIND_NAMESPACE
        && now < ends
        && ends <= latest;
    if ok {
        Ok(ends)
    } else {
        Err(upstream_protocol())
    }
}

/// Checks the agent's proof once before it is sent: a bounded SSHSIG blob that SIG's
/// cryptographic check finds valid over `stmt` under the keybind namespace, by an accepted key
/// type, from exactly the selected key. Else `E_UPSTREAM_KEY_PROOF`.
fn check_proof(key: &KeyInfo, stmt: &[u8], blob: &[u8]) -> Result<(), RdyError> {
    if blob.len() > MAX_SIG_PROOF_BYTES {
        return Err(upstream_key_proof());
    }
    let check = verify_sshsig(KEYBIND_NAMESPACE, stmt, blob);
    let ok = check.valid
        && check
            .key_type
            .as_deref()
            .is_some_and(|t| KEY_TYPES.contains(&t))
        && check.key.as_deref() == Some(key.fingerprint.as_str());
    if ok {
        Ok(())
    } else {
        Err(upstream_key_proof())
    }
}

/// Checks a Bind reply: the pending key just sent, of the requested kind, on the challenge's
/// account, with the install's own confirmation link for its hub id. Else
/// `E_UPSTREAM_PROTOCOL`.
fn check_bound(
    b: &BindResult,
    install: &Origin,
    key: &KeyInfo,
    public_key: &str,
    kind: KeyKind,
    account: &str,
) -> Result<(), RdyError> {
    let k = &b.key;
    if !bounded_value(&k.id) {
        return Err(upstream_protocol());
    }
    let confirm = install
        .join(&["app", "settings", "keys", &k.id, "confirm"])
        .map_err(|_| upstream_protocol())?;
    let ok = k.fingerprint == key.fingerprint
        && k.public_key == public_key
        && k.kind == kind
        && k.account == account
        && k.state == KeyState::Pending
        && b.confirm_url == confirm;
    if ok { Ok(()) } else { Err(upstream_protocol()) }
}

/// Whether a Retire or Revoke reply is the key acted on (its hub id and fingerprint) in
/// `state`.
fn is_changed(k: &HubKey, key_id: &str, fingerprint: &str, state: KeyState) -> bool {
    k.id == key_id && k.fingerprint == fingerprint && k.state == state
}

// ---------------------------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------------------------

/// The remote a command uses, its install and what requests to it need.
struct Target {
    remote: Remote,
    install: Origin,
    fence: OriginFence,
    ca_file: Option<PathBuf>,
    /// The hub settings' warnings: a success carries them, a failure carries them as issues.
    warnings: Vec<Issue>,
}

impl Target {
    /// `e` with this target's settings warnings added to its issues.
    fn fail(&self, mut e: RdyError) -> RdyError {
        e.issues.extend(self.warnings.iter().cloned());
        e
    }
}

/// Whether a lost reply of an operation may have changed something on the hub.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect {
    /// A read: a lost reply changed nothing.
    Read,
    /// A challenge or a mutation: a lost reply may have completed.
    Write,
}

/// The key commands of one project for the user config at `user_file`, over `api`, with the
/// caller's agent `agent`.
pub struct KeyService<'a> {
    project: PathBuf,
    user_file: PathBuf,
    api: &'a dyn HubTransport,
    clock: &'a dyn HubClock,
    agent: &'a dyn SigningAgent,
}

impl<'a> KeyService<'a> {
    /// The key commands of the project at `project` with the user config `user_file`
    /// (`<home>/rdy/config.yaml`), whose sibling `credentials.yaml` holds the sign-ins. `agent`
    /// is the caller's own SSH agent; only bind uses it.
    pub fn new(
        project: &Path,
        user_file: &Path,
        api: &'a dyn HubTransport,
        clock: &'a dyn HubClock,
        agent: &'a dyn SigningAgent,
    ) -> KeyService<'a> {
        KeyService {
            project: project.to_path_buf(),
            user_file: user_file.to_path_buf(),
            api,
            clock,
            agent,
        }
    }

    fn credentials(&self) -> HubCredentials<'a> {
        HubCredentials::new(Some(&self.project), &self.user_file, self.api, self.clock)
    }

    /// The remote `name`, else the share remote, else the only remote; its install's fence and
    /// user CA file.
    fn target(&self, name: Option<&str>) -> Result<Target, RdyError> {
        let settings = HubConfig::load(Some(&self.project), Some(&self.user_file))?;
        let with_warnings = |mut e: RdyError| {
            e.issues.extend(settings.warnings.iter().cloned());
            e
        };
        let remote = settings.select_remote(name).map_err(with_warnings)?;
        let install = remote.url.origin().clone();
        let fence = settings.fence(&install).map_err(with_warnings)?;
        let ca_file = settings.install(&install).and_then(|i| i.ca_file.clone());
        Ok(Target {
            remote,
            install,
            fence,
            ca_file,
            warnings: settings.warnings,
        })
    }

    /// One key-route exchange with `grant`'s token and bound claim, checked again for sending,
    /// to the operation's own route on the install, which the fence must trust.
    fn send(
        &self,
        t: &Target,
        grant: &AccessGrant,
        operation: HubOperation,
    ) -> Result<HubReply, HubFailure> {
        let destination = route(&t.install, &operation)?.ok_or(HubFailure::Protocol)?;
        t.fence.check(&destination)?;
        grant.claim.validate_for_send()?;
        self.api.exchange(HubCall {
            install: t.install.clone(),
            destination,
            ca_file: t.ca_file.clone(),
            claim: Some(grant.claim.clone()),
            bearer: Some(grant.access_token.clone()),
            operation,
        })
    }

    /// The error of a failed exchange. A refused actor claim reads `/me` once and is
    /// `E_UPSTREAM_ACTOR_CLAIM` with any handle change, or the `/me` failure itself; the refused
    /// request is never sent again. A lost reply of a read is `E_UPSTREAM_TRANSPORT`.
    fn failure(
        &self,
        t: &Target,
        claim: &RequestClaim,
        failure: HubFailure,
        effect: Effect,
    ) -> RdyError {
        let e = match failure {
            HubFailure::ActorClaim => match self.credentials().refresh_handle(&t.install, claim) {
                Ok(update) => {
                    let mut e = upstream_actor_claim();
                    e.issues.extend(update.warnings);
                    e
                }
                Err(e) => e,
            },
            HubFailure::Transport { .. } if effect == Effect::Read => upstream_transport(),
            f => f.into_error(&t.install),
        };
        t.fail(e)
    }

    /// Current access to the target's install, with `claim` bound to its account. The
    /// credential lock is released when it returns.
    fn access(&self, t: &Target, claim: &RequestClaim) -> Result<AccessGrant, RdyError> {
        self.credentials()
            .access(&t.install, claim)
            .map_err(|e| t.fail(e))
    }

    // -----------------------------------------------------------------------------------------
    // Bind
    // -----------------------------------------------------------------------------------------

    /// Binds the configured key of `kind` to the signed-in account of the selected remote's
    /// install: one challenge, one proof by the caller's agent, one Bind. The reply is the
    /// pending key and the link a person opens on the hub to confirm it.
    pub fn bind(
        &self,
        kind: KeyKind,
        label: Option<&str>,
        remote: Option<&str>,
        session: &Session,
    ) -> Result<BindReply, RdyError> {
        if let Some(label) = label {
            check_label(label)?;
        }
        let claim = RequestClaim::from_session(session)?;
        let t = self.target(remote)?;
        let grant = self.access(&t, &claim)?;
        let key = self.selected_key(kind).map_err(|e| t.fail(e))?;

        let asked = HubOperation::BindChallenge {
            kind,
            label: label.map(str::to_string),
        };
        let challenge = match self.send(&t, &grant, asked) {
            Ok(HubReply::BindChallenge(c)) => c,
            Ok(_) => return Err(t.fail(upstream_protocol())),
            Err(f) => return Err(self.failure(&t, &claim, f, Effect::Write)),
        };
        let now = self.clock.now_ms();
        let ends =
            check_challenge(&challenge, &grant.account, kind, label, now).map_err(|e| t.fail(e))?;

        let source = StatementSource::Keybind {
            account: &challenge.account,
            challenge: &challenge.id,
        };
        let stmt =
            statement(source, None, &key.fingerprint).map_err(|_| t.fail(upstream_protocol()))?;
        // The agent may wait for the person here: no credential, settings or project lock is
        // held.
        let blob =
            sign_sshsig(self.agent, &key, KEYBIND_NAMESPACE, &stmt).map_err(|e| t.fail(e))?;
        check_proof(&key, &stmt, &blob).map_err(|e| t.fail(e))?;
        if self.clock.now_ms() >= ends {
            return Err(t.fail(challenge_expired()));
        }
        let grant = self.access(&t, &claim)?;
        if grant.account != challenge.account {
            return Err(t.fail(upstream_auth(t.install.as_str())));
        }

        let public_key = format!(
            "{} {}",
            key.key_type,
            Base64::encode_string(&key.public_key)
        );
        let bind = HubOperation::Bind {
            challenge: challenge.id.clone(),
            public_key: public_key.clone(),
            proof: Base64::encode_string(&blob),
        };
        let bound = match self.send(&t, &grant, bind) {
            Ok(HubReply::Bind(b)) => b,
            Ok(_) => return Err(t.fail(upstream_protocol())),
            Err(f) => return Err(self.failure(&t, &claim, f, Effect::Write)),
        };
        check_bound(
            &bound,
            &t.install,
            &key,
            &public_key,
            kind,
            &challenge.account,
        )
        .map_err(|e| t.fail(e))?;
        Ok(BindReply {
            key: key.fingerprint,
            kind,
            state: KeyState::Pending,
            confirm_url: bound.confirm_url,
            remote: t.remote.name,
            warnings: t.warnings,
        })
    }

    /// The configured key of `kind`, as the caller's agent holds it: `signing.key` for a person,
    /// `signing.agent_key` for an agent, and only that one. Unset is `E_SIGNING_KEY_MISSING`.
    /// `signing.sign` does not apply: it governs automatic signing, not an explicit bind.
    fn selected_key(&self, kind: KeyKind) -> Result<KeyInfo, RdyError> {
        let eff = effective(&self.project, Some(&self.user_file))?;
        let config = SigningConfig::from_effective(&self.project, &eff)?;
        let (setting, selector) = match kind {
            KeyKind::Person => (SIGNING_KEY, config.key),
            KeyKind::Agent => (SIGNING_AGENT_KEY, config.agent_key),
        };
        let selector = selector.ok_or_else(|| signing_key_missing(setting))?;
        resolve_key(setting, &selector, self.agent)
    }

    // -----------------------------------------------------------------------------------------
    // Retire and revoke
    // -----------------------------------------------------------------------------------------

    /// Asks the selected remote's hub to retire the key `key` (a public-key line or its
    /// fingerprint). No agent is contacted.
    pub fn retire(
        &self,
        key: &str,
        remote: Option<&str>,
        session: &Session,
    ) -> Result<KeyStateReply, RdyError> {
        let fingerprint = normalize_key(key)?;
        let claim = RequestClaim::from_session(session)?;
        let t = self.target(remote)?;
        let grant = self.access(&t, &claim)?;
        let key_id = self.find(&t, &grant, &claim, &fingerprint)?;
        let op = HubOperation::Retire {
            key_id: key_id.clone(),
        };
        match self.send(&t, &grant, op) {
            Ok(HubReply::Retire(k)) if is_changed(&k, &key_id, &fingerprint, KeyState::Retired) => {
                Ok(KeyStateReply {
                    key: fingerprint,
                    state: k.state,
                    remote: t.remote.name,
                    warnings: t.warnings,
                })
            }
            Ok(_) => Err(t.fail(upstream_protocol())),
            Err(f) => Err(self.failure(&t, &claim, f, Effect::Write)),
        }
    }

    /// Asks the selected remote's hub to revoke the key `key`, compromised from
    /// `compromised_since` (RFC3339, sent as canonical UTC `compromised_from`) or, when it is
    /// absent, from the hub's request time: the member is then left out. No agent is contacted.
    pub fn revoke(
        &self,
        key: &str,
        compromised_since: Option<&str>,
        remote: Option<&str>,
        session: &Session,
    ) -> Result<RevokeReply, RdyError> {
        let fingerprint = normalize_key(key)?;
        let compromised_from = compromised_since.map(parse_compromised_since).transpose()?;
        let claim = RequestClaim::from_session(session)?;
        let t = self.target(remote)?;
        let grant = self.access(&t, &claim)?;
        let key_id = self.find(&t, &grant, &claim, &fingerprint)?;
        let op = HubOperation::Revoke {
            key_id: key_id.clone(),
            compromised_from,
        };
        let k = match self.send(&t, &grant, op) {
            Ok(HubReply::Revoke(k)) if is_changed(&k, &key_id, &fingerprint, KeyState::Revoked) => {
                k
            }
            Ok(_) => return Err(t.fail(upstream_protocol())),
            Err(f) => return Err(self.failure(&t, &claim, f, Effect::Write)),
        };
        let (Some(revoked_at), Some(compromised_from)) = (k.revoked_at, k.compromised_from) else {
            return Err(t.fail(upstream_protocol()));
        };
        Ok(RevokeReply {
            key: fingerprint,
            state: k.state,
            revoked_at,
            compromised_from,
            remote: t.remote.name,
            warnings: t.warnings,
        })
    }

    /// The hub id of the key `fingerprint`: one FindKey. No key is `E_UPSTREAM_REFUSED`, as
    /// the hub's own not-found would be; a key with another fingerprint is
    /// `E_UPSTREAM_PROTOCOL`, as the transport makes a paged or longer list.
    fn find(
        &self,
        t: &Target,
        grant: &AccessGrant,
        claim: &RequestClaim,
        fingerprint: &str,
    ) -> Result<String, RdyError> {
        let op = HubOperation::FindKey {
            fingerprint: fingerprint.to_string(),
        };
        match self.send(t, grant, op) {
            Ok(HubReply::FindKey(None)) => Err(t.fail(upstream_refused())),
            Ok(HubReply::FindKey(Some(k)))
                if k.fingerprint == fingerprint && bounded_value(&k.id) =>
            {
                Ok(k.id)
            }
            Ok(_) => Err(t.fail(upstream_protocol())),
            Err(f) => Err(self.failure(t, claim, f, Effect::Read)),
        }
    }
}
