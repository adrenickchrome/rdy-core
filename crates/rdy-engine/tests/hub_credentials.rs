//! The credential service: the sign-in card, one rotating credential per install under one
//! blocking file lock, device polling on an injected clock, the `/me` handle repair and
//! sign-out. A fake transport records every request; channels, barriers and child-process
//! marks order the races. Nothing sleeps or measures time.
//!
//! Its file modes and 0600 fixtures go through `rdy_store::pio` (FW-XP1 D-1RXEA); the expected
//! modes are Unix modes, which FW-XP1 Phase 4 ports with the Windows privacy rules.

use rdy_engine::answer::relay::{RelayInput, relayed};
use rdy_engine::hub_config::{
    Credential, CredentialFile, PendingSignIn, ReadyCredential, encode_credentials,
    parse_credentials,
};
use rdy_engine::hub_credentials::{
    AccessGrant, AuthAnswer, AuthCaller, AuthConfirmation, AuthPhase, AuthState, HandleUpdate,
    HubCredentials, InstallState, NO_USER_FILE_FIX, NO_USER_FILE_MESSAGE, require_user_file,
};
use rdy_engine::hub_types::{
    BAD_MACHINE_NAME_MESSAGE, DevicePollReply, DeviceStartReply, HubCall, HubClock, HubFailure,
    HubMetadata, HubOperation, HubReply, HubTransport, MeReply, Origin, RefreshReply, RequestClaim,
    Secret, TokenPair,
};
use rdy_engine::{
    E_ACTOR_UNBOUND, E_BAD_INPUT, E_CONFIG, E_IO, E_PERMISSION, E_UPSTREAM_AUTH,
    E_UPSTREAM_CONFIRM_HOST, E_UPSTREAM_PROTOCOL, E_UPSTREAM_REFUSED, E_UPSTREAM_TRANSPORT,
    E_UPSTREAM_UNCERTAIN, RdyError, Session, W_UPSTREAM_HANDLE_CHANGED, W_UPSTREAM_SIGNOUT_LOCAL,
};
use rdy_model::Role;
use rdy_store::fsutil::{PrivateWriteStage, with_private_write_probe_for_tests};
use rdy_store::pio;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

const INSTALL: &str = "https://hub.test";
const OTHER: &str = "https://other.test";
const THIRD: &str = "https://third.test";
const MACHINE: &str = "work laptop";
const ACCOUNT: &str = "acc_00000000000000000000000001";
const HANDLE: &str = "test-person";
const USER_CODE: &str = "BCDFGHJK";
const DEVICE_CODE: &str = "devicecodeSECRETdevicecodeSECRET";
const T0: i64 = 1_800_000_000_000;
const HOUR_MS: i64 = 3_600_000;
const CONFIG: &str = "# person settings, kept as written\nactor: human:nick   # me\n";

const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// A hub token of kind `kind` whose secret starts with `tag`, padded with `0`, and its checksum.
fn token(kind: char, tag: &str) -> String {
    let mut secret: String = tag.chars().filter(char::is_ascii_alphanumeric).collect();
    while secret.len() < 40 {
        secret.push('0');
    }
    secret.truncate(40);
    let body = format!("rdyh_{kind}_{secret}");
    let mut crc = u64::from(crc32fast::hash(body.as_bytes()));
    let mut sum = [b'0'; 6];
    for slot in sum.iter_mut().rev() {
        *slot = ALPHABET[(crc % 62) as usize];
        crc /= 62;
    }
    format!("{body}{}", std::str::from_utf8(&sum).unwrap())
}

fn old_access() -> String {
    token('a', "OLDACCESS")
}
fn old_refresh() -> String {
    token('r', "OLDREFRESH")
}
fn new_access() -> String {
    token('a', "NEWACCESS")
}
fn new_refresh() -> String {
    token('r', "NEWREFRESH")
}

fn origin(s: &str) -> Origin {
    Origin::parse(s).unwrap()
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    user_file: PathBuf,
    creds: PathBuf,
    lock: PathBuf,
    project: PathBuf,
}

/// A private temp root with `cfg/rdy/config.yaml` (0644, with comments) and a project.
fn env() -> Env {
    let dir = tempfile::Builder::new()
        .permissions(pio::permissions(0o700))
        .tempdir()
        .unwrap();
    let root = dir.path().to_path_buf();
    let rdy = root.join("cfg/rdy");
    pio::dir_mode(fs::DirBuilder::new().recursive(true), 0o700)
        .create(&rdy)
        .unwrap();
    let user_file = rdy.join("config.yaml");
    fs::write(&user_file, CONFIG).unwrap();
    fs::set_permissions(&user_file, pio::permissions(0o644)).unwrap();
    let project = root.join("project");
    pio::dir_mode(&mut fs::DirBuilder::new(), 0o700)
        .create(&project)
        .unwrap();
    Env {
        _dir: dir,
        creds: rdy.join("credentials.yaml"),
        lock: rdy.join("credentials.yaml.lock"),
        root,
        user_file,
        project,
    }
}

fn svc<'a>(e: &Env, t: &'a Fake, c: &'a Clock) -> HubCredentials<'a> {
    HubCredentials::new(Some(&e.project), &e.user_file, t, c)
}

fn mode(p: &Path) -> u32 {
    pio::mode_of(&fs::symlink_metadata(p).unwrap())
}

/// Writes the credential file (0600) holding exactly `records`.
fn seed(e: &Env, records: &[(&str, Credential)]) {
    seed_at(&e.creds, records);
}

/// Writes the credential file at `creds` (0600) holding exactly `records`.
fn seed_at(creds: &Path, records: &[(&str, Credential)]) {
    let mut file = CredentialFile::default();
    for (o, c) in records {
        file.set(origin(o), Some(c.clone()));
    }
    let bytes = encode_credentials(&file).unwrap();
    let mut f = pio::file_mode(
        OpenOptions::new().write(true).create(true).truncate(true),
        0o600,
    )
    .open(creds)
    .unwrap();
    f.write_all(&bytes).unwrap();
    fs::set_permissions(creds, pio::permissions(0o600)).unwrap();
}

/// The record of `install` on disk, read without the lock (a replacement is a rename).
fn on_disk(creds: &Path, install: &str) -> Option<Credential> {
    let bytes = match fs::read(creds) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => panic!("{e}"),
    };
    parse_credentials(&bytes)
        .unwrap()
        .get(&origin(install))
        .cloned()
}

fn state_on_disk(creds: &Path, install: &str) -> Option<&'static str> {
    on_disk(creds, install).map(|c| c.state())
}

fn disk_text(creds: &Path) -> String {
    fs::read_to_string(creds).unwrap_or_default()
}

/// Whether the credential lock is free now. A test-only try: the service only ever blocks.
fn lock_is_free(lock: &Path) -> bool {
    let f = File::open(lock).unwrap();
    match f.try_lock() {
        Ok(()) => {
            f.unlock().unwrap();
            true
        }
        Err(TryLockError::WouldBlock) => false,
        Err(TryLockError::Error(e)) => panic!("{e}"),
    }
}

fn ready(access: &str, refresh: &str, expires_at_ms: i64, handle: &str) -> Credential {
    Credential::Ready(ReadyCredential {
        access_token: Secret::new(access),
        refresh_token: Secret::new(refresh),
        expires_at_ms,
        account: ACCOUNT.to_string(),
        handle: handle.to_string(),
    })
}

fn pending_on(
    install: &str,
    machine: &str,
    expires_at_ms: i64,
    next_poll_at_ms: i64,
    interval: u64,
) -> Credential {
    Credential::Pending(PendingSignIn {
        machine_name: machine.to_string(),
        device_code: Secret::new(DEVICE_CODE),
        user_code: USER_CODE.to_string(),
        verification_uri: format!("{install}/app/device"),
        expires_at_ms,
        next_poll_at_ms,
        interval_seconds: interval,
    })
}

fn pending(expires_at_ms: i64, next_poll_at_ms: i64, interval: u64) -> Credential {
    pending_on(INSTALL, MACHINE, expires_at_ms, next_poll_at_ms, interval)
}

// ---------------------------------------------------------------------------------------------
// Replies
// ---------------------------------------------------------------------------------------------

type Reply = Result<HubReply, HubFailure>;

fn meta(install: &str) -> Reply {
    Ok(HubReply::Metadata(HubMetadata {
        issuer: format!("{install}/auth"),
        device_authorization_endpoint: format!("{install}/auth/device"),
        token_endpoint: format!("{install}/auth/token"),
        revocation_endpoint: format!("{install}/auth/revoke"),
    }))
}

fn device_start(install: &str) -> Reply {
    Ok(HubReply::DeviceStart(DeviceStartReply {
        device_code: Secret::new(DEVICE_CODE),
        user_code: USER_CODE.to_string(),
        verification_uri: format!("{install}/app/device"),
        expires_in: 600,
        interval: 5,
    }))
}

fn pair(access: &str, refresh: &str) -> TokenPair {
    TokenPair {
        access_token: Secret::new(access),
        refresh_token: Secret::new(refresh),
        expires_in: 3600,
    }
}

fn polled(access: &str, refresh: &str) -> Reply {
    Ok(HubReply::DevicePoll(DevicePollReply::Tokens(pair(
        access, refresh,
    ))))
}

fn refreshed(access: &str, refresh: &str) -> Reply {
    Ok(HubReply::Refresh(RefreshReply::Tokens(pair(
        access, refresh,
    ))))
}

fn me_as(account: &str, handle: &str) -> Reply {
    Ok(HubReply::Me(MeReply {
        account: account.to_string(),
        handle: handle.to_string(),
    }))
}

fn me(handle: &str) -> Reply {
    me_as(ACCOUNT, handle)
}

fn lost() -> HubFailure {
    HubFailure::Transport {
        may_have_sent: true,
    }
}

fn unsent() -> HubFailure {
    HubFailure::Transport {
        may_have_sent: false,
    }
}

// ---------------------------------------------------------------------------------------------
// The fake transport and clock
// ---------------------------------------------------------------------------------------------

type Handler = Box<dyn FnMut(&HubCall) -> Reply + Send>;

/// One request the fake transport saw.
#[derive(Clone, Debug)]
struct Seen {
    op: &'static str,
    destination: String,
    bearer: Option<String>,
    claim: bool,
    ca_file: Option<PathBuf>,
    /// The device code, refresh token or revoked token the request carried.
    sent: Option<String>,
    machine_name: Option<String>,
}

/// A transport that answers each request with the next queued handler, which must be for that
/// operation, and records what it saw.
#[derive(Default)]
struct Fake {
    queue: Mutex<VecDeque<(&'static str, Handler)>>,
    seen: Mutex<Vec<Seen>>,
}

fn op_name(op: &HubOperation) -> &'static str {
    match op {
        HubOperation::Metadata => "Metadata",
        HubOperation::DeviceStart { .. } => "DeviceStart",
        HubOperation::DevicePoll { .. } => "DevicePoll",
        HubOperation::Refresh { .. } => "Refresh",
        HubOperation::Me => "Me",
        HubOperation::SignOut { .. } => "SignOut",
        _ => "Key",
    }
}

impl Fake {
    fn on(&self, op: &'static str, f: impl FnMut(&HubCall) -> Reply + Send + 'static) -> &Fake {
        self.queue.lock().unwrap().push_back((op, Box::new(f)));
        self
    }

    fn reply(&self, op: &'static str, r: Reply) -> &Fake {
        self.on(op, move |_| r.clone())
    }

    fn ops(&self) -> Vec<&'static str> {
        self.seen.lock().unwrap().iter().map(|s| s.op).collect()
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn clear(&self) {
        self.seen.lock().unwrap().clear();
    }

    /// Every queued reply was used.
    fn done(&self) {
        let left: Vec<&str> = self.queue.lock().unwrap().iter().map(|(o, _)| *o).collect();
        assert!(left.is_empty(), "unused replies: {left:?}");
    }
}

impl HubTransport for Fake {
    fn exchange(&self, call: HubCall) -> Reply {
        let op = op_name(&call.operation);
        let (sent, machine_name) = match &call.operation {
            HubOperation::DevicePoll { device_code } => (Some(device_code.expose()), None),
            HubOperation::Refresh { refresh_token } => (Some(refresh_token.expose()), None),
            HubOperation::SignOut { token } => (Some(token.expose()), None),
            HubOperation::DeviceStart { machine_name } => (None, Some(machine_name.clone())),
            _ => (None, None),
        };
        self.seen.lock().unwrap().push(Seen {
            op,
            destination: call.destination.clone(),
            bearer: call.bearer.as_ref().map(|b| b.expose().to_string()),
            claim: call.claim.is_some(),
            ca_file: call.ca_file.clone(),
            sent: sent.map(str::to_string),
            machine_name,
        });
        let next = self.queue.lock().unwrap().pop_front();
        let (want, mut handler) = next.unwrap_or_else(|| panic!("an unexpected {op} request"));
        assert_eq!(want, op, "the request order");
        handler(&call)
    }
}

/// A clock that moves only when a test moves it.
struct Clock(AtomicI64);

impl Clock {
    fn at(ms: i64) -> Clock {
        Clock(AtomicI64::new(ms))
    }

    fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl HubClock for Clock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------------------------
// Callers and cards
// ---------------------------------------------------------------------------------------------

fn human() -> Session {
    Session::human("human:nick".to_string(), Role::Writer).unwrap()
}

fn agent() -> Session {
    Session::agent(
        "agent:claude".to_string(),
        Some("r7".to_string()),
        Role::Writer,
    )
    .unwrap()
}

fn claim(s: &Session) -> RequestClaim {
    RequestClaim::from_session(s).unwrap()
}

fn caller(session: &Session, implicit: bool) -> AuthCaller<'_> {
    AuthCaller {
        session,
        person: "human:nick",
        implicit,
    }
}

/// The card, as the brief writes it.
fn card(install: &str, machine: &str) -> String {
    [
        "Sign in to a hub".to_string(),
        format!("Install: {install}"),
        format!("Machine: {machine}"),
        "This replaces this machine's saved sign-in for this install.".to_string(),
        "Confirm only if you asked to sign in to this install.".to_string(),
        "Answer: confirm or cancel".to_string(),
    ]
    .iter()
    .map(|l| format!("{l}\n"))
    .collect()
}

fn hash_of(text: &str) -> String {
    rdy_model::Hash::of(text.as_bytes()).to_string()
}

fn answer(install: &str, machine: &str, answer: AuthAnswer, relay: RelayInput) -> AuthConfirmation {
    AuthConfirmation {
        card_hash: hash_of(&card(install, machine)),
        answer,
        relay,
    }
}

fn confirm(install: &str) -> AuthConfirmation {
    answer(install, MACHINE, AuthAnswer::Confirm, RelayInput::default())
}

fn words() -> RelayInput {
    RelayInput {
        evidence: Some("yes, sign in to the hub".to_string()),
        question: Some("Sign in to https://hub.test as work laptop?".to_string()),
        ..RelayInput::default()
    }
}

/// What a paused call reports to the test thread: it is inside its paused request, or it
/// returned.
enum Step<T> {
    Inside,
    /// The call's result, or its panic (an assertion inside the transport).
    Done(std::thread::Result<Result<T, RdyError>>),
}

/// Runs `call` on a test thread and reports how it ended, panic included, so the test thread
/// never waits for a call that cannot report.
fn report<T>(tx: &mpsc::Sender<Step<T>>, call: impl FnOnce() -> Result<T, RdyError>) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(call));
    tx.send(Step::Done(r)).unwrap();
}

/// Waits until the call is inside its paused request; a call that ended first fails.
#[track_caller]
fn inside<T: std::fmt::Debug>(rx: &mpsc::Receiver<Step<T>>) {
    match rx.recv().unwrap() {
        Step::Inside => {}
        Step::Done(Ok(r)) => panic!("the call returned before its request: {r:?}"),
        Step::Done(Err(p)) => std::panic::resume_unwind(p),
    }
}

/// The call's result.
#[track_caller]
fn done<T>(rx: &mpsc::Receiver<Step<T>>) -> Result<T, RdyError> {
    match rx.recv().unwrap() {
        Step::Done(Ok(r)) => r,
        Step::Done(Err(p)) => std::panic::resume_unwind(p),
        Step::Inside => panic!("a second paused request"),
    }
}

#[track_caller]
fn assert_code<T: std::fmt::Debug>(r: Result<T, RdyError>, code: &str) -> RdyError {
    match r {
        Ok(v) => panic!("expected {code}, got {v:?}"),
        Err(e) => {
            assert_eq!(e.code, code, "{e:?}");
            e
        }
    }
}

/// Runs `run` with a probe that fails the `n`th (from 1) private write at `stage`.
fn failing_write<T>(n: usize, stage: PrivateWriteStage, run: impl FnOnce() -> T) -> T {
    let mut created = 0;
    with_private_write_probe_for_tests(
        move |s, _| {
            if s == PrivateWriteStage::Created {
                created += 1;
            }
            if created == n && s == stage {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        },
        run,
    )
}

// ---------------------------------------------------------------------------------------------
// The card
// ---------------------------------------------------------------------------------------------

#[test]
fn hsa5_host_card_binds_install_and_machine_with_relay_evidence() {
    let e = env();
    let fake = Fake::default();
    let clock = Clock::at(T0);
    let s = svc(&e, &fake, &clock);
    let person = human();
    let bot = agent();
    let install = origin(INSTALL);

    // No confirmation: the card, and nothing read, sent or written.
    let shown = s
        .start(&install, MACHINE, None, caller(&person, false))
        .unwrap();
    let text = card(INSTALL, MACHINE);
    assert_eq!(
        text,
        "Sign in to a hub\nInstall: https://hub.test\nMachine: work laptop\n\
         This replaces this machine's saved sign-in for this install.\n\
         Confirm only if you asked to sign in to this install.\nAnswer: confirm or cancel\n"
    );
    let hash = hash_of(&text);
    assert_eq!(hash.len(), 64);
    assert!(
        hash.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(
        shown,
        AuthState {
            install: install.clone(),
            state: AuthPhase::ConfirmationRequired,
            card: Some(text.clone()),
            card_hash: Some(hash.clone()),
            user_code: None,
            verification_uri: None,
            expires_at_ms: None,
            next_poll_at_ms: None,
            account: None,
            human_actor: None,
            warnings: vec![],
        }
    );
    let v = serde_json::to_value(&shown).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    let mut want = vec![
        "account",
        "card",
        "card_hash",
        "expires_at_ms",
        "human_actor",
        "install",
        "next_poll_at_ms",
        "state",
        "user_code",
        "verification_uri",
        "warnings",
    ];
    want.sort();
    let mut got = keys.clone();
    got.sort();
    assert_eq!(got, want);
    assert_eq!(v["state"], "confirmation_required");
    assert!(v["user_code"].is_null() && v["account"].is_null());
    // The agent gets the same card, and the canonical origin is what it names.
    assert_eq!(
        s.start(
            &origin("https://HUB.test:443/"),
            MACHINE,
            None,
            caller(&bot, true)
        )
        .unwrap(),
        shown
    );
    assert!(fake.seen().is_empty());
    assert!(!e.creds.exists() && !e.lock.exists());

    // A hash that is missing, malformed, or of another card is refused before anything.
    let other_machine = hash_of(&card(INSTALL, "other laptop"));
    let other_install = hash_of(&card(OTHER, MACHINE));
    let bad = [
        String::new(),
        "abc".to_string(),
        hash[..63].to_string(),
        hash.to_uppercase(),
        format!("{hash} "),
        other_machine,
        other_install,
    ];
    for h in &bad {
        for a in [AuthAnswer::Confirm, AuthAnswer::Cancel] {
            let c = AuthConfirmation {
                card_hash: h.clone(),
                answer: a,
                relay: RelayInput::default(),
            };
            let err = assert_code(
                s.start(&install, MACHINE, Some(&c), caller(&person, false)),
                E_UPSTREAM_CONFIRM_HOST,
            );
            assert_eq!(err.message, "the install host has not been confirmed");
            assert_eq!(
                err.fix,
                "show the sign-in card, then relay the answer with its card_hash"
            );
        }
    }
    // This card's hash does not confirm another install or machine.
    let c = confirm(INSTALL);
    assert_code(
        s.start(&origin(OTHER), MACHINE, Some(&c), caller(&person, false)),
        E_UPSTREAM_CONFIRM_HOST,
    );
    assert_code(
        s.start(&install, "other laptop", Some(&c), caller(&person, false)),
        E_UPSTREAM_CONFIRM_HOST,
    );

    // The relay table decides who relays the answer, with its own refusals.
    let cli = RelayInput {
        relayed_by: Some("agent:claude".to_string()),
        ..words()
    };
    let refused: Vec<(&Session, RelayInput, bool, &str)> = vec![
        (&person, words(), false, E_PERMISSION),
        (&person, words(), true, E_PERMISSION),
        (
            &person,
            RelayInput {
                question: Some("q".to_string()),
                ..RelayInput::default()
            },
            false,
            E_BAD_INPUT,
        ),
        (&bot, RelayInput::default(), true, E_PERMISSION),
        (&bot, RelayInput::default(), false, E_PERMISSION),
        (&bot, words(), false, E_BAD_INPUT),
        (&bot, cli.clone(), true, E_BAD_INPUT),
        (
            &bot,
            RelayInput {
                relayed_by: Some("agent:other".to_string()),
                ..words()
            },
            false,
            E_PERMISSION,
        ),
    ];
    for (session, relay, implicit, code) in refused {
        let want = relayed(session, "human:nick", &relay, implicit)
            .unwrap_err()
            .error();
        for a in [AuthAnswer::Confirm, AuthAnswer::Cancel] {
            let c = answer(INSTALL, MACHINE, a, relay.clone());
            let err = assert_code(
                s.start(&install, MACHINE, Some(&c), caller(session, implicit)),
                code,
            );
            assert_eq!(
                (err.message, err.fix),
                (want.message.clone(), want.fix.clone())
            );
        }
    }
    assert!(fake.seen().is_empty(), "no refusal sends anything");
    assert!(!e.creds.exists() && !e.lock.exists(), "or reads or writes");

    // Cancel: nothing is sent or written.
    let human_cancel = answer(INSTALL, MACHINE, AuthAnswer::Cancel, RelayInput::default());
    let agent_cancel = answer(INSTALL, MACHINE, AuthAnswer::Cancel, words());
    for (c, who) in [
        (&human_cancel, caller(&person, false)),
        (&agent_cancel, caller(&bot, true)),
    ] {
        let st = s.start(&install, MACHINE, Some(c), who).unwrap();
        assert_eq!(
            st,
            AuthState {
                state: AuthPhase::Cancelled,
                card: None,
                card_hash: None,
                ..shown.clone()
            }
        );
    }
    assert!(fake.seen().is_empty());
    assert!(!e.creds.exists() && !e.lock.exists());

    // A person confirms at the command line: the flow starts with the confirmed machine name.
    fake.reply("Metadata", meta(INSTALL))
        .reply("DeviceStart", device_start(INSTALL));
    let st = s
        .start(
            &install,
            MACHINE,
            Some(&confirm(INSTALL)),
            caller(&person, false),
        )
        .unwrap();
    assert_eq!(
        st,
        AuthState {
            install: install.clone(),
            state: AuthPhase::Pending,
            card: None,
            card_hash: None,
            user_code: Some(USER_CODE.to_string()),
            verification_uri: Some("https://hub.test/app/device".to_string()),
            expires_at_ms: Some(T0 + 600_000),
            next_poll_at_ms: Some(T0 + 5_000),
            account: None,
            human_actor: None,
            warnings: vec![],
        }
    );
    assert_eq!(fake.ops(), ["Metadata", "DeviceStart"]);
    let start = &fake.seen()[1];
    assert_eq!(start.machine_name.as_deref(), Some(MACHINE));
    assert_eq!(start.destination, "https://hub.test/auth/device");
    assert!(!start.claim && start.bearer.is_none());
    fake.done();

    // An agent over MCP relays the person's words: its session is the relay.
    fake.clear();
    fake.reply("Metadata", meta(OTHER))
        .reply("DeviceStart", device_start(OTHER));
    let c = answer(OTHER, MACHINE, AuthAnswer::Confirm, words());
    let st = s
        .start(&origin(OTHER), MACHINE, Some(&c), caller(&bot, true))
        .unwrap();
    assert_eq!(st.state, AuthPhase::Pending);
    // An agent at the command line names itself as the relay.
    fake.reply("Metadata", meta(THIRD))
        .reply("DeviceStart", device_start(THIRD));
    let c = answer(THIRD, MACHINE, AuthAnswer::Confirm, cli);
    let st = s
        .start(&origin(THIRD), MACHINE, Some(&c), caller(&bot, false))
        .unwrap();
    assert_eq!(st.state, AuthPhase::Pending);
    assert_eq!(
        fake.ops(),
        ["Metadata", "DeviceStart", "Metadata", "DeviceStart"]
    );
    fake.done();
    // The caller's session is unchanged: the relay's person session is never the caller.
    assert_eq!(bot, agent());

    // A machine name outside its bounds is refused before the card.
    for bad in ["", " lead", "trail ", "a\u{7}b", &"x".repeat(65)] {
        let err = assert_code(
            s.start(&install, bad, None, caller(&person, false)),
            E_BAD_INPUT,
        );
        assert_eq!(err.message, BAD_MACHINE_NAME_MESSAGE);
    }
    assert_eq!(fs::read_to_string(&e.user_file).unwrap(), CONFIG);

    // A production caller with no user config location.
    let err = require_user_file(None).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (E_CONFIG, NO_USER_FILE_MESSAGE, NO_USER_FILE_FIX)
    );
    assert_eq!(
        NO_USER_FILE_MESSAGE,
        "the user config path is not available"
    );
    assert_eq!(
        NO_USER_FILE_FIX,
        "set XDG_CONFIG_HOME or HOME before signing in"
    );
    assert_eq!(
        require_user_file(Some(e.user_file.clone())).unwrap(),
        e.user_file
    );
}

// ---------------------------------------------------------------------------------------------
// Re-sign-in
// ---------------------------------------------------------------------------------------------

#[test]
fn hsa5_resignin_revokes_before_replacing_the_old_credential() {
    let e = env();
    let clock = Clock::at(T0);
    let person = human();
    let install = origin(INSTALL);
    let old = ready(&old_access(), &old_refresh(), T0 + HOUR_MS, HANDLE);
    let other = ready(&new_access(), &new_refresh(), T0 + HOUR_MS, HANDLE);
    let seed_both = || seed(&e, &[(INSTALL, old.clone()), (OTHER, other.clone())]);
    let start = |s: &HubCredentials| {
        s.start(
            &install,
            MACHINE,
            Some(&confirm(INSTALL)),
            caller(&person, false),
        )
    };

    // Revoke under the lock, then the durable invalidation, then the start, then pending.
    seed_both();
    let fake = Fake::default();
    let (creds, lock) = (e.creds.clone(), e.lock.clone());
    let old_seen = old.clone();
    fake.reply("Metadata", meta(INSTALL))
        .on("SignOut", move |call| {
            assert!(call.bearer.is_none() && call.claim.is_none());
            assert!(!lock_is_free(&lock), "the revoke holds the credential lock");
            assert_eq!(
                on_disk(&creds, INSTALL),
                Some(old_seen.clone()),
                "the old record stays until the revoke succeeds"
            );
            Ok(HubReply::SignOut)
        });
    let (creds, lock) = (e.creds.clone(), e.lock.clone());
    let other_seen = other.clone();
    fake.on("DeviceStart", move |_| {
        assert!(!lock_is_free(&lock));
        assert_eq!(state_on_disk(&creds, INSTALL), Some("reauth_required"));
        let text = disk_text(&creds);
        assert!(!text.contains(&old_refresh()) && !text.contains(&old_access()));
        assert_eq!(on_disk(&creds, OTHER), Some(other_seen.clone()));
        device_start(INSTALL)
    });
    let st = start(&svc(&e, &fake, &clock)).unwrap();
    assert_eq!(st.state, AuthPhase::Pending);
    assert_eq!(fake.ops(), ["Metadata", "SignOut", "DeviceStart"]);
    let revoke = &fake.seen()[1];
    assert_eq!(revoke.destination, "https://hub.test/auth/revoke");
    assert_eq!(
        revoke.sent,
        Some(old_refresh()),
        "the stored refresh token, once"
    );
    fake.done();
    assert_eq!(state_on_disk(&e.creds, INSTALL), Some("pending"));
    assert_eq!(on_disk(&e.creds, OTHER), Some(other.clone()));

    // Each revoke failure keeps the old record byte for byte and starts nothing.
    let failures: Vec<(Reply, &str)> = vec![
        (Err(unsent()), E_UPSTREAM_TRANSPORT),
        (Err(lost()), E_UPSTREAM_UNCERTAIN),
        (Err(HubFailure::Refused), E_UPSTREAM_REFUSED),
        (Err(HubFailure::Protocol), E_UPSTREAM_PROTOCOL),
        (me(HANDLE), E_UPSTREAM_PROTOCOL),
    ];
    for (reply, code) in failures {
        seed_both();
        let before = fs::read(&e.creds).unwrap();
        let fake = Fake::default();
        fake.reply("Metadata", meta(INSTALL))
            .reply("SignOut", reply);
        assert_code(start(&svc(&e, &fake, &clock)), code);
        assert_eq!(fs::read(&e.creds).unwrap(), before, "{code}");
        assert_eq!(fake.ops(), ["Metadata", "SignOut"], "{code}");
        fake.done();
    }

    // A metadata failure changes nothing and revokes nothing.
    seed_both();
    let before = fs::read(&e.creds).unwrap();
    let fake = Fake::default();
    fake.reply("Metadata", Err(unsent()));
    assert_code(start(&svc(&e, &fake, &clock)), E_UPSTREAM_TRANSPORT);
    assert_eq!(fs::read(&e.creds).unwrap(), before);
    assert_eq!(fake.ops(), ["Metadata"]);

    // A failed invalidation after the revoke stops before the start.
    seed_both();
    let fake = Fake::default();
    fake.reply("Metadata", meta(INSTALL))
        .reply("SignOut", Ok(HubReply::SignOut));
    let s = svc(&e, &fake, &clock);
    assert_code(
        failing_write(1, PrivateWriteStage::Created, || start(&s)),
        E_IO,
    );
    assert_eq!(fake.ops(), ["Metadata", "SignOut"]);
    assert_eq!(fs::read(&e.creds).unwrap(), before);
    fake.done();

    // A failed start after the invalidation leaves reauth_required; nothing is restored.
    seed_both();
    let fake = Fake::default();
    fake.reply("Metadata", meta(INSTALL))
        .reply("SignOut", Ok(HubReply::SignOut))
        .reply("DeviceStart", Err(HubFailure::Refused));
    assert_code(start(&svc(&e, &fake, &clock)), E_UPSTREAM_REFUSED);
    assert_eq!(state_on_disk(&e.creds, INSTALL), Some("reauth_required"));
    assert!(!disk_text(&e.creds).contains(&old_refresh()));
    assert_eq!(on_disk(&e.creds, OTHER), Some(other.clone()));

    // An unexpired pending sign-in of the same machine is returned with no request.
    seed(&e, &[(INSTALL, pending(T0 + 600_000, T0 + 5_000, 5))]);
    let fake = Fake::default();
    let st = start(&svc(&e, &fake, &clock)).unwrap();
    assert_eq!(st.state, AuthPhase::Pending);
    assert_eq!(st.next_poll_at_ms, Some(T0 + 5_000));
    assert!(fake.seen().is_empty());

    // Another machine's pending sign-in, an expired one and reauth_required start anew, with
    // no revoke: no issued token exists.
    let starts = [
        pending_on(INSTALL, "old box", T0 + 600_000, T0 + 5_000, 5),
        pending(T0, T0, 5),
        Credential::ReauthRequired,
    ];
    for c in starts {
        seed(&e, &[(INSTALL, c)]);
        let fake = Fake::default();
        let creds = e.creds.clone();
        fake.reply("Metadata", meta(INSTALL))
            .on("DeviceStart", move |_| {
                assert_eq!(state_on_disk(&creds, INSTALL), Some("reauth_required"));
                device_start(INSTALL)
            });
        let st = start(&svc(&e, &fake, &clock)).unwrap();
        assert_eq!(st.state, AuthPhase::Pending);
        assert_eq!(fake.ops(), ["Metadata", "DeviceStart"]);
        let saved = on_disk(&e.creds, INSTALL);
        let Some(Credential::Pending(p)) = saved else {
            panic!("{saved:?}")
        };
        assert_eq!(p.machine_name, MACHINE);
    }
    assert_eq!(fs::read_to_string(&e.user_file).unwrap(), CONFIG);
}

/// Fix round 1: a confirmed start that rereads after discovery returns the sign-in another
/// caller finished meanwhile. It revokes nothing and starts nothing. The other caller's write
/// happens inside this start's metadata request, when it holds no lock.
#[test]
fn hsa5_older_start_returns_a_signin_completed_during_discovery() {
    let e = env();
    let clock = Clock::at(T0);
    let person = human();
    let install = origin(INSTALL);
    let old = ready(&old_access(), &old_refresh(), T0 + HOUR_MS, HANDLE);
    let winner = ready(
        &new_access(),
        &new_refresh(),
        T0 + HOUR_MS,
        "test-person-new",
    );
    let other = ready(&old_access(), &new_refresh(), T0 + HOUR_MS, HANDLE);
    let start = |s: &HubCredentials| {
        s.start(
            &install,
            MACHINE,
            Some(&confirm(INSTALL)),
            caller(&person, false),
        )
    };

    // What the start reads first, and what another caller saves during its discovery.
    let cases: Vec<(&str, Option<Credential>, Credential)> = vec![
        ("absent, then ready", None, winner.clone()),
        ("ready, then a new ready", Some(old.clone()), winner.clone()),
        (
            "reauth_required, then ready",
            Some(Credential::ReauthRequired),
            winner.clone(),
        ),
        (
            "another machine pending, then ready",
            Some(pending_on(INSTALL, "old box", T0 + 600_000, T0 + 5_000, 5)),
            winner.clone(),
        ),
        (
            "absent, then this machine pending",
            None,
            pending(T0 + 600_000, T0 + 5_000, 5),
        ),
    ];
    for (name, first, saved) in cases {
        let mut records = vec![(OTHER, other.clone())];
        if let Some(c) = &first {
            records.push((INSTALL, c.clone()));
        }
        seed(&e, &records);
        let fake = Fake::default();
        let (creds, lock, won) = (e.creds.clone(), e.lock.clone(), saved.clone());
        let keep = other.clone();
        fake.on("Metadata", move |_| {
            assert!(lock_is_free(&lock), "{name}: discovery holds no lock");
            seed_at(&creds, &[(INSTALL, won.clone()), (OTHER, keep.clone())]);
            meta(INSTALL)
        });
        let st = start(&svc(&e, &fake, &clock)).unwrap();
        assert_eq!(fake.ops(), ["Metadata"], "{name}: no revoke and no start");
        fake.done();
        assert_eq!(on_disk(&e.creds, INSTALL), Some(saved.clone()), "{name}");
        assert_eq!(on_disk(&e.creds, OTHER), Some(other.clone()), "{name}");
        match &saved {
            Credential::Ready(_) => {
                assert_eq!(st.state, AuthPhase::SignedIn, "{name}");
                assert_eq!(st.account.as_deref(), Some(ACCOUNT), "{name}");
                assert_eq!(
                    st.human_actor.as_deref(),
                    Some("human:test-person-new"),
                    "{name}"
                );
                assert_eq!(st.expires_at_ms, Some(T0 + HOUR_MS), "{name}");
            }
            _ => {
                assert_eq!(st.state, AuthPhase::Pending, "{name}");
                assert_eq!(st.next_poll_at_ms, Some(T0 + 5_000), "{name}");
            }
        }
    }
    assert_eq!(fs::read_to_string(&e.user_file).unwrap(), CONFIG);
}

// ---------------------------------------------------------------------------------------------
// Device polling
// ---------------------------------------------------------------------------------------------

#[test]
fn hsa5_device_state_polls_once_and_obeys_the_clock() {
    let e = env();
    let clock = Clock::at(T0);
    let install = origin(INSTALL);
    let c = claim(&human());
    let keep = ready(&new_access(), &new_refresh(), T0 + HOUR_MS, HANDLE);
    seed(
        &e,
        &[
            (INSTALL, pending(T0 + 600_000, T0 + 5_000, 5)),
            (OTHER, keep.clone()),
        ],
    );
    let fake = Fake::default();
    let s = svc(&e, &fake, &clock);

    // Before the next poll time: a local read.
    let st = s.state(&install, &c).unwrap();
    assert_eq!(st.state, AuthPhase::Pending);
    assert_eq!(st.user_code.as_deref(), Some(USER_CODE));
    assert_eq!(st.next_poll_at_ms, Some(T0 + 5_000));
    assert!(fake.seen().is_empty());

    // Due: one poll, after the record no longer holds the device code.
    clock.set(T0 + 5_000);
    let creds = e.creds.clone();
    fake.reply("Metadata", meta(INSTALL))
        .on("DevicePoll", move |call| {
            assert!(call.bearer.is_none() && call.claim.is_none());
            assert_eq!(state_on_disk(&creds, INSTALL), Some("reauth_required"));
            assert!(!disk_text(&creds).contains(DEVICE_CODE));
            Ok(HubReply::DevicePoll(DevicePollReply::Pending))
        });
    let st = s.state(&install, &c).unwrap();
    assert_eq!(st.state, AuthPhase::Pending);
    assert_eq!(st.next_poll_at_ms, Some(T0 + 10_000));
    assert_eq!(st.expires_at_ms, Some(T0 + 600_000));
    assert_eq!(fake.ops(), ["Metadata", "DevicePoll"]);
    let poll = &fake.seen()[1];
    assert_eq!(poll.destination, "https://hub.test/auth/token");
    assert_eq!(poll.sent.as_deref(), Some(DEVICE_CODE));
    assert_eq!(
        on_disk(&e.creds, INSTALL),
        Some(pending(T0 + 600_000, T0 + 10_000, 5))
    );
    // The same instant again: no request.
    assert_eq!(s.state(&install, &c).unwrap(), st);
    assert_eq!(fake.ops().len(), 2);
    fake.done();

    // slow_down: five more seconds, saved as now plus the new interval.
    clock.set(T0 + 10_000);
    fake.reply("Metadata", meta(INSTALL)).reply(
        "DevicePoll",
        Ok(HubReply::DevicePoll(DevicePollReply::SlowDown)),
    );
    let st = s.state(&install, &c).unwrap();
    assert_eq!(st.next_poll_at_ms, Some(T0 + 20_000));
    assert_eq!(
        on_disk(&e.creds, INSTALL),
        Some(pending(T0 + 600_000, T0 + 20_000, 10))
    );
    clock.set(T0 + 19_999);
    fake.clear();
    assert_eq!(s.state(&install, &c).unwrap().state, AuthPhase::Pending);
    assert!(fake.seen().is_empty());
    fake.done();

    // Past the 300-second ceiling the pending attempt ends.
    clock.set(T0);
    seed(
        &e,
        &[
            (INSTALL, pending(T0 + 600_000, T0, 300)),
            (OTHER, keep.clone()),
        ],
    );
    fake.reply("Metadata", meta(INSTALL)).reply(
        "DevicePoll",
        Ok(HubReply::DevicePoll(DevicePollReply::SlowDown)),
    );
    assert_code(s.state(&install, &c), E_UPSTREAM_PROTOCOL);
    assert_eq!(state_on_disk(&e.creds, INSTALL), Some("reauth_required"));
    fake.clear();
    assert_eq!(
        s.state(&install, &c).unwrap().state,
        AuthPhase::ReauthRequired
    );
    assert!(fake.seen().is_empty());
    fake.done();

    // Expiry is local: the record goes, other installs stay.
    seed(
        &e,
        &[
            (INSTALL, pending(T0 + 600_000, T0, 5)),
            (OTHER, keep.clone()),
        ],
    );
    clock.set(T0 + 600_000);
    let st = s.state(&install, &c).unwrap();
    assert_eq!(st, signed_out(&install));
    assert_eq!(on_disk(&e.creds, INSTALL), None);
    assert_eq!(on_disk(&e.creds, OTHER), Some(keep.clone()));
    assert!(fake.seen().is_empty());

    // expired_token and invalid_grant end the sign-in.
    clock.set(T0);
    for r in [DevicePollReply::Expired, DevicePollReply::InvalidGrant] {
        seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
        fake.reply("Metadata", meta(INSTALL))
            .reply("DevicePoll", Ok(HubReply::DevicePoll(r)));
        assert_eq!(s.state(&install, &c).unwrap(), signed_out(&install));
        assert_eq!(on_disk(&e.creds, INSTALL), None);
    }
    fake.done();

    // A refusal clears to reauth_required.
    seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
    fake.reply("Metadata", meta(INSTALL))
        .reply("DevicePoll", Err(HubFailure::Refused));
    assert_code(s.state(&install, &c), E_UPSTREAM_REFUSED);
    assert_eq!(state_on_disk(&e.creds, INSTALL), Some("reauth_required"));

    // Nothing was sent: the scheduled poll is restored.
    seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
    fake.reply("Metadata", meta(INSTALL))
        .reply("DevicePoll", Err(unsent()));
    assert_code(s.state(&install, &c), E_UPSTREAM_TRANSPORT);
    assert_eq!(
        on_disk(&e.creds, INSTALL),
        Some(pending(T0 + 600_000, T0 + 5_000, 5))
    );

    // A poll that may have been consumed, or a reply that does not fit, is never sent again.
    let ambiguous: Vec<Reply> = vec![
        Err(lost()),
        Err(HubFailure::Protocol),
        Err(HubFailure::AuthRequired),
        me(HANDLE),
    ];
    for r in ambiguous {
        seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
        fake.reply("Metadata", meta(INSTALL)).reply("DevicePoll", r);
        assert_code(s.state(&install, &c), E_UPSTREAM_AUTH);
        assert_eq!(state_on_disk(&e.creds, INSTALL), Some("reauth_required"));
        assert!(!disk_text(&e.creds).contains(DEVICE_CODE));
        fake.clear();
        assert_eq!(
            s.state(&install, &c).unwrap().state,
            AuthPhase::ReauthRequired
        );
        assert!(fake.seen().is_empty());
    }
    fake.done();

    // A failed pre-send save sends nothing and keeps the pending record.
    seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
    let before = fs::read(&e.creds).unwrap();
    fake.clear();
    fake.reply("Metadata", meta(INSTALL));
    assert_code(
        failing_write(1, PrivateWriteStage::Created, || s.state(&install, &c)),
        E_IO,
    );
    assert_eq!(fake.ops(), ["Metadata"]);
    assert_eq!(fs::read(&e.creds).unwrap(), before);

    // A metadata failure changes nothing.
    fake.clear();
    fake.reply("Metadata", Err(HubFailure::Protocol));
    assert_code(s.state(&install, &c), E_UPSTREAM_PROTOCOL);
    assert_eq!(fs::read(&e.creds).unwrap(), before);
    assert_eq!(fake.ops(), ["Metadata"]);
    fake.done();

    // The other states are local reads.
    seed(&e, &[(INSTALL, keep.clone())]);
    let st = s.state(&install, &c).unwrap();
    assert_eq!(st.state, AuthPhase::SignedIn);
    assert_eq!(st.account.as_deref(), Some(ACCOUNT));
    assert_eq!(st.human_actor.as_deref(), Some("human:test-person"));
    assert_eq!(st.expires_at_ms, Some(T0 + HOUR_MS));
    assert!(st.user_code.is_none() && st.card.is_none());
    assert_eq!(
        s.state(&origin(THIRD), &c).unwrap(),
        signed_out(&origin(THIRD))
    );
    assert_eq!(fake.ops(), ["Metadata"]);
}

fn signed_out(install: &Origin) -> AuthState {
    AuthState {
        install: install.clone(),
        state: AuthPhase::SignedOut,
        card: None,
        card_hash: None,
        user_code: None,
        verification_uri: None,
        expires_at_ms: None,
        next_poll_at_ms: None,
        account: None,
        human_actor: None,
        warnings: vec![],
    }
}

#[test]
fn hsa5_signin_reads_me_before_ready_save() {
    let e = env();
    let clock = Clock::at(T0);
    let install = origin(INSTALL);
    let c = claim(&agent());
    let events = Arc::new(Mutex::new(Vec::<String>::new()));

    seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
    let fake = Fake::default();
    let ev = Arc::clone(&events);
    fake.reply("Metadata", meta(INSTALL))
        .on("DevicePoll", move |_| {
            ev.lock().unwrap().push("poll".to_string());
            polled(&new_access(), &new_refresh())
        });
    let (ev, creds, lock) = (Arc::clone(&events), e.creds.clone(), e.lock.clone());
    fake.on("Me", move |call| {
        ev.lock().unwrap().push("me".to_string());
        assert_eq!(call.destination, "https://hub.test/api/v1/me");
        assert_eq!(
            call.bearer.as_ref().map(Secret::expose),
            Some(new_access().as_str())
        );
        assert!(call.claim.is_none(), "/me carries no claim");
        assert!(!lock_is_free(&lock), "the lock is held across /me");
        assert_eq!(state_on_disk(&creds, INSTALL), Some("reauth_required"));
        assert!(!disk_text(&creds).contains(&new_access()));
        me(HANDLE)
    });
    let s = svc(&e, &fake, &clock);
    let (ev, creds) = (Arc::clone(&events), e.creds.clone());
    let st = with_private_write_probe_for_tests(
        move |stage, _| {
            if stage == PrivateWriteStage::Renamed {
                let state = state_on_disk(&creds, INSTALL).unwrap_or("none");
                ev.lock().unwrap().push(format!("saved {state}"));
            }
            Ok(())
        },
        || s.state(&install, &c),
    )
    .unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        ["saved reauth_required", "poll", "me", "saved ready"]
    );
    assert_eq!(st.state, AuthPhase::SignedIn);
    assert_eq!(st.account.as_deref(), Some(ACCOUNT));
    assert_eq!(st.human_actor.as_deref(), Some("human:test-person"));
    assert_eq!(st.expires_at_ms, Some(T0 + HOUR_MS));
    assert!(
        st.user_code.is_none() && st.verification_uri.is_none() && st.next_poll_at_ms.is_none()
    );
    assert_eq!(
        on_disk(&e.creds, INSTALL),
        Some(ready(&new_access(), &new_refresh(), T0 + HOUR_MS, HANDLE))
    );
    fake.done();

    // A failed /me leaves reauth_required, and the device code is not kept for a retry.
    let failed: Vec<Reply> = vec![
        Err(HubFailure::AuthRequired),
        Err(lost()),
        Err(HubFailure::Protocol),
        Ok(HubReply::SignOut),
    ];
    for r in failed {
        seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
        let fake = Fake::default();
        fake.reply("Metadata", meta(INSTALL))
            .reply("DevicePoll", polled(&new_access(), &new_refresh()))
            .reply("Me", r);
        assert_code(svc(&e, &fake, &clock).state(&install, &c), E_UPSTREAM_AUTH);
        assert_eq!(state_on_disk(&e.creds, INSTALL), Some("reauth_required"));
        let text = disk_text(&e.creds);
        assert!(!text.contains(DEVICE_CODE) && !text.contains(&new_access()));
        fake.done();
    }

    // A `/me` handle outside the actor form is the hub's malformed reply: E_UPSTREAM_PROTOCOL,
    // the same record and no ready credential (also pinned in id5).
    seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
    let fake = Fake::default();
    fake.reply("Metadata", meta(INSTALL))
        .reply("DevicePoll", polled(&new_access(), &new_refresh()))
        .reply("Me", me("two words"));
    assert_code(
        svc(&e, &fake, &clock).state(&install, &c),
        E_UPSTREAM_PROTOCOL,
    );
    assert_eq!(state_on_disk(&e.creds, INSTALL), Some("reauth_required"));
    let text = disk_text(&e.creds);
    assert!(!text.contains(DEVICE_CODE) && !text.contains(&new_access()));
    fake.done();

    // An issued pair that does not have its form is not used: no /me.
    seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
    let fake = Fake::default();
    let bad = TokenPair {
        expires_in: 0,
        ..pair(&new_access(), &new_refresh())
    };
    fake.reply("Metadata", meta(INSTALL)).reply(
        "DevicePoll",
        Ok(HubReply::DevicePoll(DevicePollReply::Tokens(bad))),
    );
    assert_code(svc(&e, &fake, &clock).state(&install, &c), E_UPSTREAM_AUTH);
    assert_eq!(fake.ops(), ["Metadata", "DevicePoll"]);
    assert_eq!(state_on_disk(&e.creds, INSTALL), Some("reauth_required"));

    // A failed final save before the rename: no access and reauth_required.
    let full = |fake: &Fake| {
        fake.reply("Metadata", meta(INSTALL))
            .reply("DevicePoll", polled(&new_access(), &new_refresh()))
            .reply("Me", me(HANDLE));
    };
    seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
    let fake = Fake::default();
    full(&fake);
    let s = svc(&e, &fake, &clock);
    assert_code(
        failing_write(2, PrivateWriteStage::Synced, || s.state(&install, &c)),
        E_UPSTREAM_AUTH,
    );
    assert_eq!(state_on_disk(&e.creds, INSTALL), Some("reauth_required"));
    assert!(!disk_text(&e.creds).contains(&new_refresh()));
    fake.done();

    // After the rename: no signed_in reply, and the next locked read finds the complete bytes.
    seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
    let fake = Fake::default();
    full(&fake);
    let s = svc(&e, &fake, &clock);
    assert_code(
        failing_write(2, PrivateWriteStage::DirSynced, || s.state(&install, &c)),
        E_UPSTREAM_AUTH,
    );
    fake.done();
    fake.clear();
    assert_eq!(s.state(&install, &c).unwrap().state, AuthPhase::SignedIn);
    assert!(fake.seen().is_empty());
}

// ---------------------------------------------------------------------------------------------
// One refresh across processes
// ---------------------------------------------------------------------------------------------

/// The pair a contender's refresh would return: distinct per role, so a second refresh shows.
fn role_pair(role: &str) -> (String, String) {
    (
        token('a', &format!("WIN{role}ACCESS")),
        token('r', &format!("WIN{role}REFRESH")),
    )
}

fn mark(text: &str) {
    let mut out = io::stdout().lock();
    writeln!(out, "HSA5 {text}").unwrap();
    out.flush().unwrap();
}

fn wait_line() {
    let mut line = String::new();
    io::stdin().read_line(&mut line).unwrap();
}

/// A contender's transport: metadata (held for role `b` until the parent writes a line), and
/// a refresh that is logged to the shared file and held until the parent writes a line.
struct ChildTransport {
    role: String,
    log: PathBuf,
}

impl HubTransport for ChildTransport {
    fn exchange(&self, call: HubCall) -> Reply {
        match &call.operation {
            HubOperation::Metadata => {
                mark("meta");
                if self.role == "b" {
                    wait_line();
                }
                meta(INSTALL)
            }
            HubOperation::Refresh { refresh_token } => {
                let mut log = OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(&self.log)
                    .unwrap();
                writeln!(log, "refresh {} {}", self.role, refresh_token.expose()).unwrap();
                mark("refreshing");
                wait_line();
                let (a, r) = role_pair(&self.role);
                refreshed(&a, &r)
            }
            op => panic!("a contender sent {}", op_name(op)),
        }
    }
}

/// The child half of the process test: a project (or, with no project, a credential consumer)
/// asking for access. It does nothing unless the parent started it.
#[test]
fn hsa5_contender_child() {
    let Ok(role) = std::env::var("RDY_HSA5_ROLE") else {
        return;
    };
    let user_file = PathBuf::from(std::env::var("RDY_HSA5_USER_FILE").unwrap());
    let project = std::env::var_os("RDY_HSA5_PROJECT").map(PathBuf::from);
    let log = PathBuf::from(std::env::var("RDY_HSA5_LOG").unwrap());
    let transport = ChildTransport {
        role: role.clone(),
        log,
    };
    let clock = Clock::at(T0);
    let s = HubCredentials::new(project.as_deref(), &user_file, &transport, &clock);
    let session = if role == "c" { agent() } else { human() };
    mark("start");
    match s.access(&origin(INSTALL), &claim(&session)) {
        Ok(g) => mark(&format!("done {}", g.access_token.expose())),
        Err(e) => mark(&format!("error {}", e.code)),
    }
}

struct Contender {
    child: Child,
    stdin: ChildStdin,
    lines: io::Lines<BufReader<std::process::ChildStdout>>,
}

impl Contender {
    fn spawn(e: &Env, role: &str, project: Option<&Path>, log: &Path) -> Contender {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["hsa5_contender_child", "--exact", "--nocapture"])
            .args(["--test-threads=1"])
            .env("RDY_HSA5_ROLE", role)
            .env("RDY_HSA5_USER_FILE", &e.user_file)
            .env("RDY_HSA5_LOG", log)
            .env_remove("RDY_HSA5_PROJECT")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(p) = project {
            cmd.env("RDY_HSA5_PROJECT", p);
        }
        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        Contender {
            child,
            stdin,
            lines,
        }
    }

    fn next(&mut self) -> String {
        for l in self.lines.by_ref() {
            let l = l.unwrap();
            // libtest may print "test <name> ... " on the same line first
            if let Some((_, rest)) = l.split_once("HSA5 ") {
                return rest.to_string();
            }
        }
        panic!("the contender ended before its next mark");
    }

    fn release(&mut self) {
        writeln!(self.stdin, "go").unwrap();
        self.stdin.flush().unwrap();
    }

    fn finish(mut self) {
        assert!(self.child.wait().unwrap().success());
    }
}

#[test]
fn hsa5_two_projects_and_a_consumer_share_one_refresh() {
    let e = env();
    let expiring = ready(&old_access(), &old_refresh(), T0 + 30_000, HANDLE);
    seed(&e, &[(INSTALL, expiring)]);
    let project_a = e.root.join("project-a");
    let project_b = e.root.join("project-b");
    fs::create_dir(&project_a).unwrap();
    fs::create_dir(&project_b).unwrap();
    let log = e.root.join("refresh.log");

    // B reads the expiring record, releases the lock and is held in discovery.
    let mut b = Contender::spawn(&e, "b", Some(&project_b), &log);
    assert_eq!(b.next(), "start");
    assert_eq!(b.next(), "meta");
    // A reads, discovers, takes the lock again and is held inside its refresh.
    let mut a = Contender::spawn(&e, "a", Some(&project_a), &log);
    assert_eq!(a.next(), "start");
    assert_eq!(a.next(), "meta");
    assert_eq!(a.next(), "refreshing");
    assert!(
        !lock_is_free(&e.lock),
        "A holds the credential lock through its refresh"
    );
    // B leaves discovery and the consumer starts: both wait for A's lock.
    b.release();
    let mut c = Contender::spawn(&e, "c", None, &log);
    assert_eq!(c.next(), "start");
    // A's pair exists only after this line: whoever returns it read A's saved record.
    a.release();
    let (win_access, win_refresh) = role_pair("a");
    let done = format!("done {win_access}");
    assert_eq!(a.next(), done);
    assert_eq!(
        b.next(),
        done,
        "B rereads under the lock and takes the winner's pair"
    );
    assert_eq!(c.next(), done, "the consumer gets the winner's pair");
    a.finish();
    b.finish();
    c.finish();
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        format!("refresh a {}\n", old_refresh()),
        "exactly one refresh, with the old token"
    );
    assert_eq!(
        on_disk(&e.creds, INSTALL),
        Some(ready(&win_access, &win_refresh, T0 + HOUR_MS, HANDLE))
    );
    assert_eq!(fs::read_to_string(&e.user_file).unwrap(), CONFIG);
}

#[test]
fn hsa5_state_and_installs_wait_for_the_credential_writer() {
    let e = env();
    let clock = Clock::at(T0);
    let install = origin(INSTALL);
    let keep = Credential::ReauthRequired;
    seed(
        &e,
        &[
            (INSTALL, pending(T0 + 600_000, T0, 5)),
            (OTHER, keep.clone()),
        ],
    );
    let (ev_tx, ev_rx) = mpsc::channel::<Step<AuthState>>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let writer = Fake::default();
    let in_poll = ev_tx.clone();
    writer
        .reply("Metadata", meta(INSTALL))
        .on("DevicePoll", move |_| {
            in_poll.send(Step::Inside).unwrap();
            go_rx.lock().unwrap().recv().unwrap();
            polled(&new_access(), &new_refresh())
        })
        .reply("Me", me(HANDLE));
    let readers = Fake::default();
    let (calling_tx, calling_rx) = mpsc::channel::<&str>();
    let (rows_tx, rows_rx) = mpsc::channel::<Vec<InstallState>>();
    let (state_tx, state_rx) = mpsc::channel::<AuthState>();
    std::thread::scope(|scope| {
        // Owned here, so a failed assertion ends the writer's wait instead of hanging.
        let go_tx = go_tx;
        scope.spawn(|| {
            report(&ev_tx, || {
                svc(&e, &writer, &clock).state(&install, &claim(&human()))
            })
        });
        inside(&ev_rx);
        // The writer is inside its poll, holding the lock; the disk shows reauth_required.
        assert!(!lock_is_free(&e.lock));
        assert_eq!(state_on_disk(&e.creds, INSTALL), Some("reauth_required"));
        let (calling, rows_tx) = (calling_tx.clone(), rows_tx.clone());
        let (e_ref, readers_ref, clock_ref) = (&e, &readers, &clock);
        scope.spawn(move || {
            calling.send("installs").unwrap();
            rows_tx
                .send(svc(e_ref, readers_ref, clock_ref).installs().unwrap())
                .unwrap();
        });
        let (calling, state_tx, install_ref) = (calling_tx.clone(), state_tx.clone(), &install);
        scope.spawn(move || {
            calling.send("state").unwrap();
            let s = svc(e_ref, readers_ref, clock_ref);
            state_tx
                .send(s.state(install_ref, &claim(&agent())).unwrap())
                .unwrap();
        });
        calling_rx.recv().unwrap();
        calling_rx.recv().unwrap();
        // Both readers have started; while the writer holds the lock, neither has a result.
        assert!(rows_rx.try_recv().is_err() && state_rx.try_recv().is_err());
        go_tx.send(()).unwrap();
        assert_eq!(done(&ev_rx).unwrap().state, AuthPhase::SignedIn);
    });
    // They report the committed state, never the writer's intermediate one.
    let rows = rows_rx.recv().unwrap();
    assert_eq!(
        rows,
        [
            InstallState {
                install: install.clone(),
                state: AuthPhase::SignedIn,
                account: Some(ACCOUNT.to_string()),
                human_actor: Some("human:test-person".to_string()),
                expires_at_ms: Some(T0 + HOUR_MS),
            },
            InstallState {
                install: origin(OTHER),
                state: AuthPhase::ReauthRequired,
                account: None,
                human_actor: None,
                expires_at_ms: None,
            },
        ]
    );
    let st = state_rx.recv().unwrap();
    assert_eq!(st.state, AuthPhase::SignedIn);
    assert_eq!(st.human_actor.as_deref(), Some("human:test-person"));
    assert!(readers.seen().is_empty(), "the readers sent nothing");
    writer.done();

    // installs: saved states only, sorted; an expired pending sign-in reads as signed_out.
    seed(
        &e,
        &[
            (THIRD, pending_on(THIRD, MACHINE, T0 + 600_000, T0, 5)),
            (OTHER, pending_on(OTHER, MACHINE, T0, T0, 5)),
        ],
    );
    let rows = svc(&e, &readers, &clock).installs().unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| (r.install.as_str(), r.state, r.expires_at_ms))
            .collect::<Vec<_>>(),
        [
            (OTHER, AuthPhase::SignedOut, None),
            (THIRD, AuthPhase::Pending, Some(T0 + 600_000)),
        ]
    );
    // installs reads and writes nothing else.
    assert_eq!(state_on_disk(&e.creds, OTHER), Some("pending"));
    let empty = env();
    assert!(svc(&empty, &readers, &clock).installs().unwrap().is_empty());
    assert!(!empty.creds.exists());
}

// ---------------------------------------------------------------------------------------------
// Refresh
// ---------------------------------------------------------------------------------------------

#[test]
fn hsa5_interrupted_refresh_never_reuses_the_old_token() {
    let e = env();
    let clock = Clock::at(T0);
    let install = origin(INSTALL);
    let c = claim(&human());
    let expiring = ready(&old_access(), &old_refresh(), T0 + 30_000, HANDLE);
    let no_old = |creds: &Path| {
        let text = disk_text(creds);
        assert!(
            !text.contains(&old_refresh()),
            "the old refresh token is on disk"
        );
        assert!(!text.contains(&old_access()));
    };

    // The pre-send invalidation fails: nothing is sent, and the token was never spent.
    seed(&e, &[(INSTALL, expiring.clone())]);
    let before = fs::read(&e.creds).unwrap();
    let fake = Fake::default();
    fake.reply("Metadata", meta(INSTALL));
    let s = svc(&e, &fake, &clock);
    assert_code(
        failing_write(1, PrivateWriteStage::Written, || s.access(&install, &c)),
        E_IO,
    );
    assert_eq!(fake.ops(), ["Metadata"]);
    assert_eq!(fs::read(&e.creds).unwrap(), before);
    fake.done();

    // The send fails, is lost, or its reply does not fit: no reusable token stays on disk.
    let failures: Vec<Reply> = vec![
        Err(unsent()),
        Err(lost()),
        Err(HubFailure::Refused),
        Err(HubFailure::Protocol),
        Err(HubFailure::AuthRequired),
        Ok(HubReply::Refresh(RefreshReply::AuthRequired)),
        Ok(HubReply::Refresh(RefreshReply::Tokens(TokenPair {
            expires_in: 3601,
            ..pair(&new_access(), &new_refresh())
        }))),
        Ok(HubReply::Refresh(RefreshReply::Tokens(pair(
            &new_access(),
            "rdyh_r_notatoken",
        )))),
        Ok(HubReply::SignOut),
    ];
    for r in failures {
        seed(&e, &[(INSTALL, expiring.clone())]);
        let fake = Fake::default();
        let creds = e.creds.clone();
        fake.reply("Metadata", meta(INSTALL))
            .on("Refresh", move |_| {
                assert_eq!(state_on_disk(&creds, INSTALL), Some("reauth_required"));
                r.clone()
            });
        let s = svc(&e, &fake, &clock);
        let err = assert_code(s.access(&install, &c), E_UPSTREAM_AUTH);
        assert_eq!(err.message, "sign-in is required for https://hub.test");
        assert_eq!(fake.seen()[1].sent, Some(old_refresh()));
        assert_eq!(state_on_disk(&e.creds, INSTALL), Some("reauth_required"));
        no_old(&e.creds);
        fake.done();
        // And no later call sends anything.
        fake.clear();
        assert_code(s.access(&install, &c), E_UPSTREAM_AUTH);
        assert!(fake.seen().is_empty());
    }

    // The rotated pair's save fails before its rename: no access, nothing reusable.
    for stage in [
        PrivateWriteStage::Created,
        PrivateWriteStage::Written,
        PrivateWriteStage::Synced,
    ] {
        seed(&e, &[(INSTALL, expiring.clone())]);
        let fake = Fake::default();
        fake.reply("Metadata", meta(INSTALL))
            .reply("Refresh", refreshed(&new_access(), &new_refresh()));
        let s = svc(&e, &fake, &clock);
        assert_code(
            failing_write(2, stage, || s.access(&install, &c)),
            E_UPSTREAM_AUTH,
        );
        assert_eq!(state_on_disk(&e.creds, INSTALL), Some("reauth_required"));
        no_old(&e.creds);
        assert!(!disk_text(&e.creds).contains(&new_refresh()));
        fake.done();
    }

    // After its rename: still no access returned; the next locked read finds the new pair.
    for stage in [PrivateWriteStage::Renamed, PrivateWriteStage::DirSynced] {
        seed(&e, &[(INSTALL, expiring.clone())]);
        let fake = Fake::default();
        fake.reply("Metadata", meta(INSTALL))
            .reply("Refresh", refreshed(&new_access(), &new_refresh()));
        let s = svc(&e, &fake, &clock);
        assert_code(
            failing_write(2, stage, || s.access(&install, &c)),
            E_UPSTREAM_AUTH,
        );
        no_old(&e.creds);
        fake.done();
        fake.clear();
        let g = s.access(&install, &c).unwrap();
        assert_eq!(g.access_token.expose(), new_access());
        assert!(
            fake.seen().is_empty(),
            "the saved pair is used, not refreshed again"
        );
    }

    // A record that is not ready is E_UPSTREAM_AUTH with no request.
    for rec in [Credential::ReauthRequired, pending(T0 + 600_000, T0, 5)] {
        seed(&e, &[(INSTALL, rec)]);
        let fake = Fake::default();
        assert_code(svc(&e, &fake, &clock).access(&install, &c), E_UPSTREAM_AUTH);
        assert!(fake.seen().is_empty());
    }
    let fake = Fake::default();
    assert_code(
        svc(&e, &fake, &clock).access(&origin(THIRD), &c),
        E_UPSTREAM_AUTH,
    );
    assert!(fake.seen().is_empty());
}

#[test]
// The probes call through `&SerProbe` on purpose: that borrow is what selects the fallback.
#[allow(clippy::needless_borrow)]
fn hsa5_rotated_tokens_are_saved_before_access_is_returned() {
    let e = env();
    let clock = Clock::at(T0);
    let install = origin(INSTALL);
    let person = human();
    let bot = agent();
    // 60 seconds or less left: refreshed; more: cached.
    seed(
        &e,
        &[(
            INSTALL,
            ready(&old_access(), &old_refresh(), T0 + 60_000, HANDLE),
        )],
    );
    let events = Rc::new(RefCell::new(Vec::<String>::new()));
    let fake = Fake::default();
    let (creds, lock) = (e.creds.clone(), e.lock.clone());
    fake.reply("Metadata", meta(INSTALL))
        .on("Refresh", move |call| {
            assert!(call.bearer.is_none() && call.claim.is_none());
            assert_eq!(call.destination, "https://hub.test/auth/token");
            assert!(!lock_is_free(&lock));
            assert_eq!(state_on_disk(&creds, INSTALL), Some("reauth_required"));
            refreshed(&new_access(), &new_refresh())
        });
    let s = svc(&e, &fake, &clock);
    let (ev, creds, lock) = (Rc::clone(&events), e.creds.clone(), e.lock.clone());
    let reader: Rc<RefCell<Option<std::thread::JoinHandle<Vec<InstallState>>>>> =
        Rc::new(RefCell::new(None));
    let (reader_in, user_file) = (Rc::clone(&reader), e.user_file.clone());
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let mut writes = 0;
    let grant = with_private_write_probe_for_tests(
        move |stage, path| {
            if stage == PrivateWriteStage::Created {
                writes += 1;
                assert_eq!(
                    mode(path),
                    0o600,
                    "the secret temp is 0600 before its bytes"
                );
            }
            if writes == 2 && stage == PrivateWriteStage::Synced {
                // Paused before the rotated pair is renamed into place: the lock is held,
                // the disk has no pair, and a reader that starts now must wait.
                assert!(!lock_is_free(&lock));
                assert_eq!(state_on_disk(&creds, INSTALL), Some("reauth_required"));
                let (uf, done) = (user_file.clone(), done_tx.clone());
                *reader_in.borrow_mut() = Some(std::thread::spawn(move || {
                    let fake = Fake::default();
                    let clock = Clock::at(T0);
                    let rows = HubCredentials::new(None, &uf, &fake, &clock)
                        .installs()
                        .unwrap();
                    done.send(()).unwrap();
                    rows
                }));
                ev.borrow_mut().push("paused".to_string());
            }
            if writes == 2 && stage == PrivateWriteStage::Renamed {
                assert_eq!(
                    on_disk(&creds, INSTALL),
                    Some(ready(&new_access(), &new_refresh(), T0 + HOUR_MS, HANDLE))
                );
                ev.borrow_mut().push("saved".to_string());
            }
            Ok(())
        },
        || s.access(&install, &claim(&person)),
    )
    .unwrap();
    events.borrow_mut().push("returned".to_string());
    assert_eq!(*events.borrow(), ["paused", "saved", "returned"]);
    assert_eq!(grant.access_token.expose(), new_access());
    assert_eq!(grant.expires_at_ms, T0 + HOUR_MS);
    assert_eq!(grant.account, ACCOUNT);
    assert_eq!(grant.human_actor, "human:test-person");
    assert_eq!(grant.install, install);
    assert_eq!(grant.claim.actor, "human:test-person");
    assert_eq!(grant.claim.run.as_deref(), Some("cli"));
    grant.claim.validate_for_send().unwrap();
    fake.done();
    // The reader that started during the pause saw only the committed pair.
    let rows = reader.borrow_mut().take().unwrap().join().unwrap();
    done_rx.recv().unwrap();
    assert_eq!(rows[0].state, AuthPhase::SignedIn);
    assert_eq!(rows[0].expires_at_ms, Some(T0 + HOUR_MS));

    // Cached while more than a minute is left; an agent keeps its own actor and run.
    fake.clear();
    clock.set(T0 + HOUR_MS - 60_001);
    let g = s.access(&install, &claim(&bot)).unwrap();
    assert_eq!(g.access_token.expose(), new_access());
    assert_eq!(
        (g.claim.actor.as_str(), g.claim.run.as_deref()),
        ("agent:claude", Some("r7"))
    );
    assert!(fake.seen().is_empty());
    // At one minute left it refreshes again, with the rotated token.
    clock.set(T0 + HOUR_MS - 60_000);
    let (a2, r2) = (token('a', "THIRDACCESS"), token('r', "THIRDREFRESH"));
    fake.reply("Metadata", meta(INSTALL))
        .reply("Refresh", refreshed(&a2, &r2));
    let g = s.access(&install, &claim(&bot)).unwrap();
    assert_eq!(g.access_token.expose(), a2);
    assert_eq!(fake.seen()[1].sent, Some(new_refresh()));
    assert_eq!(fs::read_to_string(&e.user_file).unwrap(), CONFIG);
    fake.done();

    // AccessGrant has no Serialize, and its Debug hides the token.
    assert!(!(&SerProbe::<AccessGrant>(PhantomData)).serializes());
    assert!(
        (&SerProbe::<HandleUpdate>(PhantomData)).serializes(),
        "the probe works"
    );
    let shown = format!("{g:?} {g:#?}");
    assert!(!shown.contains(&a2), "{shown}");
}

struct SerProbe<T>(PhantomData<T>);
trait SerializesYes {
    fn serializes(&self) -> bool {
        true
    }
}
impl<T: serde::Serialize> SerializesYes for SerProbe<T> {}
trait SerializesNo {
    fn serializes(&self) -> bool {
        false
    }
}
impl<T> SerializesNo for &SerProbe<T> {}

#[test]
fn hsa5_refresh_leaves_0644_config_bytes_comments_and_mode_unchanged() {
    let e = env();
    let ca = "/nonexistent/rdy-test/ca.pem";
    let config =
        format!("{CONFIG}hub:\n  installs:\n    {INSTALL}:\n      ca_file: {ca}   # the test CA\n");
    fs::write(&e.user_file, &config).unwrap();
    fs::set_permissions(&e.user_file, pio::permissions(0o644)).unwrap();
    let unchanged = |what: &str| {
        assert_eq!(fs::read_to_string(&e.user_file).unwrap(), config, "{what}");
        assert_eq!(mode(&e.user_file), 0o644, "{what}");
        assert!(
            !e.user_file.with_file_name("config.yaml.lock").exists(),
            "{what}"
        );
        if e.creds.exists() {
            assert_eq!(mode(&e.creds), 0o600, "{what}");
            assert_eq!(mode(&e.lock), 0o600, "{what}");
        }
    };
    let clock = Clock::at(T0);
    let install = origin(INSTALL);
    let person = human();
    let fake = Fake::default();
    let s = svc(&e, &fake, &clock);
    let temps = Rc::new(RefCell::new(Vec::<u32>::new()));
    let seen_temps = Rc::clone(&temps);
    with_private_write_probe_for_tests(
        move |stage, path| {
            if stage == PrivateWriteStage::Created {
                seen_temps.borrow_mut().push(mode(path));
            }
            Ok(())
        },
        || {
            fake.reply("Metadata", meta(INSTALL))
                .reply("DeviceStart", device_start(INSTALL));
            s.start(
                &install,
                MACHINE,
                Some(&confirm(INSTALL)),
                caller(&person, false),
            )
            .unwrap();
            unchanged("start");
            clock.set(T0 + 5_000);
            fake.reply("Metadata", meta(INSTALL))
                .reply("DevicePoll", polled(&old_access(), &old_refresh()))
                .reply("Me", me(HANDLE));
            s.state(&install, &claim(&person)).unwrap();
            unchanged("sign-in");
            clock.set(T0 + 5_000 + HOUR_MS);
            fake.reply("Metadata", meta(INSTALL))
                .reply("Refresh", refreshed(&new_access(), &new_refresh()));
            s.access(&install, &claim(&person)).unwrap();
            unchanged("refresh");
            fake.reply("Me", me("test-person-new"));
            s.refresh_handle(&install, &claim(&person)).unwrap();
            unchanged("handle");
            fake.reply("Metadata", meta(INSTALL))
                .reply("SignOut", Ok(HubReply::SignOut));
            s.sign_out(&install, &claim(&person)).unwrap();
            unchanged("sign-out");
        },
    );
    fake.done();
    // Every private write made its temp 0600 before writing.
    let temps = temps.borrow();
    assert!(temps.len() >= 6, "{temps:?}");
    assert!(temps.iter().all(|m| *m == 0o600), "{temps:?}");
    // The settings snapshot reached every request: its CA file, read-only.
    let seen = fake.seen();
    assert_eq!(seen.len(), 10);
    assert!(
        seen.iter()
            .all(|r| r.ca_file.as_deref() == Some(Path::new(ca))),
        "{seen:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Handle repair
// ---------------------------------------------------------------------------------------------

#[test]
fn hsa5_handle_change_is_reported_once_without_rewriting_actors() {
    let e = env();
    let clock = Clock::at(T0);
    let install = origin(INSTALL);
    let person = human();
    let bot = agent();
    let (ch, ca) = (claim(&person), claim(&bot));
    let saved = ready(
        &old_access(),
        &old_refresh(),
        T0 + HOUR_MS,
        "test-person-old",
    );
    seed(&e, &[(INSTALL, saved.clone())]);
    let fake = Fake::default();
    let s = svc(&e, &fake, &clock);

    // Access binds a genuine human session to the cached handle; an agent keeps its claim.
    let g = s.access(&install, &ch).unwrap();
    assert_eq!(g.claim.actor, "human:test-person-old");
    assert_eq!(g.human_actor, "human:test-person-old");
    let g = s.access(&install, &ca).unwrap();
    assert_eq!(g.claim.actor, "agent:claude");
    assert!(fake.seen().is_empty());

    // A new handle is saved and reported once.
    fake.reply("Me", me(HANDLE));
    let u = s.refresh_handle(&install, &ch).unwrap();
    assert_eq!(u.human_actor, "human:test-person");
    assert_eq!(u.warnings.len(), 1);
    let w = &u.warnings[0];
    assert_eq!(w.code, W_UPSTREAM_HANDLE_CHANGED);
    assert_eq!(
        w.message,
        "the hub handle changed from test-person-old to test-person"
    );
    assert_eq!(w.fix, "use human:test-person for this install");
    assert!(w.block.is_none());
    let me_call = &fake.seen()[0];
    assert_eq!(me_call.bearer, Some(old_access()));
    assert!(!me_call.claim);
    assert_eq!(
        on_disk(&e.creds, INSTALL),
        Some(ready(&old_access(), &old_refresh(), T0 + HOUR_MS, HANDLE))
    );
    // The same handle later: no warning.
    fake.reply("Me", me(HANDLE));
    let u = s.refresh_handle(&install, &ca).unwrap();
    assert_eq!(u.human_actor, "human:test-person");
    assert!(u.warnings.is_empty());
    assert_eq!(fake.ops(), ["Me", "Me"]);
    fake.done();
    // Claims now bind the new handle; the agent's stays as it was.
    assert_eq!(
        s.access(&install, &ch).unwrap().claim.actor,
        "human:test-person"
    );
    let g = s.access(&install, &ca).unwrap();
    assert_eq!(
        (g.claim.actor.as_str(), g.claim.run.as_deref()),
        ("agent:claude", Some("r7"))
    );

    // A supplied wrong human claim is refused locally.
    let mut forged = claim(&bot);
    forged.actor = "human:test-person".to_string();
    fake.clear();
    assert_code(s.access(&install, &forged), E_ACTOR_UNBOUND);
    assert_code(s.refresh_handle(&install, &forged), E_ACTOR_UNBOUND);
    assert!(fake.seen().is_empty());

    // The record changes while /me is out: the result is dropped and nothing is overwritten.
    let (a2, r2) = (token('a', "RACEACCESS"), token('r', "RACEREFRESH"));
    let (uf, project, creds, lock) = (
        e.user_file.clone(),
        e.project.clone(),
        e.creds.clone(),
        e.lock.clone(),
    );
    let (ra, rr) = (a2.clone(), r2.clone());
    fake.on("Me", move |_| {
        assert!(lock_is_free(&lock), "the repair holds no lock across /me");
        // Another caller refreshes meanwhile.
        let other = Fake::default();
        other
            .reply("Metadata", meta(INSTALL))
            .reply("Refresh", refreshed(&ra, &rr));
        let later = Clock::at(T0 + HOUR_MS);
        HubCredentials::new(Some(&project), &uf, &other, &later)
            .access(&origin(INSTALL), &claim(&human()))
            .unwrap();
        other.done();
        assert_eq!(state_on_disk(&creds, INSTALL), Some("ready"));
        me("test-person-race")
    });
    assert_code(s.refresh_handle(&install, &ch), E_UPSTREAM_AUTH);
    assert_eq!(
        on_disk(&e.creds, INSTALL),
        Some(ready(&a2, &r2, T0 + 2 * HOUR_MS, HANDLE)),
        "the newer record is kept, with its handle"
    );
    fake.done();

    // Another account: the record is cleared to reauth_required.
    seed(&e, &[(INSTALL, saved.clone())]);
    fake.reply("Me", me_as("acc_00000000000000000000000002", HANDLE));
    assert_code(s.refresh_handle(&install, &ch), E_UPSTREAM_AUTH);
    assert_eq!(state_on_disk(&e.creds, INSTALL), Some("reauth_required"));

    // A /me failure is returned as it is, once, and changes nothing.
    for (r, code) in [
        (Err(unsent()), E_UPSTREAM_TRANSPORT),
        (Err(HubFailure::AuthRequired), E_UPSTREAM_AUTH),
        (Err(HubFailure::Protocol), E_UPSTREAM_PROTOCOL),
    ] {
        seed(&e, &[(INSTALL, saved.clone())]);
        let before = fs::read(&e.creds).unwrap();
        fake.clear();
        fake.reply("Me", r);
        assert_code(s.refresh_handle(&install, &ch), code);
        assert_eq!(fake.ops(), ["Me"]);
        assert_eq!(fs::read(&e.creds).unwrap(), before);
    }
    fake.done();
    // Nothing that is not ready is repaired.
    seed(&e, &[(INSTALL, Credential::ReauthRequired)]);
    fake.clear();
    assert_code(s.refresh_handle(&install, &ch), E_UPSTREAM_AUTH);
    assert!(fake.seen().is_empty());

    // The actor config and the sessions are untouched.
    assert_eq!(fs::read_to_string(&e.user_file).unwrap(), CONFIG);
    assert_eq!(person, human());
    assert_eq!(bot, agent());
}

#[test]
fn id5_invalid_handle_cannot_form_a_human_claim() {
    let e = env();
    let clock = Clock::at(T0);
    let install = origin(INSTALL);
    let (person, bot) = (human(), agent());
    let (ch, ca) = (claim(&person), claim(&bot));
    let long = "a".repeat(65);
    let invalid: Vec<&str> = vec![
        "Test-Person",
        "NICK",
        "two words",
        "a b",
        "a/b",
        "/",
        "é",
        "Клод",
        "-a",
        "a-",
        ".a",
        "a_",
        "a!",
        "a:b",
        "a\u{a0}",
        "a\nb",
        "",
        &long,
    ];
    let longest = "a".repeat(64);
    let valid: Vec<&str> = vec!["a", "7", "a.b_c-d", &longest];

    // Sign-in: an invalid `/me` handle is the hub's malformed reply (`E_UPSTREAM_PROTOCOL`), and
    // it never becomes a ready record.
    for handle in &invalid {
        seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
        let fake = Fake::default();
        fake.reply("Metadata", meta(INSTALL))
            .reply("DevicePoll", polled(&new_access(), &new_refresh()))
            .reply("Me", me(handle));
        assert_code(
            svc(&e, &fake, &clock).state(&install, &ca),
            E_UPSTREAM_PROTOCOL,
        );
        assert_eq!(
            state_on_disk(&e.creds, INSTALL),
            Some("reauth_required"),
            "{handle:?}"
        );
        assert!(!disk_text(&e.creds).contains(&new_access()), "{handle:?}");
        fake.done();
    }
    // A transport failure of the same `/me` step keeps `E_UPSTREAM_AUTH`.
    seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
    let fake = Fake::default();
    fake.reply("Metadata", meta(INSTALL))
        .reply("DevicePoll", polled(&new_access(), &new_refresh()))
        .reply("Me", Err(unsent()));
    assert_code(svc(&e, &fake, &clock).state(&install, &ca), E_UPSTREAM_AUTH);
    assert_eq!(state_on_disk(&e.creds, INSTALL), Some("reauth_required"));
    assert!(!disk_text(&e.creds).contains(&new_access()));
    fake.done();
    for handle in &valid {
        seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
        let fake = Fake::default();
        fake.reply("Metadata", meta(INSTALL))
            .reply("DevicePoll", polled(&new_access(), &new_refresh()))
            .reply("Me", me(handle));
        let st = svc(&e, &fake, &clock).state(&install, &ca).unwrap();
        assert_eq!(st.state, AuthPhase::SignedIn);
        assert_eq!(st.human_actor, Some(format!("human:{handle}")));
        assert_eq!(
            on_disk(&e.creds, INSTALL),
            Some(ready(&new_access(), &new_refresh(), T0 + HOUR_MS, handle))
        );
        fake.done();
    }

    // The `/me` repair: an invalid handle is a contract mismatch and nothing is saved.
    let saved = ready(&old_access(), &old_refresh(), T0 + HOUR_MS, HANDLE);
    for who in [&ch, &ca] {
        for handle in &invalid {
            seed(&e, &[(INSTALL, saved.clone())]);
            let before = fs::read(&e.creds).unwrap();
            let fake = Fake::default();
            fake.reply("Me", me(handle));
            let r = svc(&e, &fake, &clock).refresh_handle(&install, who);
            assert_code(r, E_UPSTREAM_PROTOCOL);
            assert_eq!(fs::read(&e.creds).unwrap(), before, "{handle:?}");
            fake.done();
        }
        for handle in &valid {
            seed(&e, &[(INSTALL, saved.clone())]);
            let fake = Fake::default();
            fake.reply("Me", me(handle));
            let u = svc(&e, &fake, &clock)
                .refresh_handle(&install, who)
                .unwrap();
            assert_eq!(u.human_actor, format!("human:{handle}"));
            assert_eq!(
                on_disk(&e.creds, INSTALL),
                Some(ready(&old_access(), &old_refresh(), T0 + HOUR_MS, handle))
            );
            fake.done();
        }
    }

    // A cached handle forms a human claim only when it is a name; no request is made. A record
    // holds only a nonempty handle without control characters, so those cases are skipped here.
    let storable = |h: &&&str| !h.is_empty() && !h.chars().any(char::is_control);
    for handle in invalid.iter().filter(storable) {
        seed(
            &e,
            &[(
                INSTALL,
                ready(&old_access(), &old_refresh(), T0 + HOUR_MS, handle),
            )],
        );
        let fake = Fake::default();
        let s = svc(&e, &fake, &clock);
        assert_code(s.access(&install, &ch), E_UPSTREAM_PROTOCOL);
        assert_code(s.refresh_handle(&install, &ch), E_UPSTREAM_PROTOCOL);
        assert!(fake.seen().is_empty(), "{handle:?}");
        // An agent claim keeps its bytes and never takes the handle.
        let g = s.access(&install, &ca).unwrap();
        assert_eq!(g.claim.actor, "agent:claude");
        assert_eq!(g.claim.run.as_deref(), Some("r7"));
    }
    for handle in &valid {
        seed(
            &e,
            &[(
                INSTALL,
                ready(&old_access(), &old_refresh(), T0 + HOUR_MS, handle),
            )],
        );
        let fake = Fake::default();
        let g = svc(&e, &fake, &clock).access(&install, &ch).unwrap();
        assert_eq!(g.claim.actor, format!("human:{handle}"));
        assert!(fake.seen().is_empty());
    }
    assert_eq!(person, human());
    assert_eq!(bot, agent());
}

// ---------------------------------------------------------------------------------------------
// Sign-out
// ---------------------------------------------------------------------------------------------

#[test]
fn hsa5_signout_releases_before_revoke_and_cannot_delete_a_new_signin() {
    let e = env();
    let clock = Clock::at(T0);
    let install = origin(INSTALL);
    let person = human();
    let c = claim(&person);
    let keep = pending_on(OTHER, MACHINE, T0 + 600_000, T0 + 5_000, 5);
    seed(
        &e,
        &[
            (
                INSTALL,
                ready(&old_access(), &old_refresh(), T0 + HOUR_MS, HANDLE),
            ),
            (OTHER, keep.clone()),
        ],
    );

    // The record is removed, the lock released, then the old token is revoked; a sign-in that
    // lands meanwhile survives the old reply.
    let (ev_tx, ev_rx) = mpsc::channel::<Step<AuthState>>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let fake = Fake::default();
    let (creds, lock) = (e.creds.clone(), e.lock.clone());
    let in_revoke = ev_tx.clone();
    fake.reply("Metadata", meta(INSTALL))
        .on("SignOut", move |call| {
            assert!(call.bearer.is_none() && call.claim.is_none());
            assert!(lock_is_free(&lock), "the revoke holds no lock");
            assert_eq!(on_disk(&creds, INSTALL), None, "removed before the revoke");
            in_revoke.send(Step::Inside).unwrap();
            go_rx.lock().unwrap().recv().unwrap();
            Ok(HubReply::SignOut)
        });
    let newer = Fake::default();
    newer
        .reply("Metadata", meta(INSTALL))
        .reply("DeviceStart", device_start(INSTALL));
    std::thread::scope(|scope| {
        let go_tx = go_tx;
        scope.spawn(|| report(&ev_tx, || svc(&e, &fake, &clock).sign_out(&install, &c)));
        inside(&ev_rx);
        let st = svc(&e, &newer, &clock)
            .start(
                &install,
                MACHINE,
                Some(&confirm(INSTALL)),
                caller(&person, false),
            )
            .unwrap();
        assert_eq!(st.state, AuthPhase::Pending);
        go_tx.send(()).unwrap();
        assert_eq!(done(&ev_rx).unwrap(), signed_out(&install));
    });
    assert_eq!(fake.ops(), ["Metadata", "SignOut"]);
    assert_eq!(fake.seen()[1].sent, Some(old_refresh()));
    assert_eq!(fake.seen()[1].destination, "https://hub.test/auth/revoke");
    assert_eq!(
        newer.ops(),
        ["Metadata", "DeviceStart"],
        "no revoke: nothing was saved"
    );
    assert_eq!(state_on_disk(&e.creds, INSTALL), Some("pending"));
    assert_eq!(on_disk(&e.creds, OTHER), Some(keep.clone()));
    fake.done();
    newer.done();

    // A failed revoke or discovery: signed out locally, with the warning; nothing comes back.
    for (m, r) in [
        (meta(INSTALL), Some(Err(lost()))),
        (meta(INSTALL), Some(Err(HubFailure::Refused))),
        (
            meta(INSTALL),
            Some(Ok(HubReply::Me(MeReply {
                account: ACCOUNT.to_string(),
                handle: HANDLE.to_string(),
            }))),
        ),
        (Err(unsent()), None),
    ] {
        seed(
            &e,
            &[
                (
                    INSTALL,
                    ready(&old_access(), &old_refresh(), T0 + HOUR_MS, HANDLE),
                ),
                (OTHER, keep.clone()),
            ],
        );
        let fake = Fake::default();
        fake.reply("Metadata", m);
        if let Some(r) = r {
            fake.reply("SignOut", r);
        }
        let st = svc(&e, &fake, &clock).sign_out(&install, &c).unwrap();
        assert_eq!(st.state, AuthPhase::SignedOut);
        assert_eq!(st.warnings.len(), 1);
        let w = &st.warnings[0];
        assert_eq!(w.code, W_UPSTREAM_SIGNOUT_LOCAL);
        assert_eq!(
            w.message,
            "the local credential was removed; hub sign-out was not confirmed"
        );
        assert_eq!(w.fix, "end the credential on the hub if it is still active");
        assert_eq!(on_disk(&e.creds, INSTALL), None);
        assert_eq!(on_disk(&e.creds, OTHER), Some(keep.clone()));
        assert!(!disk_text(&e.creds).contains(&old_refresh()));
        fake.done();
    }

    // Pending and reauth_required go with no request or warning; nothing is a no-op.
    for rec in [pending(T0 + 600_000, T0, 5), Credential::ReauthRequired] {
        seed(&e, &[(INSTALL, rec), (OTHER, keep.clone())]);
        let fake = Fake::default();
        assert_eq!(
            svc(&e, &fake, &clock).sign_out(&install, &c).unwrap(),
            signed_out(&install)
        );
        assert!(fake.seen().is_empty());
        assert_eq!(on_disk(&e.creds, INSTALL), None);
        assert_eq!(on_disk(&e.creds, OTHER), Some(keep.clone()));
    }
    let before = fs::read(&e.creds).unwrap();
    let fake = Fake::default();
    assert_eq!(
        svc(&e, &fake, &clock).sign_out(&install, &c).unwrap(),
        signed_out(&install)
    );
    assert_eq!(fs::read(&e.creds).unwrap(), before);
    assert!(fake.seen().is_empty());

    // A failed local removal is its file error, and nothing is sent.
    seed(
        &e,
        &[(
            INSTALL,
            ready(&old_access(), &old_refresh(), T0 + HOUR_MS, HANDLE),
        )],
    );
    let before = fs::read(&e.creds).unwrap();
    let fake = Fake::default();
    let s = svc(&e, &fake, &clock);
    assert_code(
        failing_write(1, PrivateWriteStage::Created, || s.sign_out(&install, &c)),
        E_IO,
    );
    assert!(fake.seen().is_empty());
    assert_eq!(fs::read(&e.creds).unwrap(), before);
}

// ---------------------------------------------------------------------------------------------
// Locks during discovery and repair
// ---------------------------------------------------------------------------------------------

#[test]
fn hsa5_metadata_and_handle_reads_hold_no_credential_lock() {
    let e = env();
    let clock = Clock::at(T0);
    let install = origin(INSTALL);
    let person = human();
    let c = claim(&person);
    let (uf, lock) = (e.user_file.clone(), e.lock.clone());
    // A concurrent local read: it would wait forever if the lock were held, so the test first
    // proves the lock is free, then reads.
    let free_read = move |want: AuthPhase| {
        assert!(lock_is_free(&lock), "no credential lock is held here");
        let none = Fake::default();
        let clock = Clock::at(T0);
        let rows = HubCredentials::new(None, &uf, &none, &clock)
            .installs()
            .unwrap();
        assert_eq!(rows[0].state, want);
    };
    let held = |lock: PathBuf| move || assert!(!lock_is_free(&lock));

    // Refresh: discovery free, refresh held.
    seed(
        &e,
        &[(
            INSTALL,
            ready(&old_access(), &old_refresh(), T0 + 30_000, HANDLE),
        )],
    );
    let fake = Fake::default();
    let (f1, h1) = (free_read.clone(), held(e.lock.clone()));
    fake.on("Metadata", move |_| {
        f1(AuthPhase::SignedIn);
        meta(INSTALL)
    })
    .on("Refresh", move |_| {
        h1();
        refreshed(&new_access(), &new_refresh())
    });
    let s = svc(&e, &fake, &clock);
    s.access(&install, &c).unwrap();

    // Handle repair: /me free.
    let f2 = free_read.clone();
    fake.on("Me", move |_| {
        f2(AuthPhase::SignedIn);
        me(HANDLE)
    });
    s.refresh_handle(&install, &c).unwrap();

    // Re-sign-in: discovery free; revoke and start held.
    let (f3, h2, h3) = (
        free_read.clone(),
        held(e.lock.clone()),
        held(e.lock.clone()),
    );
    fake.on("Metadata", move |_| {
        f3(AuthPhase::SignedIn);
        meta(INSTALL)
    })
    .on("SignOut", move |_| {
        h2();
        Ok(HubReply::SignOut)
    })
    .on("DeviceStart", move |_| {
        h3();
        device_start(INSTALL)
    });
    s.start(
        &install,
        MACHINE,
        Some(&confirm(INSTALL)),
        caller(&person, false),
    )
    .unwrap();

    // Poll: discovery free; poll and /me held.
    clock.set(T0 + 5_000);
    let (f4, h4, h5) = (
        free_read.clone(),
        held(e.lock.clone()),
        held(e.lock.clone()),
    );
    fake.on("Metadata", move |_| {
        f4(AuthPhase::Pending);
        meta(INSTALL)
    })
    .on("DevicePoll", move |_| {
        h4();
        polled(&old_access(), &old_refresh())
    })
    .on("Me", move |_| {
        h5();
        me(HANDLE)
    });
    assert_eq!(s.state(&install, &c).unwrap().state, AuthPhase::SignedIn);

    // Sign-out: discovery and revoke free.
    let (lock, creds) = (e.lock.clone(), e.creds.clone());
    let lock2 = lock.clone();
    fake.on("Metadata", move |_| {
        assert!(lock_is_free(&lock));
        assert_eq!(on_disk(&creds, INSTALL), None);
        meta(INSTALL)
    })
    .on("SignOut", move |_| {
        assert!(lock_is_free(&lock2));
        Ok(HubReply::SignOut)
    });
    s.sign_out(&install, &c).unwrap();
    fake.done();
    assert_eq!(
        fake.ops(),
        [
            "Metadata",
            "Refresh",
            "Me",
            "Metadata",
            "SignOut",
            "DeviceStart",
            "Metadata",
            "DevicePoll",
            "Me",
            "Metadata",
            "SignOut"
        ]
    );
}

// ---------------------------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------------------------

#[test]
fn hsa5_all_public_states_and_aggregates_hide_secrets() {
    let e = env();
    let clock = Clock::at(T0);
    let install = origin(INSTALL);
    let person = human();
    let c = claim(&person);
    let fake = Fake::default();
    let s = svc(&e, &fake, &clock);
    let mut texts: Vec<(String, String)> = Vec::new();
    let mut push = |what: &str, json: String, debug: String| {
        texts.push((format!("{what} json"), json));
        texts.push((format!("{what} debug"), debug));
    };
    let state = |st: &AuthState| {
        (
            serde_json::to_string(st).unwrap(),
            format!("{st:?} {st:#?}"),
        )
    };
    let error = |e: &RdyError| (serde_json::to_string(e).unwrap(), format!("{e:?} {e}"));

    let card = s
        .start(&install, MACHINE, None, caller(&person, false))
        .unwrap();
    let (j, d) = state(&card);
    push("card", j, d);
    fake.reply("Metadata", meta(INSTALL))
        .reply("DeviceStart", device_start(INSTALL));
    let waiting = s
        .start(
            &install,
            MACHINE,
            Some(&confirm(INSTALL)),
            caller(&person, false),
        )
        .unwrap();
    let (j, d) = state(&waiting);
    push("pending", j, d);
    let v = serde_json::to_value(&waiting).unwrap();
    assert_eq!(v["verification_uri"], "https://hub.test/app/device");
    assert!(!v["verification_uri"].as_str().unwrap().contains(USER_CODE));
    let rows = s.installs().unwrap();
    push(
        "pending rows",
        serde_json::to_string(&rows).unwrap(),
        format!("{rows:?}"),
    );

    clock.set(T0 + 5_000);
    fake.reply("Metadata", meta(INSTALL))
        .reply("DevicePoll", polled(&old_access(), &old_refresh()))
        .reply("Me", me("test-person-old"));
    let signed = s.state(&install, &c).unwrap();
    let (j, d) = state(&signed);
    push("signed in", j, d);
    let rows = s.installs().unwrap();
    push(
        "rows",
        serde_json::to_string(&rows).unwrap(),
        format!("{rows:?}"),
    );

    let g = s.access(&install, &c).unwrap();
    assert_eq!(
        g.access_token.expose(),
        old_access(),
        "only expose() gives the token"
    );
    // AccessGrant has no JSON form: only its Debug is checked.
    push("grant", String::new(), format!("{g:?} {g:#?}"));
    fake.reply("Me", me(HANDLE));
    let u = s.refresh_handle(&install, &c).unwrap();
    push(
        "handle",
        serde_json::to_string(&u).unwrap(),
        format!("{u:?}"),
    );

    clock.set(T0 + 5_000 + HOUR_MS);
    fake.reply("Metadata", meta(INSTALL))
        .reply("Refresh", Err(lost()));
    let err = s.access(&install, &c).unwrap_err();
    let (j, d) = error(&err);
    push("refresh error", j, d);
    seed(
        &e,
        &[(INSTALL, ready(&new_access(), &new_refresh(), T0, HANDLE))],
    );
    fake.reply("Metadata", meta(INSTALL))
        .reply("SignOut", Err(HubFailure::Refused));
    let out = s.sign_out(&install, &c).unwrap();
    let (j, d) = state(&out);
    push("sign-out warning", j, d);
    seed(&e, &[(INSTALL, pending(T0 + 600_000, T0, 5))]);
    clock.set(T0);
    fake.reply("Metadata", meta(INSTALL))
        .reply("DevicePoll", Err(lost()));
    let err = s.state(&install, &c).unwrap_err();
    let (j, d) = error(&err);
    push("poll error", j, d);
    fake.done();

    let secrets = [
        old_access(),
        old_refresh(),
        new_access(),
        new_refresh(),
        DEVICE_CODE.to_string(),
    ];
    for (what, text) in &texts {
        for secret in &secrets {
            assert!(
                !text.contains(secret.as_str()),
                "{what} shows a secret: {text}"
            );
        }
        for name in [
            "device_code",
            "access_token",
            "refresh_token",
            "verification_uri_complete",
        ] {
            if what.ends_with("json") {
                assert!(!text.contains(name), "{what} has {name}: {text}");
            }
        }
    }
    // No secret reaches the settings file.
    assert_eq!(fs::read_to_string(&e.user_file).unwrap(), CONFIG);
}
