//! One rotating credential per hub install (n-XZDQD, n-7PQHM, D-B8N21): the sign-in card, the
//! device sign-in state machine, the only refresh of an access token, the `/me` handle repair
//! and sign-out.
//!
//! - **One file, one lock.** Every credential lives in the user config's sibling
//!   `credentials.yaml` (0600), never in `config.yaml`. Every read and write, `installs` and
//!   `state` included, holds the blocking lock of `credentials.yaml.lock` through
//!   [`LockedConfig`] and rereads the whole bounded file under it. One lock covers every install,
//!   because one file is replaced; records of other installs are kept as they are. A contender
//!   waits for the lock and then reads the winner's credential: there is no busy error, lease,
//!   lock retry, background thread or poller.
//! - **Discovery outside the lock.** When a call needs the network, it decides under the lock,
//!   releases it, reads the install's settings and discovers the OAuth endpoints, then takes the
//!   lock again, rereads and decides again. If another caller finished meanwhile, its result is
//!   returned and no consuming request is repeated. A discovery failure changes no credential.
//! - **Consuming requests keep the lock** through their durable transition: a device poll, a
//!   refresh, the revoke of a replaced sign-in and a device start. Before a device code or a
//!   refresh token is sent, the record is saved as `reauth_required`, so a crash or a lost reply
//!   can never send it again; a failed save sends nothing. Only sign-out's revoke and the
//!   snapshot-only `/me` repair run with the lock released.
//! - **Confirmation is a card.** A sign-in starts only after the person answered a card that
//!   names the install and the machine: its hash binds the answer to those values, and the
//!   existing relay table checks who relays it. No pending item, nonce file or stored answer is
//!   made for it.
//! - **Nothing secret leaves.** Replies carry no token, device code or complete verification
//!   link. [`AccessGrant`] holds the access token as a [`Secret`] and has no Serialize; the
//!   refresh token never leaves this module.
use crate::answer::relay::{RelayInput, relayed};
use crate::config_edit::LockedConfig;
use crate::error::{
    E_CONFIG, RdyError, upstream_auth, upstream_confirm_host, upstream_handle_changed,
    upstream_protocol, upstream_refused, upstream_signout_local, upstream_transport,
};
use crate::hub_config::{
    CREDENTIAL_FILE, Credential, CredentialFile, HubConfig, PendingSignIn, ReadyCredential,
    credential_file, encode_credentials, parse_credentials,
};
use crate::hub_http::route;
use crate::hub_types::{
    DevicePollReply, HubCall, HubClock, HubFailure, HubMetadata, HubOperation, HubReply,
    HubTransport, MAX_ACTOR_BYTES, MAX_CONFIG_BYTES, Origin, RefreshReply, RequestClaim, Secret,
    TokenPair, check_machine_name, deadline_ms, slow_down,
};
use crate::session::Session;
use rdy_model::{EvidenceKind, Hash};
use rdy_schema::Issue;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Cached access is returned only while it has more than this left, in milliseconds.
pub const ACCESS_MARGIN_MS: i64 = 60_000;

/// The message when no user config location is known.
pub const NO_USER_FILE_MESSAGE: &str = "the user config path is not available";
/// The fix of [`NO_USER_FILE_MESSAGE`].
pub const NO_USER_FILE_FIX: &str = "set XDG_CONFIG_HOME or HOME before signing in";

const HUMAN_PREFIX: &str = "human:";

/// `E_CONFIG` when no user config location is known.
pub fn no_user_file() -> RdyError {
    RdyError::new(E_CONFIG, NO_USER_FILE_MESSAGE, NO_USER_FILE_FIX)
}

/// The user config a production caller passes to [`HubCredentials::new`]: the location
/// `config_levels::user_file_from_env` found, else [`no_user_file`].
pub fn require_user_file(found: Option<PathBuf>) -> Result<PathBuf, RdyError> {
    found.ok_or_else(no_user_file)
}

// ---------------------------------------------------------------------------------------------
// Inputs and replies
// ---------------------------------------------------------------------------------------------

/// Who starts a sign-in: the actual caller's session, the bounded configured human actor the
/// relay table names as the person, and whether the call came over MCP (`implicit`), where the
/// session is the relay.
#[derive(Clone, Copy, Debug)]
pub struct AuthCaller<'a> {
    pub session: &'a Session,
    pub person: &'a str,
    pub implicit: bool,
}

/// The person's answer to the sign-in card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthAnswer {
    Confirm,
    Cancel,
}

/// A submitted answer to the sign-in card: the hash of the card the person saw, their answer,
/// and the relay of their words. The install and machine name are the ones passed with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthConfirmation {
    pub card_hash: String,
    pub answer: AuthAnswer,
    pub relay: RelayInput,
}

/// The state a reply reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthPhase {
    ConfirmationRequired,
    Cancelled,
    SignedOut,
    Pending,
    SignedIn,
    ReauthRequired,
}

/// A sign-in reply. Absent fields are null: the card and its hash appear only for
/// `confirmation_required`; the user code, verification link and next poll time only for
/// `pending`; the account and human actor only for `signed_in`. `expires_at_ms` is the pending
/// sign-in's end, or the access token's expiry when signed in. It holds no token, device code or
/// complete verification link.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AuthState {
    pub install: Origin,
    pub state: AuthPhase,
    pub card: Option<String>,
    pub card_hash: Option<String>,
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub expires_at_ms: Option<i64>,
    pub next_poll_at_ms: Option<i64>,
    pub account: Option<String>,
    pub human_actor: Option<String>,
    pub warnings: Vec<Issue>,
}

impl AuthState {
    fn bare(install: &Origin, state: AuthPhase) -> AuthState {
        AuthState {
            install: install.clone(),
            state,
            card: None,
            card_hash: None,
            user_code: None,
            verification_uri: None,
            expires_at_ms: None,
            next_poll_at_ms: None,
            account: None,
            human_actor: None,
            warnings: Vec::new(),
        }
    }

    fn pending(install: &Origin, p: &PendingSignIn) -> AuthState {
        AuthState {
            user_code: Some(p.user_code.clone()),
            verification_uri: Some(p.verification_uri.clone()),
            expires_at_ms: Some(p.expires_at_ms),
            next_poll_at_ms: Some(p.next_poll_at_ms),
            ..AuthState::bare(install, AuthPhase::Pending)
        }
    }

    fn signed_in(install: &Origin, r: &ReadyCredential) -> AuthState {
        AuthState {
            expires_at_ms: Some(r.expires_at_ms),
            account: Some(r.account.clone()),
            human_actor: Some(human_actor(&r.handle)),
            ..AuthState::bare(install, AuthPhase::SignedIn)
        }
    }
}

/// One saved sign-in: its install and state (`signed_out` for a pending sign-in whose time has
/// run out), with the account and human actor when signed in, and the pending sign-in's end or
/// the access token's expiry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstallState {
    pub install: Origin,
    pub state: AuthPhase,
    pub account: Option<String>,
    pub human_actor: Option<String>,
    pub expires_at_ms: Option<i64>,
}

/// Current access to one install: the access token, its expiry, the account, the cached human
/// actor and the caller's claim bound to that account. Debug is redacted; there is no
/// Serialize.
#[derive(Clone, Debug)]
pub struct AccessGrant {
    pub install: Origin,
    pub access_token: Secret,
    pub expires_at_ms: i64,
    pub account: String,
    pub human_actor: String,
    pub claim: RequestClaim,
}

/// The result of a `/me` handle repair: the account's current human actor, and
/// `W_UPSTREAM_HANDLE_CHANGED` when the handle changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HandleUpdate {
    pub human_actor: String,
    pub warnings: Vec<Issue>,
}

/// `human:<handle>`.
fn human_actor(handle: &str) -> String {
    format!("{HUMAN_PREFIX}{handle}")
}

/// The sign-in card: six lines, each ending LF.
fn render_card(install: &Origin, machine_name: &str) -> String {
    format!(
        "Sign in to a hub\n\
         Install: {install}\n\
         Machine: {machine_name}\n\
         This replaces this machine's saved sign-in for this install.\n\
         Confirm only if you asked to sign in to this install.\n\
         Answer: confirm or cancel\n"
    )
}

/// The card's hash: blake3 of its UTF-8 bytes, as 64 lowercase hex digits.
fn card_hash(card: &str) -> String {
    Hash::of(card.as_bytes()).to_string()
}

/// Whether `handle` makes a human actor the hub grammar takes: 1 or more visible ASCII
/// characters within the actor bound (the rule of `RequestClaim::bind_human`).
fn handle_ok(handle: &str) -> bool {
    !handle.is_empty()
        && HUMAN_PREFIX.len() + handle.len() <= MAX_ACTOR_BYTES
        && handle.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// Whether a ready record's access has more than [`ACCESS_MARGIN_MS`] left at `now`.
fn fresh(now: i64, r: &ReadyCredential) -> bool {
    now.checked_add(ACCESS_MARGIN_MS)
        .is_some_and(|t| t < r.expires_at_ms)
}

/// `claim` bound to the account `handle`, checked for sending.
fn bind(claim: &RequestClaim, handle: &str) -> Result<RequestClaim, RdyError> {
    let bound = claim.bind_human(handle)?;
    bound.validate_for_send()?;
    Ok(bound)
}

fn grant(install: &Origin, r: &ReadyCredential, claim: RequestClaim) -> AccessGrant {
    AccessGrant {
        install: install.clone(),
        access_token: r.access_token.clone(),
        expires_at_ms: r.expires_at_ms,
        account: r.account.clone(),
        human_actor: human_actor(&r.handle),
        claim,
    }
}

// ---------------------------------------------------------------------------------------------
// The locked credential file
// ---------------------------------------------------------------------------------------------

/// The credential file read under its lock, which is held until drop.
struct Held {
    cfg: LockedConfig,
    file: CredentialFile,
}

impl Held {
    fn get(&self, install: &Origin) -> Option<Credential> {
        self.file.get(install).cloned()
    }

    /// Sets (or with `None` removes) the record of `install` and replaces the file durably;
    /// every other record is kept. On an error the file may hold the old or, after a
    /// post-rename failure, the complete new bytes.
    fn put(&mut self, install: &Origin, credential: Option<Credential>) -> Result<(), RdyError> {
        let mut next = self.file.clone();
        next.set(install.clone(), credential);
        let bytes = encode_credentials(&next)?;
        self.cfg.replace(&bytes)?;
        self.file = next;
        Ok(())
    }
}

/// An operation's non-secret settings snapshot and the install's OAuth endpoints.
struct Discovery {
    ca_file: Option<PathBuf>,
    meta: HubMetadata,
}

// ---------------------------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------------------------

/// The credential service of the user config at `user_file`: the only renewer of its installs'
/// access, for every project and credential consumer of this user.
pub struct HubCredentials<'a> {
    project: Option<PathBuf>,
    user_file: PathBuf,
    transport: &'a dyn HubTransport,
    clock: &'a dyn HubClock,
}

impl<'a> HubCredentials<'a> {
    /// The service for the user config `user_file`, whose sibling `credentials.yaml` holds the
    /// credentials. `project`, when given, adds its `.rdy/local.yaml` install settings (pinned
    /// origins); the CA file comes from the user config. Neither settings file is ever written.
    pub fn new(
        project: Option<&Path>,
        user_file: &Path,
        transport: &'a dyn HubTransport,
        clock: &'a dyn HubClock,
    ) -> HubCredentials<'a> {
        HubCredentials {
            project: project.map(Path::to_path_buf),
            user_file: user_file.to_path_buf(),
            transport,
            clock,
        }
    }

    /// The config home and the credential file's path below it: the user config's directory
    /// is `<home>/<dir>`, and the file is `<dir>/credentials.yaml`.
    fn location(&self) -> Result<(PathBuf, PathBuf), RdyError> {
        let file = credential_file(&self.user_file);
        let dir = file.parent().ok_or_else(no_user_file)?;
        let name = dir.file_name().ok_or_else(no_user_file)?;
        let home = dir
            .parent()
            .filter(|h| !h.as_os_str().is_empty())
            .ok_or_else(no_user_file)?;
        Ok((home.to_path_buf(), Path::new(name).join(CREDENTIAL_FILE)))
    }

    /// Takes the credential lock (waiting for another holder) and reads the whole file.
    fn lock(&self) -> Result<Held, RdyError> {
        let (home, rel) = self.location()?;
        let cfg = LockedConfig::open(&home, &rel, true, MAX_CONFIG_BYTES)?;
        let file = parse_credentials(cfg.bytes())?;
        Ok(Held { cfg, file })
    }

    /// The user CA file of `install`, from the settings as they are now.
    fn ca_file(&self, install: &Origin) -> Result<Option<PathBuf>, RdyError> {
        let settings = HubConfig::load(self.project.as_deref(), Some(&self.user_file))?;
        Ok(settings.install(install).and_then(|i| i.ca_file.clone()))
    }

    /// One exchange with no claim: the OAuth and `/me` routes never carry one.
    fn exchange(
        &self,
        install: &Origin,
        ca_file: &Option<PathBuf>,
        destination: &str,
        bearer: Option<&Secret>,
        operation: HubOperation,
    ) -> Result<HubReply, HubFailure> {
        self.transport.exchange(HubCall {
            install: install.clone(),
            destination: destination.to_string(),
            ca_file: ca_file.clone(),
            claim: None,
            bearer: bearer.cloned(),
            operation,
        })
    }

    /// Reads the settings and discovers the OAuth endpoints. Called with no credential lock
    /// held; a failure changes no credential.
    fn discover(&self, install: &Origin) -> Result<Discovery, RdyError> {
        let ca_file = self.ca_file(install)?;
        let destination = route(install, &HubOperation::Metadata)?.ok_or_else(upstream_protocol)?;
        match self.exchange(
            install,
            &ca_file,
            &destination,
            None,
            HubOperation::Metadata,
        ) {
            Ok(HubReply::Metadata(meta)) => {
                meta.validate(install)?;
                Ok(Discovery { ca_file, meta })
            }
            Ok(_) => Err(upstream_protocol()),
            Err(f) => Err(f.into_error(install)),
        }
    }

    /// `GET /api/v1/me` with `access` and no claim.
    fn me(
        &self,
        install: &Origin,
        ca_file: &Option<PathBuf>,
        access: &Secret,
    ) -> Result<HubReply, HubFailure> {
        let destination = route(install, &HubOperation::Me)?.ok_or(HubFailure::Protocol)?;
        self.exchange(
            install,
            ca_file,
            &destination,
            Some(access),
            HubOperation::Me,
        )
    }

    // -----------------------------------------------------------------------------------------
    // Start
    // -----------------------------------------------------------------------------------------

    /// Starts a sign-in to `install` as `machine_name`.
    ///
    /// With no `confirmation`, the reply is `confirmation_required` with the card to show and
    /// its hash; nothing is read, sent or written. A submission must carry the hash of the card
    /// for this same install and machine name (else `E_UPSTREAM_CONFIRM_HOST`) and a relay the
    /// relay table accepts for the caller (its own refusal), all before any request or write.
    /// Cancel is `cancelled`. Confirm revokes this install's saved sign-in, then starts a new
    /// device sign-in: `pending`, with the user code and the code-free link.
    pub fn start(
        &self,
        install: &Origin,
        machine_name: &str,
        confirmation: Option<&AuthConfirmation>,
        caller: AuthCaller,
    ) -> Result<AuthState, RdyError> {
        check_machine_name(machine_name)?;
        let card = render_card(install, machine_name);
        let hash = card_hash(&card);
        let Some(confirmation) = confirmation else {
            return Ok(AuthState {
                card: Some(card),
                card_hash: Some(hash),
                ..AuthState::bare(install, AuthPhase::ConfirmationRequired)
            });
        };
        if confirmation.card_hash != hash {
            return Err(upstream_confirm_host());
        }
        let mut relay = confirmation.relay.clone();
        relay.kind = Some(EvidenceKind::Card);
        // Only the validation is used: the act's caller stays the actual session.
        relayed(caller.session, caller.person, &relay, caller.implicit).map_err(|r| r.error())?;
        match confirmation.answer {
            AuthAnswer::Cancel => Ok(AuthState::bare(install, AuthPhase::Cancelled)),
            AuthAnswer::Confirm => self.confirmed_start(install, machine_name),
        }
    }

    /// An unexpired pending sign-in of `machine_name`, which a start returns as it is.
    fn same_pending(&self, held: &Held, install: &Origin, machine_name: &str) -> Option<AuthState> {
        match held.get(install) {
            Some(Credential::Pending(p))
                if p.machine_name == machine_name && self.clock.now_ms() < p.expires_at_ms =>
            {
                Some(AuthState::pending(install, &p))
            }
            _ => None,
        }
    }

    fn confirmed_start(&self, install: &Origin, machine_name: &str) -> Result<AuthState, RdyError> {
        if let Some(s) = self.same_pending(&self.lock()?, install, machine_name) {
            return Ok(s);
        }
        let d = self.discover(install)?;
        let mut held = self.lock()?;
        if let Some(s) = self.same_pending(&held, install, machine_name) {
            return Ok(s);
        }
        if let Some(Credential::Ready(old)) = held.get(install) {
            // The old refresh token is revoked once, under the lock; only HTTP 200 lets its
            // record go. Any failure keeps the record as it is and starts nothing.
            let token = HubOperation::SignOut {
                token: old.refresh_token.clone(),
            };
            match self.exchange(
                install,
                &d.ca_file,
                &d.meta.revocation_endpoint,
                None,
                token,
            ) {
                Ok(HubReply::SignOut) => {}
                Ok(_) => return Err(upstream_protocol()),
                Err(f) => return Err(f.into_error(install)),
            }
        }
        // Invalidated before the start: a failed save stops here.
        held.put(install, Some(Credential::ReauthRequired))?;
        let start = HubOperation::DeviceStart {
            machine_name: machine_name.to_string(),
        };
        let endpoint = &d.meta.device_authorization_endpoint;
        let reply = match self.exchange(install, &d.ca_file, endpoint, None, start) {
            Ok(HubReply::DeviceStart(r)) => r,
            Ok(_) => return Err(upstream_protocol()),
            Err(f) => return Err(f.into_error(install)),
        };
        reply.validate(install)?;
        let now = self.clock.now_ms();
        let pending = PendingSignIn {
            machine_name: machine_name.to_string(),
            device_code: reply.device_code,
            user_code: reply.user_code,
            verification_uri: reply.verification_uri,
            expires_at_ms: deadline_ms(now, reply.expires_in)?,
            next_poll_at_ms: deadline_ms(now, reply.interval)?,
            interval_seconds: reply.interval,
        };
        held.put(install, Some(Credential::Pending(pending.clone())))?;
        Ok(AuthState::pending(install, &pending))
    }

    // -----------------------------------------------------------------------------------------
    // State
    // -----------------------------------------------------------------------------------------

    /// The sign-in state of `install`. A pending sign-in that is due is polled once; every
    /// other state is a local read. A pending sign-in whose time has run out is removed.
    /// `_claim` is the caller's: the OAuth and `/me` routes carry none.
    pub fn state(&self, install: &Origin, _claim: &RequestClaim) -> Result<AuthState, RdyError> {
        if let Some(s) = self.local_state(&mut self.lock()?, install)? {
            return Ok(s);
        }
        let d = self.discover(install)?;
        let mut held = self.lock()?;
        if let Some(s) = self.local_state(&mut held, install)? {
            return Ok(s);
        }
        self.poll(&mut held, install, &d)
    }

    /// The state when no request is needed; `None` when a pending sign-in is due for a poll.
    fn local_state(
        &self,
        held: &mut Held,
        install: &Origin,
    ) -> Result<Option<AuthState>, RdyError> {
        let now = self.clock.now_ms();
        Ok(Some(match held.get(install) {
            None => AuthState::bare(install, AuthPhase::SignedOut),
            Some(Credential::ReauthRequired) => AuthState::bare(install, AuthPhase::ReauthRequired),
            Some(Credential::Ready(r)) => AuthState::signed_in(install, &r),
            Some(Credential::Pending(p)) if now >= p.expires_at_ms => {
                held.put(install, None)?;
                AuthState::bare(install, AuthPhase::SignedOut)
            }
            Some(Credential::Pending(p)) if now < p.next_poll_at_ms => {
                AuthState::pending(install, &p)
            }
            Some(Credential::Pending(_)) => return Ok(None),
        }))
    }

    /// One device poll of the due pending sign-in, under the lock.
    fn poll(
        &self,
        held: &mut Held,
        install: &Origin,
        d: &Discovery,
    ) -> Result<AuthState, RdyError> {
        let Some(Credential::Pending(p)) = held.get(install) else {
            return Err(upstream_protocol());
        };
        let now = self.clock.now_ms();
        let mut scheduled = p.clone();
        scheduled.next_poll_at_ms = deadline_ms(now, p.interval_seconds)?;
        // A device code is sent only from a record that no longer holds it.
        held.put(install, Some(Credential::ReauthRequired))?;
        let poll = HubOperation::DevicePoll {
            device_code: p.device_code.clone(),
        };
        let reply = self.exchange(install, &d.ca_file, &d.meta.token_endpoint, None, poll);
        match reply {
            Ok(HubReply::DevicePoll(DevicePollReply::Pending)) => {
                held.put(install, Some(Credential::Pending(scheduled.clone())))?;
                Ok(AuthState::pending(install, &scheduled))
            }
            Ok(HubReply::DevicePoll(DevicePollReply::SlowDown)) => {
                let next =
                    slow_down(p.interval_seconds).and_then(|i| deadline_ms(now, i).map(|t| (i, t)));
                // Past the ceiling the record stays reauth_required.
                let (interval, at) = next.map_err(|_| upstream_protocol())?;
                scheduled.interval_seconds = interval;
                scheduled.next_poll_at_ms = at;
                held.put(install, Some(Credential::Pending(scheduled.clone())))?;
                Ok(AuthState::pending(install, &scheduled))
            }
            Ok(HubReply::DevicePoll(DevicePollReply::Expired | DevicePollReply::InvalidGrant)) => {
                held.put(install, None)?;
                Ok(AuthState::bare(install, AuthPhase::SignedOut))
            }
            Ok(HubReply::DevicePoll(DevicePollReply::Tokens(pair))) => {
                self.finish_sign_in(held, install, d, pair)
            }
            // Nothing was sent: the scheduled poll is restored.
            Err(HubFailure::Transport {
                may_have_sent: false,
            }) => {
                held.put(install, Some(Credential::Pending(scheduled)))?;
                Err(upstream_transport())
            }
            Err(HubFailure::Local(e)) => {
                held.put(install, Some(Credential::Pending(scheduled)))?;
                Err(e)
            }
            Err(HubFailure::Refused) => Err(upstream_refused()),
            // The grant may have been consumed: it is never sent again.
            Ok(_) | Err(_) => Err(upstream_auth(install.as_str())),
        }
    }

    /// The issued pair: `/me` once, under the lock, then the ready record. A failure leaves the
    /// record reauth_required.
    fn finish_sign_in(
        &self,
        held: &mut Held,
        install: &Origin,
        d: &Discovery,
        pair: TokenPair,
    ) -> Result<AuthState, RdyError> {
        let auth = || upstream_auth(install.as_str());
        pair.validate().map_err(|_| auth())?;
        let me = match self.me(install, &d.ca_file, &pair.access_token) {
            Ok(HubReply::Me(me)) if handle_ok(&me.handle) => me,
            _ => return Err(auth()),
        };
        let ready = ReadyCredential {
            expires_at_ms: deadline_ms(self.clock.now_ms(), pair.expires_in).map_err(|_| auth())?,
            access_token: pair.access_token,
            refresh_token: pair.refresh_token,
            account: me.account,
            handle: me.handle,
        };
        held.put(install, Some(Credential::Ready(ready.clone())))
            .map_err(|_| auth())?;
        Ok(AuthState::signed_in(install, &ready))
    }

    // -----------------------------------------------------------------------------------------
    // Access and handle repair
    // -----------------------------------------------------------------------------------------

    /// Current access to `install` with `claim` bound to its account: the cached access token
    /// while it has more than a minute left, else the one refresh, saved before it is returned.
    /// A contender that waited for the lock gets the winner's pair. Anything but a ready record
    /// is `E_UPSTREAM_AUTH`; so is every refresh failure, which leaves no reusable refresh token
    /// on disk. A human claim that cannot be bound is refused before any request.
    pub fn access(&self, install: &Origin, claim: &RequestClaim) -> Result<AccessGrant, RdyError> {
        let auth = || upstream_auth(install.as_str());
        {
            let held = self.lock()?;
            let Some(Credential::Ready(r)) = held.get(install) else {
                return Err(auth());
            };
            let bound = bind(claim, &r.handle)?;
            if fresh(self.clock.now_ms(), &r) {
                return Ok(grant(install, &r, bound));
            }
        }
        let d = self.discover(install)?;
        let mut held = self.lock()?;
        let Some(Credential::Ready(r)) = held.get(install) else {
            return Err(auth());
        };
        let bound = bind(claim, &r.handle)?;
        if fresh(self.clock.now_ms(), &r) {
            return Ok(grant(install, &r, bound));
        }
        // The refresh token is sent only from a record that no longer holds it.
        held.put(install, Some(Credential::ReauthRequired))?;
        let refresh = HubOperation::Refresh {
            refresh_token: r.refresh_token.clone(),
        };
        let pair = match self.exchange(install, &d.ca_file, &d.meta.token_endpoint, None, refresh) {
            Ok(HubReply::Refresh(RefreshReply::Tokens(pair))) => pair,
            _ => return Err(auth()),
        };
        pair.validate().map_err(|_| auth())?;
        let next = ReadyCredential {
            expires_at_ms: deadline_ms(self.clock.now_ms(), pair.expires_in).map_err(|_| auth())?,
            access_token: pair.access_token,
            refresh_token: pair.refresh_token,
            account: r.account,
            handle: r.handle,
        };
        held.put(install, Some(Credential::Ready(next.clone())))
            .map_err(|_| auth())?;
        Ok(grant(install, &next, bound))
    }

    /// Reads `/me` again after the hub refused an actor claim: with current access, a snapshot
    /// of the ready record, and the lock released for the request. The result applies only to
    /// that same record (else `E_UPSTREAM_AUTH`, nothing written). Another account clears the
    /// record to reauth_required (`E_UPSTREAM_AUTH`); a new handle is saved and reported once
    /// with `W_UPSTREAM_HANDLE_CHANGED`. A `/me` failure is returned as it is, never retried.
    pub fn refresh_handle(
        &self,
        install: &Origin,
        claim: &RequestClaim,
    ) -> Result<HandleUpdate, RdyError> {
        let auth = || upstream_auth(install.as_str());
        self.access(install, claim)?;
        let snapshot = match self.lock()?.get(install) {
            Some(Credential::Ready(r)) => r,
            _ => return Err(auth()),
        };
        let ca_file = self.ca_file(install)?;
        let me = match self.me(install, &ca_file, &snapshot.access_token) {
            Ok(HubReply::Me(me)) => me,
            Ok(_) => return Err(upstream_protocol()),
            Err(f) => return Err(f.into_error(install)),
        };
        let mut held = self.lock()?;
        if held.get(install) != Some(Credential::Ready(snapshot.clone())) {
            return Err(auth());
        }
        if me.account != snapshot.account {
            held.put(install, Some(Credential::ReauthRequired))?;
            return Err(auth());
        }
        if !handle_ok(&me.handle) {
            return Err(upstream_protocol());
        }
        let mut warnings = Vec::new();
        if me.handle != snapshot.handle {
            let next = ReadyCredential {
                handle: me.handle.clone(),
                ..snapshot.clone()
            };
            held.put(install, Some(Credential::Ready(next)))?;
            warnings.push(upstream_handle_changed(&snapshot.handle, &me.handle));
        }
        Ok(HandleUpdate {
            human_actor: human_actor(&me.handle),
            warnings,
        })
    }

    // -----------------------------------------------------------------------------------------
    // Sign-out and the saved sign-ins
    // -----------------------------------------------------------------------------------------

    /// Signs out of `install`: the record is removed durably first, then, with the lock
    /// released, a signed-in record's refresh token is revoked once. A revoke that fails or
    /// cannot be sent leaves `signed_out` with `W_UPSTREAM_SIGNOUT_LOCAL`; nothing is put back.
    /// A pending or reauth_required record is removed with no request; no record is a no-op.
    /// `_claim` is the caller's: the revocation route carries none.
    pub fn sign_out(&self, install: &Origin, _claim: &RequestClaim) -> Result<AuthState, RdyError> {
        let token = {
            let mut held = self.lock()?;
            match held.get(install) {
                None => return Ok(AuthState::bare(install, AuthPhase::SignedOut)),
                Some(Credential::Ready(r)) => {
                    held.put(install, None)?;
                    r.refresh_token
                }
                Some(_) => {
                    held.put(install, None)?;
                    return Ok(AuthState::bare(install, AuthPhase::SignedOut));
                }
            }
        };
        let revoked = self.discover(install).and_then(|d| {
            let op = HubOperation::SignOut { token };
            match self.exchange(install, &d.ca_file, &d.meta.revocation_endpoint, None, op) {
                Ok(HubReply::SignOut) => Ok(()),
                Ok(_) => Err(upstream_protocol()),
                Err(f) => Err(f.into_error(install)),
            }
        });
        let mut s = AuthState::bare(install, AuthPhase::SignedOut);
        if revoked.is_err() {
            s.warnings.push(upstream_signout_local());
        }
        Ok(s)
    }

    /// The saved sign-ins, sorted by install, read under the lock. Nothing is sent or written.
    pub fn installs(&self) -> Result<Vec<InstallState>, RdyError> {
        let held = self.lock()?;
        let now = self.clock.now_ms();
        Ok(held
            .file
            .origins()
            .filter_map(|o| held.file.get(o).map(|c| (o, c)))
            .map(|(install, c)| {
                let row = |state, expires_at_ms| InstallState {
                    install: install.clone(),
                    state,
                    account: None,
                    human_actor: None,
                    expires_at_ms,
                };
                match c {
                    Credential::Pending(p) if now >= p.expires_at_ms => {
                        row(AuthPhase::SignedOut, None)
                    }
                    Credential::Pending(p) => row(AuthPhase::Pending, Some(p.expires_at_ms)),
                    Credential::ReauthRequired => row(AuthPhase::ReauthRequired, None),
                    Credential::Ready(r) => InstallState {
                        account: Some(r.account.clone()),
                        human_actor: Some(human_actor(&r.handle)),
                        ..row(AuthPhase::SignedIn, Some(r.expires_at_ms))
                    },
                }
            })
            .collect())
    }
}
