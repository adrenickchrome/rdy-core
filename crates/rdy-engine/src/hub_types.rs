//! The hub client's closed contracts: canonical origins and the origin fence, the hub token form,
//! the install-bound request claim, bounded inputs and the typed operations a transport carries.
//!
//! Everything here is pure: no network call, credential file, store or clock is read. A secret is
//! held in [`Secret`], whose Debug is redacted and which implements neither Display nor
//! Serialize; no error built here formats its input.
use crate::error::{
    E_BAD_INPUT, E_LIMIT, RdyError, actor_unbound, upstream_actor_claim, upstream_auth,
    upstream_origin, upstream_protocol, upstream_refused, upstream_token, upstream_transport,
    upstream_uncertain,
};
use crate::session::Session;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

/// The length of a hub token, in bytes: prefix, kind letter, `_`, secret and checksum.
pub const TOKEN_BYTES: usize = 53;
/// The hub token prefixes: the hosted hub and a self-hosted install.
pub const TOKEN_PREFIXES: [&str; 2] = ["rdyh_", "rdyx_"];
/// The token alphabet, in digit order: position n has digit value n.
pub const TOKEN_ALPHABET: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
/// The secret characters of a hub token.
pub const TOKEN_SECRET_CHARS: usize = 40;
/// The checksum characters that end a hub token.
pub const TOKEN_CHECKSUM_CHARS: usize = 6;

/// The longest URL, in bytes.
pub const MAX_URL_BYTES: usize = 4096;
/// The longest device code, in bytes.
pub const MAX_DEVICE_CODE_BYTES: usize = 4096;
/// The longest account, challenge or key hub id, in bytes.
pub const MAX_ID_BYTES: usize = 4096;
/// The longest actor claim, in bytes.
pub const MAX_ACTOR_BYTES: usize = 512;
/// The longest run claim, in bytes.
pub const MAX_RUN_BYTES: usize = 256;
/// The longest machine name, in bytes.
pub const MAX_MACHINE_NAME_BYTES: usize = 256;
/// The longest machine name, in Unicode code points.
pub const MAX_MACHINE_NAME_CHARS: usize = 64;
/// The longest key label, in bytes.
pub const MAX_LABEL_BYTES: usize = 256;
/// The longest key label, in Unicode code points.
pub const MAX_LABEL_CHARS: usize = 64;
/// The longest JSON body, in bytes.
pub const MAX_JSON_BYTES: usize = 1_048_576;
/// The longest settings or credential file, in bytes.
pub const MAX_CONFIG_BYTES: usize = 1_048_576;
/// The longest CA bundle, in bytes.
pub const MAX_CA_BYTES: usize = 1_048_576;
/// The longest scalar value, in bytes.
pub const MAX_SCALAR_BYTES: usize = 16384;
/// The longest header block, in bytes.
pub const MAX_HEADER_BYTES: usize = 32768;
/// The longest timestamp, in bytes.
pub const MAX_TIMESTAMP_BYTES: usize = 64;
/// The longest key-binding statement, in bytes (SIG's bound).
pub const MAX_SIG_STATEMENT_BYTES: usize = rdy_model::signature::MAX_STATEMENT;
/// The longest public key blob, in bytes (SIG's bound).
pub const MAX_SIG_KEY_BYTES: usize = crate::signing_crypto::PUBLIC_KEY_MAX;
/// The longest SSHSIG proof blob, in bytes (SIG's bound).
pub const MAX_SIG_PROOF_BYTES: usize = rdy_model::signature::MAX_SIG;
/// The longest statement value, in bytes (SIG's bound).
pub const MAX_SIG_VALUE_BYTES: usize = rdy_model::signature::MAX_VALUE;
/// The longest signing namespace, in bytes (SIG's bound).
pub const MAX_SIG_NAMESPACE_BYTES: usize = rdy_model::signature::MAX_NAMESPACE;
/// The deepest nesting of a JSON or YAML document.
pub const MAX_DEPTH: usize = 32;
/// The most members and elements of a JSON or YAML document, in total.
pub const MAX_MEMBERS: usize = 4096;
/// The most header fields of one request or reply.
pub const MAX_HEADERS: usize = 64;
/// The most effective remotes.
pub const MAX_REMOTES: usize = 64;
/// The most hub installs in the settings or credential file.
pub const MAX_INSTALLS: usize = 32;
/// The most pinned origins of one install.
pub const MAX_ORIGINS: usize = 16;
/// The most certificates of a CA bundle.
pub const MAX_CA_CERTS: usize = 64;

/// The length of a device sign-in user code.
pub const USER_CODE_LEN: usize = 8;
/// The letters of a device sign-in user code.
pub const USER_CODE_ALPHABET: &str = "BCDFGHJKLMNPQRSTVWXZ";
/// The longest device sign-in life, in seconds.
pub const MAX_DEVICE_LIFE_SECONDS: u64 = 600;
/// The poll interval a device sign-in starts with, in seconds.
pub const INITIAL_POLL_SECONDS: u64 = 5;
/// What a `slow_down` reply adds to the poll interval, in seconds.
pub const SLOW_DOWN_SECONDS: u64 = 5;
/// The longest poll interval, in seconds: past it the pending attempt ends.
pub const MAX_POLL_SECONDS: u64 = 300;
/// The longest access token life, in seconds.
pub const MAX_ACCESS_LIFE_SECONDS: u64 = 3600;

/// The OAuth client id of rdy: a public client with no secret.
pub const CLIENT_ID: &str = "rdy";
/// The scope a device sign-in asks for.
pub const DEVICE_SCOPE: &str = "pull push upload change";
/// The grant type of a device poll.
pub const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// The grant type of a refresh.
pub const REFRESH_GRANT: &str = "refresh_token";
/// The namespace of a key binding's proof.
pub const KEYBIND_NAMESPACE: &str = crate::signing_crypto::NS_KEYBIND;
/// The header that carries the actor claim.
pub const ACTOR_HEADER: &str = "Rdy-Actor";
/// The header that carries the run claim. It is never sent without [`ACTOR_HEADER`].
pub const RUN_HEADER: &str = "Rdy-Run";

/// The fix of a hub `E_LIMIT`.
pub const HUB_LIMIT_FIX: &str = "reduce the input to the stated limit";
/// The message of an `E_BAD_INPUT` hub address.
pub const BAD_URL_MESSAGE: &str = "the hub address is not an HTTPS URL";
/// The fix of an `E_BAD_INPUT` hub address.
pub const BAD_URL_FIX: &str = "use an HTTPS address without credentials, a query or a fragment";
/// The message of an `E_BAD_INPUT` key label.
pub const BAD_LABEL_MESSAGE: &str =
    "label must contain 1 to 64 characters without edge whitespace or control characters";
/// The fix of an `E_BAD_INPUT` key label.
pub const BAD_LABEL_FIX: &str = "change label, or leave it out";
/// The message of an `E_BAD_INPUT` machine name.
pub const BAD_MACHINE_NAME_MESSAGE: &str =
    "machine_name must contain 1 to 64 characters without edge whitespace or control characters";
/// The fix of an `E_BAD_INPUT` machine name.
pub const BAD_MACHINE_NAME_FIX: &str = "change machine_name";
/// The message of an `E_BAD_INPUT` compromise time.
pub const BAD_TIME_MESSAGE: &str = "compromised_since is not an RFC3339 time";
/// The fix of an `E_BAD_INPUT` compromise time.
pub const BAD_TIME_FIX: &str = "use an RFC3339 time with a time zone";

/// The unit of a hub `E_LIMIT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitUnit {
    Bytes,
    Levels,
    Entries,
    Remotes,
    Installs,
    Origins,
    Certificates,
    Headers,
}

impl LimitUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            LimitUnit::Bytes => "bytes",
            LimitUnit::Levels => "levels",
            LimitUnit::Entries => "entries",
            LimitUnit::Remotes => "remotes",
            LimitUnit::Installs => "installs",
            LimitUnit::Origins => "origins",
            LimitUnit::Certificates => "certificates",
            LimitUnit::Headers => "headers",
        }
    }
}

/// `E_LIMIT` for a hub input over `limit` `unit`.
pub fn hub_limit(limit: usize, unit: LimitUnit) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("hub input exceeds {limit} {}", unit.as_str()),
        HUB_LIMIT_FIX,
    )
}

/// `Ok` when `n` is at most `limit`, else [`hub_limit`].
pub fn check_count(n: usize, limit: usize, unit: LimitUnit) -> Result<(), RdyError> {
    if n > limit {
        return Err(hub_limit(limit, unit));
    }
    Ok(())
}

fn bad_url() -> RdyError {
    RdyError::new(E_BAD_INPUT, BAD_URL_MESSAGE, BAD_URL_FIX)
}

fn bad_label() -> RdyError {
    RdyError::new(E_BAD_INPUT, BAD_LABEL_MESSAGE, BAD_LABEL_FIX)
}

fn bad_machine_name() -> RdyError {
    RdyError::new(E_BAD_INPUT, BAD_MACHINE_NAME_MESSAGE, BAD_MACHINE_NAME_FIX)
}

fn bad_time() -> RdyError {
    RdyError::new(E_BAD_INPUT, BAD_TIME_MESSAGE, BAD_TIME_FIX)
}

// ---------------------------------------------------------------------------------------------
// Token form
// ---------------------------------------------------------------------------------------------

/// The checksum characters of a CRC: the unsigned value in [`TOKEN_ALPHABET`], most significant
/// digit first, left-padded with `0`.
fn checksum_chars(crc: u32) -> [u8; TOKEN_CHECKSUM_CHARS] {
    let alphabet = TOKEN_ALPHABET.as_bytes();
    let mut out = [b'0'; TOKEN_CHECKSUM_CHARS];
    let mut value = u64::from(crc);
    for slot in out.iter_mut().rev() {
        *slot = alphabet[(value % 62) as usize];
        value /= 62;
    }
    out
}

fn in_token_alphabet(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

/// Checks the form of a hub token: `rdyh_` or `rdyx_`, one lower-case ASCII letter, `_`, 40
/// alphabet characters, then the six-character CRC-32/ISO-HDLC checksum of the first 47 bytes.
/// The kind letter is never tested against the letters in use. Any other text is
/// `E_UPSTREAM_TOKEN`, which never names the token.
pub fn token_form(token: &str) -> Result<(), RdyError> {
    let b = token.as_bytes();
    let body = TOKEN_BYTES - TOKEN_CHECKSUM_CHARS;
    let ok = b.len() == TOKEN_BYTES
        && TOKEN_PREFIXES.iter().any(|p| b.starts_with(p.as_bytes()))
        && b[5].is_ascii_lowercase()
        && b[6] == b'_'
        && b[7..].iter().all(|&c| in_token_alphabet(c))
        && checksum_chars(crc32fast::hash(&b[..body])) == b[body..];
    if ok { Ok(()) } else { Err(upstream_token()) }
}

// ---------------------------------------------------------------------------------------------
// Origins, remote URLs and the origin fence
// ---------------------------------------------------------------------------------------------

/// The raw checks every hub address passes before `url` normalizes it: no white space, control
/// character or backslash anywhere (the parser would drop or rewrite them), an explicit
/// `https://`, and a nonempty authority with no userinfo and no percent-encoded byte. Returns the
/// text after the authority.
fn raw_rest(input: &str) -> Option<&str> {
    if input
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return None;
    }
    let scheme = input.get(..8)?;
    if !scheme.eq_ignore_ascii_case("https://") {
        return None;
    }
    let after = &input[8..];
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..end];
    if authority.is_empty() || authority.contains(['@', '%']) {
        return None;
    }
    Some(&after[end..])
}

/// Parses a hub address that passed [`raw_rest`]: HTTPS with a host and no userinfo or fragment;
/// a query only when `query` allows it.
fn parse_https(input: &str, query: bool) -> Option<url::Url> {
    let url = url::Url::parse(input).ok()?;
    let ok = url.scheme() == "https"
        && url.host_str().is_some_and(|h| !h.is_empty())
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && (query || url.query().is_none());
    ok.then_some(url)
}

fn origin_of(url: &url::Url) -> Origin {
    Origin(url.origin().ascii_serialization())
}

/// A canonical HTTPS origin: `https://<host>[:<port>]` with the host in IDNA ASCII lower case,
/// the default port dropped and no trailing slash.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Origin(String);

impl Origin {
    /// Parses an install origin: absolute HTTPS, a host, no userinfo, query or fragment, and an
    /// empty or `/` path. Over 4096 bytes is `E_LIMIT`; anything else is `E_BAD_INPUT`.
    pub fn parse(input: &str) -> Result<Origin, RdyError> {
        if input.len() > MAX_URL_BYTES {
            return Err(hub_limit(MAX_URL_BYTES, LimitUnit::Bytes));
        }
        let rest = raw_rest(input).ok_or_else(bad_url)?;
        if !(rest.is_empty() || rest == "/") {
            return Err(bad_url());
        }
        let url = parse_https(input, false).ok_or_else(bad_url)?;
        let origin = origin_of(&url);
        check_count(origin.0.len(), MAX_URL_BYTES, LimitUnit::Bytes)?;
        Ok(origin)
    }

    /// The canonical text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The URL of this origin with `segments` appended, each encoded as exactly one path segment:
    /// every byte outside RFC 3986's unreserved set (`/`, `?`, `#` and `%` included) is
    /// percent-encoded, so no byte is lost or reread as a separator. An empty, `.` or `..`
    /// segment, which cannot stay one segment, and a segment with a control character, which a
    /// URL parser would drop, are `E_UPSTREAM_PROTOCOL`. A joined URL over 4096 bytes, origin,
    /// separators and encoding counted, is `E_LIMIT`; it is refused before it is built.
    pub fn join(&self, segments: &[&str]) -> Result<String, RdyError> {
        let grow = |total: usize, n: usize| {
            total
                .checked_add(n)
                .filter(|&t| t <= MAX_URL_BYTES)
                .ok_or_else(|| hub_limit(MAX_URL_BYTES, LimitUnit::Bytes))
        };
        // With no segment the path is `/`, as a URL parser writes it.
        let mut total = grow(self.0.len(), usize::from(segments.is_empty()))?;
        for s in segments {
            if matches!(*s, "" | "." | "..") || s.chars().any(char::is_control) {
                return Err(upstream_protocol());
            }
            total = grow(total, 1)?;
            for &b in s.as_bytes() {
                total = grow(total, if segment_byte_kept(b) { 1 } else { 3 })?;
            }
        }
        let mut out = String::with_capacity(total);
        out.push_str(&self.0);
        if segments.is_empty() {
            out.push('/');
        }
        for s in segments {
            out.push('/');
            for &b in s.as_bytes() {
                if segment_byte_kept(b) {
                    out.push(char::from(b));
                } else {
                    out.push('%');
                    out.push(char::from(HEX_UPPER[usize::from(b >> 4)]));
                    out.push(char::from(HEX_UPPER[usize::from(b & 0xf)]));
                }
            }
        }
        // The URL parser must read back exactly this text, with one path segment per segment.
        let url = url::Url::parse(&out).map_err(|_| upstream_protocol())?;
        let count = url.path_segments().map_or(0, Iterator::count);
        if url.as_str() != out || count != segments.len().max(1) {
            return Err(upstream_protocol());
        }
        Ok(out)
    }
}

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// A byte that stays as it is in a path segment: RFC 3986's unreserved set.
fn segment_byte_kept(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

impl fmt::Debug for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Origin {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Origin {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Origin, D::Error> {
        let text = String::deserialize(d)?;
        // The error never quotes the text.
        Origin::parse(&text).map_err(|_| serde::de::Error::custom(BAD_URL_MESSAGE))
    }
}

/// A canonical remote URL: an install origin plus an optional path, under the origin's rules,
/// with no trailing slash.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RemoteUrl {
    url: String,
    origin: Origin,
}

impl RemoteUrl {
    /// Parses a remote URL: absolute HTTPS, a host, no userinfo, query or fragment. Over 4096
    /// bytes is `E_LIMIT`; anything else is `E_BAD_INPUT`.
    pub fn parse(input: &str) -> Result<RemoteUrl, RdyError> {
        if input.len() > MAX_URL_BYTES {
            return Err(hub_limit(MAX_URL_BYTES, LimitUnit::Bytes));
        }
        raw_rest(input).ok_or_else(bad_url)?;
        let url = parse_https(input, false).ok_or_else(bad_url)?;
        let origin = origin_of(&url);
        let path = url.path().trim_end_matches('/');
        let text = format!("{}{path}", origin.0);
        check_count(text.len(), MAX_URL_BYTES, LimitUnit::Bytes)?;
        Ok(RemoteUrl { url: text, origin })
    }

    /// The canonical text.
    pub fn as_str(&self) -> &str {
        &self.url
    }

    /// The install origin.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }
}

impl fmt::Display for RemoteUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.url)
    }
}

/// The destinations one install's requests may reach: its own origin and its explicit pinned
/// origins. No DNS suffix, shared address or other install's list widens it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginFence {
    install: Origin,
    pinned: BTreeSet<Origin>,
}

impl OriginFence {
    /// The fence of `install` with its pinned origins: at most 16, else `E_LIMIT`.
    pub fn new(
        install: Origin,
        pinned: impl IntoIterator<Item = Origin>,
    ) -> Result<OriginFence, RdyError> {
        let mut set = BTreeSet::new();
        for (i, o) in pinned.into_iter().enumerate() {
            check_count(i + 1, MAX_ORIGINS, LimitUnit::Origins)?;
            set.insert(o);
        }
        Ok(OriginFence {
            install,
            pinned: set,
        })
    }

    /// The install.
    pub fn install(&self) -> &Origin {
        &self.install
    }

    /// The install's pinned origins.
    pub fn pinned(&self) -> &BTreeSet<Origin> {
        &self.pinned
    }

    /// The origin of `destination` when the fence trusts it: an HTTPS URL with no userinfo or
    /// fragment whose origin is the install or one of its pinned origins. Over 4096 bytes is
    /// `E_LIMIT`; any other destination is `E_UPSTREAM_ORIGIN`.
    pub fn check(&self, destination: &str) -> Result<Origin, RdyError> {
        if destination.len() > MAX_URL_BYTES {
            return Err(hub_limit(MAX_URL_BYTES, LimitUnit::Bytes));
        }
        let refused = || upstream_origin(self.install.as_str());
        raw_rest(destination).ok_or_else(refused)?;
        let url = parse_https(destination, true).ok_or_else(refused)?;
        let origin = origin_of(&url);
        if origin == self.install || self.pinned.contains(&origin) {
            Ok(origin)
        } else {
            Err(refused())
        }
    }
}

/// `Ok` when `uri` is an HTTPS URL on `install` with exactly the path `path` and no userinfo,
/// query or fragment; else `E_UPSTREAM_PROTOCOL`.
fn check_endpoint(
    install: &Origin,
    uri: &str,
    path: impl Fn(&str) -> bool,
) -> Result<(), RdyError> {
    if uri.len() > MAX_URL_BYTES || raw_rest(uri).is_none() {
        return Err(upstream_protocol());
    }
    let url = parse_https(uri, false).ok_or_else(upstream_protocol)?;
    if origin_of(&url) != *install || !path(url.path()) {
        return Err(upstream_protocol());
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------------------------

/// A token or device code. Debug is redacted; there is no Display or Serialize. Only
/// [`Secret::expose`] gives the bytes, for the credential encoder, the OAuth and bearer request
/// encoder and the explicit machine-token reply.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }

    /// The secret bytes. Every caller is a named encoder.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(redacted)")
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Secret, D::Error> {
        String::deserialize(d).map(Secret)
    }
}

// ---------------------------------------------------------------------------------------------
// Request claims
// ---------------------------------------------------------------------------------------------

const HUMAN_PREFIX: &str = "human:";
const ACTOR_PREFIXES: [&str; 3] = [HUMAN_PREFIX, "agent:", "ci:"];

/// Header-safe claim text, which reaches the hub byte for byte: a value the transport's header
/// type accepts unchanged (UTF-8 included), no control character (tab, DEL and C1 included) and
/// no space at either end, which HTTP would strip.
fn header_safe(text: &str) -> bool {
    !text.is_empty()
        && !text.chars().any(char::is_control)
        && !text.starts_with(' ')
        && !text.ends_with(' ')
        && ureq::http::HeaderValue::from_str(text).is_ok_and(|h| h.as_bytes() == text.as_bytes())
}

fn check_actor(actor: &str) -> Result<(), RdyError> {
    check_count(actor.len(), MAX_ACTOR_BYTES, LimitUnit::Bytes)?;
    let suffix = ACTOR_PREFIXES
        .iter()
        .find_map(|p| actor.strip_prefix(p))
        .ok_or_else(actor_unbound)?;
    if suffix.is_empty() || !header_safe(actor) {
        return Err(actor_unbound());
    }
    Ok(())
}

fn check_run(run: Option<&str>) -> Result<(), RdyError> {
    if let Some(run) = run {
        check_count(run.len(), MAX_RUN_BYTES, LimitUnit::Bytes)?;
        if !header_safe(run) {
            return Err(actor_unbound());
        }
    }
    Ok(())
}

/// The actor and run a hub request claims (`Rdy-Actor`, `Rdy-Run`). Agents and CI keep their
/// session's bytes; a human claim is derived from the install's account handle, and only for a
/// genuine human session. Building one never changes the local session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestClaim {
    pub actor: String,
    /// Absent when the session has no run.
    pub run: Option<String>,
    /// The session was a genuine human session (`SessionKind::Human`).
    human_session: bool,
    /// The account handle a human claim is bound to.
    handle: Option<String>,
}

impl RequestClaim {
    /// The claim of `session`: its actor and run, checked against the hub grammar (`human:`,
    /// `agent:` or `ci:` with a nonempty suffix, header-safe bytes). An agent session claiming
    /// `human:` is `E_ACTOR_UNBOUND`.
    pub fn from_session(session: &Session) -> Result<RequestClaim, RdyError> {
        check_actor(&session.actor)?;
        check_run(session.run.as_deref())?;
        let human_session = session.is_human();
        if !human_session && session.actor.starts_with(HUMAN_PREFIX) {
            return Err(actor_unbound());
        }
        Ok(RequestClaim {
            actor: session.actor.clone(),
            run: session.run.clone(),
            human_session,
            handle: None,
        })
    }

    /// The install-bound claim for the account handle `handle`: a genuine human session's
    /// `human:` actor becomes `human:<handle>`; any agent or CI actor stays byte for byte. A
    /// handle that is not 1 or more visible ASCII characters within the actor bound is
    /// `E_UPSTREAM_PROTOCOL`.
    pub fn bind_human(&self, handle: &str) -> Result<RequestClaim, RdyError> {
        if !(self.human_session && self.actor.starts_with(HUMAN_PREFIX)) {
            return Ok(self.clone());
        }
        let fits = HUMAN_PREFIX
            .len()
            .checked_add(handle.len())
            .is_some_and(|n| n <= MAX_ACTOR_BYTES);
        if !fits || handle.is_empty() || !handle.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return Err(upstream_protocol());
        }
        Ok(RequestClaim {
            actor: format!("{HUMAN_PREFIX}{handle}"),
            run: self.run.clone(),
            human_session: true,
            handle: Some(handle.to_string()),
        })
    }

    /// Checks the claim just before it is sent: the hub grammar again, and a `human:` actor only
    /// from a genuine human session bound to exactly that handle. Else `E_ACTOR_UNBOUND`.
    pub fn validate_for_send(&self) -> Result<(), RdyError> {
        check_actor(&self.actor)?;
        check_run(self.run.as_deref())?;
        if let Some(handle) = self.actor.strip_prefix(HUMAN_PREFIX)
            && !(self.human_session && self.handle.as_deref() == Some(handle))
        {
            return Err(actor_unbound());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Bounded inputs and reply values
// ---------------------------------------------------------------------------------------------

/// 1 to `max_chars` code points within `max_bytes` bytes, no Cc character and no Unicode white
/// space at either end. Never trimmed. The byte bound is checked before the text is walked.
fn bounded_text(text: &str, max_bytes: usize, max_chars: usize) -> bool {
    if text.is_empty() || text.len() > max_bytes {
        return false;
    }
    let mut count = 0usize;
    for c in text.chars() {
        if c.is_control() {
            return false;
        }
        count += 1;
    }
    let edge = |c: Option<char>| c.is_some_and(char::is_whitespace);
    count <= max_chars && !edge(text.chars().next()) && !edge(text.chars().next_back())
}

/// Checks a key label: 1-64 code points, at most 256 bytes, no Cc character and no white space
/// at either end. Else `E_BAD_INPUT`.
pub fn check_label(label: &str) -> Result<(), RdyError> {
    if bounded_text(label, MAX_LABEL_BYTES, MAX_LABEL_CHARS) {
        Ok(())
    } else {
        Err(bad_label())
    }
}

/// Checks a machine name under the label's rules. Else `E_BAD_INPUT`.
pub fn check_machine_name(name: &str) -> Result<(), RdyError> {
    if bounded_text(name, MAX_MACHINE_NAME_BYTES, MAX_MACHINE_NAME_CHARS) {
        Ok(())
    } else {
        Err(bad_machine_name())
    }
}

/// Checks a user code: exactly eight letters from [`USER_CODE_ALPHABET`]. Else
/// `E_UPSTREAM_PROTOCOL`.
pub fn check_user_code(code: &str) -> Result<(), RdyError> {
    let ok = code.len() == USER_CODE_LEN
        && code
            .bytes()
            .all(|b| USER_CODE_ALPHABET.as_bytes().contains(&b));
    if ok { Ok(()) } else { Err(upstream_protocol()) }
}

/// Checks a device sign-in life: 1-600 seconds. Else `E_UPSTREAM_PROTOCOL`.
pub fn check_device_expires_in(seconds: u64) -> Result<(), RdyError> {
    if (1..=MAX_DEVICE_LIFE_SECONDS).contains(&seconds) {
        Ok(())
    } else {
        Err(upstream_protocol())
    }
}

/// Checks the initial poll interval: exactly 5 seconds. Else `E_UPSTREAM_PROTOCOL`.
pub fn check_initial_interval(seconds: u64) -> Result<(), RdyError> {
    if seconds == INITIAL_POLL_SECONDS {
        Ok(())
    } else {
        Err(upstream_protocol())
    }
}

/// The poll interval after a `slow_down` reply: five seconds more. Past 300 seconds, or on
/// overflow, the pending attempt ends with `E_UPSTREAM_PROTOCOL`.
pub fn slow_down(interval: u64) -> Result<u64, RdyError> {
    interval
        .checked_add(SLOW_DOWN_SECONDS)
        .filter(|&n| n <= MAX_POLL_SECONDS)
        .ok_or_else(upstream_protocol)
}

/// Checks an access token life: 1-3600 seconds. Else `E_UPSTREAM_PROTOCOL`.
pub fn check_access_expires_in(seconds: u64) -> Result<(), RdyError> {
    if (1..=MAX_ACCESS_LIFE_SECONDS).contains(&seconds) {
        Ok(())
    } else {
        Err(upstream_protocol())
    }
}

/// The instant `seconds` after `now_ms`, in milliseconds since the epoch, with checked
/// arithmetic. A negative clock or an overflow is `E_UPSTREAM_PROTOCOL`.
pub fn deadline_ms(now_ms: i64, seconds: u64) -> Result<i64, RdyError> {
    if now_ms < 0 {
        return Err(upstream_protocol());
    }
    i64::try_from(seconds)
        .ok()
        .and_then(|s| s.checked_mul(1000))
        .and_then(|ms| now_ms.checked_add(ms))
        .ok_or_else(upstream_protocol)
}

/// The most fractional-second digits a time may have.
const MAX_FRACTION_DIGITS: usize = 9;

/// Parses an RFC3339 compromise time and returns it as canonical UTC RFC3339: `Z`, and only the
/// fractional digits the instant needs. At most nine fractional digits; a leap second, whose
/// instant cannot be kept, is refused. Over 64 bytes is `E_LIMIT`; anything else that is not an
/// RFC3339 time with a time zone is `E_BAD_INPUT`. No clock is read.
pub fn parse_compromised_since(input: &str) -> Result<String, RdyError> {
    check_count(input.len(), MAX_TIMESTAMP_BYTES, LimitUnit::Bytes)?;
    let b = input.as_bytes();
    // `time` takes any byte between date and time, any number of fractional digits, and maps a
    // leap second to the nanosecond before it: check those parts first.
    if !input.is_ascii() || b.len() < 20 || !matches!(b[10], b'T' | b't') || &b[17..19] == b"60" {
        return Err(bad_time());
    }
    if b[19] == b'.' {
        let digits = b[20..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits > MAX_FRACTION_DIGITS {
            return Err(bad_time());
        }
    }
    let parsed = time::OffsetDateTime::parse(input, &time::format_description::well_known::Rfc3339)
        .map_err(|_| bad_time())?;
    let utc = time::OffsetDateTime::from_unix_timestamp_nanos(parsed.unix_timestamp_nanos())
        .map_err(|_| bad_time())?;
    if !(0..=9999).contains(&utc.year()) {
        return Err(bad_time());
    }
    let mut out = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        utc.year(),
        u8::from(utc.month()),
        utc.day(),
        utc.hour(),
        utc.minute(),
        utc.second()
    );
    let nanos = utc.nanosecond();
    if nanos != 0 {
        let digits = format!("{nanos:09}");
        out.push('.');
        out.push_str(digits.trim_end_matches('0'));
    }
    out.push('Z');
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------------------------

/// Whose key a binding names: a person's (`signing.key`) or an agent's (`signing.agent_key`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyKind {
    Person,
    Agent,
}

impl KeyKind {
    /// The wire text: `person` or `agent`.
    pub fn as_str(self) -> &'static str {
        match self {
            KeyKind::Person => "person",
            KeyKind::Agent => "agent",
        }
    }

    /// The label the hub stores when a binding gives none.
    pub fn default_label(self) -> &'static str {
        match self {
            KeyKind::Person => "person key",
            KeyKind::Agent => "agent key",
        }
    }
}

/// A hub key's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyState {
    Pending,
    Active,
    Retired,
    Revoked,
}

/// The hub's key object: the twelve members of every reply that holds a key, under their wire
/// names. `id` is the hub id (`skey_…`) that key routes take. Every member must be present; the
/// five times may be null.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubKey {
    pub id: String,
    pub account: String,
    pub fingerprint: String,
    pub public_key: String,
    pub label: String,
    pub kind: KeyKind,
    pub state: KeyState,
    #[serde(deserialize_with = "nullable")]
    pub expires_at: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub bound_at: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub retired_at: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub revoked_at: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub compromised_from: Option<String>,
}

/// A member every reply holds whose value may be null. With `deserialize_with`, serde refuses an
/// absent member instead of reading it as null.
fn nullable<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}

/// A key-binding challenge (HTTP 201).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyChallenge {
    pub id: String,
    pub account: String,
    pub kind: KeyKind,
    pub label: String,
    pub namespace: String,
    pub nonce: String,
    pub expires_at: String,
}

/// A bind reply (HTTP 201): the pending key and the link a person opens to confirm it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindResult {
    pub key: HubKey,
    pub confirm_url: String,
}

// ---------------------------------------------------------------------------------------------
// Operations and replies
// ---------------------------------------------------------------------------------------------

/// One typed hub operation. There is no caller-supplied method, header or body.
#[derive(Clone, Debug)]
pub enum HubOperation {
    Metadata,
    DeviceStart {
        machine_name: String,
    },
    DevicePoll {
        device_code: Secret,
    },
    Refresh {
        refresh_token: Secret,
    },
    Me,
    SignOut {
        token: Secret,
    },
    BindChallenge {
        kind: KeyKind,
        label: Option<String>,
    },
    Bind {
        challenge: String,
        public_key: String,
        proof: String,
    },
    /// `fingerprint` is SIG's normalized `SHA256:` form.
    FindKey {
        fingerprint: String,
    },
    /// `key_id` is the hub id from FindKey.
    Retire {
        key_id: String,
    },
    Revoke {
        key_id: String,
        compromised_from: Option<String>,
    },
}

/// One exchange: the install, the destination, the optional user CA file, claim and bearer, and
/// the operation. Debug is redacted.
#[derive(Clone, Debug)]
pub struct HubCall {
    pub install: Origin,
    pub destination: String,
    pub ca_file: Option<PathBuf>,
    pub claim: Option<RequestClaim>,
    pub bearer: Option<Secret>,
    pub operation: HubOperation,
}

/// The OAuth endpoints an install's metadata names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubMetadata {
    pub issuer: String,
    pub device_authorization_endpoint: String,
    pub token_endpoint: String,
    pub revocation_endpoint: String,
}

impl HubMetadata {
    /// Checks that the issuer is `<install>/auth` and that each endpoint is an HTTPS URL on the
    /// install under `/auth/`. Else `E_UPSTREAM_PROTOCOL`.
    pub fn validate(&self, install: &Origin) -> Result<(), RdyError> {
        if self.issuer != format!("{}/auth", install.as_str()) {
            return Err(upstream_protocol());
        }
        for endpoint in [
            &self.device_authorization_endpoint,
            &self.token_endpoint,
            &self.revocation_endpoint,
        ] {
            check_endpoint(install, endpoint, |p| {
                p.strip_prefix("/auth/")
                    .is_some_and(|rest| !rest.is_empty())
            })?;
        }
        Ok(())
    }
}

/// The device page of `install`: `<install>/app/device`, as [`Origin::join`] writes it.
pub fn device_page(install: &Origin) -> Result<String, RdyError> {
    install.join(&["app", "device"])
}

/// Whether `uri` is the device page of `install`, byte for byte. A URL parser would also accept
/// a path that only normalizes to it (`/app/<code>/../device`, encoded dots, a trailing slash),
/// and the link is shown as it is, so the reply decoder, this type's check and the credential
/// codec all compare the text itself.
pub fn is_device_page(install: &Origin, uri: &str) -> bool {
    device_page(install).is_ok_and(|page| page == uri)
}

/// A device sign-in start (HTTP 200). There is no complete verification URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceStartReply {
    pub device_code: Secret,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

impl DeviceStartReply {
    /// Checks the reply against the settled profile: a 1-4096 byte device code with no control
    /// character, an eight-letter user code, exactly the install's device page
    /// ([`is_device_page`]), a 1-600 second life and a 5 second interval. Else
    /// `E_UPSTREAM_PROTOCOL`.
    pub fn validate(&self, install: &Origin) -> Result<(), RdyError> {
        let code = self.device_code.expose();
        if code.is_empty()
            || code.len() > MAX_DEVICE_CODE_BYTES
            || code.chars().any(char::is_control)
        {
            return Err(upstream_protocol());
        }
        check_user_code(&self.user_code)?;
        if !is_device_page(install, &self.verification_uri) {
            return Err(upstream_protocol());
        }
        check_device_expires_in(self.expires_in)?;
        check_initial_interval(self.interval)
    }
}

/// An issued token pair and the access token's life in seconds. The service computes the
/// absolute expiry from its injected clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenPair {
    pub access_token: Secret,
    pub refresh_token: Secret,
    pub expires_in: u64,
}

impl TokenPair {
    /// Checks both token forms (`E_UPSTREAM_TOKEN`) and the 1-3600 second life
    /// (`E_UPSTREAM_PROTOCOL`). No permission is inferred from a kind letter.
    pub fn validate(&self) -> Result<(), RdyError> {
        token_form(self.access_token.expose())?;
        token_form(self.refresh_token.expose())?;
        check_access_expires_in(self.expires_in)
    }
}

/// A device poll's outcome. `InvalidGrant` ends the pending sign-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DevicePollReply {
    Pending,
    SlowDown,
    Expired,
    InvalidGrant,
    Tokens(TokenPair),
}

/// A refresh's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshReply {
    Tokens(TokenPair),
    AuthRequired,
}

/// What the client keeps of `/me`: the account hub id and its current handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeReply {
    pub account: String,
    pub handle: String,
}

/// The closed reply of each operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HubReply {
    Metadata(HubMetadata),
    DeviceStart(DeviceStartReply),
    DevicePoll(DevicePollReply),
    Refresh(RefreshReply),
    Me(MeReply),
    SignOut,
    BindChallenge(KeyChallenge),
    Bind(BindResult),
    FindKey(Option<HubKey>),
    Retire(HubKey),
    Revoke(HubKey),
}

/// Why an exchange failed. `Local` carries a preflight error (length, origin, token, CA or
/// claim) unchanged.
#[derive(Clone, Debug)]
pub enum HubFailure {
    ActorClaim,
    AuthRequired,
    Refused,
    /// `may_have_sent` is true when the request may have reached the hub.
    Transport {
        may_have_sent: bool,
    },
    Protocol,
    Local(RdyError),
}

impl HubFailure {
    /// The error for a failure of a request to `install`, in the shared texts.
    pub fn into_error(self, install: &Origin) -> RdyError {
        match self {
            HubFailure::ActorClaim => upstream_actor_claim(),
            HubFailure::AuthRequired => upstream_auth(install.as_str()),
            HubFailure::Refused => upstream_refused(),
            HubFailure::Transport {
                may_have_sent: false,
            } => upstream_transport(),
            HubFailure::Transport {
                may_have_sent: true,
            } => upstream_uncertain(),
            HubFailure::Protocol => upstream_protocol(),
            HubFailure::Local(e) => e,
        }
    }
}

impl From<RdyError> for HubFailure {
    fn from(e: RdyError) -> HubFailure {
        HubFailure::Local(e)
    }
}

/// Sends one typed hub operation. A transport never retries.
pub trait HubTransport: Send + Sync {
    fn exchange(&self, call: HubCall) -> Result<HubReply, HubFailure>;
}

/// The clock of the hub services; tests inject a fake one.
pub trait HubClock: Send + Sync {
    /// Milliseconds since the epoch.
    fn now_ms(&self) -> i64;
}

/// The production clock: [`crate::now_ms`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemHubClock;

impl HubClock for SystemHubClock {
    fn now_ms(&self) -> i64 {
        crate::session::now_ms()
    }
}
