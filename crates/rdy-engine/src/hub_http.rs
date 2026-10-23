//! The hub's HTTPS transport: one bounded exchange per typed [`HubOperation`].
//!
//! - **Closed requests.** Each operation has one method, destination rule, header set and body
//!   encoding, all fixed here. No caller supplies a method, header, query or body. OAuth
//!   endpoints come only from the install's metadata; the fixed routes come from [`route`].
//! - **Preflight before any connection.** In order: the install, the inputs' bounds and the
//!   request claim, the destination, the bearer or OAuth token form, the encoded body, then the
//!   CA bundle. A failure there is [`HubFailure::Local`] and nothing is sent.
//! - **One request, never retried.** Each exchange builds a fresh agent with no idle
//!   connection, no proxy, no cookie, no compression and no redirect. A failure before the
//!   connection and its TLS handshake complete is `Transport {may_have_sent: false}`; any later
//!   loss is `Transport {may_have_sent: true}`. A redirect is a contract mismatch.
//! - **Bounded replies.** At most 64 header fields and 32768 header bytes, a body of at most
//!   1048576 bytes, and JSON of depth 32 with 4096 members and elements in total, no duplicate
//!   key and valid UTF-8. Application objects deny unknown members; OAuth replies ignore their
//!   extension members within the same bounds.
//! - **Nothing raw escapes.** No URL, request, reply, library error or hub message is logged or
//!   returned: failures are the closed [`HubFailure`] values. The engine's `log` features
//!   compile every dependency record out.
//! - **Uploads share the sender.** [`HttpHubTransport::upload`] is the typed upload client's
//!   seam, not an arbitrary-URL tool: its destination may be the install or one of the install's
//!   pinned origins, checked before any authorization is prepared. It shares the CA loading,
//!   hostname check, token and claim checks, closed headers, agent settings, connection marking
//!   and failure suppression above; its body streams with its exact length and its reply cap is
//!   the caller's, never over 16 MiB. Sign-in and key requests stay on the install alone.
use crate::error::{
    E_BAD_INPUT, E_UPSTREAM_ORIGIN, RdyError, actor_unbound, signing_selector, upload_body,
    upstream_auth, upstream_ca, upstream_key_proof, upstream_origin, upstream_protocol,
    upstream_token,
};
use crate::hub_types::{
    BindResult, CLIENT_ID, DEVICE_CODE_GRANT, DEVICE_SCOPE, DevicePollReply, DeviceStartReply,
    HubCall, HubFailure, HubKey, HubMetadata, HubOperation, HubReply, HubTransport,
    KEYBIND_NAMESPACE, KeyChallenge, KeyKind, KeyState, LimitUnit, MAX_CA_BYTES, MAX_CA_CERTS,
    MAX_DEPTH, MAX_DEVICE_CODE_BYTES, MAX_HEADER_BYTES, MAX_HEADERS, MAX_ID_BYTES, MAX_JSON_BYTES,
    MAX_MEMBERS, MAX_SCALAR_BYTES, MAX_SIG_KEY_BYTES, MAX_SIG_PROOF_BYTES, MAX_TIMESTAMP_BYTES,
    MAX_URL_BYTES, MeReply, Origin, OriginFence, REFRESH_GRANT, RefreshReply, RequestClaim, Secret,
    TokenPair, check_count, check_label, check_machine_name, hub_limit, is_device_page,
    parse_compromised_since, token_form,
};
use crate::identity;
use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use ssh_encoding::base64::{Base64, Encoding};
use std::cell::Cell;
use std::collections::BTreeSet;
use std::fmt;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use ureq::SendBody;
use ureq::http::{self, HeaderName, HeaderValue, Method};
use ureq::tls::{Certificate, PemItem, RootCerts, TlsConfig, TlsProvider};
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{ConnectionDetails, Connector, DefaultConnector, Transport};

/// The longest wait for a connection and its TLS handshake.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest whole exchange.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest upload body, in bytes.
pub const MAX_UPLOAD_BODY_BYTES: u64 = 2_147_483_648;
/// The highest reply cap an upload caller may choose, in bytes.
pub const MAX_UPLOAD_REPLY_BYTES: usize = 16_777_216;
/// The copying buffer an upload body streams through, in bytes.
pub const UPLOAD_BUFFER_BYTES: usize = 65_536;
/// The slowest average rate an upload must keep, in bytes per second: an upload's whole exchange
/// may take [`REQUEST_TIMEOUT`] plus one second for each this many body and reply bytes.
pub const UPLOAD_MIN_RATE: u64 = 131_072;
/// The longest upload media type, in bytes.
const MAX_MEDIA_TYPE_BYTES: usize = 256;
/// The JSON bounds of every typed exchange reply: HA2a's settled constants.
const EXCHANGE_JSON: JsonLimits = JsonLimits {
    bytes: MAX_JSON_BYTES,
    depth: MAX_DEPTH,
    values: MAX_MEMBERS,
    scalar_bytes: MAX_SCALAR_BYTES,
};
/// The metadata route (RFC 8414, issuer `<install>/auth`).
const METADATA_ROUTE: [&str; 3] = [".well-known", "oauth-authorization-server", "auth"];
/// The OAuth endpoints' path prefix.
const AUTH_PREFIX: &str = "/auth/";
/// The longest base64 text of an SSHSIG proof of at most [`MAX_SIG_PROOF_BYTES`] bytes.
const MAX_PROOF_TEXT: usize = MAX_SIG_PROOF_BYTES.div_ceil(3) * 4;
/// The length of a challenge nonce: 32 bytes in unpadded base64url.
const NONCE_CHARS: usize = 43;
/// The start of SIG's key fingerprints.
const FINGERPRINT_PREFIX: &str = "SHA256:";
/// The hub id prefixes of an account, a key and a key-binding challenge.
const ACCOUNT_PREFIX: &str = "acc_";
const KEY_PREFIX: &str = "skey_";
const CHALLENGE_PREFIX: &str = "kch_";

const FORM: &str = "application/x-www-form-urlencoded";
const JSON: &str = "application/json";

/// The method of an upload request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadMethod {
    Get,
    Post,
}

/// One upload request for [`HttpHubTransport::upload`].
///
/// - `destination`: an HTTPS URL on the install or one of its pinned origins.
/// - `content_type`: the media type of a POST body; for a GET, which has no body, the media type
///   it accepts. A POST always accepts `application/json`.
/// - `body` and `body_len`: a POST's body and its exact length, at most
///   [`MAX_UPLOAD_BODY_BYTES`]; no body is a length of 0. A GET has neither.
/// - `reply_limit`: the caller's reply cap, at most [`MAX_UPLOAD_REPLY_BYTES`].
pub struct UploadHttpRequest<'a> {
    pub method: UploadMethod,
    pub destination: &'a str,
    pub content_type: &'a str,
    pub body: Option<&'a mut dyn Read>,
    pub body_len: u64,
    pub reply_limit: usize,
}

impl fmt::Debug for UploadHttpRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The destination may carry an upload id: it is not printed.
        f.debug_struct("UploadHttpRequest")
            .field("method", &self.method)
            .field("content_type", &self.content_type)
            .field("body", &self.body.is_some())
            .field("body_len", &self.body_len)
            .field("reply_limit", &self.reply_limit)
            .finish_non_exhaustive()
    }
}

/// The bounds of one JSON document: its bytes, nesting depth, members and elements in total,
/// and the length of each string and key. The caller owns them; nothing in a body changes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonLimits {
    pub bytes: usize,
    pub depth: usize,
    pub values: usize,
    pub scalar_bytes: usize,
}

/// The hub transport over HTTPS (Rustls), fenced to one install.
pub struct HttpHubTransport {
    fence: OriginFence,
    /// The platform's trusted roots, read once when the transport is built.
    native: Arc<Vec<Certificate<'static>>>,
}

impl fmt::Debug for HttpHubTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpHubTransport")
            .field("install", self.fence.install())
            .field("pinned", self.fence.pinned())
            .field("native_roots", &self.native.len())
            .finish()
    }
}

impl HttpHubTransport {
    /// A transport for the install of `fence`. It reads the platform's trusted roots; a store
    /// that cannot be read adds no root, and a request then needs the user CA.
    pub fn new(fence: OriginFence) -> Result<Self, RdyError> {
        let found = rustls_native_certs::load_native_certs();
        let native = found
            .certs
            .iter()
            .map(|c| Certificate::from_der(c.as_ref()).to_owned())
            .collect();
        Ok(HttpHubTransport {
            fence,
            native: Arc::new(native),
        })
    }

    /// The origin fence of this transport's install.
    pub fn fence(&self) -> &OriginFence {
        &self.fence
    }
}

/// The fixed URL of `op` on `install`: the metadata route and the account and key routes.
/// `None` for an OAuth endpoint, which only the install's metadata names.
pub fn route(install: &Origin, op: &HubOperation) -> Result<Option<String>, RdyError> {
    const KEYS: [&str; 4] = ["api", "v1", "me", "keys"];
    let key_route =
        |key_id: &str, act: &str| install.join(&["api", "v1", "me", "keys", key_id, act]);
    Ok(Some(match op {
        HubOperation::Metadata => install.join(&METADATA_ROUTE)?,
        HubOperation::Me => install.join(&["api", "v1", "me"])?,
        HubOperation::BindChallenge { .. } => {
            install.join(&["api", "v1", "me", "keys", "challenges"])?
        }
        HubOperation::Bind { .. } => install.join(&KEYS)?,
        HubOperation::FindKey { fingerprint } => {
            let base = install.join(&KEYS)?;
            check_count(fingerprint.len(), MAX_ID_BYTES, LimitUnit::Bytes)?;
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("fingerprint", fingerprint)
                .finish();
            let url = format!("{base}?{query}");
            check_count(url.len(), MAX_URL_BYTES, LimitUnit::Bytes)?;
            url
        }
        HubOperation::Retire { key_id } => key_route(key_id, "retire")?,
        HubOperation::Revoke { key_id, .. } => key_route(key_id, "revoke")?,
        HubOperation::DeviceStart { .. }
        | HubOperation::DevicePoll { .. }
        | HubOperation::Refresh { .. }
        | HubOperation::SignOut { .. } => return Ok(None),
    }))
}

/// Which rules an operation's request follows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    /// The metadata route: no bearer, no claim.
    Metadata,
    /// An OAuth endpoint from metadata: a form body, no bearer, no claim.
    OAuth,
    /// `GET /api/v1/me`: bearer, no claim.
    Me,
    /// A key route: bearer and the caller's claim.
    Key,
}

fn class(op: &HubOperation) -> Class {
    match op {
        HubOperation::Metadata => Class::Metadata,
        HubOperation::DeviceStart { .. }
        | HubOperation::DevicePoll { .. }
        | HubOperation::Refresh { .. }
        | HubOperation::SignOut { .. } => Class::OAuth,
        HubOperation::Me => Class::Me,
        HubOperation::BindChallenge { .. }
        | HubOperation::Bind { .. }
        | HubOperation::FindKey { .. }
        | HubOperation::Retire { .. }
        | HubOperation::Revoke { .. } => Class::Key,
    }
}

/// A request ready to send: everything checked and encoded.
struct Prepared<'a> {
    method: Method,
    url: String,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Payload<'a>,
    /// The longest reply body read.
    reply_limit: usize,
    /// The longest whole exchange.
    timeout: Duration,
}

/// A request body.
enum Payload<'a> {
    /// None: a GET.
    Empty,
    /// Encoded bytes; ureq sends their length (`content-length: 0` when empty).
    Bytes(Vec<u8>),
    /// A body streamed with its exact length, which the request's headers already hold.
    Stream(Exact<'a>),
}

impl HubTransport for HttpHubTransport {
    fn exchange(&self, call: HubCall) -> Result<HubReply, HubFailure> {
        let prepared = self.prepare(&call)?;
        let roots = self.roots(call.ca_file.as_deref())?;
        let (status, body) = send(prepared, roots)?;
        decode(&call, status, &body)
    }
}

impl HttpHubTransport {
    /// Sends one upload request and returns its status and its bounded reply body, which the
    /// typed caller decodes. In order, before any connection: the install must be the fence's,
    /// the destination must pass the fence (the install or one of its pinned origins), then the
    /// request's bounds, the claim, the bearer's form and the closed header set, then the CA
    /// bundle. A failure there is [`HubFailure::Local`] and nothing is sent.
    ///
    /// A body that ends before `body_len`, runs past it or fails to read is `E_IO`: the request
    /// it began is never completed, because the last bytes are held until the body's end is
    /// seen. A redirect is never followed. Nothing is retried.
    pub fn upload(
        &self,
        install: &Origin,
        ca_file: Option<&Path>,
        claim: &RequestClaim,
        bearer: &Secret,
        request: UploadHttpRequest<'_>,
    ) -> Result<(u16, Vec<u8>), HubFailure> {
        let prepared = self.prepare_upload(install, claim, bearer, request)?;
        let roots = self.roots(ca_file)?;
        send(prepared, roots)
    }

    /// Every check of an upload before a connection, in the contract's order.
    fn prepare_upload<'a>(
        &self,
        install: &Origin,
        claim: &RequestClaim,
        bearer: &Secret,
        request: UploadHttpRequest<'a>,
    ) -> Result<Prepared<'a>, RdyError> {
        let fenced = self.fence.install();
        // 1. The install, then the destination: the install or its own pin. The fence's
        // refusal keeps its pin-aware fix and names neither the destination nor a token.
        if install != fenced {
            return Err(upstream_origin(fenced.as_str()));
        }
        self.fence.check(request.destination)?;
        // 2. The request's bounds, before anything is read or allocated.
        let UploadHttpRequest {
            method,
            destination,
            content_type,
            body,
            body_len,
            reply_limit,
        } = request;
        if body_len > MAX_UPLOAD_BODY_BYTES {
            let cap = usize::try_from(MAX_UPLOAD_BODY_BYTES).unwrap_or(usize::MAX);
            return Err(hub_limit(cap, LimitUnit::Bytes));
        }
        check_count(reply_limit, MAX_UPLOAD_REPLY_BYTES, LimitUnit::Bytes)?;
        let media = media_type(content_type)?;
        let (stream, empty) = match (method, body) {
            (UploadMethod::Get, None) if body_len == 0 => (None, None),
            (UploadMethod::Post, None) if body_len == 0 => (None, None),
            (UploadMethod::Post, Some(reader)) if body_len == 0 => (None, Some(reader)),
            (UploadMethod::Post, Some(reader)) => (Some(reader), None),
            _ => return Err(upload_body()),
        };
        // 3. The claim and the bearer's form.
        claim.validate_for_send()?;
        token_form(bearer.expose())?;
        // 4. The closed header set.
        let json = HeaderValue::from_static(JSON);
        let mut headers = Vec::new();
        let method = match method {
            UploadMethod::Get => {
                headers.push((http::header::ACCEPT, media));
                Method::GET
            }
            UploadMethod::Post => {
                headers.push((http::header::ACCEPT, json));
                headers.push((http::header::CONTENT_TYPE, media));
                Method::POST
            }
        };
        if stream.is_some() {
            headers.push((http::header::CONTENT_LENGTH, HeaderValue::from(body_len)));
        }
        push_bearer(&mut headers, bearer)?;
        push_claim(&mut headers, claim)?;
        check_headers(&headers)?;
        // 5. The body. An empty one must end at once: that is seen before connecting.
        if let Some(reader) = empty
            && !at_end(reader)
        {
            return Err(upload_body());
        }
        let payload = match (method == Method::GET, stream) {
            (true, _) => Payload::Empty,
            (false, None) => Payload::Bytes(Vec::new()),
            (false, Some(reader)) => Payload::Stream(Exact::new(reader, body_len)),
        };
        // A slow link gets time in proportion to the bytes it must carry.
        let reply_bytes = u64::try_from(reply_limit).unwrap_or(u64::MAX);
        let carried = body_len.saturating_add(reply_bytes);
        let timeout =
            REQUEST_TIMEOUT.saturating_add(Duration::from_secs(carried.div_ceil(UPLOAD_MIN_RATE)));
        Ok(Prepared {
            method,
            url: destination.to_string(),
            headers,
            body: payload,
            reply_limit,
            timeout,
        })
    }
}

/// An upload media type as one header value: 1-256 bytes, no control character and no space at
/// either end. Else `E_BAD_INPUT`.
fn media_type(text: &str) -> Result<HeaderValue, RdyError> {
    let ok = !text.is_empty()
        && text.len() <= MAX_MEDIA_TYPE_BYTES
        && !text.chars().any(char::is_control)
        && !text.starts_with(' ')
        && !text.ends_with(' ');
    ok.then(|| HeaderValue::from_str(text).ok())
        .flatten()
        .filter(|v| v.as_bytes() == text.as_bytes())
        .ok_or_else(|| {
            RdyError::new(
                E_BAD_INPUT,
                "the upload media type is not one header value",
                "use a media type such as application/octet-stream",
            )
        })
}

impl HttpHubTransport {
    /// Every check and encoding before a connection, in the contract's order.
    fn prepare(&self, call: &HubCall) -> Result<Prepared<'static>, RdyError> {
        let install = self.fence.install();
        if call.install != *install {
            return Err(upstream_origin(install.as_str()));
        }
        let class = class(&call.operation);
        // 1. Input bounds and the request claim.
        check_inputs(&call.operation)?;
        let claim = match class {
            Class::Key => {
                let claim = call.claim.as_ref().ok_or_else(actor_unbound)?;
                claim.validate_for_send()?;
                Some(claim)
            }
            // Bootstrap routes carry the account alone: a claim is never sent there.
            Class::Metadata | Class::OAuth | Class::Me => None,
        };
        // 2. The destination: fenced, on the install itself, and the operation's own route.
        // Sign-in and key requests never use a pin, so every origin refusal has the
        // install-only fix.
        let origin = self.fence.check(&call.destination).map_err(|e| {
            if e.code == E_UPSTREAM_ORIGIN {
                upstream_origin(install.as_str())
            } else {
                e
            }
        })?;
        if origin != *install {
            return Err(upstream_origin(install.as_str()));
        }
        match route(install, &call.operation)? {
            Some(expected) if expected == call.destination => {}
            Some(_) => return Err(upstream_protocol()),
            None if oauth_endpoint(install, &call.destination) => {}
            None => return Err(upstream_protocol()),
        }
        // 3. The bearer and OAuth token forms.
        let bearer = match class {
            Class::Me | Class::Key => {
                let bearer = call
                    .bearer
                    .as_ref()
                    .ok_or_else(|| upstream_auth(install.as_str()))?;
                token_form(bearer.expose())?;
                Some(bearer)
            }
            Class::Metadata | Class::OAuth => None,
        };
        match &call.operation {
            HubOperation::Refresh { refresh_token } => token_form(refresh_token.expose())?,
            HubOperation::SignOut { token } => token_form(token.expose())?,
            _ => {}
        }
        // 4. The bounded body and the closed header set.
        let (method, body) = encode(&call.operation)?;
        let mut headers = vec![(http::header::ACCEPT, HeaderValue::from_static(JSON))];
        if let Some((ctype, _)) = &body {
            headers.push((http::header::CONTENT_TYPE, HeaderValue::from_static(ctype)));
        }
        if let Some(bearer) = bearer {
            push_bearer(&mut headers, bearer)?;
        }
        if let Some(claim) = claim {
            push_claim(&mut headers, claim)?;
        }
        check_headers(&headers)?;
        let body = match (method == Method::GET, body) {
            (true, _) => Payload::Empty,
            (false, body) => Payload::Bytes(body.map(|(_, b)| b).unwrap_or_default()),
        };
        Ok(Prepared {
            method,
            url: call.destination.clone(),
            headers,
            body,
            reply_limit: MAX_JSON_BYTES,
            timeout: REQUEST_TIMEOUT,
        })
    }

    /// The trusted roots of one exchange: the platform's and the user CA bundle's.
    fn roots(&self, ca_file: Option<&Path>) -> Result<Vec<Certificate<'static>>, RdyError> {
        let mut roots = self.native.as_ref().clone();
        if let Some(path) = ca_file {
            roots.extend(read_ca(path)?);
        }
        Ok(roots)
    }
}

/// Adds the bearer's `Authorization`, marked sensitive.
fn push_bearer(
    headers: &mut Vec<(HeaderName, HeaderValue)>,
    bearer: &Secret,
) -> Result<(), RdyError> {
    let mut value = HeaderValue::try_from(format!("Bearer {}", bearer.expose()))
        .map_err(|_| upstream_token())?;
    value.set_sensitive(true);
    headers.push((http::header::AUTHORIZATION, value));
    Ok(())
}

/// At most 64 header fields in at most 32768 bytes.
fn check_headers(headers: &[(HeaderName, HeaderValue)]) -> Result<(), RdyError> {
    check_count(headers.len(), MAX_HEADERS, LimitUnit::Headers)?;
    let mut size = 0usize;
    for (n, v) in headers {
        size = size
            .checked_add(n.as_str().len() + v.len() + 4)
            .filter(|&s| s <= MAX_HEADER_BYTES)
            .ok_or_else(|| hub_limit(MAX_HEADER_BYTES, LimitUnit::Bytes))?;
    }
    Ok(())
}

/// Adds `Rdy-Actor` and, only with it, `Rdy-Run`: the claim's bytes exactly.
fn push_claim(
    headers: &mut Vec<(HeaderName, HeaderValue)>,
    claim: &RequestClaim,
) -> Result<(), RdyError> {
    let value = |text: &str| HeaderValue::from_bytes(text.as_bytes()).map_err(|_| actor_unbound());
    headers.push((HeaderName::from_static("rdy-actor"), value(&claim.actor)?));
    if let Some(run) = &claim.run {
        headers.push((HeaderName::from_static("rdy-run"), value(run)?));
    }
    Ok(())
}

/// Nonempty text within `max` bytes with no control character. Over the bound is `E_LIMIT`;
/// anything else is `E_UPSTREAM_PROTOCOL` (these values come from hub replies).
fn plain(text: &str, max: usize) -> Result<(), RdyError> {
    check_count(text.len(), max, LimitUnit::Bytes)?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err(upstream_protocol());
    }
    Ok(())
}

/// Bounds every caller input of `op` before anything is encoded.
fn check_inputs(op: &HubOperation) -> Result<(), RdyError> {
    match op {
        HubOperation::Metadata | HubOperation::Me => {}
        HubOperation::DeviceStart { machine_name } => check_machine_name(machine_name)?,
        HubOperation::DevicePoll { device_code } => {
            plain(device_code.expose(), MAX_DEVICE_CODE_BYTES)?;
        }
        // Their form is checked with the bearer, after the destination.
        HubOperation::Refresh { .. } | HubOperation::SignOut { .. } => {}
        HubOperation::BindChallenge { label, .. } => {
            if let Some(label) = label {
                check_label(label)?;
            }
        }
        HubOperation::Bind {
            challenge,
            public_key,
            proof,
        } => {
            plain(challenge, MAX_ID_BYTES)?;
            protocol_unless(id_of(CHALLENGE_PREFIX, challenge))?;
            check_count(public_key.len(), MAX_SIG_KEY_BYTES, LimitUnit::Bytes)?;
            if !public_key_line(public_key) {
                return Err(upstream_key_proof());
            }
            check_proof(proof)?;
        }
        HubOperation::FindKey { fingerprint } => {
            check_count(fingerprint.len(), MAX_ID_BYTES, LimitUnit::Bytes)?;
            if crate::signing_crypto::key_fingerprint(fingerprint).as_deref() != Ok(fingerprint) {
                return Err(signing_selector("key"));
            }
        }
        HubOperation::Retire { key_id } => {
            plain(key_id, MAX_ID_BYTES)?;
            protocol_unless(id_of(KEY_PREFIX, key_id))?;
        }
        HubOperation::Revoke {
            key_id,
            compromised_from,
        } => {
            plain(key_id, MAX_ID_BYTES)?;
            protocol_unless(id_of(KEY_PREFIX, key_id))?;
            if let Some(t) = compromised_from {
                parse_compromised_since(t)?;
            }
        }
    }
    Ok(())
}

/// `E_UPSTREAM_PROTOCOL` unless `ok`.
fn protocol_unless(ok: bool) -> Result<(), RdyError> {
    if ok { Ok(()) } else { Err(upstream_protocol()) }
}

/// An SSHSIG proof in standard base64 with padding and no line break, of at most
/// [`MAX_SIG_PROOF_BYTES`] decoded bytes. The text is bounded before it is decoded; the decoded
/// length is computed with checked arithmetic, then a strict decoder refuses misplaced padding
/// and set unused bits. Over the bound is `E_LIMIT`; anything else is `E_UPSTREAM_KEY_PROOF`.
fn check_proof(proof: &str) -> Result<(), RdyError> {
    check_count(proof.len(), MAX_PROOF_TEXT, LimitUnit::Bytes)?;
    let b = proof.as_bytes();
    if b.is_empty() || !b.len().is_multiple_of(4) {
        return Err(upstream_key_proof());
    }
    let pad = b.iter().rev().take_while(|&&c| c == b'=').count();
    let shape = pad <= 2
        && b[..b.len() - pad]
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/');
    if !shape {
        return Err(upstream_key_proof());
    }
    let decoded = (b.len() / 4)
        .checked_mul(3)
        .and_then(|n| n.checked_sub(pad))
        .ok_or_else(upstream_key_proof)?;
    check_count(decoded, MAX_SIG_PROOF_BYTES, LimitUnit::Bytes)?;
    let mut out = vec![0u8; decoded];
    match Base64::decode(proof, &mut out) {
        Ok(bytes) if bytes.len() == decoded => Ok(()),
        _ => Err(upstream_key_proof()),
    }
}

/// An OpenSSH public-key line with no comment, as SIG reads one: `<type> <base64>` with exactly
/// one space, no other white space or line, and a whole key of a type SIG can read. A
/// fingerprint names a key but is not one.
fn public_key_line(text: &str) -> bool {
    let mut fields = text.split(' ');
    let (Some(kind), Some(blob), None) = (fields.next(), fields.next(), fields.next()) else {
        return false;
    };
    let field_ok =
        |f: &str| !f.is_empty() && !f.chars().any(|c| c.is_whitespace() || c.is_control());
    text.len() <= MAX_SIG_KEY_BYTES
        && field_ok(kind)
        && field_ok(blob)
        && !text.starts_with(FINGERPRINT_PREFIX)
        && crate::signing_crypto::key_fingerprint(text).is_ok()
}

#[derive(Serialize)]
struct ChallengeBody<'a> {
    kind: KeyKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<&'a str>,
}

#[derive(Serialize)]
struct BindBody<'a> {
    challenge: &'a str,
    public_key: &'a str,
    proof: &'a str,
}

#[derive(Serialize)]
struct RevokeBody {
    compromised_from: String,
}

fn form(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    s.finish().into_bytes()
}

fn json_body<T: Serialize>(value: &T) -> Result<Vec<u8>, RdyError> {
    let bytes = serde_json::to_vec(value).map_err(|_| upstream_protocol())?;
    check_count(bytes.len(), MAX_JSON_BYTES, LimitUnit::Bytes)?;
    Ok(bytes)
}

/// A request body and its media type.
type Body = Option<(&'static str, Vec<u8>)>;

/// The method and body of `op`. The OAuth forms are the settled device client profile.
fn encode(op: &HubOperation) -> Result<(Method, Body), RdyError> {
    let post_form = |pairs: &[(&str, &str)]| -> Result<(Method, Body), RdyError> {
        Ok((Method::POST, Some((FORM, form(pairs)))))
    };
    let post_json = |bytes: Vec<u8>| -> Result<(Method, Body), RdyError> {
        Ok((Method::POST, Some((JSON, bytes))))
    };
    match op {
        HubOperation::Metadata | HubOperation::Me | HubOperation::FindKey { .. } => {
            Ok((Method::GET, None))
        }
        HubOperation::DeviceStart { machine_name } => post_form(&[
            ("client_id", CLIENT_ID),
            ("scope", DEVICE_SCOPE),
            ("machine_name", machine_name),
        ]),
        HubOperation::DevicePoll { device_code } => post_form(&[
            ("grant_type", DEVICE_CODE_GRANT),
            ("device_code", device_code.expose()),
            ("client_id", CLIENT_ID),
        ]),
        HubOperation::Refresh { refresh_token } => post_form(&[
            ("grant_type", REFRESH_GRANT),
            ("refresh_token", refresh_token.expose()),
            ("client_id", CLIENT_ID),
        ]),
        HubOperation::SignOut { token } => {
            post_form(&[("token", token.expose()), ("client_id", CLIENT_ID)])
        }
        HubOperation::BindChallenge { kind, label } => post_json(json_body(&ChallengeBody {
            kind: *kind,
            label: label.as_deref(),
        })?),
        HubOperation::Bind {
            challenge,
            public_key,
            proof,
        } => post_json(json_body(&BindBody {
            challenge,
            public_key,
            proof,
        })?),
        // No body: the hub retires now, or revokes from its request time.
        HubOperation::Retire { .. }
        | HubOperation::Revoke {
            compromised_from: None,
            ..
        } => Ok((Method::POST, None)),
        HubOperation::Revoke {
            compromised_from: Some(t),
            ..
        } => post_json(json_body(&RevokeBody {
            compromised_from: parse_compromised_since(t)?,
        })?),
    }
}

/// An OAuth endpoint the client may use: HTTPS on `install` itself with a path under `/auth/`,
/// no userinfo, query, fragment or percent-encoded byte, and a path that URL normalization
/// leaves unchanged (no dot or empty segment).
fn oauth_endpoint(install: &Origin, uri: &str) -> bool {
    if uri.len() > MAX_URL_BYTES
        || uri
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return false;
    }
    let Some(rest) = uri
        .get(..8)
        .filter(|s| s.eq_ignore_ascii_case("https://"))
        .map(|_| &uri[8..])
    else {
        return false;
    };
    let Some(slash) = rest.find('/') else {
        return false;
    };
    let (authority, path) = rest.split_at(slash);
    if authority.is_empty()
        || authority.contains(['@', '%', '?', '#'])
        || path.contains(['?', '#', '%'])
    {
        return false;
    }
    let Ok(url) = url::Url::parse(uri) else {
        return false;
    };
    let segments_ok = path.strip_prefix(AUTH_PREFIX).is_some_and(|tail| {
        !tail.is_empty() && tail.split('/').all(|s| !matches!(s, "" | "." | ".."))
    });
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.origin().ascii_serialization() == install.as_str()
        && url.path() == path
        && segments_ok
}

// ---------------------------------------------------------------------------------------------
// The user CA bundle
// ---------------------------------------------------------------------------------------------

/// Reads the user CA bundle: an absolute path to a regular file of at most 1048576 bytes that
/// holds 1-64 PEM certificates and no other block, each one a trust anchor the TLS library can
/// read. Else `E_UPSTREAM_CA`: the library's own store would drop an unreadable root silently.
fn read_ca(path: &Path) -> Result<Vec<Certificate<'static>>, RdyError> {
    if !path.is_absolute() {
        return Err(upstream_ca());
    }
    let meta = std::fs::metadata(path).map_err(|_| upstream_ca())?;
    if !meta.is_file() {
        return Err(upstream_ca());
    }
    let file = std::fs::File::open(path).map_err(|_| upstream_ca())?;
    let mut bytes = Vec::new();
    file.take(MAX_CA_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| upstream_ca())?;
    if bytes.len() > MAX_CA_BYTES {
        return Err(upstream_ca());
    }
    // Every block must be a certificate: a key or any other block makes the file unusable.
    let blocks = bytes.windows(11).filter(|w| *w == b"-----BEGIN ").count();
    let mut certs = Vec::new();
    for item in ureq::tls::parse_pem(&bytes) {
        match item {
            Ok(PemItem::Certificate(c)) => {
                if certs.len() == MAX_CA_CERTS || !trust_anchor(c.der()) {
                    return Err(upstream_ca());
                }
                certs.push(c.to_owned());
            }
            _ => return Err(upstream_ca()),
        }
    }
    if certs.is_empty() || certs.len() != blocks {
        return Err(upstream_ca());
    }
    Ok(certs)
}

/// Whether `der` is a certificate the TLS library accepts as a trust anchor: the parser its
/// trust store uses for every root.
fn trust_anchor(der: &[u8]) -> bool {
    rustls::RootCertStore::empty()
        .add(rustls::pki_types::CertificateDer::from(der))
        .is_ok()
}

// ---------------------------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------------------------

/// The default connector chain, marking when a connection and its TLS handshake are complete:
/// every failure before that is certain not to have sent the request.
struct Marked {
    inner: DefaultConnector,
    connected: Arc<AtomicBool>,
}

impl fmt::Debug for Marked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Marked")
    }
}

impl Connector<()> for Marked {
    type Out = Box<dyn Transport>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        let out = self.inner.connect(details, chained)?;
        if out.is_some() {
            self.connected.store(true, Ordering::SeqCst);
        }
        Ok(out)
    }
}

/// A request body read to exactly `remaining` more bytes through one copying buffer. Each fill
/// that reaches the end first checks that the source has ended, so the bytes that would
/// complete the request are held back from a body that runs long. A short, long or failed
/// source sets `refused` and fails every later read; the reader's own error text is dropped.
struct Exact<'a> {
    inner: &'a mut dyn Read,
    remaining: u64,
    buf: Vec<u8>,
    start: usize,
    end: usize,
    refused: bool,
}

impl<'a> Exact<'a> {
    fn new(inner: &'a mut dyn Read, len: u64) -> Exact<'a> {
        let size = usize::try_from(len).map_or(UPLOAD_BUFFER_BYTES, |n| n.min(UPLOAD_BUFFER_BYTES));
        Exact {
            inner,
            remaining: len,
            buf: vec![0; size],
            start: 0,
            end: 0,
            refused: false,
        }
    }

    fn refuse(&mut self) -> std::io::Result<usize> {
        self.refused = true;
        Err(std::io::Error::other("upload body refused"))
    }
}

impl Read for Exact<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.refused {
            return self.refuse();
        }
        if out.is_empty() {
            return Ok(0);
        }
        if self.start == self.end {
            if self.remaining == 0 {
                return Ok(0);
            }
            let want =
                usize::try_from(self.remaining).map_or(self.buf.len(), |r| r.min(self.buf.len()));
            let n = match read_some(self.inner, &mut self.buf[..want]) {
                Ok(n) if n > 0 && n <= want => n,
                _ => return self.refuse(),
            };
            self.remaining -= n as u64;
            if self.remaining == 0 && !at_end(self.inner) {
                return self.refuse();
            }
            self.start = 0;
            self.end = n;
        }
        let n = out.len().min(self.end - self.start);
        out[..n].copy_from_slice(&self.buf[self.start..self.start + n]);
        self.start += n;
        Ok(n)
    }
}

/// One read of `r`, repeated while it is interrupted.
fn read_some(r: &mut dyn Read, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match r.read(buf) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

/// Whether `r` has ended: a one-byte read gives nothing. A failed read is not an end.
fn at_end(r: &mut dyn Read) -> bool {
    let mut probe = [0u8; 1];
    matches!(read_some(r, &mut probe), Ok(0))
}

/// Sends one request on a fresh agent and reads the bounded reply: its status and body.
fn send(p: Prepared<'_>, roots: Vec<Certificate<'static>>) -> Result<(u16, Vec<u8>), HubFailure> {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .https_only(true)
        .proxy(None)
        .max_redirects(0)
        .max_response_header_size(MAX_HEADER_BYTES)
        .input_buffer_size(MAX_HEADER_BYTES)
        .max_idle_connections(0)
        .max_idle_connections_per_host(0)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(p.timeout))
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::Rustls)
                .root_certs(RootCerts::Specific(Arc::new(roots)))
                .build(),
        )
        .build();
    let connected = Arc::new(AtomicBool::new(false));
    let agent = ureq::Agent::with_parts(
        config,
        Marked {
            inner: DefaultConnector::new(),
            connected: Arc::clone(&connected),
        },
        DefaultResolver::default(),
    );
    let mut builder = http::Request::builder()
        .method(p.method.clone())
        .uri(p.url.as_str());
    for (name, value) in p.headers {
        builder = builder.header(name, value);
    }
    // A GET has no body. A POST without one sends `content-length: 0`; an empty `()` body
    // would be sent chunked. A streamed body's length is already in the headers.
    let result = match p.body {
        Payload::Empty => builder.body(()).map(|r| agent.run(r)),
        Payload::Bytes(body) => builder.body(body).map(|r| agent.run(r)),
        Payload::Stream(mut exact) => {
            let result = builder
                .body(SendBody::from_reader(&mut exact))
                .map(|r| agent.run(r));
            // The request was never completed: nothing reached the hub as a request.
            if exact.refused {
                return Err(HubFailure::Local(upload_body()));
            }
            result
        }
    };
    let mut response = match result {
        Err(_) => {
            return Err(HubFailure::Transport {
                may_have_sent: false,
            });
        }
        Ok(Err(e)) => return Err(failure(&e, connected.load(Ordering::SeqCst))),
        Ok(Ok(r)) => r,
    };
    let status = response.status();
    if response.headers().len() > MAX_HEADERS {
        return Err(HubFailure::Protocol);
    }
    // A redirect is never followed, and its body is not read.
    if status.is_redirection() {
        return Err(HubFailure::Protocol);
    }
    // Read one byte past the limit, so an over-long body is seen.
    let read_limit = u64::try_from(p.reply_limit)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let body = response
        .body_mut()
        .with_config()
        .limit(read_limit)
        .read_to_vec()
        .map_err(|e| failure(&e, true))?;
    if body.len() > p.reply_limit {
        return Err(HubFailure::Protocol);
    }
    Ok((status.as_u16(), body))
}

/// A library error as a closed failure. Before the connection was ready nothing was sent; after
/// it, a reply over a bound or out of grammar is a contract mismatch and anything else may have
/// reached the hub.
fn failure(e: &ureq::Error, sent: bool) -> HubFailure {
    if !sent {
        return HubFailure::Transport {
            may_have_sent: false,
        };
    }
    match e {
        ureq::Error::LargeResponseHeader(..)
        | ureq::Error::BodyExceedsLimit(_)
        | ureq::Error::Protocol(_) => HubFailure::Protocol,
        _ => HubFailure::Transport {
            may_have_sent: true,
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Bounded JSON
// ---------------------------------------------------------------------------------------------

/// Walks one JSON value with serde's visitors, checking its depth, its total members and
/// elements, its scalar lengths and its keys for duplicates against the caller's limits.
struct Guard<'a> {
    depth: usize,
    members: &'a Cell<usize>,
    limits: JsonLimits,
}

impl Guard<'_> {
    fn inner(&self) -> Result<Guard<'_>, &'static str> {
        let depth = self.depth.checked_add(1).ok_or("too deep")?;
        if depth > self.limits.depth {
            return Err("too deep");
        }
        Ok(Guard {
            depth,
            members: self.members,
            limits: self.limits,
        })
    }

    fn at_depth(&self) -> Guard<'_> {
        Guard {
            depth: self.depth,
            members: self.members,
            limits: self.limits,
        }
    }

    fn count(&self) -> Result<(), &'static str> {
        let n = self
            .members
            .get()
            .checked_add(1)
            .ok_or("too many members")?;
        if n > self.limits.values {
            return Err("too many members");
        }
        self.members.set(n);
        Ok(())
    }
}

impl<'de> DeserializeSeed<'de> for Guard<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Guard<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded JSON")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<(), E> {
        if v.len() > self.limits.scalar_bytes {
            return Err(E::custom("scalar too long"));
        }
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let inner = self.inner().map_err(de::Error::custom)?;
        while seq.next_element_seed(inner.at_depth())?.is_some() {
            inner.count().map_err(de::Error::custom)?;
        }
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let inner = self.inner().map_err(de::Error::custom)?;
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            inner.count().map_err(de::Error::custom)?;
            if key.len() > inner.limits.scalar_bytes || !keys.insert(key) {
                return Err(de::Error::custom("duplicate or long key"));
            }
            map.next_value_seed(inner.at_depth())?;
        }
        Ok(())
    }
}

/// Checks that `body` is one JSON value within `limits`: at most `bytes` bytes of UTF-8, no
/// duplicate key at any depth, no trailing data, and its depth, total members and elements, and
/// string and key lengths within bounds. Any failure is [`HubFailure::Protocol`]; no parser text
/// is kept.
pub fn check_json(body: &[u8], limits: JsonLimits) -> Result<(), HubFailure> {
    if body.len() > limits.bytes || std::str::from_utf8(body).is_err() {
        return Err(HubFailure::Protocol);
    }
    let members = Cell::new(0);
    let mut de = serde_json::Deserializer::from_slice(body);
    Guard {
        depth: 0,
        members: &members,
        limits,
    }
    .deserialize(&mut de)
    .and_then(|()| de.end())
    .map_err(|_| HubFailure::Protocol)
}

/// [`check_json`], then reads `body` as `T`. A value of the wrong shape is
/// [`HubFailure::Protocol`] too, without serde's text.
pub fn decode_bounded_json<'a, T: Deserialize<'a>>(
    body: &'a [u8],
    limits: JsonLimits,
) -> Result<T, HubFailure> {
    check_json(body, limits)?;
    serde_json::from_slice(body).map_err(|_| HubFailure::Protocol)
}

/// A typed exchange reply under HA2a's settled JSON bounds.
fn json<'a, T: Deserialize<'a>>(body: &'a [u8]) -> Result<T, HubFailure> {
    decode_bounded_json(body, EXCHANGE_JSON)
}

// ---------------------------------------------------------------------------------------------
// Reply objects
// ---------------------------------------------------------------------------------------------

/// A member whose presence alone matters, whatever its value (null included).
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<()>, D::Error> {
    IgnoredAny::deserialize(d).map(|_| Some(()))
}

/// A member that counts when it holds anything but null.
fn present_value<'de, D: Deserializer<'de>>(d: D) -> Result<Option<()>, D::Error> {
    Option::<IgnoredAny>::deserialize(d).map(|v| v.map(|_| ()))
}

/// A member every reply holds whose value may be null.
fn nullable<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}

/// RFC 8414 metadata. Other members are OAuth extensions: bounded and ignored.
#[derive(Deserialize)]
struct MetadataDto {
    issuer: String,
    device_authorization_endpoint: String,
    token_endpoint: String,
    revocation_endpoint: String,
    grant_types_supported: Vec<String>,
    code_challenge_methods_supported: Vec<String>,
    token_endpoint_auth_methods_supported: Vec<String>,
}

/// A device authorization reply (RFC 8628). Other members are bounded and ignored.
#[derive(Deserialize)]
struct DeviceDto {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
    #[serde(default, deserialize_with = "present")]
    verification_uri_complete: Option<()>,
}

/// A token reply. `scope` and other extension members are bounded and ignored.
#[derive(Deserialize)]
struct TokenDto {
    access_token: String,
    refresh_token: String,
    token_type: String,
    expires_in: u64,
}

/// An OAuth error reply: only `error` is read.
#[derive(Deserialize)]
struct OAuthErrorDto {
    error: String,
}

/// The hub's API error object: only `code` is read.
#[derive(Deserialize)]
struct ApiErrorDto {
    code: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum AccountKind {
    Person,
    Operator,
    Agent,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum AccountState {
    Active,
    Suspended,
    Deleted,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EarlierHandle {
    #[serde(rename = "handle")]
    _handle: String,
    /// Discarded, but still a time.
    until: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Organization {
    #[serde(rename = "id")]
    _id: String,
    #[serde(rename = "state")]
    _state: String,
    #[serde(rename = "name")]
    _name: String,
    #[serde(rename = "role")]
    _role: String,
}

/// The closed `/me` object. Only `id` and `handle` are kept.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MeDto {
    id: String,
    handle: String,
    earlier_handles: Vec<EarlierHandle>,
    #[serde(rename = "display_name")]
    _display_name: String,
    #[serde(rename = "kind")]
    _kind: AccountKind,
    #[serde(rename = "state")]
    _state: AccountState,
    #[serde(rename = "primary_email", deserialize_with = "nullable")]
    _primary_email: Option<String>,
    #[serde(rename = "two_factor")]
    _two_factor: bool,
    #[serde(rename = "sign_in_sources")]
    _sign_in_sources: Vec<String>,
    #[serde(rename = "organizations")]
    _organizations: Vec<Organization>,
    #[serde(rename = "can_create_organization")]
    _can_create_organization: bool,
    /// Discarded, but still a time when it is given.
    #[serde(default)]
    away_until: Option<String>,
    #[serde(rename = "version")]
    _version: String,
    /// Discarded, but still a time.
    created_at: String,
}

/// A key list: `next` may be absent or null.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyListDto {
    items: Vec<HubKey>,
    #[serde(default, deserialize_with = "present_value")]
    next: Option<()>,
}

// ---------------------------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------------------------

fn protocol_if(bad: bool) -> Result<(), HubFailure> {
    if bad {
        Err(HubFailure::Protocol)
    } else {
        Ok(())
    }
}

/// An id within its bound with no control character.
fn id_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_ID_BYTES && !s.chars().any(char::is_control)
}

/// The hub id of one kind of object: its `prefix` and at least one more byte, within the id
/// bound and with no control character.
fn id_of(prefix: &str, s: &str) -> bool {
    id_ok(s) && s.len() > prefix.len() && s.starts_with(prefix)
}

/// A reply time: RFC3339 within its 64-byte bound. The time parser alone takes any byte
/// between date and time, so the `T`/`t` separator is checked first. The compromise-input rules
/// (at most nine fractional digits, no leap second) bind only what rdy sends, not hub times:
/// any number of fractional digits within the bound and a valid leap second are accepted, and
/// the reply keeps the hub's text.
fn time_ok(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() <= MAX_TIMESTAMP_BYTES
        && s.is_ascii()
        && b.len() >= 20
        && matches!(b[10], b'T' | b't')
        && time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).is_ok()
}

/// A handle that can form a `human:` claim (the rule of [`RequestClaim::bind_human`]).
fn handle_ok(s: &str) -> bool {
    identity::is_name(s)
}

/// 32 bytes in unpadded base64url.
fn nonce_ok(nonce: &str) -> bool {
    nonce.len() == NONCE_CHARS
        && nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The token endpoint's OAuth errors in the settled profile, with their status.
const TOKEN_ERRORS: [(&str, u16); 8] = [
    ("authorization_pending", 400),
    ("slow_down", 400),
    ("access_denied", 400),
    ("expired_token", 400),
    ("invalid_grant", 400),
    ("invalid_request", 400),
    ("unsupported_grant_type", 400),
    ("invalid_client", 401),
];
/// The device endpoint's OAuth errors.
const DEVICE_ERRORS: [(&str, u16); 3] = [
    ("invalid_request", 400),
    ("invalid_scope", 400),
    ("invalid_client", 401),
];
/// The revocation endpoint's OAuth errors.
const REVOCATION_ERRORS: [(&str, u16); 2] = [("invalid_request", 400), ("invalid_client", 401)];

/// How a recognized hub API error code is classified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApiOutcome {
    ActorClaim,
    AuthRequired,
    Refused,
}

/// The hub's API error codes the client recognizes, each with one HTTP status it is sent with:
/// the one hub-code table. A code may have more than one row (`E_HUB_BAD_INPUT`). The first 17
/// rows are HA2a's; the rest are the publication upload's (hub tb-1V67R, tb-NZXSA, tb-WDA9Q,
/// tb-K416A, tb-4FYKZ, tb-CVS65 and tb-SVP3W).
const API_ERRORS: [(&str, u16, ApiOutcome); 52] = [
    ("E_HUB_ACTOR_CLAIM", 403, ApiOutcome::ActorClaim),
    ("E_HUB_NO_CREDENTIAL", 401, ApiOutcome::AuthRequired),
    ("E_HUB_CREDENTIAL_UNKNOWN", 401, ApiOutcome::AuthRequired),
    ("E_HUB_CREDENTIAL_EXPIRED", 401, ApiOutcome::AuthRequired),
    ("E_HUB_CREDENTIAL_REVOKED", 401, ApiOutcome::AuthRequired),
    ("E_HUB_KEY_NOT_SIGNING", 422, ApiOutcome::Refused),
    ("E_HUB_KEY_CHALLENGE", 422, ApiOutcome::Refused),
    ("E_HUB_KEY_PROOF", 422, ApiOutcome::Refused),
    ("E_HUB_KEY_TYPE", 422, ApiOutcome::Refused),
    ("E_HUB_KEY_LIMIT", 422, ApiOutcome::Refused),
    ("E_HUB_VALIDATION", 422, ApiOutcome::Refused),
    ("E_HUB_KEY_TAKEN", 409, ApiOutcome::Refused),
    ("E_HUB_KEY_STATE", 409, ApiOutcome::Refused),
    ("E_HUB_NOT_FOUND", 404, ApiOutcome::Refused),
    ("E_HUB_KEY_ACTOR", 403, ApiOutcome::Refused),
    ("E_HUB_OUT_OF_REACH", 403, ApiOutcome::Refused),
    ("E_HUB_BAD_INPUT", 400, ApiOutcome::Refused),
    // tb-1V67R
    ("E_HUB_BAD_INPUT", 408, ApiOutcome::Refused),
    ("E_HUB_BAD_INPUT", 431, ApiOutcome::Refused),
    ("E_HUB_API_VERSION", 404, ApiOutcome::Refused),
    ("E_HUB_INTERNAL", 500, ApiOutcome::Refused),
    // tb-NZXSA
    ("E_HUB_TWO_CREDENTIALS", 400, ApiOutcome::Refused),
    ("E_HUB_ACCESS_LEVEL", 403, ApiOutcome::Refused),
    ("E_HUB_CLIENT_UNVERIFIED", 403, ApiOutcome::Refused),
    ("E_HUB_SIGN_IN_SOURCE", 403, ApiOutcome::Refused),
    ("E_HUB_REPO_ARCHIVED", 403, ApiOutcome::Refused),
    // tb-WDA9Q
    ("E_HUB_EMAIL_UNVERIFIED", 403, ApiOutcome::Refused),
    // tb-K416A
    ("E_HUB_STREAM_INVALID", 422, ApiOutcome::Refused),
    ("E_HUB_QUARANTINE_FULL", 422, ApiOutcome::Refused),
    ("E_HUB_STORE_LIMIT", 422, ApiOutcome::Refused),
    ("E_HUB_CORE_CHECK_FAILED", 422, ApiOutcome::Refused),
    ("E_HUB_OTHER_PROJECT", 409, ApiOutcome::Refused),
    // tb-4FYKZ
    ("E_HUB_STREAM_FORMAT", 422, ApiOutcome::Refused),
    ("E_HUB_RDY_TOO_OLD", 422, ApiOutcome::Refused),
    ("E_HUB_INSTALL_TOO_OLD", 422, ApiOutcome::Refused),
    // tb-CVS65
    ("E_HUB_OVER_BUDGET", 429, ApiOutcome::Refused),
    ("E_HUB_BUSY", 503, ApiOutcome::Refused),
    ("E_HUB_REPO_MOVING", 503, ApiOutcome::Refused),
    ("E_HUB_READ_ONLY", 503, ApiOutcome::Refused),
    // tb-SVP3W
    ("E_HUB_MANIFEST_PATH", 422, ApiOutcome::Refused),
    ("E_HUB_ARTIFACT_LIMIT", 422, ApiOutcome::Refused),
    ("E_HUB_PROFILE_MISSING", 422, ApiOutcome::Refused),
    ("E_HUB_PROFILE_MISMATCH", 422, ApiOutcome::Refused),
    ("E_HUB_FILE_HASH", 422, ApiOutcome::Refused),
    ("E_HUB_MANIFEST_FILES", 422, ApiOutcome::Refused),
    ("E_HUB_RENDER_FAILED", 422, ApiOutcome::Refused),
    ("E_HUB_CANDIDATE_LIMIT", 422, ApiOutcome::Refused),
    ("E_HUB_TAG_PUBLISHED", 409, ApiOutcome::Refused),
    ("E_HUB_TAG_TAKEN", 409, ApiOutcome::Refused),
    ("E_HUB_REMOVAL_UNCOVERED", 409, ApiOutcome::Refused),
    ("E_HUB_SITE_AUDIENCE", 409, ApiOutcome::Refused),
    ("E_HUB_PUBLIC_SITES_OFF", 403, ApiOutcome::Refused),
];

/// The [`API_ERRORS`] row of `code`: its static spelling and classification. With
/// `Some(status)`, the code and status must form a row; a known code at no row's status is
/// [`HubFailure::Protocol`]. The pair is matched before the code alone, so every row of a code
/// with several statuses is reachable. `None` skips the status check (an issue has no status).
/// An unknown code is `Ok(None)`.
pub(crate) fn api_error_entry(
    code: &str,
    status: Option<u16>,
) -> Result<Option<(&'static str, ApiOutcome)>, HubFailure> {
    match status {
        Some(s) => {
            if let Some(row) = API_ERRORS.iter().find(|r| r.0 == code && r.1 == s) {
                return Ok(Some((row.0, row.2)));
            }
            if API_ERRORS.iter().any(|r| r.0 == code) {
                return Err(HubFailure::Protocol);
            }
            Ok(None)
        }
        None => Ok(API_ERRORS.iter().find(|r| r.0 == code).map(|r| (r.0, r.2))),
    }
}

/// The OAuth error of a failed reply, checked against the endpoint's `listed` errors: a listed
/// code at its status is `Ok(code)`; at another status it is a contract mismatch; an unlisted
/// code stops the call as refused. No description or URI is read.
fn oauth_error(status: u16, body: &[u8], listed: &[(&str, u16)]) -> Result<String, HubFailure> {
    let dto: OAuthErrorDto = json(body)?;
    match listed.iter().find(|(code, _)| *code == dto.error) {
        Some((_, s)) if *s == status => Ok(dto.error),
        Some(_) => Err(HubFailure::Protocol),
        None => Err(HubFailure::Refused),
    }
}

/// A failed API reply: a recognized code at its status, else a mismatch or a refusal. The
/// message, fix and body are never read into the result.
fn api_error(status: u16, body: &[u8]) -> HubFailure {
    let dto: ApiErrorDto = match json(body) {
        Ok(d) => d,
        Err(f) => return f,
    };
    match api_error_entry(&dto.code, Some(status)) {
        Ok(Some((_, ApiOutcome::ActorClaim))) => HubFailure::ActorClaim,
        Ok(Some((_, ApiOutcome::AuthRequired))) => HubFailure::AuthRequired,
        Ok(Some((_, ApiOutcome::Refused))) | Ok(None) => HubFailure::Refused,
        Err(f) => f,
    }
}

/// A token reply: both token forms, a Bearer type (any case) and a 1-3600 second life.
fn tokens(body: &[u8]) -> Result<TokenPair, HubFailure> {
    let dto: TokenDto = json(body)?;
    protocol_if(!dto.token_type.eq_ignore_ascii_case("Bearer"))?;
    let pair = TokenPair {
        access_token: Secret::new(dto.access_token),
        refresh_token: Secret::new(dto.refresh_token),
        expires_in: dto.expires_in,
    };
    pair.validate().map_err(|_| HubFailure::Protocol)?;
    Ok(pair)
}

/// A key object's values: a key id and an account id, an OpenSSH public-key line with no
/// comment whose fingerprint is the object's, a valid label and RFC3339 times.
fn key_ok(key: &HubKey) -> bool {
    let times = [
        &key.expires_at,
        &key.bound_at,
        &key.retired_at,
        &key.revoked_at,
        &key.compromised_from,
    ];
    id_of(KEY_PREFIX, &key.id)
        && id_of(ACCOUNT_PREFIX, &key.account)
        && public_key_line(&key.public_key)
        && crate::signing_crypto::key_fingerprint(&key.public_key).as_deref()
            == Ok(key.fingerprint.as_str())
        && check_label(&key.label).is_ok()
        && times.iter().all(|t| t.as_deref().is_none_or(time_ok))
}

/// Reads the reply of `call` from its status and bounded body.
fn decode(call: &HubCall, status: u16, body: &[u8]) -> Result<HubReply, HubFailure> {
    let install = &call.install;
    match &call.operation {
        HubOperation::Metadata => {
            protocol_if(status != 200)?;
            let dto: MetadataDto = json(body)?;
            let has = |list: &[String], v: &str| list.iter().any(|x| x == v);
            protocol_if(
                !has(&dto.grant_types_supported, DEVICE_CODE_GRANT)
                    || !has(&dto.grant_types_supported, REFRESH_GRANT)
                    || !has(&dto.code_challenge_methods_supported, "S256")
                    || !has(&dto.token_endpoint_auth_methods_supported, "none"),
            )?;
            let m = HubMetadata {
                issuer: dto.issuer,
                device_authorization_endpoint: dto.device_authorization_endpoint,
                token_endpoint: dto.token_endpoint,
                revocation_endpoint: dto.revocation_endpoint,
            };
            m.validate(install).map_err(|_| HubFailure::Protocol)?;
            let endpoints = [
                &m.device_authorization_endpoint,
                &m.token_endpoint,
                &m.revocation_endpoint,
            ];
            protocol_if(!endpoints.iter().all(|e| oauth_endpoint(install, e)))?;
            Ok(HubReply::Metadata(m))
        }
        HubOperation::DeviceStart { .. } => {
            if status != 200 {
                oauth_error(status, body, &DEVICE_ERRORS)?;
                return Err(HubFailure::Refused);
            }
            let dto: DeviceDto = json(body)?;
            // The hub never sends a complete link: one would hold the user code.
            protocol_if(dto.verification_uri_complete.is_some())?;
            // The link is exactly the install's device page, byte for byte: a URL parser would
            // also accept a path that only normalizes to it (`/app/<code>/../device`, encoded
            // dots), and the reply returns the raw text.
            protocol_if(!is_device_page(install, &dto.verification_uri))?;
            let reply = DeviceStartReply {
                device_code: Secret::new(dto.device_code),
                user_code: dto.user_code,
                verification_uri: dto.verification_uri,
                expires_in: dto.expires_in,
                interval: dto.interval,
            };
            reply.validate(install).map_err(|_| HubFailure::Protocol)?;
            Ok(HubReply::DeviceStart(reply))
        }
        HubOperation::DevicePoll { .. } => {
            if status == 200 {
                return Ok(HubReply::DevicePoll(DevicePollReply::Tokens(tokens(body)?)));
            }
            let reply = match oauth_error(status, body, &TOKEN_ERRORS)?.as_str() {
                "authorization_pending" => DevicePollReply::Pending,
                "slow_down" => DevicePollReply::SlowDown,
                "expired_token" => DevicePollReply::Expired,
                "invalid_grant" => DevicePollReply::InvalidGrant,
                _ => return Err(HubFailure::Refused),
            };
            Ok(HubReply::DevicePoll(reply))
        }
        HubOperation::Refresh { .. } => {
            if status == 200 {
                return Ok(HubReply::Refresh(RefreshReply::Tokens(tokens(body)?)));
            }
            match oauth_error(status, body, &TOKEN_ERRORS)?.as_str() {
                "invalid_grant" => Ok(HubReply::Refresh(RefreshReply::AuthRequired)),
                // A device-grant answer does not fit a refresh.
                "authorization_pending" | "slow_down" | "expired_token" => {
                    Err(HubFailure::Protocol)
                }
                _ => Err(HubFailure::Refused),
            }
        }
        HubOperation::SignOut { .. } => {
            if status == 200 {
                // The reply bytes, bounded when read, are ignored.
                return Ok(HubReply::SignOut);
            }
            oauth_error(status, body, &REVOCATION_ERRORS)?;
            Err(HubFailure::Refused)
        }
        op => decode_api(install, op, status, body),
    }
}

/// Reads an account or key route's reply.
fn decode_api(
    install: &Origin,
    op: &HubOperation,
    status: u16,
    body: &[u8],
) -> Result<HubReply, HubFailure> {
    let expected = match op {
        HubOperation::BindChallenge { .. } | HubOperation::Bind { .. } => 201,
        _ => 200,
    };
    if status != expected {
        if (200..300).contains(&status) {
            return Err(HubFailure::Protocol);
        }
        return Err(api_error(status, body));
    }
    match op {
        HubOperation::Me => {
            let dto: MeDto = json(body)?;
            let times_ok = time_ok(&dto.created_at)
                && dto.away_until.as_deref().is_none_or(time_ok)
                && dto.earlier_handles.iter().all(|h| time_ok(&h.until));
            protocol_if(!id_of(ACCOUNT_PREFIX, &dto.id) || !handle_ok(&dto.handle) || !times_ok)?;
            Ok(HubReply::Me(MeReply {
                account: dto.id,
                handle: dto.handle,
            }))
        }
        HubOperation::BindChallenge { kind, label } => {
            let c: KeyChallenge = json(body)?;
            let want_label = label.as_deref().unwrap_or(kind.default_label());
            protocol_if(
                !id_of(CHALLENGE_PREFIX, &c.id)
                    || !id_of(ACCOUNT_PREFIX, &c.account)
                    || c.kind != *kind
                    || c.label != want_label
                    || c.namespace != KEYBIND_NAMESPACE
                    || !nonce_ok(&c.nonce)
                    || !time_ok(&c.expires_at),
            )?;
            Ok(HubReply::BindChallenge(c))
        }
        HubOperation::Bind { public_key, .. } => {
            let b: BindResult = json(body)?;
            protocol_if(
                !key_ok(&b.key)
                    || b.key.state != KeyState::Pending
                    || b.key.public_key != *public_key,
            )?;
            let confirm = install
                .join(&["app", "settings", "keys", &b.key.id, "confirm"])
                .map_err(|_| HubFailure::Protocol)?;
            protocol_if(b.confirm_url != confirm)?;
            Ok(HubReply::Bind(b))
        }
        HubOperation::FindKey { fingerprint } => {
            let list: KeyListDto = json(body)?;
            protocol_if(list.next.is_some() || list.items.len() > 1)?;
            let key = list.items.into_iter().next();
            if let Some(k) = &key {
                protocol_if(!key_ok(k) || k.fingerprint != *fingerprint)?;
            }
            Ok(HubReply::FindKey(key))
        }
        HubOperation::Retire { key_id } => {
            let k: HubKey = json(body)?;
            protocol_if(
                !key_ok(&k)
                    || k.id != *key_id
                    || k.state != KeyState::Retired
                    || k.retired_at.is_none(),
            )?;
            Ok(HubReply::Retire(k))
        }
        HubOperation::Revoke { key_id, .. } => {
            let k: HubKey = json(body)?;
            protocol_if(
                !key_ok(&k)
                    || k.id != *key_id
                    || k.state != KeyState::Revoked
                    || k.revoked_at.is_none()
                    || k.compromised_from.is_none(),
            )?;
            Ok(HubReply::Revoke(k))
        }
        _ => Err(HubFailure::Protocol),
    }
}
