//! `rdy auth` (D-KQ4CA, n-XZDQD, n-N055T): the command-line fallback of `rdy_auth`, plus the
//! machine-token door for a credential consumer. It runs before any project lookup, store or
//! daemon: it reads the user config only, and the engine's credential service does the rest
//! through the same request checks and replies as `rdy_auth`.
//!
//! - **Person or relay.** With no `RDY_ACTOR` the person signs in as themselves. An agent answers
//!   the card only by relaying the person's words (`--relayed-by` with `--evidence`); the
//!   service's relay check decides, and nothing here skips it.
//! - **The token door.** `rdy --json auth token` gives a consumer one line with current access,
//!   only to a pipe: never to a terminal, never as text, and never the refresh token.
//! - **The user config's warnings.** With no project, the shared config-warning print does not
//!   run, so this command reads the user config's warnings itself. A state reply carries them in
//!   its warnings, as `rdy_auth` does; they follow the installs list and the token door's line on
//!   stderr; a failure after they are read carries them as its issues.
//! - **Sign-out is local first.** Settings that do not load do not stop the removal of the saved
//!   sign-in: only the hub sign-out after it needs them.
//! - **One command on a terminal (D-VRH3Z).** When stdin and stdout are both terminals and the
//!   call has no `--json`, `--card-hash`, `--answer` or `--relayed-by`, `auth sign-in` asks the
//!   card's question in place, starts the device sign-in on a confirm, offers to open the link
//!   and waits with the service's own check until the sign-in ends. Only `IsTerminal` on the two
//!   streams decides: no setting, variable or flag can select this path. The person's answer
//!   goes through the same service call and relay check as `--answer`. A stop between checks
//!   leaves the sign-in pending, and `auth state` finishes it.
use crate::config_warning_line;
use crate::output::print_error;
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::config_levels::user_config_warnings;
use rdy_engine::hub_config::HubConfig;
use rdy_engine::hub_credentials::{
    AccessGrant, AuthAnswer, AuthConfirmation, AuthPhase, AuthState,
};
use rdy_engine::hub_types::{Origin, OriginFence, RequestClaim};
use rdy_engine::{E_BAD_INPUT, RdyError, Session, classify};
use rdy_mcp::tools::{
    AuthAnswerParam, AuthOp, AuthParams, AuthReply, AuthRequest, AuthSurface, auth_person,
    auth_request, auth_user_file, run_auth, sign_out, with_credentials, with_warnings,
};
use rdy_model::Role;
use rdy_schema::Issue;
use serde_json::{Value, json};
use std::io::{BufRead, IsTerminal, Read, Write};
use std::process::{Command, ExitCode, Stdio};

/// The person's answer to the sign-in card.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum AnswerArg {
    Confirm,
    Cancel,
}

#[derive(clap::Args)]
pub(crate) struct SignInArgs {
    /// The install's HTTPS origin.
    #[arg(long)]
    install: String,
    /// This machine's name on the hub: 1 to 64 characters; the host name by default.
    #[arg(long)]
    machine_name: Option<String>,
    /// The card hash the first call printed, to answer that card.
    #[arg(long)]
    card_hash: Option<String>,
    /// The person's answer to the card: confirm or cancel. Needs --card-hash.
    #[arg(long, value_enum)]
    answer: Option<AnswerArg>,
    /// The agent that relays the person's words: your own actor. The run comes from your session.
    #[arg(long, value_name = "ACTOR")]
    relayed_by: Option<String>,
    /// The person's words, quoted.
    #[arg(long)]
    evidence: Option<String>,
    /// The question the person was asked.
    #[arg(long)]
    question: Option<String>,
    /// Where the person said it (default: cli).
    #[arg(long)]
    session_ref: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct InstallArg {
    /// The install's HTTPS origin.
    #[arg(long)]
    install: String,
}

#[derive(Subcommand)]
pub(crate) enum AuthCmd {
    /// Start a hub sign-in. On a terminal it asks you to confirm, offers to open the link and waits until the sign-in ends. Otherwise, or with --json, --card-hash, --answer or --relayed-by, the first call prints the confirmation card; answer it with --card-hash and --answer. Then open the printed link and enter the code.
    SignIn(SignInArgs),
    /// Show the sign-in state of an install; a pending sign-in is checked once.
    State(InstallArg),
    /// Remove this machine's sign-in for an install and end it on the hub.
    SignOut(InstallArg),
    /// List the saved sign-ins.
    Installs,
    /// For a credential consumer: print current access for an install as one JSON line. Needs --json and piped output.
    Token(InstallArg),
}

/// Where standard output goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputKind {
    Terminal,
    Other,
}

impl OutputKind {
    /// The kind of this process's standard output.
    pub(crate) fn of_stdout() -> OutputKind {
        if std::io::stdout().is_terminal() {
            OutputKind::Terminal
        } else {
            OutputKind::Other
        }
    }
}

/// The token door: `E_BAD_INPUT` unless the reply is JSON and goes to a pipe, not a terminal.
pub(crate) fn token_door(as_json: bool, out: OutputKind) -> Result<(), RdyError> {
    if as_json && out == OutputKind::Other {
        return Ok(());
    }
    Err(RdyError::new(
        E_BAD_INPUT,
        "auth token needs --json and a pipe for its output",
        "run this command from the credential consumer with piped output",
    ))
}

/// The machine-token reply: the only encoder of access. One JSON object with the install, the
/// access token, its expiry and the cached human actor; never the refresh token.
fn token_line(g: &AccessGrant) -> String {
    json!({
        "install": g.install.as_str(),
        "access_token": g.access_token.expose(),
        "expires_at_ms": g.expires_at_ms,
        "human_actor": g.human_actor,
    })
    .to_string()
}

/// The standalone caller: the bounded configured person (for a sign-out whose settings do not
/// load, the user config's actor read on its own), classified with
/// `RDY_ACTOR` and `RDY_RUN` as a writer (an identity that is not Unicode is refused).
fn caller(
    settings: Option<&HubConfig>,
    user_file: &std::path::Path,
) -> Result<(Session, String), RdyError> {
    let person = auth_person(settings, user_file)?;
    let (actor, run) = crate::conn::session_env()?;
    let session = classify(&person, actor.as_deref(), run.as_deref(), Role::Writer)?;
    Ok((session, person))
}

/// The checked request of a person command.
fn request(cmd: AuthCmd) -> Result<AuthRequest, RdyError> {
    let bare = |op, install| AuthParams {
        op,
        install,
        machine_name: None,
        card_hash: None,
        answer: None,
        evidence: None,
        question: None,
        session_ref: None,
    };
    match cmd {
        AuthCmd::SignIn(a) => auth_request(
            AuthParams {
                machine_name: a.machine_name,
                card_hash: a.card_hash,
                answer: a.answer.map(|x| match x {
                    AnswerArg::Confirm => AuthAnswerParam::Confirm,
                    AnswerArg::Cancel => AuthAnswerParam::Cancel,
                }),
                evidence: a.evidence,
                question: a.question,
                session_ref: a.session_ref,
                ..bare(AuthOp::Start, Some(a.install))
            },
            a.relayed_by,
        ),
        AuthCmd::State(a) => auth_request(bare(AuthOp::State, Some(a.install)), None),
        AuthCmd::SignOut(a) => auth_request(bare(AuthOp::SignOut, Some(a.install)), None),
        AuthCmd::Installs => auth_request(bare(AuthOp::Installs, None), None),
        AuthCmd::Token(_) => unreachable!("the token door has its own path"),
    }
}

/// The text of one state.
fn state_text(s: &AuthState) -> String {
    let install = &s.install;
    let text = |v: &Option<String>| v.clone().unwrap_or_default();
    match s.state {
        AuthPhase::ConfirmationRequired => format!(
            "{}card hash: {}\nanswer with --answer and --card-hash",
            text(&s.card),
            text(&s.card_hash)
        ),
        AuthPhase::Cancelled => format!("sign-in cancelled for {install}"),
        AuthPhase::Pending => format!(
            "sign-in pending for {install}\ncode: {}\nopen: {}\ncheck again with rdy auth state --install {install}",
            text(&s.user_code),
            text(&s.verification_uri)
        ),
        AuthPhase::SignedIn => format!("signed in to {install} as {}", text(&s.human_actor)),
        AuthPhase::SignedOut => format!("signed out of {install}"),
        AuthPhase::ReauthRequired => format!("sign-in is required for {install}"),
    }
}

/// The text of a reply: its lines, then each warning in the shared warning form.
fn reply_text(r: &AuthReply) -> Result<String, RdyError> {
    let (mut out, warnings): (String, &[Issue]) = match r {
        AuthReply::State(s) => (state_text(s), &s.warnings),
        AuthReply::Installs(rows) if rows.is_empty() => ("no saved hub sign-ins".into(), &[]),
        AuthReply::Installs(rows) => {
            let lines: Vec<String> = rows
                .iter()
                .map(|row| {
                    let state = serde_json::to_value(row.state)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default();
                    let actor = row.human_actor.as_deref().unwrap_or("-");
                    format!("{} {state} {actor}", row.install)
                })
                .collect();
            (lines.join("\n"), &[])
        }
    };
    for w in warnings {
        out.push('\n');
        out.push_str(&config_warning_line(w));
    }
    Ok(out)
}

/// One person command: its JSON (the `rdy_auth` reply, with the config's `warnings`) and its
/// text. `settings` is `None` only for sign-out, whose removal needs none. A card request of
/// `auth sign-in` that [`in_place_wanted`] allows on `console` is answered in place
/// ([`sign_in_in_place`]), with the system's browser opener.
fn person(
    cmd: AuthCmd,
    settings: Option<&HubConfig>,
    user_file: &std::path::Path,
    warnings: &[Issue],
    as_json: bool,
    console: &mut dyn Console,
) -> CmdResult {
    let in_place = matches!(&cmd, AuthCmd::SignIn(a) if in_place_wanted(a, as_json, console));
    let req = request(cmd)?;
    let (session, person) = caller(settings, user_file)?;
    let reply = match (req, settings) {
        (AuthRequest::SignOut { install }, _) => {
            AuthReply::state(sign_out(&session, None, user_file, &install)?)
        }
        (req, Some(settings)) => {
            let surface = AuthSurface {
                session: &session,
                person: &person,
                implicit: false,
                project: None,
                user_file,
                settings,
            };
            match req {
                AuthRequest::Start {
                    install,
                    machine_name,
                    confirmation: None,
                } if in_place => {
                    let mut steps = ServiceSteps {
                        surface: &surface,
                        install: &install,
                        machine_name: &machine_name,
                    };
                    let mut opener = SystemOpener;
                    sign_in_in_place(&install, &machine_name, console, &mut opener, &mut steps)?
                }
                req => run_auth(&surface, req)?,
            }
        }
        (_, None) => {
            return Err(RdyError::new(
                rdy_engine::E_INTERNAL,
                "a sign-in command ran without its settings",
                "report this as a bug in rdy",
            ));
        }
    };
    let reply = reply.with_warnings(warnings);
    Ok((
        reply.to_json()?,
        reply_text(&reply)?,
        reply.carries_warnings(),
    ))
}

/// The machine-token door: current access for `install`, renewed by this process under the
/// credential lock when it is about to expire; a due pending sign-in is checked once first
/// (D-VRH3Z).
fn token(
    install: &str,
    settings: &HubConfig,
    user_file: &std::path::Path,
) -> Result<String, RdyError> {
    let install = Origin::parse(install)?;
    let (session, person) = caller(Some(settings), user_file)?;
    let claim = RequestClaim::from_session(&session)?;
    let surface = AuthSurface {
        session: &session,
        person: &person,
        implicit: false,
        project: None,
        user_file,
        settings,
    };
    let grant = with_credentials(&surface, Some(&install), |c| {
        c.access_on_use(&install, &claim)
    })?;
    Ok(token_line(&grant))
}

/// A person command's JSON, its text, and whether the reply carries the config's warnings.
type CmdResult = Result<(Value, String, bool), RdyError>;

fn fail(e: RdyError, as_json: bool) -> ExitCode {
    print_error(&ErrorReply::from(e), as_json);
    ExitCode::FAILURE
}

/// Each warning on stderr, in the shared form.
fn print_warnings(warnings: &[Issue]) {
    for w in warnings {
        eprintln!("{}", config_warning_line(w));
    }
}

/// `rdy auth <cmd>`: no project, store or daemon. The user config's warnings are read once,
/// right after its location: a state reply carries them in its warnings (its JSON, or after its
/// text); the installs list and the token door's one line are followed by them on stderr; a
/// failure after the read carries them as its issues. Settings that do not load refuse every
/// command but sign-out, which removes the saved sign-in without them.
pub(crate) fn run(cmd: AuthCmd, as_json: bool) -> ExitCode {
    if let AuthCmd::Token(_) = &cmd
        && let Err(e) = token_door(as_json, OutputKind::of_stdout())
    {
        return fail(e, as_json);
    }
    let user_file = match auth_user_file() {
        Ok(f) => f,
        Err(e) => return fail(e, as_json),
    };
    let warnings = user_config_warnings(&user_file);
    let refuse = |e: RdyError| fail(with_warnings(e, &warnings), as_json);
    let settings = match HubConfig::load(None, Some(&user_file)) {
        Ok(s) => Some(s),
        Err(_) if matches!(cmd, AuthCmd::SignOut(_)) => None,
        Err(e) => return refuse(e),
    };
    if let (AuthCmd::Token(a), Some(settings)) = (&cmd, &settings) {
        return match token(&a.install, settings, &user_file) {
            Ok(line) => {
                let mut out = std::io::stdout().lock();
                if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                    return ExitCode::FAILURE;
                }
                print_warnings(&warnings);
                ExitCode::SUCCESS
            }
            Err(e) => refuse(e),
        };
    }
    let mut console = StdConsole;
    let result = person(
        cmd,
        settings.as_ref(),
        &user_file,
        &warnings,
        as_json,
        &mut console,
    );
    emit(result, as_json, &warnings)
}

/// Prints a person command's result: its JSON or text, then the config's warnings when the
/// reply does not carry them; a failure carries them as its issues.
fn emit(result: CmdResult, as_json: bool, warnings: &[Issue]) -> ExitCode {
    match result {
        Ok((v, text, carried)) => {
            if as_json {
                println!("{v}");
            } else {
                println!("{text}");
            }
            if !carried {
                print_warnings(warnings);
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(with_warnings(e, warnings), as_json),
    }
}

// ---------------------------------------------------------------------------------------------
// One command on a terminal (D-VRH3Z)
// ---------------------------------------------------------------------------------------------

/// The person's terminal as the in-place sign-in sees it: production reads the real standard
/// streams ([`StdConsole`]), and tests give their own.
pub(crate) trait Console {
    /// Whether standard input is a terminal.
    fn stdin_is_terminal(&self) -> bool;
    /// Whether standard output is a terminal.
    fn stdout_is_terminal(&self) -> bool;
    /// One line without its end, or `None` at the end of input or on a read error.
    fn read_line(&mut self) -> Option<String>;
    /// Writes `text` to standard output now.
    fn write(&mut self, text: &str);
}

/// The most bytes of one answer read from the terminal; a longer line is cut there, which no
/// answer word is.
const MAX_ANSWER_BYTES: u64 = 4096;

/// This process's standard streams; only `IsTerminal` tells a terminal.
pub(crate) struct StdConsole;

impl Console for StdConsole {
    fn stdin_is_terminal(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    fn stdout_is_terminal(&self) -> bool {
        std::io::stdout().is_terminal()
    }

    fn read_line(&mut self) -> Option<String> {
        let mut line = String::new();
        match std::io::stdin()
            .lock()
            .take(MAX_ANSWER_BYTES)
            .read_line(&mut line)
        {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                let end = line.trim_end_matches(['\n', '\r']).len();
                line.truncate(end);
                Some(line)
            }
        }
    }

    fn write(&mut self, text: &str) {
        let mut out = std::io::stdout().lock();
        // A closed terminal leaves nothing to tell: the reply's own print reports it.
        let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
    }
}

/// Opens a link in the person's browser.
pub(crate) trait Opener {
    /// Starts the browser on `link`; `false` when it could not be started.
    fn open(&mut self, link: &str) -> bool;
}

/// The browser command for `link`: the platform's opener (`open` on macOS, `explorer.exe` on
/// Windows, `xdg-open` elsewhere) with the link as its one argument, started directly, never
/// through a shell.
fn browser_command(link: &str) -> Command {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer.exe"
    } else {
        "xdg-open"
    };
    let mut cmd = Command::new(program);
    cmd.arg(link)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

/// The system's browser opener. It does not wait on the browser: the opener's own exit is
/// reaped on a thread of its own.
struct SystemOpener;

impl Opener for SystemOpener {
    fn open(&mut self, link: &str) -> bool {
        match browser_command(link).spawn() {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                true
            }
            Err(_) => false,
        }
    }
}

/// The hub steps of an in-place sign-in: the same service calls as `auth sign-in` and
/// `auth state`.
pub(crate) trait SignInSteps {
    /// `auth sign-in`: the card with no `confirmation`, else the answer to it.
    fn start(&mut self, confirmation: Option<AuthConfirmation>) -> Result<AuthReply, RdyError>;
    /// Waits on `pending` with `auth state`'s check until the sign-in ends.
    fn wait(&mut self, pending: &AuthState) -> Result<AuthReply, RdyError>;
}

/// The credential service's steps for the caller of `surface`.
struct ServiceSteps<'s, 'a> {
    surface: &'s AuthSurface<'a>,
    install: &'s Origin,
    machine_name: &'s str,
}

impl SignInSteps for ServiceSteps<'_, '_> {
    fn start(&mut self, confirmation: Option<AuthConfirmation>) -> Result<AuthReply, RdyError> {
        let req = AuthRequest::Start {
            install: self.install.clone(),
            machine_name: self.machine_name.to_string(),
            confirmation,
        };
        run_auth(self.surface, req)
    }

    fn wait(&mut self, pending: &AuthState) -> Result<AuthReply, RdyError> {
        let claim = RequestClaim::from_session(self.surface.session)?;
        let mut sleep_until = |at_ms: i64| {
            let left = at_ms.saturating_sub(rdy_engine::now_ms());
            if let Ok(ms) = u64::try_from(left) {
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
        };
        with_credentials(self.surface, Some(self.install), |c| {
            c.wait(self.install, &claim, pending, &mut sleep_until)
        })
        .map(AuthReply::state)
    }
}

/// Whether `auth sign-in` answers its card in place: stdin and stdout are both terminals, and
/// the call has none of `--json`, `--card-hash`, `--answer` and `--relayed-by`.
pub(crate) fn in_place_wanted(a: &SignInArgs, as_json: bool, console: &dyn Console) -> bool {
    !as_json
        && a.card_hash.is_none()
        && a.answer.is_none()
        && a.relayed_by.is_none()
        && console.stdin_is_terminal()
        && console.stdout_is_terminal()
}

/// `y` or `yes`, in any case.
fn is_yes(line: &str) -> bool {
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Whether the browser may open `link`: an HTTPS link on the install's own origin (scheme, host
/// and port), with no userinfo, as the origin fence checks it.
fn may_open(link: &str, install: &Origin) -> bool {
    OriginFence::new(install.clone(), [])
        .and_then(|fence| fence.check(link))
        .is_ok()
}

/// The answer to a prompt: the line, or `None` at the end of input, where the prompt's line is
/// ended so the next print starts on its own line.
fn ask(console: &mut dyn Console, prompt: &str) -> Option<String> {
    console.write(prompt);
    let line = console.read_line();
    if line.is_none() {
        console.write("\n");
    }
    line
}

/// `auth sign-in --install <origin>` on a terminal: prints the card and asks
/// `Sign in to <origin> as <machine>? [y/N]`. `y` or `yes` in any case confirms; any other line
/// or the end of input cancels (`sign-in cancelled for <origin>`). Either answer is the person's
/// own, sent with the card's hash as `--answer` sends it. After a confirm that starts a device
/// sign-in, it prints the code and the link, asks `Open the link in your browser? [Y/n]` (an
/// empty line, `y` or `yes` opens it), opens only a link [`may_open`] allows, and then waits
/// until the sign-in ends. A browser that does not open leaves the printed link. Any other reply
/// is returned as it is.
pub(crate) fn sign_in_in_place(
    install: &Origin,
    machine_name: &str,
    console: &mut dyn Console,
    opener: &mut dyn Opener,
    steps: &mut dyn SignInSteps,
) -> Result<AuthReply, RdyError> {
    let card = steps.start(None)?;
    let (text, card_hash) = match &card {
        AuthReply::State(s) if s.state == AuthPhase::ConfirmationRequired => (
            s.card.clone().unwrap_or_default(),
            s.card_hash.clone().unwrap_or_default(),
        ),
        _ => return Ok(card),
    };
    console.write(&text);
    let line = ask(
        console,
        &format!("Sign in to {install} as {machine_name}? [y/N] "),
    );
    let answer = if line.as_deref().is_some_and(is_yes) {
        AuthAnswer::Confirm
    } else {
        AuthAnswer::Cancel
    };
    let reply = steps.start(Some(AuthConfirmation {
        card_hash,
        answer,
        relay: RelayInput::default(),
    }))?;
    let pending = match reply {
        AuthReply::State(s) if s.state == AuthPhase::Pending => *s,
        other => return Ok(other),
    };
    let link = pending.verification_uri.clone().unwrap_or_default();
    console.write(&format!(
        "sign-in pending for {install}\ncode: {}\nopen: {link}\n",
        pending.user_code.as_deref().unwrap_or_default()
    ));
    let open = ask(console, "Open the link in your browser? [Y/n] ")
        .is_some_and(|l| l.trim().is_empty() || is_yes(&l));
    if open && may_open(&link, install) {
        // A browser that does not open leaves the printed link.
        let _ = opener.open(&link);
    }
    steps.wait(&pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_engine::hub_credentials::AuthAnswer;
    use std::collections::VecDeque;

    const HUB: &str = "https://hub.test";
    const LINK: &str = "https://hub.test/app/device";

    /// A terminal of the test's making: the two terminal flags, the lines it gives (then the end
    /// of input) and everything written to it.
    struct FakeConsole {
        stdin: bool,
        stdout: bool,
        lines: VecDeque<String>,
        out: String,
        reads: usize,
    }

    impl FakeConsole {
        fn new(lines: &[&str]) -> FakeConsole {
            FakeConsole {
                stdin: true,
                stdout: true,
                lines: lines.iter().map(|l| (*l).to_string()).collect(),
                out: String::new(),
                reads: 0,
            }
        }
    }

    impl Console for FakeConsole {
        fn stdin_is_terminal(&self) -> bool {
            self.stdin
        }
        fn stdout_is_terminal(&self) -> bool {
            self.stdout
        }
        fn read_line(&mut self) -> Option<String> {
            self.reads += 1;
            self.lines.pop_front()
        }
        fn write(&mut self, text: &str) {
            self.out.push_str(text);
        }
    }

    /// A browser opener that records each link and opens or fails as told.
    struct FakeOpener {
        works: bool,
        opened: Vec<String>,
    }

    impl FakeOpener {
        fn new(works: bool) -> FakeOpener {
            FakeOpener {
                works,
                opened: Vec::new(),
            }
        }
    }

    impl Opener for FakeOpener {
        fn open(&mut self, link: &str) -> bool {
            self.opened.push(link.to_string());
            self.works
        }
    }

    fn origin() -> Origin {
        Origin::parse(HUB).unwrap()
    }

    fn state(phase: AuthPhase) -> AuthState {
        AuthState {
            install: origin(),
            state: phase,
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

    /// The hub steps of the test: the card, then the answer's reply (cancelled for a cancel,
    /// pending with `link` for a confirm), then the wait's end (signed in).
    struct FakeSteps {
        link: String,
        answers: Vec<AuthAnswer>,
        waited: Vec<AuthState>,
    }

    impl FakeSteps {
        fn new(link: &str) -> FakeSteps {
            FakeSteps {
                link: link.to_string(),
                answers: Vec::new(),
                waited: Vec::new(),
            }
        }
    }

    impl SignInSteps for FakeSteps {
        fn start(&mut self, confirmation: Option<AuthConfirmation>) -> Result<AuthReply, RdyError> {
            let Some(c) = confirmation else {
                return Ok(AuthReply::state(AuthState {
                    card: Some("Sign in to a hub\nInstall: https://hub.test\n".into()),
                    card_hash: Some("h".repeat(64)),
                    ..state(AuthPhase::ConfirmationRequired)
                }));
            };
            assert_eq!(c.card_hash, "h".repeat(64), "the card's own hash");
            assert_eq!(c.relay, RelayInput::default(), "the person's own answer");
            self.answers.push(c.answer);
            Ok(AuthReply::state(match c.answer {
                AuthAnswer::Cancel => state(AuthPhase::Cancelled),
                AuthAnswer::Confirm => AuthState {
                    user_code: Some("BCDFGHJK".into()),
                    verification_uri: Some(self.link.clone()),
                    expires_at_ms: Some(600_000),
                    next_poll_at_ms: Some(5_000),
                    ..state(AuthPhase::Pending)
                },
            }))
        }

        fn wait(&mut self, pending: &AuthState) -> Result<AuthReply, RdyError> {
            self.waited.push(pending.clone());
            Ok(AuthReply::state(AuthState {
                human_actor: Some("human:test-person".into()),
                ..state(AuthPhase::SignedIn)
            }))
        }
    }

    /// One in-place sign-in with the fakes: (its reply's text, the console, the opener, the
    /// steps).
    fn in_place(
        lines: &[&str],
        link: &str,
        opener_works: bool,
    ) -> (String, FakeConsole, FakeOpener, FakeSteps) {
        let mut console = FakeConsole::new(lines);
        let mut opener = FakeOpener::new(opener_works);
        let mut steps = FakeSteps::new(link);
        let reply = sign_in_in_place(&origin(), "laptop", &mut console, &mut opener, &mut steps)
            .expect("a reply");
        (reply_text(&reply).unwrap(), console, opener, steps)
    }

    const PROMPT: &str = "Sign in to https://hub.test as laptop? [y/N] ";
    const OPEN_PROMPT: &str = "Open the link in your browser? [Y/n] ";

    /// y or yes in any case confirms; any other line, an empty line or the end of input cancels
    /// with today's cancel reply, line and exit code.
    #[test]
    fn si1_confirm_words_and_cancel() {
        for word in ["y", "Y", "yes", "YES", "Yes"] {
            let (text, console, _, steps) = in_place(&[word, "n"], LINK, true);
            assert_eq!(steps.answers, [AuthAnswer::Confirm], "{word}");
            assert_eq!(text, "signed in to https://hub.test as human:test-person");
            assert!(
                console.out.starts_with(&format!(
                    "Sign in to a hub\nInstall: https://hub.test\n{PROMPT}"
                )),
                "{word}: {}",
                console.out
            );
        }
        let cancel_today = AuthReply::state(state(AuthPhase::Cancelled));
        for lines in [&["n"][..], &["no"], &[""], &["yess"], &[]] {
            let (text, console, opener, steps) = in_place(lines, LINK, true);
            assert_eq!(steps.answers, [AuthAnswer::Cancel], "{lines:?}");
            assert_eq!(text, "sign-in cancelled for https://hub.test", "{lines:?}");
            assert_eq!(text, reply_text(&cancel_today).unwrap());
            assert_eq!(console.reads, 1, "one question only");
            assert!(!console.out.contains(OPEN_PROMPT));
            assert!(opener.opened.is_empty() && steps.waited.is_empty());
        }
        let cancelled = emit(
            Ok((
                cancel_today.to_json().unwrap(),
                reply_text(&cancel_today).unwrap(),
                true,
            )),
            false,
            &[],
        );
        assert_eq!(cancelled, ExitCode::SUCCESS, "today's cancel exit code");
    }

    fn sign_in_args() -> SignInArgs {
        SignInArgs {
            install: HUB.into(),
            machine_name: None,
            card_hash: None,
            answer: None,
            relayed_by: None,
            evidence: None,
            question: None,
            session_ref: None,
        }
    }

    /// --json, --card-hash, --answer, --relayed-by, or a stdin or stdout that is not a
    /// terminal: today's card path, with no line read.
    #[test]
    fn si2_flags_or_no_terminal_keep_the_card_path() {
        let console = FakeConsole::new(&[]);
        assert!(in_place_wanted(&sign_in_args(), false, &console));
        let flagged: [fn(&mut SignInArgs); 3] = [
            |a| a.card_hash = Some("h".into()),
            |a| a.answer = Some(AnswerArg::Confirm),
            |a| a.relayed_by = Some("agent:claude".into()),
        ];
        for set in flagged {
            let mut a = sign_in_args();
            set(&mut a);
            assert!(!in_place_wanted(&a, false, &console));
        }
        assert!(!in_place_wanted(&sign_in_args(), true, &console), "--json");
        for (stdin, stdout) in [(false, true), (true, false), (false, false)] {
            let mut c = FakeConsole::new(&["y"]);
            (c.stdin, c.stdout) = (stdin, stdout);
            assert!(!in_place_wanted(&sign_in_args(), false, &c));
            assert_eq!(c.reads, 0, "no line is read");
        }
    }

    /// After a confirm: the code and the link, then the open question; an empty line or y opens
    /// the link, n does not; the opener gets the link as its one argument.
    #[test]
    fn si4_open_prompt() {
        for (line, opens) in [
            ("", true),
            ("y", true),
            ("YES", true),
            ("n", false),
            ("no", false),
        ] {
            let (text, console, opener, steps) = in_place(&["y", line], LINK, true);
            assert_eq!(
                console.out,
                format!(
                    "Sign in to a hub\nInstall: https://hub.test\n{PROMPT}sign-in pending for https://hub.test\ncode: BCDFGHJK\nopen: {LINK}\n{OPEN_PROMPT}"
                ),
                "{line:?}"
            );
            let want: &[&str] = if opens { &[LINK] } else { &[] };
            assert_eq!(opener.opened, want, "{line:?}");
            assert_eq!(steps.waited.len(), 1, "the wait goes on");
            assert_eq!(steps.waited[0].verification_uri.as_deref(), Some(LINK));
            assert_eq!(text, "signed in to https://hub.test as human:test-person");
        }
        // the end of input opens nothing, and the wait goes on
        let (_, _, opener, steps) = in_place(&["y"], LINK, true);
        assert!(opener.opened.is_empty());
        assert_eq!(steps.waited.len(), 1);
        // the real opener: the platform's program with the link as its only argument
        let cmd = browser_command(LINK);
        let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(args, [std::ffi::OsStr::new(LINK)]);
        let program = cmd.get_program().to_string_lossy().into_owned();
        assert!(
            ["open", "xdg-open", "explorer.exe"].contains(&program.as_str()),
            "{program}"
        );
        assert!(!["sh", "cmd", "cmd.exe"].contains(&program.as_str()));
    }

    /// Only an https link on the install's own origin is opened: another scheme, host or port,
    /// or a link with userinfo, is not. The link stays printed and the wait goes on.
    #[test]
    fn si5_opener_guard() {
        for link in [
            "http://hub.test/app/device",
            "https://other.test/app/device",
            "https://hub.test:8443/app/device",
            "https://user@hub.test/app/device",
            "https://user:pw@hub.test/app/device",
            "https://hub.test.evil/app/device",
            "javascript:alert(1)",
        ] {
            let (text, console, opener, steps) = in_place(&["y", ""], link, true);
            assert!(opener.opened.is_empty(), "{link}");
            assert!(console.out.contains(&format!("open: {link}\n")), "{link}");
            assert_eq!(steps.waited.len(), 1, "{link}");
            assert_eq!(text, "signed in to https://hub.test as human:test-person");
        }
        assert!(may_open(LINK, &origin()));
        assert!(may_open("https://hub.test:443/app/device", &origin()));
    }

    /// A browser that cannot be opened leaves the printed link, and the wait goes on.
    #[test]
    fn si6_opener_failure_keeps_the_link_and_waits() {
        let (text, console, opener, steps) = in_place(&["y", ""], LINK, false);
        assert_eq!(opener.opened, [LINK]);
        assert!(console.out.contains(&format!("open: {LINK}\n")));
        assert!(console.out.ends_with(OPEN_PROMPT));
        assert_eq!(steps.waited.len(), 1);
        assert_eq!(text, "signed in to https://hub.test as human:test-person");
    }

    /// The token door with the output kind injected: only JSON to a pipe passes, with the
    /// shared texts; a terminal is refused even with --json.
    #[test]
    fn hsa8_token_output_requires_json_and_a_pipe() {
        assert!(token_door(true, OutputKind::Other).is_ok());
        for (as_json, out) in [
            (true, OutputKind::Terminal),
            (false, OutputKind::Terminal),
            (false, OutputKind::Other),
        ] {
            let e = token_door(as_json, out).expect_err("refused");
            assert_eq!(
                (e.code, e.message.as_str(), e.fix.as_str()),
                (
                    E_BAD_INPUT,
                    "auth token needs --json and a pipe for its output",
                    "run this command from the credential consumer with piped output"
                ),
                "{as_json} {out:?}"
            );
        }
    }
}
