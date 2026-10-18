//! hsa4: the hub's HTTPS transport. Every test runs the real transport against a loopback TLS
//! fixture with a generated CA; no public network, sleep or elapsed-time assertion is used.
#[path = "support/hub_tls.rs"]
mod hub_tls;

use hub_tls::{Ca, Recorded, Reply, Server};
use rdy_engine::hub_http::{HttpHubTransport, route};
use rdy_engine::hub_types::*;
use rdy_engine::{
    E_ACTOR_UNBOUND, E_BAD_INPUT, E_LIMIT, E_UPSTREAM_AUTH, E_UPSTREAM_CA, E_UPSTREAM_KEY_PROOF,
    E_UPSTREAM_ORIGIN, E_UPSTREAM_PROTOCOL, E_UPSTREAM_TOKEN, Session, SessionKind,
};
use rdy_model::Role;
use std::path::PathBuf;

const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const REFRESH: &str = "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC";
/// A refresh token the hub rotates in.
const REFRESH_2: &str = "rdyx_a_TESTVECTORTESTVECTORTESTVECTORTESTVECTOR2kpMVZ";
/// Tokens that fail the form check.
const BAD_TOKENS: [&str; 4] = [
    "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYA",
    "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabc48FkQz",
    "rdyz_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd48L11G",
    "",
];
const DEVICE_CODE: &str = "device-code-sentinel-7Hq2";
const USER_CODE: &str = "BCDFGHJK";
const MACHINE: &str = "laptop";

const ACCOUNT: &str = "acc_00000000000000000000000001";
const CHALLENGE_ID: &str = "kch_00000000000000000000000002";
const KEY_ID: &str = "skey_00000000000000000000000003";
const FINGERPRINT: &str = "SHA256:RHFBqxmd4FLrdHDM2DGtlKdMxg/nN9xD3ZpkWHSvlaM";
const PUBLIC_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMxZFwIQHLofF5C/SY2UPr6zgqjzUfcX6+XICjAhsvur";
const PROOF: &str = "U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAgzFkXAhAcuh8XkL9JjZQ+vrOCqPNR9xfr5cgKMCGy+6sAAAAOcmR5LWtleWJpbmQtdjEAAAAAAAAABnNoYTUxMgAAAFMAAAALc3NoLWVkMjU1MTkAAABA12AaXZO7xGPGx60J5a1JLmiKfRLNj0h+d0zhRo6jsA9h9riGCaqukgi6feNhe0qfxybjswnpsk/FX3YA0v/nDQ==";
const COMPROMISED: &str = "2026-10-17T11:00:00Z";

// The settled reply fixtures. `{o}` is the fixture's generated origin.
const ME_REPLY: &str = r#"{"id":"acc_00000000000000000000000001","handle":"test-person","earlier_handles":[{"handle":"test-person-old","until":"2026-09-01T08:00:00Z"}],"display_name":"Test Person","kind":"person","state":"active","primary_email":null,"two_factor":true,"sign_in_sources":["local"],"organizations":[{"id":"org_00000000000000000000000004","state":"active","name":"test-org-a","role":"owner"},{"id":"org_00000000000000000000000005","state":"active","name":"test-org-b","role":"member"}],"can_create_organization":true,"version":"test-version-1","created_at":"2026-08-01T08:00:00Z"}"#;
const CHALLENGE_REQUEST: &str = r#"{"kind":"person","label":"laptop key"}"#;
const CHALLENGE_REPLY: &str = r#"{"id":"kch_00000000000000000000000002","account":"acc_00000000000000000000000001","kind":"person","label":"laptop key","namespace":"rdy-keybind-v1","nonce":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","expires_at":"2026-10-17T12:10:00Z"}"#;
const BIND_REQUEST: &str = r#"{"challenge":"kch_00000000000000000000000002","public_key":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMxZFwIQHLofF5C/SY2UPr6zgqjzUfcX6+XICjAhsvur","proof":"U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAgzFkXAhAcuh8XkL9JjZQ+vrOCqPNR9xfr5cgKMCGy+6sAAAAOcmR5LWtleWJpbmQtdjEAAAAAAAAABnNoYTUxMgAAAFMAAAALc3NoLWVkMjU1MTkAAABA12AaXZO7xGPGx60J5a1JLmiKfRLNj0h+d0zhRo6jsA9h9riGCaqukgi6feNhe0qfxybjswnpsk/FX3YA0v/nDQ=="}"#;
const BIND_REPLY: &str = r#"{"key":{"id":"skey_00000000000000000000000003","account":"acc_00000000000000000000000001","fingerprint":"SHA256:RHFBqxmd4FLrdHDM2DGtlKdMxg/nN9xD3ZpkWHSvlaM","public_key":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMxZFwIQHLofF5C/SY2UPr6zgqjzUfcX6+XICjAhsvur","label":"laptop key","kind":"person","state":"pending","expires_at":null,"bound_at":null,"retired_at":null,"revoked_at":null,"compromised_from":null},"confirm_url":"{o}/app/settings/keys/skey_00000000000000000000000003/confirm"}"#;
const FIND_TARGET: &str =
    "/api/v1/me/keys?fingerprint=SHA256%3ARHFBqxmd4FLrdHDM2DGtlKdMxg%2FnN9xD3ZpkWHSvlaM";
const LIST_REPLY: &str = r#"{"items":[{"id":"skey_00000000000000000000000003","account":"acc_00000000000000000000000001","fingerprint":"SHA256:RHFBqxmd4FLrdHDM2DGtlKdMxg/nN9xD3ZpkWHSvlaM","public_key":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMxZFwIQHLofF5C/SY2UPr6zgqjzUfcX6+XICjAhsvur","label":"laptop key","kind":"person","state":"active","expires_at":null,"bound_at":"2026-10-17T12:05:00Z","retired_at":null,"revoked_at":null,"compromised_from":null}],"next":null}"#;
const RETIRE_REPLY: &str = r#"{"id":"skey_00000000000000000000000003","account":"acc_00000000000000000000000001","fingerprint":"SHA256:RHFBqxmd4FLrdHDM2DGtlKdMxg/nN9xD3ZpkWHSvlaM","public_key":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMxZFwIQHLofF5C/SY2UPr6zgqjzUfcX6+XICjAhsvur","label":"laptop key","kind":"person","state":"retired","expires_at":null,"bound_at":"2026-10-17T12:05:00Z","retired_at":"2026-10-18T09:00:00Z","revoked_at":null,"compromised_from":null}"#;
const REVOKE_REQUEST: &str = r#"{"compromised_from":"2026-10-17T11:00:00Z"}"#;
const REVOKE_REPLY: &str = r#"{"id":"skey_00000000000000000000000003","account":"acc_00000000000000000000000001","fingerprint":"SHA256:RHFBqxmd4FLrdHDM2DGtlKdMxg/nN9xD3ZpkWHSvlaM","public_key":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMxZFwIQHLofF5C/SY2UPr6zgqjzUfcX6+XICjAhsvur","label":"laptop key","kind":"person","state":"revoked","expires_at":null,"bound_at":"2026-10-17T12:05:00Z","retired_at":null,"revoked_at":"2026-10-18T09:00:00Z","compromised_from":"2026-10-17T11:00:00Z"}"#;

// The OAuth replies of the settled device profile. The endpoint paths are this fixture's; the
// client reads whatever paths the metadata names.
const METADATA_REPLY: &str = r#"{"issuer":"{o}/auth","authorization_endpoint":"{o}/auth/authorize","device_authorization_endpoint":"{o}/auth/device","token_endpoint":"{o}/auth/token","revocation_endpoint":"{o}/auth/revoke","registration_endpoint":"{o}/auth/register","scopes_supported":["pull","push","upload","change"],"response_types_supported":["code"],"grant_types_supported":["authorization_code","urn:ietf:params:oauth:grant-type:device_code","refresh_token"],"code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none"]}"#;
const DEVICE_REPLY: &str = r#"{"device_code":"device-code-sentinel-7Hq2","user_code":"BCDFGHJK","verification_uri":"{o}/app/device","expires_in":600,"interval":5}"#;
const TOKEN_REPLY: &str = r#"{"access_token":"rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf","refresh_token":"rdyx_a_TESTVECTORTESTVECTORTESTVECTORTESTVECTOR2kpMVZ","token_type":"Bearer","expires_in":3600,"scope":"pull push upload change"}"#;

/// Builds one operation for a table row.
type OpMaker = fn() -> HubOperation;

fn agent_session(actor: &str, run: Option<&str>) -> Session {
    Session {
        actor: actor.to_string(),
        run: run.map(str::to_string),
        role: Role::Writer,
        kind: SessionKind::Agent,
    }
}

fn agent_claim() -> RequestClaim {
    RequestClaim::from_session(&agent_session("agent:claude", Some("r7"))).unwrap()
}

fn sub(template: &str, origin: &str) -> String {
    template.replace("{o}", origin)
}

/// A short name for a failure, for assertions.
fn kind(f: &HubFailure) -> String {
    match f {
        HubFailure::ActorClaim => "ActorClaim".into(),
        HubFailure::AuthRequired => "AuthRequired".into(),
        HubFailure::Refused => "Refused".into(),
        HubFailure::Transport { may_have_sent } => format!("Transport({may_have_sent})"),
        HubFailure::Protocol => "Protocol".into(),
        HubFailure::Local(e) => format!("Local({})", e.code),
    }
}

fn failure(r: Result<HubReply, HubFailure>) -> String {
    match r {
        Ok(reply) => panic!("expected a failure, got {reply:?}"),
        Err(f) => kind(&f),
    }
}

fn local(code: &str) -> String {
    format!("Local({code})")
}

/// Every operation, with the inputs of the settled fixtures.
fn all_operations() -> Vec<HubOperation> {
    vec![
        HubOperation::Metadata,
        HubOperation::DeviceStart {
            machine_name: MACHINE.into(),
        },
        HubOperation::DevicePoll {
            device_code: Secret::new(DEVICE_CODE),
        },
        HubOperation::Refresh {
            refresh_token: Secret::new(REFRESH),
        },
        HubOperation::Me,
        HubOperation::SignOut {
            token: Secret::new(REFRESH),
        },
        HubOperation::BindChallenge {
            kind: KeyKind::Person,
            label: Some("laptop key".into()),
        },
        HubOperation::Bind {
            challenge: CHALLENGE_ID.into(),
            public_key: PUBLIC_KEY.into(),
            proof: PROOF.into(),
        },
        HubOperation::FindKey {
            fingerprint: FINGERPRINT.into(),
        },
        HubOperation::Retire {
            key_id: KEY_ID.into(),
        },
        HubOperation::Revoke {
            key_id: KEY_ID.into(),
            compromised_from: Some(COMPROMISED.into()),
        },
    ]
}

/// The success reply of `op`'s settled fixture.
fn success(op: &HubOperation, origin: &str) -> Reply {
    match op {
        HubOperation::Metadata => Reply::json(200, &sub(METADATA_REPLY, origin)),
        HubOperation::DeviceStart { .. } => Reply::json(200, &sub(DEVICE_REPLY, origin)),
        HubOperation::DevicePoll { .. } | HubOperation::Refresh { .. } => {
            Reply::json(200, TOKEN_REPLY)
        }
        HubOperation::Me => Reply::json(200, ME_REPLY),
        HubOperation::SignOut { .. } => Reply::empty(200),
        HubOperation::BindChallenge { .. } => Reply::json(201, CHALLENGE_REPLY),
        HubOperation::Bind { .. } => Reply::json(201, &sub(BIND_REPLY, origin)),
        HubOperation::FindKey { .. } => Reply::json(200, LIST_REPLY),
        HubOperation::Retire { .. } => Reply::json(200, RETIRE_REPLY),
        HubOperation::Revoke { .. } => Reply::json(200, REVOKE_REPLY),
    }
}

/// The fixture's Bind with `public_key` and `proof`.
fn bind_with(public_key: &str, proof: &str) -> HubOperation {
    HubOperation::Bind {
        challenge: CHALLENGE_ID.into(),
        public_key: public_key.into(),
        proof: proof.into(),
    }
}

/// The padded standard base64 of `n` zero bytes.
fn zero_proof(n: usize) -> String {
    let mut s = "A".repeat(n / 3 * 4);
    match n % 3 {
        1 => s.push_str("AA=="),
        2 => s.push_str("AAA="),
        _ => {}
    }
    s
}

fn is_key_route(op: &HubOperation) -> bool {
    matches!(
        op,
        HubOperation::BindChallenge { .. }
            | HubOperation::Bind { .. }
            | HubOperation::FindKey { .. }
            | HubOperation::Retire { .. }
            | HubOperation::Revoke { .. }
    )
}

fn needs_bearer(op: &HubOperation) -> bool {
    is_key_route(op) || matches!(op, HubOperation::Me)
}

/// A fixture hub, a transport fenced to it and its CA file.
struct Hub {
    server: Server,
    install: Origin,
    transport: HttpHubTransport,
    dir: tempfile::TempDir,
    ca: PathBuf,
}

impl Hub {
    fn start() -> Hub {
        Hub::over(Server::start())
    }

    fn over(server: Server) -> Hub {
        let install = Origin::parse(server.origin()).unwrap();
        let transport = HttpHubTransport::new(OriginFence::new(install.clone(), []).unwrap())
            .expect("transport");
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, server.ca_pem()).unwrap();
        Hub {
            server,
            install,
            transport,
            dir,
            ca,
        }
    }

    fn origin(&self) -> String {
        self.install.as_str().to_string()
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.install.as_str())
    }

    /// A CA file holding `pem`.
    fn ca_file(&self, name: &str, pem: &[u8]) -> PathBuf {
        let p = self.dir.path().join(name);
        std::fs::write(&p, pem).unwrap();
        p
    }

    /// The call of `op` as a service would make it: the route or this fixture's metadata
    /// endpoint, the CA file, an agent claim on key routes and the access token where needed.
    fn call(&self, op: HubOperation) -> HubCall {
        let destination = match &op {
            HubOperation::DeviceStart { .. } => self.url("/auth/device"),
            HubOperation::DevicePoll { .. } | HubOperation::Refresh { .. } => {
                self.url("/auth/token")
            }
            HubOperation::SignOut { .. } => self.url("/auth/revoke"),
            // An input the route refuses gets a placeholder: the transport refuses it first.
            other => route(&self.install, other)
                .ok()
                .flatten()
                .unwrap_or_else(|| self.url("/")),
        };
        HubCall {
            install: self.install.clone(),
            destination,
            ca_file: Some(self.ca.clone()),
            claim: is_key_route(&op).then(agent_claim),
            bearer: needs_bearer(&op).then(|| Secret::new(ACCESS)),
            operation: op,
        }
    }

    /// Sends `call` with `reply` queued; returns the result and the request the hub read.
    fn send(&mut self, reply: Reply, call: HubCall) -> (Result<HubReply, HubFailure>, Recorded) {
        let before = self.server.requests().len();
        self.server.reply(reply);
        self.server.accept_n(1);
        let r = self.transport.exchange(call);
        let log = self.server.requests();
        assert_eq!(log.len(), before + 1, "one request per call: {r:?}");
        (r, log.last().unwrap().clone())
    }

    /// Sends `call` on one connection that must not deliver a request (a failed handshake).
    fn send_unread(&mut self, call: HubCall) -> Result<HubReply, HubFailure> {
        let before = self.server.requests().len();
        self.server.accept_n(1);
        let r = self.transport.exchange(call);
        assert_eq!(self.server.requests().len(), before, "no request: {r:?}");
        r
    }

    /// Calls with a reply queued for nothing: the call must fail before it connects.
    fn refuse(&self, call: HubCall) -> String {
        failure(self.transport.exchange(call))
    }

    fn finish(self) {
        assert_eq!(self.server.finish(), 0, "every queued reply was used");
    }
}

// ---------------------------------------------------------------------------------------------

#[test]
fn hsa4_each_operation_matches_the_hub_contract() {
    let mut hub = Hub::start();
    let o = hub.origin();
    let form = "application/x-www-form-urlencoded";
    let json = "application/json";
    for op in all_operations() {
        let name = format!("{op:?}");
        let mut call = hub.call(op.clone());
        // A claim and a bearer on a bootstrap route are never sent.
        call.claim.get_or_insert_with(agent_claim);
        call.bearer.get_or_insert_with(|| Secret::new(ACCESS));
        let (r, req) = hub.send(success(&op, &o), call);
        assert!(r.is_ok(), "{name}: {r:?}");
        assert_eq!(req.header("accept"), Some(json), "{name}");
        let claim = req.header("rdy-actor").is_some() || req.header("rdy-run").is_some();
        let bearer = req.header("authorization");
        let (method, target, ctype, body): (&str, String, Option<&str>, String) = match &op {
            HubOperation::Metadata => (
                "GET",
                "/.well-known/oauth-authorization-server/auth".into(),
                None,
                String::new(),
            ),
            HubOperation::DeviceStart { .. } => (
                "POST",
                "/auth/device".into(),
                Some(form),
                "client_id=rdy&scope=pull+push+upload+change&machine_name=laptop".into(),
            ),
            HubOperation::DevicePoll { .. } => (
                "POST",
                "/auth/token".into(),
                Some(form),
                format!(
                    "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code={DEVICE_CODE}&client_id=rdy"
                ),
            ),
            HubOperation::Refresh { .. } => (
                "POST",
                "/auth/token".into(),
                Some(form),
                format!("grant_type=refresh_token&refresh_token={REFRESH}&client_id=rdy"),
            ),
            HubOperation::Me => ("GET", "/api/v1/me".into(), None, String::new()),
            HubOperation::SignOut { .. } => (
                "POST",
                "/auth/revoke".into(),
                Some(form),
                format!("token={REFRESH}&client_id=rdy"),
            ),
            HubOperation::BindChallenge { .. } => (
                "POST",
                "/api/v1/me/keys/challenges".into(),
                Some(json),
                CHALLENGE_REQUEST.into(),
            ),
            HubOperation::Bind { .. } => (
                "POST",
                "/api/v1/me/keys".into(),
                Some(json),
                BIND_REQUEST.into(),
            ),
            HubOperation::FindKey { .. } => ("GET", FIND_TARGET.into(), None, String::new()),
            HubOperation::Retire { .. } => (
                "POST",
                format!("/api/v1/me/keys/{KEY_ID}/retire"),
                None,
                String::new(),
            ),
            HubOperation::Revoke { .. } => (
                "POST",
                format!("/api/v1/me/keys/{KEY_ID}/revoke"),
                Some(json),
                REVOKE_REQUEST.into(),
            ),
        };
        assert_eq!(req.method, method, "{name}");
        assert_eq!(req.target, target, "{name}");
        assert_eq!(req.header("content-type"), ctype, "{name}");
        assert_eq!(req.body_text(), body, "{name}");
        if body.is_empty() {
            let length = (method == "POST").then_some("0");
            assert_eq!(
                req.header("content-length"),
                length,
                "{name}: no body is sent"
            );
            assert_eq!(req.header("transfer-encoding"), None, "{name}");
        }
        let expected_bearer = needs_bearer(&op).then(|| format!("Bearer {ACCESS}"));
        assert_eq!(bearer, expected_bearer.as_deref(), "{name}");
        assert_eq!(claim, is_key_route(&op), "{name}: claim headers");
        for (n, v) in &req.headers {
            assert!(
                !n.contains("cookie") && n != "proxy-authorization",
                "{name}: {n}"
            );
            if n != "authorization" {
                assert!(!v.contains(ACCESS), "{name}: {n}");
            }
        }
        // No form member a profile omits is sent.
        assert!(!req.body_text().contains("token_type_hint"), "{name}");
        if matches!(op, HubOperation::Refresh { .. }) {
            assert!(!req.body_text().contains("scope"), "refresh sends no scope");
        }
        if matches!(op, HubOperation::Bind { .. }) {
            assert!(
                !req.body_text().contains("statement"),
                "no statement is sent"
            );
        }
    }

    // A challenge without a label sends only the kind; key_expiry is never sent.
    let call = hub.call(HubOperation::BindChallenge {
        kind: KeyKind::Agent,
        label: None,
    });
    let reply = CHALLENGE_REPLY.replace(
        r#""kind":"person","label":"laptop key""#,
        r#""kind":"agent","label":"agent key""#,
    );
    let (r, req) = hub.send(Reply::json(201, &reply), call);
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(req.body_text(), r#"{"kind":"agent"}"#);

    // A revoke without a time sends no body: the hub uses its request time.
    let call = hub.call(HubOperation::Revoke {
        key_id: KEY_ID.into(),
        compromised_from: None,
    });
    let (r, req) = hub.send(Reply::json(200, REVOKE_REPLY), call);
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(req.body, b"");
    assert_eq!(req.header("content-type"), None);
    assert_eq!(req.header("content-length"), Some("0"));
    assert_eq!(req.header("transfer-encoding"), None);

    // A compromise time is sent in its canonical UTC form.
    let call = hub.call(HubOperation::Revoke {
        key_id: KEY_ID.into(),
        compromised_from: Some("2026-10-17T13:00:00+02:00".into()),
    });
    let (r, req) = hub.send(Reply::json(200, REVOKE_REPLY), call);
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(req.body_text(), REVOKE_REQUEST);

    // A success status other than the operation's is a contract mismatch.
    for (op, status) in [
        (HubOperation::Me, 201),
        (
            HubOperation::BindChallenge {
                kind: KeyKind::Person,
                label: Some("laptop key".into()),
            },
            200,
        ),
        (
            HubOperation::Bind {
                challenge: CHALLENGE_ID.into(),
                public_key: PUBLIC_KEY.into(),
                proof: PROOF.into(),
            },
            200,
        ),
        (
            HubOperation::Retire {
                key_id: KEY_ID.into(),
            },
            204,
        ),
        (HubOperation::Metadata, 201),
        (
            HubOperation::SignOut {
                token: Secret::new(REFRESH),
            },
            204,
        ),
    ] {
        let body = match success(&op, &o) {
            Reply::Raw(b) => {
                let text = String::from_utf8(b).unwrap();
                text.split_once("\r\n\r\n").unwrap().1.to_string()
            }
            Reply::Close => unreachable!(),
        };
        let (r, _) = hub.send(Reply::json(status, &body), hub.call(op));
        assert_eq!(failure(r), "Protocol", "status {status}");
    }

    // A bodyless success fails where a DTO is required.
    for op in all_operations() {
        if matches!(op, HubOperation::SignOut { .. }) {
            continue;
        }
        let status = match op {
            HubOperation::BindChallenge { .. } | HubOperation::Bind { .. } => 201,
            _ => 200,
        };
        let (r, _) = hub.send(Reply::empty(status), hub.call(op));
        assert_eq!(failure(r), "Protocol");
    }

    // The hub's API errors: recognized pairs, mismatched pairs and unlisted codes.
    let api_error = |code: &str| {
        format!(
            r#"{{"code":"{code}","message":"m","fix":"f","issues":[],"request_id":"req_1","retry_after":null}}"#
        )
    };
    let cases: [(&str, u16, &str); 23] = [
        ("E_HUB_ACTOR_CLAIM", 403, "ActorClaim"),
        ("E_HUB_NO_CREDENTIAL", 401, "AuthRequired"),
        ("E_HUB_CREDENTIAL_UNKNOWN", 401, "AuthRequired"),
        ("E_HUB_CREDENTIAL_EXPIRED", 401, "AuthRequired"),
        ("E_HUB_CREDENTIAL_REVOKED", 401, "AuthRequired"),
        ("E_HUB_KEY_NOT_SIGNING", 422, "Refused"),
        ("E_HUB_KEY_CHALLENGE", 422, "Refused"),
        ("E_HUB_KEY_PROOF", 422, "Refused"),
        ("E_HUB_KEY_TYPE", 422, "Refused"),
        ("E_HUB_KEY_LIMIT", 422, "Refused"),
        ("E_HUB_VALIDATION", 422, "Refused"),
        ("E_HUB_KEY_TAKEN", 409, "Refused"),
        ("E_HUB_KEY_STATE", 409, "Refused"),
        ("E_HUB_NOT_FOUND", 404, "Refused"),
        ("E_HUB_KEY_ACTOR", 403, "Refused"),
        ("E_HUB_OUT_OF_REACH", 403, "Refused"),
        ("E_HUB_BAD_INPUT", 400, "Refused"),
        // a recognized code at another status
        ("E_HUB_ACTOR_CLAIM", 401, "Protocol"),
        ("E_HUB_NO_CREDENTIAL", 403, "Protocol"),
        ("E_HUB_KEY_TAKEN", 422, "Protocol"),
        ("E_HUB_BAD_INPUT", 422, "Protocol"),
        // an unlisted code
        ("E_HUB_SOMETHING_NEW", 418, "Refused"),
        ("E_HUB_RATE", 429, "Refused"),
    ];
    for (code, status, want) in cases {
        let call = hub.call(HubOperation::Retire {
            key_id: KEY_ID.into(),
        });
        let (r, _) = hub.send(Reply::json(status, &api_error(code)), call);
        assert_eq!(failure(r), want, "{code}/{status}");
    }
    for body in [
        "",
        "not json",
        r#"{"message":"no code"}"#,
        r#"{"code":7}"#,
        r#"[]"#,
    ] {
        let call = hub.call(HubOperation::Me);
        let (r, _) = hub.send(Reply::json(401, body), call);
        assert_eq!(failure(r), "Protocol", "{body}");
    }

    // OAuth errors: the profile's codes per endpoint and status.
    let oauth = |code: &str| {
        format!(r#"{{"error":"{code}","error_description":"d","error_uri":"{o}/doc"}}"#)
    };
    let poll = || HubOperation::DevicePoll {
        device_code: Secret::new(DEVICE_CODE),
    };
    let refresh = || HubOperation::Refresh {
        refresh_token: Secret::new(REFRESH),
    };
    let start = || HubOperation::DeviceStart {
        machine_name: MACHINE.into(),
    };
    let sign_out = || HubOperation::SignOut {
        token: Secret::new(REFRESH),
    };
    let ok = |r: Result<HubReply, HubFailure>| format!("{:?}", r.expect("a reply"));
    let (r, _) = hub.send(
        Reply::json(400, &oauth("authorization_pending")),
        hub.call(poll()),
    );
    assert_eq!(ok(r), "DevicePoll(Pending)");
    let (r, _) = hub.send(Reply::json(400, &oauth("slow_down")), hub.call(poll()));
    assert_eq!(ok(r), "DevicePoll(SlowDown)");
    let (r, _) = hub.send(Reply::json(400, &oauth("expired_token")), hub.call(poll()));
    assert_eq!(ok(r), "DevicePoll(Expired)");
    let (r, _) = hub.send(Reply::json(400, &oauth("invalid_grant")), hub.call(poll()));
    assert_eq!(ok(r), "DevicePoll(InvalidGrant)");
    let (r, _) = hub.send(
        Reply::json(400, &oauth("invalid_grant")),
        hub.call(refresh()),
    );
    assert_eq!(ok(r), "Refresh(AuthRequired)");
    let refused: [(OpMaker, u16, &str, &str); 14] = [
        (poll, 400, "access_denied", "Refused"),
        (poll, 400, "invalid_request", "Refused"),
        (poll, 400, "unsupported_grant_type", "Refused"),
        (poll, 401, "invalid_client", "Refused"),
        (poll, 400, "something_new", "Refused"),
        (poll, 401, "authorization_pending", "Protocol"),
        (poll, 400, "invalid_client", "Protocol"),
        (refresh, 401, "invalid_grant", "Protocol"),
        (refresh, 400, "invalid_request", "Refused"),
        (start, 400, "invalid_request", "Refused"),
        (start, 400, "invalid_scope", "Refused"),
        (start, 401, "invalid_client", "Refused"),
        (sign_out, 400, "invalid_request", "Refused"),
        (sign_out, 401, "invalid_client", "Refused"),
    ];
    for (op, status, code, want) in refused {
        let (r, _) = hub.send(Reply::json(status, &oauth(code)), hub.call(op()));
        assert_eq!(failure(r), want, "{code}/{status}");
    }
    for body in [
        "",
        "<html>",
        r#"{"error":7}"#,
        r#"{"code":"E_HUB_BAD_INPUT"}"#,
    ] {
        let (r, _) = hub.send(Reply::json(400, body), hub.call(poll()));
        assert_eq!(failure(r), "Protocol", "{body}");
    }

    // Token replies need both tokens, a Bearer type (any case) and a 1-3600 second life.
    for (body, want) in [
        (TOKEN_REPLY.replace("\"Bearer\"", "\"bearer\""), "ok"),
        (TOKEN_REPLY.replace("\"Bearer\"", "\"BEARER\""), "ok"),
        (TOKEN_REPLY.replace("\"Bearer\"", "\"DPoP\""), "Protocol"),
        (TOKEN_REPLY.replace("3600", "3601"), "Protocol"),
        (TOKEN_REPLY.replace("3600", "0"), "Protocol"),
        (TOKEN_REPLY.replace(REFRESH_2, "rdyh_r_short"), "Protocol"),
        (TOKEN_REPLY.replace(ACCESS, BAD_TOKENS[0]), "Protocol"),
        (
            TOKEN_REPLY.replace(r#","scope":"pull push upload change""#, ""),
            "ok",
        ),
        (
            TOKEN_REPLY.replace(r#","token_type":"Bearer""#, ""),
            "Protocol",
        ),
    ] {
        let (r, _) = hub.send(Reply::json(200, &body), hub.call(refresh()));
        match want {
            "ok" => assert!(r.is_ok(), "{body}: {r:?}"),
            _ => assert_eq!(failure(r), want, "{body}"),
        }
    }
    hub.finish();
}

#[test]
fn hsa4_oauth_endpoints_come_only_from_metadata() {
    let mut hub = Hub::start();
    let o = hub.origin();
    // Whatever paths under /auth/ the metadata names are the endpoints.
    let varied = sub(METADATA_REPLY, &o)
        .replace("/auth/device\"", "/auth/v2/device-start\"")
        .replace("/auth/token\"", "/auth/x/tok\"")
        .replace("/auth/revoke\"", "/auth/x/rev\"");
    let (r, req) = hub.send(Reply::json(200, &varied), hub.call(HubOperation::Metadata));
    assert_eq!(req.target, "/.well-known/oauth-authorization-server/auth");
    let Ok(HubReply::Metadata(m)) = r else {
        panic!("{r:?}")
    };
    assert_eq!(
        m,
        HubMetadata {
            issuer: format!("{o}/auth"),
            device_authorization_endpoint: format!("{o}/auth/v2/device-start"),
            token_endpoint: format!("{o}/auth/x/tok"),
            revocation_endpoint: format!("{o}/auth/x/rev"),
        }
    );
    for (op, endpoint, reply) in [
        (
            HubOperation::DeviceStart {
                machine_name: MACHINE.into(),
            },
            m.device_authorization_endpoint.clone(),
            sub(DEVICE_REPLY, &o),
        ),
        (
            HubOperation::DevicePoll {
                device_code: Secret::new(DEVICE_CODE),
            },
            m.token_endpoint.clone(),
            TOKEN_REPLY.to_string(),
        ),
        (
            HubOperation::SignOut {
                token: Secret::new(REFRESH),
            },
            m.revocation_endpoint.clone(),
            String::new(),
        ),
    ] {
        let mut call = hub.call(op);
        call.destination = endpoint.clone();
        let (r, req) = hub.send(Reply::json(200, &reply), call);
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(hub.url(&req.target), endpoint);
    }

    // Metadata that names a foreign issuer or endpoint, or lacks the profile, is refused.
    let base = sub(METADATA_REPLY, &o);
    let other = "https://127.0.0.1:1";
    let bad: Vec<String> = vec![
        base.replace(
            &format!(r#""issuer":"{o}/auth""#),
            &format!(r#""issuer":"{other}/auth""#),
        ),
        base.replace(
            &format!(r#""issuer":"{o}/auth""#),
            &format!(r#""issuer":"{o}/auth/""#),
        ),
        base.replace(
            &format!(r#""issuer":"{o}/auth""#),
            &format!(r#""issuer":"{o}""#),
        ),
        base.replace(
            &format!("{o}/auth/token\""),
            &format!("{other}/auth/token\""),
        ),
        base.replace(&format!("{o}/auth/token\""), &format!("{o}/api/v1/me\"")),
        base.replace(&format!("{o}/auth/token\""), &format!("{o}/auth/\"")),
        base.replace(&format!("{o}/auth/token\""), &format!("{o}/auth\"")),
        base.replace(
            &format!("{o}/auth/token\""),
            &format!("{o}/auth/../api/v1/me\""),
        ),
        base.replace(
            &format!("{o}/auth/token\""),
            &format!("{o}/auth/%2e%2e/api\""),
        ),
        base.replace(&format!("{o}/auth/token\""), &format!("{o}/auth/./token\"")),
        base.replace(&format!("{o}/auth/token\""), &format!("{o}/auth//token\"")),
        base.replace(
            &format!("{o}/auth/token\""),
            &format!("{o}/auth/token?x=1\""),
        ),
        base.replace(&format!("{o}/auth/token\""), &format!("{o}/auth/token#x\"")),
        base.replace(
            &format!("{o}/auth/device\""),
            &format!("{}/auth/device\"", o.replace("https://", "https://u@")),
        ),
        base.replace(
            &format!("{o}/auth/revoke\""),
            &format!("{}/auth/revoke\"", o.replace("https://", "http://")),
        ),
        base.replace(&format!("{o}/auth/revoke\""), "/auth/revoke\""),
        base.replace(r#""urn:ietf:params:oauth:grant-type:device_code","#, ""),
        base.replace(r#","refresh_token"]"#, "]"),
        base.replace(r#""S256""#, r#""plain""#),
        base.replace(r#"["none"]"#, r#"["client_secret_basic"]"#),
        base.replace(&format!(r#""revocation_endpoint":"{o}/auth/revoke","#), ""),
        base.replace(r#","code_challenge_methods_supported":["S256"]"#, ""),
    ];
    for body in bad {
        let (r, _) = hub.send(Reply::json(200, &body), hub.call(HubOperation::Metadata));
        assert_eq!(failure(r), "Protocol", "{body}");
    }

    // A credential exchange goes only to a same-install endpoint under /auth/; there is no
    // fallback path.
    for (dest, want) in [
        (hub.url("/api/v1/me"), local(E_UPSTREAM_PROTOCOL)),
        (hub.url("/auth/"), local(E_UPSTREAM_PROTOCOL)),
        (hub.url("/auth/../api/v1/me"), local(E_UPSTREAM_PROTOCOL)),
        (hub.url("/auth/%2e%2e/api"), local(E_UPSTREAM_PROTOCOL)),
        (hub.url("/auth/./token"), local(E_UPSTREAM_PROTOCOL)),
        (hub.url("/auth/token?grant=1"), local(E_UPSTREAM_PROTOCOL)),
        (hub.url("/auth/token#x"), local(E_UPSTREAM_ORIGIN)),
        (
            "https://127.0.0.1:1/auth/token".to_string(),
            local(E_UPSTREAM_ORIGIN),
        ),
        (
            o.replace("https://", "http://") + "/auth/token",
            local(E_UPSTREAM_ORIGIN),
        ),
    ] {
        let mut call = hub.call(HubOperation::DevicePoll {
            device_code: Secret::new(DEVICE_CODE),
        });
        call.destination = dest.clone();
        assert_eq!(hub.refuse(call), want, "{dest}");
    }
    // Fixed routes are the operation's own.
    for dest in [
        hub.url("/api/v1/me/"),
        hub.url("/auth/me"),
        hub.url("/api/v1/me?x=1"),
    ] {
        let mut call = hub.call(HubOperation::Me);
        call.destination = dest.clone();
        assert_eq!(hub.refuse(call), local(E_UPSTREAM_PROTOCOL), "{dest}");
    }
    assert_eq!(
        route(&hub.install, &HubOperation::Metadata).unwrap(),
        Some(hub.url("/.well-known/oauth-authorization-server/auth"))
    );
    for op in [
        HubOperation::DeviceStart {
            machine_name: MACHINE.into(),
        },
        HubOperation::DevicePoll {
            device_code: Secret::new(DEVICE_CODE),
        },
        HubOperation::Refresh {
            refresh_token: Secret::new(REFRESH),
        },
        HubOperation::SignOut {
            token: Secret::new(REFRESH),
        },
    ] {
        assert_eq!(route(&hub.install, &op).unwrap(), None, "{op:?}");
    }
    hub.finish();
}

#[test]
fn hsa4_verification_links_never_hold_the_code() {
    let mut hub = Hub::start();
    let o = hub.origin();
    let start = || HubOperation::DeviceStart {
        machine_name: MACHINE.into(),
    };
    let good = sub(DEVICE_REPLY, &o);
    let (r, _) = hub.send(Reply::json(200, &good), hub.call(start()));
    let Ok(HubReply::DeviceStart(d)) = r else {
        panic!("{r:?}")
    };
    assert_eq!(d.verification_uri, format!("{o}/app/device"));
    assert!(!d.verification_uri.contains(USER_CODE));
    assert_eq!(d.user_code, USER_CODE);
    assert_eq!(d.device_code.expose(), DEVICE_CODE);
    assert_eq!((d.expires_in, d.interval), (600, 5));

    let uri = format!(r#""verification_uri":"{o}/app/device""#);
    let bad = vec![
        // a complete URI is refused even though OAuth allows it, and even as null
        good.replace(
            &uri,
            &format!(r#"{uri},"verification_uri_complete":"{o}/app/device?user_code={USER_CODE}""#),
        ),
        good.replace(&uri, &format!(r#"{uri},"verification_uri_complete":null"#)),
        good.replace(
            &uri,
            &format!(r#""verification_uri":"{o}/app/device?user_code={USER_CODE}""#),
        ),
        good.replace(
            &uri,
            &format!(r#""verification_uri":"{o}/app/device#{USER_CODE}""#),
        ),
        good.replace(
            &uri,
            &format!(r#""verification_uri":"{o}/app/device/{USER_CODE}""#),
        ),
        good.replace(&uri, &format!(r#""verification_uri":"{o}/app/device?""#)),
        good.replace(
            &uri,
            r#""verification_uri":"https://127.0.0.1:1/app/device""#,
        ),
        good.replace(&uri, &format!(r#""verification_uri":"{o}/app/device/""#)),
        // Dot segments, literal or encoded, normalize away: the link must be exactly the
        // install's device page, so a code hidden in a removed segment never comes back.
        good.replace(
            &uri,
            &format!(r#""verification_uri":"{o}/app/{USER_CODE}/../device""#),
        ),
        good.replace(
            &uri,
            &format!(r#""verification_uri":"{o}/app/{USER_CODE}/%2e%2e/device""#),
        ),
        good.replace(
            &uri,
            &format!(r#""verification_uri":"{o}/app/{USER_CODE}/%2E%2E/device""#),
        ),
        good.replace(
            &uri,
            &format!(r#""verification_uri":"{o}/app/{USER_CODE}/.%2e/device""#),
        ),
        good.replace(&uri, &format!(r#""verification_uri":"{o}/app/./device""#)),
        good.replace(&uri, &format!(r#""verification_uri":"{o}/app/%2e/device""#)),
        good.replace(
            &uri,
            &format!(r#""verification_uri":"{o}/{USER_CODE}/../app/device""#),
        ),
        good.replace(USER_CODE, "BCDFGHJ"),
        good.replace(USER_CODE, "BCDFGHJA"),
        good.replace(USER_CODE, "bcdfghjk"),
        good.replace("600", "601"),
        good.replace("600", "0"),
        good.replace(r#""interval":5"#, r#""interval":4"#),
        good.replace(DEVICE_CODE, ""),
        good.replace(r#""user_code":"BCDFGHJK","#, ""),
    ];
    for body in bad {
        let (r, _) = hub.send(Reply::json(200, &body), hub.call(start()));
        assert_eq!(failure(r), "Protocol", "{body}");
    }
    hub.finish();
}

#[test]
fn hsa4_a_user_ca_is_required_and_hostname_checks_remain() {
    let mut hub = Hub::start();
    // The fixture's CA is in no platform store: without the user CA the handshake fails before
    // any request byte is sent.
    let mut call = hub.call(HubOperation::Me);
    call.ca_file = None;
    assert_eq!(failure(hub.send_unread(call)), "Transport(false)");
    // A different CA is no better.
    let other = Ca::new();
    let mut call = hub.call(HubOperation::Me);
    call.ca_file = Some(hub.ca_file("other.pem", other.pem().as_bytes()));
    assert_eq!(failure(hub.send_unread(call)), "Transport(false)");
    // The right CA works, also inside a bundle of 64 certificates.
    let (r, _) = hub.send(Reply::json(200, ME_REPLY), hub.call(HubOperation::Me));
    assert!(r.is_ok(), "{r:?}");
    let mut bundle = String::new();
    for _ in 0..63 {
        bundle.push_str(other.pem());
    }
    bundle.push_str(hub.server.ca_pem());
    let mut call = hub.call(HubOperation::Me);
    call.ca_file = Some(hub.ca_file("bundle.pem", bundle.as_bytes()));
    let (r, _) = hub.send(Reply::json(200, ME_REPLY), call);
    assert!(r.is_ok(), "{r:?}");

    // A CA file that cannot be used fails before connecting.
    let right = hub.server.ca_pem().to_string();
    let key_block = rcgen::KeyPair::generate().unwrap().serialize_pem();
    let mut too_many = String::new();
    for _ in 0..65 {
        too_many.push_str(&right);
    }
    let mut too_big = right.clone().into_bytes();
    too_big.extend(std::iter::repeat_n(b'\n', MAX_CA_BYTES + 1 - too_big.len()));
    let mut just_fits = right.clone().into_bytes();
    just_fits.extend(std::iter::repeat_n(b'\n', MAX_CA_BYTES - just_fits.len()));
    let not_der = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
    let bad_base64 = "-----BEGIN CERTIFICATE-----\n@@@@\n-----END CERTIFICATE-----\n";
    // Certificate-shaped DER the TLS library cannot read as a trust anchor: SEQUENCE {SEQUENCE
    // {}} and SEQUENCE {SEQUENCE {INTEGER 1}}. Its trust store would drop them silently.
    let empty_tbs = "-----BEGIN CERTIFICATE-----\nMAIwAA==\n-----END CERTIFICATE-----\n";
    let serial_only = "-----BEGIN CERTIFICATE-----\nMAYwBAICAAE=\n-----END CERTIFICATE-----\n";
    let unusable: Vec<(PathBuf, &str)> = vec![
        (hub.dir.path().join("missing.pem"), "missing"),
        (PathBuf::from("relative/ca.pem"), "relative"),
        (hub.dir.path().to_path_buf(), "a directory"),
        (hub.ca_file("empty.pem", b""), "empty"),
        (hub.ca_file("text.pem", b"not a certificate"), "not PEM"),
        (
            hub.ca_file("key.pem", format!("{right}{key_block}").as_bytes()),
            "a private key block",
        ),
        (hub.ca_file("many.pem", too_many.as_bytes()), "65 certificates"),
        (hub.ca_file("big.pem", &too_big), "over 1 MiB"),
        (hub.ca_file("der.pem", not_der.as_bytes()), "not DER"),
        (hub.ca_file("b64.pem", bad_base64.as_bytes()), "bad base64"),
        (
            hub.ca_file("empty-tbs.pem", empty_tbs.as_bytes()),
            "an empty signed part",
        ),
        (
            hub.ca_file("serial.pem", serial_only.as_bytes()),
            "a signed part with only a serial",
        ),
        (
            hub.ca_file("mixed.pem", format!("{right}{empty_tbs}").as_bytes()),
            "one good and one invalid certificate",
        ),
        (
            hub.ca_file("mixed2.pem", format!("{serial_only}{right}").as_bytes()),
            "one invalid and one good certificate",
        ),
        (
            hub.ca_file(
                "csr.pem",
                format!("{right}-----BEGIN CERTIFICATE REQUEST-----\nAAAA\n-----END CERTIFICATE REQUEST-----\n").as_bytes(),
            ),
            "another block",
        ),
    ];
    for (path, why) in unusable {
        let mut call = hub.call(HubOperation::Me);
        call.ca_file = Some(path);
        assert_eq!(hub.refuse(call), local(E_UPSTREAM_CA), "{why}");
    }
    let mut call = hub.call(HubOperation::Me);
    call.ca_file = Some(hub.ca_file("fits.pem", &just_fits));
    let (r, _) = hub.send(Reply::json(200, ME_REPLY), call);
    assert!(r.is_ok(), "1 MiB exactly: {r:?}");
    hub.finish();

    // A chain from the trusted CA for another name fails the hostname check.
    let mut wrong = Hub::over(Server::with_names(&["127.0.0.2"]));
    assert_eq!(
        failure(wrong.send_unread(wrong.call(HubOperation::Me))),
        "Transport(false)"
    );
    wrong.finish();
}

#[test]
fn hsa4_redirects_and_cross_install_destinations_never_receive_a_token() {
    let mut a = Hub::start();
    let b = Hub::start();
    let b_me = b.url("/api/v1/me");
    // A redirect is a contract mismatch and is never followed.
    for (status, op) in [
        (302, HubOperation::Me),
        (301, HubOperation::Metadata),
        (
            307,
            HubOperation::Bind {
                challenge: CHALLENGE_ID.into(),
                public_key: PUBLIC_KEY.into(),
                proof: PROOF.into(),
            },
        ),
        (
            308,
            HubOperation::DevicePoll {
                device_code: Secret::new(DEVICE_CODE),
            },
        ),
        (
            303,
            HubOperation::Refresh {
                refresh_token: Secret::new(REFRESH),
            },
        ),
    ] {
        let reply = Reply::with_headers(
            status,
            &[
                format!("location: {b_me}"),
                "content-length: 0".into(),
                "connection: close".into(),
            ],
            b"",
        );
        let (r, _) = a.send(reply, a.call(op));
        assert_eq!(failure(r), "Protocol", "{status}");
    }
    assert_eq!(a.server.requests().len(), 5, "one request per call");

    // A destination on another install is refused before connecting, pinned or not.
    let mut call = a.call(HubOperation::Me);
    call.destination = b_me.clone();
    assert_eq!(a.refuse(call), local(E_UPSTREAM_ORIGIN));
    let mut call = a.call(HubOperation::DevicePoll {
        device_code: Secret::new(DEVICE_CODE),
    });
    call.destination = b.url("/auth/token");
    assert_eq!(a.refuse(call), local(E_UPSTREAM_ORIGIN));
    // Even when A pins B's origin, credential and account routes stay on the install.
    let pinned =
        HttpHubTransport::new(OriginFence::new(a.install.clone(), [b.install.clone()]).unwrap())
            .unwrap();
    for op in all_operations() {
        let mut call = a.call(op.clone());
        let path = call
            .destination
            .strip_prefix(a.install.as_str())
            .unwrap()
            .to_string();
        call.destination = b.url(&path);
        let e = pinned.exchange(call).expect_err("refused");
        let HubFailure::Local(e) = e else {
            panic!("{op:?}: {}", kind(&e))
        };
        assert_eq!(e.code, E_UPSTREAM_ORIGIN, "{op:?}");
        assert!(e.message.contains(a.install.as_str()), "{}", e.message);
    }
    // A call for another install than the transport's fence is refused.
    let mut call = b.call(HubOperation::Me);
    call.ca_file = Some(a.ca.clone());
    assert_eq!(a.refuse(call), local(E_UPSTREAM_ORIGIN));
    // Metadata from A that names B's endpoints is refused.
    let foreign = sub(METADATA_REPLY, &a.origin()).replace(
        &format!("{}/auth/token\"", a.origin()),
        &format!("{}/auth/token\"", b.origin()),
    );
    let (r, _) = a.send(Reply::json(200, &foreign), a.call(HubOperation::Metadata));
    assert_eq!(failure(r), "Protocol");
    a.finish();
    // B never saw a connection.
    b.finish();
}

#[test]
fn hsa4_every_request_keeps_its_caller_claim() {
    let mut hub = Hub::start();
    let key_ops: Vec<HubOperation> = all_operations().into_iter().filter(is_key_route).collect();
    let o = hub.origin();
    let claims = [
        (
            RequestClaim::from_session(&agent_session("agent:claude", Some("r7"))).unwrap(),
            "agent:claude",
            Some("r7"),
        ),
        (
            RequestClaim::from_session(&agent_session("agent:claude", None)).unwrap(),
            "agent:claude",
            None,
        ),
        (
            RequestClaim::from_session(&agent_session("agent:Клод", Some("прогон-1"))).unwrap(),
            "agent:Клод",
            Some("прогон-1"),
        ),
        (
            RequestClaim::from_session(&agent_session("ci:nightly", Some("build 7"))).unwrap(),
            "ci:nightly",
            Some("build 7"),
        ),
        (
            RequestClaim::from_session(&Session::human("human:nick".into(), Role::Writer))
                .unwrap()
                .bind_human("test-person")
                .unwrap(),
            "human:test-person",
            Some("cli"),
        ),
    ];
    for (claim, actor, run) in &claims {
        for op in &key_ops {
            let mut call = hub.call(op.clone());
            call.claim = Some(claim.clone());
            let (r, req) = hub.send(success(op, &o), call);
            assert!(r.is_ok(), "{op:?}: {r:?}");
            assert_eq!(req.header("rdy-actor"), Some(*actor), "{op:?}");
            assert_eq!(req.header("rdy-run"), *run, "{op:?}");
            let actor_lines = req
                .header_names()
                .iter()
                .filter(|n| **n == "rdy-actor")
                .count();
            assert_eq!(actor_lines, 1);
        }
    }
    // Bootstrap routes omit the claim headers, whatever the call holds.
    for op in all_operations().into_iter().filter(|op| !is_key_route(op)) {
        let mut call = hub.call(op.clone());
        call.claim = Some(claims[0].0.clone());
        let (r, req) = hub.send(success(&op, &o), call);
        assert!(r.is_ok(), "{op:?}: {r:?}");
        assert_eq!(req.header("rdy-actor"), None, "{op:?}");
        assert_eq!(req.header("rdy-run"), None, "{op:?}");
    }
    // A key route needs a claim that is valid to send; nothing is sent otherwise.
    let unbound_human =
        RequestClaim::from_session(&Session::human("human:nick".into(), Role::Writer)).unwrap();
    let mut forged = claims[0].0.clone();
    forged.actor = "human:test-person".into();
    let mut bad_run = claims[0].0.clone();
    bad_run.run = Some("r\n7".into());
    let mut long_actor = claims[0].0.clone();
    long_actor.actor = format!("agent:{}", "a".repeat(MAX_ACTOR_BYTES));
    for op in &key_ops {
        for (claim, want) in [
            (None, local(E_ACTOR_UNBOUND)),
            (Some(unbound_human.clone()), local(E_ACTOR_UNBOUND)),
            (Some(forged.clone()), local(E_ACTOR_UNBOUND)),
            (Some(bad_run.clone()), local(E_ACTOR_UNBOUND)),
            (Some(long_actor.clone()), local(E_LIMIT)),
        ] {
            let mut call = hub.call(op.clone());
            call.claim = claim;
            assert_eq!(hub.refuse(call), want, "{op:?}");
        }
    }
    hub.finish();
}

#[test]
fn hsa4_bad_tokens_are_refused_before_connect() {
    let mut hub = Hub::start();
    for op in all_operations().into_iter().filter(needs_bearer) {
        for bad in BAD_TOKENS {
            let mut call = hub.call(op.clone());
            call.bearer = Some(Secret::new(bad));
            assert_eq!(hub.refuse(call), local(E_UPSTREAM_TOKEN), "{op:?}");
        }
        let mut call = hub.call(op.clone());
        call.bearer = None;
        assert_eq!(hub.refuse(call), local(E_UPSTREAM_AUTH), "{op:?}");
    }
    for bad in BAD_TOKENS {
        let call = hub.call(HubOperation::Refresh {
            refresh_token: Secret::new(bad),
        });
        assert_eq!(hub.refuse(call), local(E_UPSTREAM_TOKEN));
        let call = hub.call(HubOperation::SignOut {
            token: Secret::new(bad),
        });
        assert_eq!(hub.refuse(call), local(E_UPSTREAM_TOKEN));
    }
    // Device codes, machine names, labels and key inputs are bounded before connecting.
    let refusals: Vec<(HubOperation, String)> = vec![
        (
            HubOperation::DevicePoll {
                device_code: Secret::new(""),
            },
            local(E_UPSTREAM_PROTOCOL),
        ),
        (
            HubOperation::DevicePoll {
                device_code: Secret::new("code\nmore"),
            },
            local(E_UPSTREAM_PROTOCOL),
        ),
        (
            HubOperation::DevicePoll {
                device_code: Secret::new("d".repeat(MAX_DEVICE_CODE_BYTES + 1)),
            },
            local(E_LIMIT),
        ),
        (
            HubOperation::DeviceStart {
                machine_name: " laptop".into(),
            },
            local(E_BAD_INPUT),
        ),
        (
            HubOperation::DeviceStart {
                machine_name: "m".repeat(65),
            },
            local(E_BAD_INPUT),
        ),
        (
            HubOperation::BindChallenge {
                kind: KeyKind::Person,
                label: Some("l".repeat(65)),
            },
            local(E_BAD_INPUT),
        ),
        (
            HubOperation::BindChallenge {
                kind: KeyKind::Person,
                label: Some(String::new()),
            },
            local(E_BAD_INPUT),
        ),
        (
            HubOperation::Bind {
                challenge: "c".repeat(MAX_ID_BYTES + 1),
                public_key: PUBLIC_KEY.into(),
                proof: PROOF.into(),
            },
            local(E_LIMIT),
        ),
        (
            HubOperation::Bind {
                challenge: CHALLENGE_ID.into(),
                public_key: format!("{PUBLIC_KEY} laptop"),
                proof: PROOF.into(),
            },
            local(E_UPSTREAM_KEY_PROOF),
        ),
        (
            HubOperation::Bind {
                challenge: CHALLENGE_ID.into(),
                public_key: PUBLIC_KEY.into(),
                proof: "not base64!".into(),
            },
            local(E_UPSTREAM_KEY_PROOF),
        ),
        (
            HubOperation::Bind {
                challenge: CHALLENGE_ID.into(),
                public_key: PUBLIC_KEY.into(),
                proof: "QUFB".repeat(MAX_SIG_PROOF_BYTES),
            },
            local(E_LIMIT),
        ),
        // The proof bound is on the decoded bytes: 16385 and 16386 bytes have the same base64
        // length (21848) as 16384.
        (bind_with(PUBLIC_KEY, &zero_proof(16385)), local(E_LIMIT)),
        (bind_with(PUBLIC_KEY, &zero_proof(16386)), local(E_LIMIT)),
        // Padding only at the end, at most two, and no unused bits set.
        (bind_with(PUBLIC_KEY, "AB=="), local(E_UPSTREAM_KEY_PROOF)),
        (bind_with(PUBLIC_KEY, "AAB="), local(E_UPSTREAM_KEY_PROOF)),
        (bind_with(PUBLIC_KEY, "A==="), local(E_UPSTREAM_KEY_PROOF)),
        (bind_with(PUBLIC_KEY, "===="), local(E_UPSTREAM_KEY_PROOF)),
        (bind_with(PUBLIC_KEY, "AA=A"), local(E_UPSTREAM_KEY_PROOF)),
        (
            bind_with(PUBLIC_KEY, &format!("{}\n", &PROOF[..64])),
            local(E_UPSTREAM_KEY_PROOF),
        ),
        // The public key is an OpenSSH line with no comment: never a fingerprint, an empty
        // field, other white space or another line.
        (bind_with(FINGERPRINT, PROOF), local(E_UPSTREAM_KEY_PROOF)),
        (
            bind_with(&format!("{FINGERPRINT} "), PROOF),
            local(E_UPSTREAM_KEY_PROOF),
        ),
        (
            bind_with(&format!("{PUBLIC_KEY} "), PROOF),
            local(E_UPSTREAM_KEY_PROOF),
        ),
        (
            bind_with(&format!(" {PUBLIC_KEY}"), PROOF),
            local(E_UPSTREAM_KEY_PROOF),
        ),
        (
            bind_with(&PUBLIC_KEY.replacen(' ', "  ", 1), PROOF),
            local(E_UPSTREAM_KEY_PROOF),
        ),
        (
            bind_with(&PUBLIC_KEY.replacen(' ', "\t", 1), PROOF),
            local(E_UPSTREAM_KEY_PROOF),
        ),
        (
            bind_with(&format!("{PUBLIC_KEY}\n"), PROOF),
            local(E_UPSTREAM_KEY_PROOF),
        ),
        (
            bind_with(&format!("{PUBLIC_KEY}\n{PUBLIC_KEY}"), PROOF),
            local(E_UPSTREAM_KEY_PROOF),
        ),
        (bind_with("", PROOF), local(E_UPSTREAM_KEY_PROOF)),
        (
            bind_with("ssh-ed25519 ", PROOF),
            local(E_UPSTREAM_KEY_PROOF),
        ),
        // Ids name their object: a challenge is `kch_…` and a key `skey_…`.
        (
            HubOperation::Bind {
                challenge: ACCOUNT.into(),
                public_key: PUBLIC_KEY.into(),
                proof: PROOF.into(),
            },
            local(E_UPSTREAM_PROTOCOL),
        ),
        (
            HubOperation::Bind {
                challenge: "kch_".into(),
                public_key: PUBLIC_KEY.into(),
                proof: PROOF.into(),
            },
            local(E_UPSTREAM_PROTOCOL),
        ),
        (
            HubOperation::Retire {
                key_id: ACCOUNT.into(),
            },
            local(E_UPSTREAM_PROTOCOL),
        ),
        (
            HubOperation::Revoke {
                key_id: CHALLENGE_ID.into(),
                compromised_from: None,
            },
            local(E_UPSTREAM_PROTOCOL),
        ),
        (
            HubOperation::Retire {
                key_id: "skey_".into(),
            },
            local(E_UPSTREAM_PROTOCOL),
        ),
        (
            HubOperation::FindKey {
                fingerprint: "SHA256:short".into(),
            },
            local(E_BAD_INPUT),
        ),
        (
            HubOperation::Retire {
                key_id: "skey_1\n".into(),
            },
            local(E_UPSTREAM_PROTOCOL),
        ),
        (
            HubOperation::Retire {
                key_id: "..".into(),
            },
            local(E_UPSTREAM_PROTOCOL),
        ),
        (
            HubOperation::Retire {
                key_id: "k".repeat(MAX_ID_BYTES + 1),
            },
            local(E_LIMIT),
        ),
        (
            HubOperation::Revoke {
                key_id: KEY_ID.into(),
                compromised_from: Some("yesterday".into()),
            },
            local(E_BAD_INPUT),
        ),
    ];
    for (op, want) in refusals {
        let name: String = format!("{op:?}").chars().take(240).collect();
        assert_eq!(hub.refuse(hub.call(op)), want, "{name}");
    }
    // A proof of exactly 16384 bytes is sent.
    let proof = zero_proof(MAX_SIG_PROOF_BYTES);
    let (r, req) = hub.send(
        Reply::json(201, &sub(BIND_REPLY, &hub.origin())),
        hub.call(bind_with(PUBLIC_KEY, &proof)),
    );
    assert!(r.is_ok(), "{r:?}");
    assert!(req.body_text().contains(&proof));
    // The destination is checked before the token.
    let mut call = hub.call(HubOperation::Me);
    call.destination = "https://127.0.0.1:1/api/v1/me".into();
    call.bearer = Some(Secret::new(BAD_TOKENS[0]));
    assert_eq!(hub.refuse(call), local(E_UPSTREAM_ORIGIN));
    // No token permission is inferred locally: any well-formed token is sent as given.
    let call = hub.call(HubOperation::Refresh {
        refresh_token: Secret::new(ACCESS),
    });
    let (r, req) = hub.send(Reply::json(200, TOKEN_REPLY), call);
    assert!(r.is_ok(), "{r:?}");
    assert!(req.body_text().contains(ACCESS));
    hub.finish();
}

/// The total members and elements of a JSON document.
fn members(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::Array(a) => a.len() + a.iter().map(members).sum::<usize>(),
        serde_json::Value::Object(m) => m.len() + m.values().map(members).sum::<usize>(),
        _ => 0,
    }
}

#[test]
fn hsa4_bodies_headers_and_json_are_bounded() {
    let mut hub = Hub::start();
    let o = hub.origin();
    let me = || HubOperation::Me;
    // A chunked body is read.
    let chunked = |body: &str, pieces: usize| {
        let mut out = Vec::new();
        let size = body.len().div_ceil(pieces).max(1);
        for c in body.as_bytes().chunks(size) {
            out.extend_from_slice(format!("{:x}\r\n", c.len()).as_bytes());
            out.extend_from_slice(c);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    };
    let te = [
        "content-type: application/json".to_string(),
        "transfer-encoding: chunked".into(),
        "connection: close".into(),
    ];
    let (r, _) = hub.send(
        Reply::with_headers(200, &te, &chunked(ME_REPLY, 7)),
        hub.call(me()),
    );
    assert!(r.is_ok(), "{r:?}");
    // 1 MiB of body is the limit, chunked or not.
    let padded = |n: usize| {
        let mut s = String::from(ME_REPLY);
        s.insert_str(1, &" ".repeat(n - ME_REPLY.len()));
        s
    };
    let (r, _) = hub.send(Reply::json(200, &padded(MAX_JSON_BYTES)), hub.call(me()));
    assert!(r.is_ok(), "1 MiB exactly: {r:?}");
    let (r, _) = hub.send(
        Reply::json(200, &padded(MAX_JSON_BYTES + 1)),
        hub.call(me()),
    );
    assert_eq!(failure(r), "Protocol");
    let (r, _) = hub.send(
        Reply::with_headers(200, &te, &chunked(&padded(MAX_JSON_BYTES + 1), 40)),
        hub.call(me()),
    );
    assert_eq!(failure(r), "Protocol");
    let (r, _) = hub.send(
        Reply::with_headers(200, &te, &chunked(&padded(MAX_JSON_BYTES), 40)),
        hub.call(me()),
    );
    assert!(r.is_ok(), "{r:?}");
    // An error body is bounded the same way.
    let (r, _) = hub.send(
        Reply::json(401, &" ".repeat(MAX_JSON_BYTES + 1)),
        hub.call(me()),
    );
    assert_eq!(failure(r), "Protocol");

    // At most 64 header fields, and at most 32768 header bytes.
    let fields = |n: usize| -> Vec<String> {
        let mut h = vec![
            "content-type: application/json".to_string(),
            format!("content-length: {}", ME_REPLY.len()),
            "connection: close".into(),
        ];
        for i in h.len()..n {
            h.push(format!("x-extra-{i}: v"));
        }
        h
    };
    let (r, _) = hub.send(
        Reply::with_headers(200, &fields(64), ME_REPLY.as_bytes()),
        hub.call(me()),
    );
    assert!(r.is_ok(), "64 fields: {r:?}");
    let (r, _) = hub.send(
        Reply::with_headers(200, &fields(65), ME_REPLY.as_bytes()),
        hub.call(me()),
    );
    assert_eq!(failure(r), "Protocol");
    // A header block over 32768 bytes fills the fixed input buffer and is never read as a
    // reply: after the request was sent, that is an unread reply.
    let mut big = fields(3);
    big.push(format!("x-big: {}", "v".repeat(MAX_HEADER_BYTES)));
    let (r, _) = hub.send(
        Reply::with_headers(200, &big, ME_REPLY.as_bytes()),
        hub.call(me()),
    );
    assert_eq!(failure(r), "Transport(true)");
    let mut fits = fields(3);
    let used: usize =
        fits.iter().map(|h| h.len() + 2).sum::<usize>() + "HTTP/1.1 200 OK\r\n\r\n".len();
    fits.push(format!(
        "x-big: {}",
        "v".repeat(MAX_HEADER_BYTES - used - "x-big: \r\n".len())
    ));
    let (r, _) = hub.send(
        Reply::with_headers(200, &fits, ME_REPLY.as_bytes()),
        hub.call(me()),
    );
    assert!(r.is_ok(), "32768 header bytes: {r:?}");

    // Duplicate keys are refused anywhere, ignored members included.
    let metadata = sub(METADATA_REPLY, &o);
    let dup_cases = [
        ME_REPLY.replacen(
            r#""handle":"test-person","#,
            r#""handle":"test-person","handle":"other","#,
            1,
        ),
        metadata.replacen(
            r#""scopes_supported""#,
            r#""scopes_supported":[],"scopes_supported""#,
            1,
        ),
        metadata.replacen("{", r#"{"x":{"a":1,"a":2},"#, 1),
        TOKEN_REPLY.replacen("{", r#"{"scope":"a","#, 1),
    ];
    for body in dup_cases {
        let op = if body.contains("issuer") {
            HubOperation::Metadata
        } else if body.contains("access_token") {
            HubOperation::Refresh {
                refresh_token: Secret::new(REFRESH),
            }
        } else {
            HubOperation::Me
        };
        let (r, _) = hub.send(Reply::json(200, &body), hub.call(op));
        assert_eq!(failure(r), "Protocol", "{body}");
    }

    // Depth 32 is the limit: the top-level object is level 1.
    let nested = |levels: usize| format!("{}{}{}", "[".repeat(levels), "1", "]".repeat(levels));
    let with_member = |member: &str| metadata.replacen('{', &format!(r#"{{"x":{member},"#), 1);
    let (r, _) = hub.send(
        Reply::json(200, &with_member(&nested(31))),
        hub.call(HubOperation::Metadata),
    );
    assert!(r.is_ok(), "depth 32: {r:?}");
    let (r, _) = hub.send(
        Reply::json(200, &with_member(&nested(32))),
        hub.call(HubOperation::Metadata),
    );
    assert_eq!(failure(r), "Protocol", "depth 33");

    // 4096 members and elements in total is the limit.
    let base: serde_json::Value = serde_json::from_str(&with_member("[]")).unwrap();
    let room = MAX_MEMBERS - members(&base);
    let elements = |n: usize| format!("[{}]", vec!["0"; n].join(","));
    let (r, _) = hub.send(
        Reply::json(200, &with_member(&elements(room))),
        hub.call(HubOperation::Metadata),
    );
    assert!(r.is_ok(), "4096 members: {r:?}");
    let (r, _) = hub.send(
        Reply::json(200, &with_member(&elements(room + 1))),
        hub.call(HubOperation::Metadata),
    );
    assert_eq!(failure(r), "Protocol", "4097 members");

    // A long scalar, invalid UTF-8 and trailing bytes are refused; extension members are not.
    let long = with_member(&format!("\"{}\"", "s".repeat(MAX_SCALAR_BYTES + 1)));
    let (r, _) = hub.send(Reply::json(200, &long), hub.call(HubOperation::Metadata));
    assert_eq!(failure(r), "Protocol", "long scalar");
    let fits = with_member(&format!("\"{}\"", "s".repeat(MAX_SCALAR_BYTES)));
    let (r, _) = hub.send(Reply::json(200, &fits), hub.call(HubOperation::Metadata));
    assert!(r.is_ok(), "scalar at the bound: {r:?}");
    let mut invalid = ME_REPLY.as_bytes().to_vec();
    let at = ME_REPLY.find("Test Person").unwrap();
    invalid[at] = 0xff;
    let headers = [
        "content-type: application/json".to_string(),
        format!("content-length: {}", invalid.len()),
        "connection: close".into(),
    ];
    let (r, _) = hub.send(Reply::with_headers(200, &headers, &invalid), hub.call(me()));
    assert_eq!(failure(r), "Protocol", "invalid UTF-8");
    let (r, _) = hub.send(
        Reply::json(200, &format!("{ME_REPLY} {{}}")),
        hub.call(me()),
    );
    assert_eq!(failure(r), "Protocol", "trailing bytes");
    // Closed application objects deny unknown members; OAuth replies ignore extensions.
    let (r, _) = hub.send(
        Reply::json(200, &ME_REPLY.replacen('{', r#"{"extra":1,"#, 1)),
        hub.call(me()),
    );
    assert_eq!(failure(r), "Protocol", "unknown /me member");
    let (r, _) = hub.send(
        Reply::json(200, &ME_REPLY.replacen('{', r#"{"away_until":null,"#, 1)),
        hub.call(me()),
    );
    assert!(r.is_ok(), "away_until: {r:?}");
    let (r, _) = hub.send(
        Reply::json(200, &RETIRE_REPLY.replacen('{', r#"{"note":"x","#, 1)),
        hub.call(HubOperation::Retire {
            key_id: KEY_ID.into(),
        }),
    );
    assert_eq!(failure(r), "Protocol", "unknown key member");
    let (r, _) = hub.send(
        Reply::json(
            200,
            &TOKEN_REPLY.replacen('{', r#"{"id_token":"x","ext":{"a":[1]},"#, 1),
        ),
        hub.call(HubOperation::Refresh {
            refresh_token: Secret::new(REFRESH),
        }),
    );
    assert!(r.is_ok(), "token extensions: {r:?}");
    hub.finish();
}

#[test]
fn hsa4_mutations_are_never_retried() {
    let mut hub = Hub::start();
    // A lost reply after the request was sent is an uncertain outcome, never a second request.
    for op in all_operations() {
        let name = format!("{op:?}");
        let (r, _) = hub.send(Reply::Close, hub.call(op.clone()));
        assert_eq!(failure(r), "Transport(true)", "{name}");
        // A reply cut inside its body is lost the same way.
        let cut = Reply::with_headers(
            200,
            &[
                "content-type: application/json".to_string(),
                "content-length: 400".into(),
            ],
            b"{\"id\":",
        );
        let (r, _) = hub.send(cut, hub.call(op));
        assert_eq!(failure(r), "Transport(true)", "{name}");
    }
    let n = all_operations().len();
    let log = hub.server.requests();
    assert_eq!(log.len(), 2 * n);
    // Each request came on its own connection: nothing was replayed.
    let conns: std::collections::BTreeSet<usize> = log.iter().map(|r| r.conn).collect();
    assert_eq!(conns.len(), 2 * n);
    hub.finish();
}

const LOG_CHILD: &str = "RDY_HSA4_LOG_CHILD";

/// A logger that counts every record it receives.
struct Capture(std::sync::atomic::AtomicUsize);

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, _: &log::Record<'_>) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn flush(&self) {}
}

static CAPTURE: Capture = Capture(std::sync::atomic::AtomicUsize::new(0));

/// The child half: a trace logger, every operation (successes and failures), zero records.
fn log_child() {
    log::set_logger(&CAPTURE).expect("first logger");
    log::set_max_level(log::LevelFilter::Trace);
    // The logger is installed: a record passed to it directly is counted.
    log::logger().log(&log::Record::builder().args(format_args!("probe")).build());
    assert_eq!(CAPTURE.0.swap(0, std::sync::atomic::Ordering::SeqCst), 1);
    let mut hub = Hub::start();
    let o = hub.origin();
    for op in all_operations() {
        let (r, _) = hub.send(success(&op, &o), hub.call(op.clone()));
        assert!(r.is_ok(), "{r:?}");
        let (r, _) = hub.send(Reply::Close, hub.call(op.clone()));
        assert!(r.is_err());
        let (r, _) = hub.send(
            Reply::json(400, r#"{"error":"invalid_request"}"#),
            hub.call(op),
        );
        assert!(r.is_err());
    }
    let mut call = hub.call(HubOperation::Me);
    call.ca_file = None;
    assert!(hub.send_unread(call).is_err());
    hub.finish();
    assert_eq!(log::STATIC_MAX_LEVEL, log::LevelFilter::Off);
    assert_eq!(CAPTURE.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    println!("hsa4 log child: 0 records");
}

fn assert_hidden(text: &str, secrets: &[&str]) {
    for s in secrets {
        assert!(!text.contains(s), "{s} leaked into: {text}");
    }
}

#[test]
fn hsa4_raw_failures_and_dependency_logs_never_echo_secrets() {
    if std::env::var_os(LOG_CHILD).is_some() {
        log_child();
        return;
    }
    let mut hub = Hub::start();
    let o = hub.origin();
    let secrets = [
        ACCESS,
        REFRESH,
        DEVICE_CODE,
        "SENTINEL-MESSAGE",
        "SENTINEL-FIX",
    ];
    let check = |f: HubFailure, install: &Origin| {
        assert_hidden(&format!("{f:?}"), &secrets);
        let e = f.into_error(install);
        assert_hidden(&format!("{e:?}"), &secrets);
        assert_hidden(&serde_json::to_string(&e).unwrap(), &secrets);
    };
    // The hub's own message, fix and body never escape, even when they quote a token.
    let api = format!(
        r#"{{"code":"E_HUB_VALIDATION","message":"SENTINEL-MESSAGE {ACCESS}","fix":"SENTINEL-FIX {REFRESH}","issues":[{{"m":"{ACCESS}"}}],"request_id":"r"}}"#
    );
    let (r, _) = hub.send(Reply::json(422, &api), hub.call(HubOperation::Me));
    check(r.expect_err("refused"), &hub.install);
    let unknown = api.replace("E_HUB_VALIDATION", &format!("E_{}", "X".repeat(20)));
    let (r, _) = hub.send(Reply::json(400, &unknown), hub.call(HubOperation::Me));
    check(r.expect_err("refused"), &hub.install);
    let oauth = format!(
        r#"{{"error":"invalid_request","error_description":"SENTINEL-MESSAGE {REFRESH}","error_uri":"{o}/{DEVICE_CODE}"}}"#
    );
    let (r, _) = hub.send(
        Reply::json(400, &oauth),
        hub.call(HubOperation::DevicePoll {
            device_code: Secret::new(DEVICE_CODE),
        }),
    );
    check(r.expect_err("refused"), &hub.install);
    // A malformed reply that echoes the token is a bare contract mismatch.
    let (r, _) = hub.send(
        Reply::json(200, &format!(r#"{{"echo":"{ACCESS}"}}"#)),
        hub.call(HubOperation::Me),
    );
    check(r.expect_err("protocol"), &hub.install);
    // Library errors are never carried: a failed handshake and a lost reply.
    let mut call = hub.call(HubOperation::Me);
    call.ca_file = None;
    check(hub.send_unread(call).expect_err("tls"), &hub.install);
    let (r, _) = hub.send(
        Reply::Close,
        hub.call(HubOperation::Refresh {
            refresh_token: Secret::new(REFRESH),
        }),
    );
    check(r.expect_err("lost"), &hub.install);
    // Refused tokens are never quoted, and a call's Debug hides its secrets.
    let mut call = hub.call(HubOperation::SignOut {
        token: Secret::new(REFRESH),
    });
    assert_hidden(&format!("{call:?}"), &secrets);
    call.operation = HubOperation::SignOut {
        token: Secret::new(format!("{REFRESH}x")),
    };
    check(
        hub.transport.exchange(call).expect_err("bad token"),
        &hub.install,
    );
    assert_hidden(&format!("{:?}", hub.transport), &secrets);
    // Successful replies keep secrets only inside Secret.
    let (r, _) = hub.send(
        Reply::json(200, TOKEN_REPLY),
        hub.call(HubOperation::Refresh {
            refresh_token: Secret::new(REFRESH),
        }),
    );
    assert_hidden(&format!("{:?}", r.expect("tokens")), &[ACCESS, REFRESH_2]);
    let (r, _) = hub.send(
        Reply::json(200, &sub(DEVICE_REPLY, &o)),
        hub.call(HubOperation::DeviceStart {
            machine_name: MACHINE.into(),
        }),
    );
    assert_hidden(&format!("{:?}", r.expect("device")), &[DEVICE_CODE]);
    hub.finish();

    // Dependency logging: in a child process with a trace logger, every operation writes zero
    // records, because the engine's `log` features compile every record out.
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "hsa4_raw_failures_and_dependency_logs_never_echo_secrets",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(LOG_CHILD, "1")
        .output()
        .expect("child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("hsa4 log child: 0 records"), "{stdout}");
}

/// The request head the transport writes, as the hub reads it: the body length of a POST, the
/// library's `user-agent`, `host`, then `lines`.
fn head(start: &str, origin: &str, body: &str, lines: &[String]) -> String {
    let host = origin.strip_prefix("https://").unwrap();
    let mut out = format!("{start} HTTP/1.1\r\n");
    if start.starts_with("POST ") {
        out.push_str(&format!("content-length: {}\r\n", body.len()));
    }
    out.push_str(&format!("user-agent: ureq/3.4.2\r\nhost: {host}"));
    for l in lines {
        out.push_str("\r\n");
        out.push_str(l);
    }
    out
}

#[test]
fn hsa4_hub_fixtures_round_trip() {
    let mut hub = Hub::start();
    let o = hub.origin();
    let key = |state: KeyState,
               bound: Option<&str>,
               retired: Option<&str>,
               revoked: Option<&str>,
               from: Option<&str>| HubKey {
        id: KEY_ID.into(),
        account: ACCOUNT.into(),
        fingerprint: FINGERPRINT.into(),
        public_key: PUBLIC_KEY.into(),
        label: "laptop key".into(),
        kind: KeyKind::Person,
        state,
        expires_at: None,
        bound_at: bound.map(str::to_string),
        retired_at: retired.map(str::to_string),
        revoked_at: revoked.map(str::to_string),
        compromised_from: from.map(str::to_string),
    };
    let bound = Some("2026-10-17T12:05:00Z");
    let accept = "accept: application/json".to_string();
    let bearer = format!("authorization: Bearer {ACCESS}");
    let actor = "rdy-actor: agent:claude".to_string();
    let run = "rdy-run: r7".to_string();
    let json = "content-type: application/json".to_string();
    let form = "content-type: application/x-www-form-urlencoded".to_string();
    struct Case {
        op: HubOperation,
        reply: Reply,
        start: String,
        lines: Vec<String>,
        body: String,
        want: HubReply,
    }
    let device_body = "client_id=rdy&scope=pull+push+upload+change&machine_name=laptop";
    let poll_body = format!(
        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code={DEVICE_CODE}&client_id=rdy"
    );
    let refresh_body = format!("grant_type=refresh_token&refresh_token={REFRESH}&client_id=rdy");
    let revoke_body = format!("token={REFRESH}&client_id=rdy");
    let tokens = TokenPair {
        access_token: Secret::new(ACCESS),
        refresh_token: Secret::new(REFRESH_2),
        expires_in: 3600,
    };
    let cases = vec![
        Case {
            op: HubOperation::Metadata,
            reply: Reply::json(200, &sub(METADATA_REPLY, &o)),
            start: "GET /.well-known/oauth-authorization-server/auth".into(),
            lines: vec![accept.clone()],
            body: String::new(),
            want: HubReply::Metadata(HubMetadata {
                issuer: format!("{o}/auth"),
                device_authorization_endpoint: format!("{o}/auth/device"),
                token_endpoint: format!("{o}/auth/token"),
                revocation_endpoint: format!("{o}/auth/revoke"),
            }),
        },
        Case {
            op: HubOperation::DeviceStart {
                machine_name: MACHINE.into(),
            },
            reply: Reply::json(200, &sub(DEVICE_REPLY, &o)),
            start: "POST /auth/device".into(),
            lines: vec![accept.clone(), form.clone()],
            body: device_body.into(),
            want: HubReply::DeviceStart(DeviceStartReply {
                device_code: Secret::new(DEVICE_CODE),
                user_code: USER_CODE.into(),
                verification_uri: format!("{o}/app/device"),
                expires_in: 600,
                interval: 5,
            }),
        },
        Case {
            op: HubOperation::DevicePoll {
                device_code: Secret::new(DEVICE_CODE),
            },
            reply: Reply::json(200, TOKEN_REPLY),
            start: "POST /auth/token".into(),
            lines: vec![accept.clone(), form.clone()],
            body: poll_body.clone(),
            want: HubReply::DevicePoll(DevicePollReply::Tokens(tokens.clone())),
        },
        Case {
            op: HubOperation::Refresh {
                refresh_token: Secret::new(REFRESH),
            },
            reply: Reply::json(200, TOKEN_REPLY),
            start: "POST /auth/token".into(),
            lines: vec![accept.clone(), form.clone()],
            body: refresh_body.clone(),
            want: HubReply::Refresh(RefreshReply::Tokens(tokens.clone())),
        },
        Case {
            op: HubOperation::Me,
            reply: Reply::json(200, ME_REPLY),
            start: "GET /api/v1/me".into(),
            lines: vec![accept.clone(), bearer.clone()],
            body: String::new(),
            want: HubReply::Me(MeReply {
                account: ACCOUNT.into(),
                handle: "test-person".into(),
            }),
        },
        Case {
            op: HubOperation::SignOut {
                token: Secret::new(REFRESH),
            },
            reply: Reply::empty(200),
            start: "POST /auth/revoke".into(),
            lines: vec![accept.clone(), form.clone()],
            body: revoke_body.clone(),
            want: HubReply::SignOut,
        },
        Case {
            op: HubOperation::BindChallenge {
                kind: KeyKind::Person,
                label: Some("laptop key".into()),
            },
            reply: Reply::json(201, CHALLENGE_REPLY),
            start: "POST /api/v1/me/keys/challenges".into(),
            lines: vec![
                accept.clone(),
                json.clone(),
                bearer.clone(),
                actor.clone(),
                run.clone(),
            ],
            body: CHALLENGE_REQUEST.into(),
            want: HubReply::BindChallenge(KeyChallenge {
                id: CHALLENGE_ID.into(),
                account: ACCOUNT.into(),
                kind: KeyKind::Person,
                label: "laptop key".into(),
                namespace: "rdy-keybind-v1".into(),
                nonce: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                expires_at: "2026-10-17T12:10:00Z".into(),
            }),
        },
        Case {
            op: HubOperation::Bind {
                challenge: CHALLENGE_ID.into(),
                public_key: PUBLIC_KEY.into(),
                proof: PROOF.into(),
            },
            reply: Reply::json(201, &sub(BIND_REPLY, &o)),
            start: "POST /api/v1/me/keys".into(),
            lines: vec![
                accept.clone(),
                json.clone(),
                bearer.clone(),
                actor.clone(),
                run.clone(),
            ],
            body: BIND_REQUEST.into(),
            want: HubReply::Bind(BindResult {
                key: key(KeyState::Pending, None, None, None, None),
                confirm_url: format!("{o}/app/settings/keys/{KEY_ID}/confirm"),
            }),
        },
        Case {
            op: HubOperation::FindKey {
                fingerprint: FINGERPRINT.into(),
            },
            reply: Reply::json(200, LIST_REPLY),
            start: format!("GET {FIND_TARGET}"),
            lines: vec![accept.clone(), bearer.clone(), actor.clone(), run.clone()],
            body: String::new(),
            want: HubReply::FindKey(Some(key(KeyState::Active, bound, None, None, None))),
        },
        Case {
            op: HubOperation::Retire {
                key_id: KEY_ID.into(),
            },
            reply: Reply::json(200, RETIRE_REPLY),
            start: format!("POST /api/v1/me/keys/{KEY_ID}/retire"),
            lines: vec![accept.clone(), bearer.clone(), actor.clone(), run.clone()],
            body: String::new(),
            want: HubReply::Retire(key(
                KeyState::Retired,
                bound,
                Some("2026-10-18T09:00:00Z"),
                None,
                None,
            )),
        },
        Case {
            op: HubOperation::Revoke {
                key_id: KEY_ID.into(),
                compromised_from: Some(COMPROMISED.into()),
            },
            reply: Reply::json(200, REVOKE_REPLY),
            start: format!("POST /api/v1/me/keys/{KEY_ID}/revoke"),
            lines: vec![
                accept.clone(),
                json.clone(),
                bearer.clone(),
                actor.clone(),
                run.clone(),
            ],
            body: REVOKE_REQUEST.into(),
            want: HubReply::Revoke(key(
                KeyState::Revoked,
                bound,
                None,
                Some("2026-10-18T09:00:00Z"),
                Some(COMPROMISED),
            )),
        },
    ];
    for c in cases {
        let name = format!("{:?}", c.op);
        let (r, req) = hub.send(c.reply, hub.call(c.op));
        assert_eq!(req.head, head(&c.start, &o, &c.body, &c.lines), "{name}");
        assert_eq!(req.body_text(), c.body, "{name}");
        assert_eq!(r.expect("reply"), c.want, "{name}");
    }

    // An empty list is no key; a list with another key, two keys or a cursor is refused.
    let find = || HubOperation::FindKey {
        fingerprint: FINGERPRINT.into(),
    };
    let (r, _) = hub.send(Reply::json(200, r#"{"items":[]}"#), hub.call(find()));
    assert_eq!(r.expect("list"), HubReply::FindKey(None));
    let (r, _) = hub.send(
        Reply::json(200, r#"{"items":[],"next":null}"#),
        hub.call(find()),
    );
    assert_eq!(r.expect("list"), HubReply::FindKey(None));
    let item = LIST_REPLY
        .strip_prefix(r#"{"items":["#)
        .unwrap()
        .strip_suffix(r#"],"next":null}"#)
        .unwrap();
    for body in [
        format!(r#"{{"items":[{item},{item}],"next":null}}"#),
        format!(r#"{{"items":[{item}],"next":"cursor"}}"#),
        LIST_REPLY.replace("RHFBqxmd4FLrdHDM2DGtlKdMxg", "XHFBqxmd4FLrdHDM2DGtlKdMxg"),
        LIST_REPLY.replace(r#""next":null"#, r#""next":null,"total":1"#),
        LIST_REPLY.replace(r#","next":null"#, ""),
    ] {
        let (r, _) = hub.send(Reply::json(200, &body), hub.call(find()));
        if body == LIST_REPLY.replace(r#","next":null"#, "") {
            assert!(r.is_ok(), "an absent next: {r:?}");
        } else {
            assert_eq!(failure(r), "Protocol", "{body}");
        }
    }
    // Replies that contradict the request or the settled values are refused.
    let challenge = || HubOperation::BindChallenge {
        kind: KeyKind::Person,
        label: Some("laptop key".into()),
    };
    let bind = || HubOperation::Bind {
        challenge: CHALLENGE_ID.into(),
        public_key: PUBLIC_KEY.into(),
        proof: PROOF.into(),
    };
    let retire = || HubOperation::Retire {
        key_id: KEY_ID.into(),
    };
    let revoke = || HubOperation::Revoke {
        key_id: KEY_ID.into(),
        compromised_from: None,
    };
    let bind_reply = sub(BIND_REPLY, &o);
    let contradictions: Vec<(HubOperation, u16, String)> = vec![
        (
            challenge(),
            201,
            CHALLENGE_REPLY.replace("rdy-keybind-v1", "rdy-other-v1"),
        ),
        (
            challenge(),
            201,
            CHALLENGE_REPLY.replace(r#""kind":"person""#, r#""kind":"agent""#),
        ),
        (
            challenge(),
            201,
            CHALLENGE_REPLY.replace(r#""label":"laptop key""#, r#""label":"person key""#),
        ),
        (
            challenge(),
            201,
            CHALLENGE_REPLY.replace("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "AAAA"),
        ),
        (
            challenge(),
            201,
            CHALLENGE_REPLY.replace("2026-10-17T12:10:00Z", "soon"),
        ),
        (
            bind(),
            201,
            bind_reply.replace(r#""state":"pending""#, r#""state":"active""#),
        ),
        (
            bind(),
            201,
            bind_reply.replace("/confirm\"", "/confirm?code=1\""),
        ),
        (
            bind(),
            201,
            bind_reply.replace(&format!("{o}/app"), "https://127.0.0.1:1/app"),
        ),
        (
            bind(),
            201,
            bind_reply.replace(
                "keys/skey_00000000000000000000000003/confirm",
                "keys/skey_9/confirm",
            ),
        ),
        (
            bind(),
            201,
            bind_reply.replace("RHFBqxmd4FLrdHDM2DGtlKdMxg", "XHFBqxmd4FLrdHDM2DGtlKdMxg"),
        ),
        (
            retire(),
            200,
            RETIRE_REPLY.replace(r#""state":"retired""#, r#""state":"active""#),
        ),
        (
            retire(),
            200,
            RETIRE_REPLY.replace(KEY_ID, "skey_00000000000000000000000009"),
        ),
        (
            revoke(),
            200,
            REVOKE_REPLY.replace(r#""state":"revoked""#, r#""state":"retired""#),
        ),
        (
            revoke(),
            200,
            REVOKE_REPLY.replace(
                r#""revoked_at":"2026-10-18T09:00:00Z""#,
                r#""revoked_at":null"#,
            ),
        ),
        (
            revoke(),
            200,
            REVOKE_REPLY.replace(
                r#""compromised_from":"2026-10-17T11:00:00Z""#,
                r#""compromised_from":null"#,
            ),
        ),
        (
            HubOperation::Me,
            200,
            ME_REPLY.replace(r#""handle":"test-person""#, r#""handle":"test person""#),
        ),
        (
            HubOperation::Me,
            200,
            ME_REPLY.replace(r#""kind":"person""#, r#""kind":"robot""#),
        ),
        (
            HubOperation::Me,
            200,
            ME_REPLY.replace(r#","created_at":"2026-08-01T08:00:00Z""#, ""),
        ),
        // Every time is RFC3339 within 64 bytes, the ones `/me` discards included; the time
        // parser's own leniency (any separator byte) is not enough.
        (
            HubOperation::Me,
            200,
            ME_REPLY.replace(
                "2026-08-01T08:00:00Z",
                &format!("2026-08-01T08:00:00.{}Z", "0".repeat(44)),
            ),
        ),
        (
            HubOperation::Me,
            200,
            ME_REPLY.replace("2026-08-01T08:00:00Z", "2026-08-01x08:00:00Z"),
        ),
        (
            HubOperation::Me,
            200,
            ME_REPLY.replace("2026-09-01T08:00:00Z", "not a time"),
        ),
        (
            HubOperation::Me,
            200,
            ME_REPLY.replace(r#","version""#, r#","away_until":"not a time","version""#),
        ),
        (
            challenge(),
            201,
            CHALLENGE_REPLY.replace("2026-10-17T12:10:00Z", "2026-10-17 12:10:00Z"),
        ),
        (
            retire(),
            200,
            RETIRE_REPLY.replace("2026-10-18T09:00:00Z", "2026-10-18x09:00:00Z"),
        ),
        (
            revoke(),
            200,
            REVOKE_REPLY.replace("2026-10-17T11:00:00Z", "2026-10-17T11:00:00.0000000000Z"),
        ),
        (
            find(),
            200,
            LIST_REPLY.replace("2026-10-17T12:05:00Z", "2026-10-17T12:05:00"),
        ),
        // Ids name their object: an account is `acc_…`, a key `skey_…`, a challenge `kch_…`.
        (HubOperation::Me, 200, ME_REPLY.replace(ACCOUNT, KEY_ID)),
        (HubOperation::Me, 200, ME_REPLY.replace(ACCOUNT, "acc_")),
        (find(), 200, LIST_REPLY.replace(KEY_ID, ACCOUNT)),
        (
            find(),
            200,
            LIST_REPLY.replace(
                &format!(r#""account":"{ACCOUNT}""#),
                &format!(r#""account":"{KEY_ID}""#),
            ),
        ),
        (
            challenge(),
            201,
            CHALLENGE_REPLY.replace(CHALLENGE_ID, "challenge-2"),
        ),
        (
            challenge(),
            201,
            CHALLENGE_REPLY.replace(CHALLENGE_ID, ACCOUNT),
        ),
        (
            challenge(),
            201,
            CHALLENGE_REPLY.replace(
                &format!(r#""account":"{ACCOUNT}""#),
                r#""account":"someone""#,
            ),
        ),
        (bind(), 201, bind_reply.replace(KEY_ID, ACCOUNT)),
        // A key object's public key is the OpenSSH line, never its fingerprint or a commented
        // or padded line.
        (find(), 200, LIST_REPLY.replace(PUBLIC_KEY, FINGERPRINT)),
        (
            find(),
            200,
            LIST_REPLY.replace(PUBLIC_KEY, &format!("{FINGERPRINT} ")),
        ),
        (
            find(),
            200,
            LIST_REPLY.replace(PUBLIC_KEY, &format!("{PUBLIC_KEY} laptop")),
        ),
        (
            find(),
            200,
            LIST_REPLY.replace(PUBLIC_KEY, &format!("{PUBLIC_KEY} ")),
        ),
        (
            retire(),
            200,
            RETIRE_REPLY.replace(PUBLIC_KEY, &format!("{PUBLIC_KEY}\\n")),
        ),
    ];
    for (op, status, body) in contradictions {
        let (r, _) = hub.send(Reply::json(status, &body), hub.call(op));
        assert_eq!(failure(r), "Protocol", "{body}");
    }
    hub.finish();
}

fn is_send_sync<T: Send + Sync + 'static>(_: &T) {}

/// A reply with a length and no `connection: close`: a client may keep the connection.
fn keep_alive_me() -> Reply {
    Reply::with_headers(
        200,
        &[
            "content-type: application/json".to_string(),
            format!("content-length: {}", ME_REPLY.len()),
        ],
        ME_REPLY.as_bytes(),
    )
}

/// The engine's own part of the blocking boundary: one synchronous `exchange` builds, uses and
/// drops its client. It makes no claim about the MCP or daemon dispatch, which their own tests
/// cover. The fixture holds every connection open, so a client kept across calls would send the
/// second request on the first connection.
#[test]
fn hsa4_each_exchange_builds_and_drops_its_own_client() {
    let mut server = Server::keep_alive();
    let install = Origin::parse(server.origin()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, server.ca_pem()).unwrap();
    let url = route(&install, &HubOperation::Me).unwrap().unwrap();

    // The fixture works: a pooling client sends both its requests on one held connection.
    server.reply(keep_alive_me());
    server.reply(keep_alive_me());
    server.accept_n(1);
    let roots: Vec<_> = ureq::tls::parse_pem(server.ca_pem().as_bytes())
        .filter_map(|item| match item {
            Ok(ureq::tls::PemItem::Certificate(c)) => Some(c.to_owned()),
            _ => None,
        })
        .collect();
    let pooling: ureq::Agent = ureq::Agent::config_builder()
        .proxy(None)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .root_certs(ureq::tls::RootCerts::Specific(std::sync::Arc::new(roots)))
                .build(),
        )
        .build()
        .into();
    for _ in 0..2 {
        let mut r = pooling.get(&url).call().expect("pooled call");
        r.body_mut().read_to_vec().expect("pooled body");
    }
    drop(pooling);
    server.wait_closed(0);
    assert_eq!(server.accepted(), 1);
    assert_eq!(
        server.requests().iter().map(|r| r.conn).collect::<Vec<_>>(),
        vec![0, 0],
        "a pooling client reuses the held connection"
    );

    // The transport: two sequential exchanges on one transport, called directly on this thread
    // with no runtime. Each opens its own connection while the first is still held open by the
    // fixture, and each connection is closed by the client before the transport is dropped.
    server.reply(keep_alive_me());
    server.reply(keep_alive_me());
    server.accept_n(2);
    let call = HubCall {
        install: install.clone(),
        destination: url,
        ca_file: Some(ca),
        claim: None,
        bearer: Some(Secret::new(ACCESS)),
        operation: HubOperation::Me,
    };
    let transport =
        HttpHubTransport::new(OriginFence::new(install.clone(), []).unwrap()).expect("transport");
    is_send_sync(&transport);
    let first = transport.exchange(call.clone());
    assert!(first.is_ok(), "{first:?}");
    let second = transport.exchange(call);
    assert!(second.is_ok(), "{second:?}");
    assert_eq!(server.accepted(), 3, "two exchanges open two connections");
    assert_eq!(
        server.requests()[2..]
            .iter()
            .map(|r| r.conn)
            .collect::<Vec<_>>(),
        vec![1, 2],
        "no connection is reused across exchanges"
    );
    // Nothing outlives a call: the client closed both connections while the transport lives.
    server.wait_closed(1);
    server.wait_closed(2);
    drop(transport);
    assert_eq!(server.finish(), 0);
}
