//! `qa-release-trust`: reads public release-key records, verifies them against the pinned
//! historical root, and selects the one release key of a version; see
//! `rdy_qa::release_keys`. It reads public data only: it never signs and never contacts a key
//! service.
//!
//! - `select` prints one JSON object with exactly `version`, `fingerprint`, `signer_line`,
//!   `transition_sha256` and `policy_commit`. `--signing` also refuses a revoked or retired key.
//! - `verify` checks a release manifest and its signature under the version's key only, then
//!   prints the same object.
//! - `render` prints the README trust section; `versions` prints the generated `versions.txt`;
//!   `records` prints the generated `records.txt` of a directory's names.
//!
//! Without `--keys`, only the historical root is known. A refusal is one
//! `release trust: <reason>` line on stderr, exit 1; invalid arguments exit 2.
use clap::{Args, Parser, Subcommand};
use rdy_qa::release_keys::{self, MANIFEST_MAX, RELEASE_SIG_MAX, ReleaseKeys, Signer, TrustError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The refusal of the arguments (stderr, with an LF; exit 2).
const MSG_INVALID_ARGUMENTS: &str = "release trust: invalid arguments";

#[derive(Parser)]
#[command(
    name = "qa-release-trust",
    about = "Verify public rdy release-key records and select the one release key of a version"
)]
struct Cli {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Args)]
struct Keys {
    /// The `release-keys/` directory. Without it, only the historical root is known.
    #[arg(long, value_name = "DIR", requires = "policy_commit")]
    keys: Option<PathBuf>,
    /// The 40-hex commit the directory was read at; the result repeats it.
    #[arg(long, value_name = "SHA", requires = "keys")]
    policy_commit: Option<String>,
}

#[derive(Subcommand)]
enum Mode {
    /// Print the one release key of a version.
    Select {
        /// The plain release version, `0.<minor>.0`.
        #[arg(long, value_name = "V")]
        version: String,
        #[command(flatten)]
        keys: Keys,
        /// For a new signing: also refuse a revoked key and the retired historical root.
        #[arg(long)]
        signing: bool,
    },
    /// Verify a release manifest and its signature under the version's key only.
    Verify {
        /// The plain release version, `0.<minor>.0`.
        #[arg(long, value_name = "V")]
        version: String,
        /// The `SHA256SUMS` file.
        #[arg(long, value_name = "FILE")]
        manifest: PathBuf,
        /// The `SHA256SUMS.sig` file.
        #[arg(long, value_name = "FILE")]
        signature: PathBuf,
        #[command(flatten)]
        keys: Keys,
    },
    /// Print the README trust section.
    Render {
        /// The `release-keys/` directory.
        #[arg(long, value_name = "DIR")]
        keys: Option<PathBuf>,
    },
    /// Print the generated `versions.txt`.
    Versions {
        /// The `release-keys/` directory.
        #[arg(long, value_name = "DIR")]
        keys: Option<PathBuf>,
    },
    /// Print the generated `records.txt` of a directory's names.
    Records {
        /// The `release-keys/` directory.
        #[arg(long, value_name = "DIR")]
        keys: PathBuf,
    },
}

/// The `select` and `verify` result: exactly these fields, in sorted order.
#[derive(serde::Serialize)]
struct TrustResult<'a> {
    fingerprint: &'a str,
    policy_commit: Option<&'a str>,
    signer_line: &'a str,
    transition_sha256: Option<&'a str>,
    version: &'a str,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) if e.kind() == clap::error::ErrorKind::DisplayHelp => {
            return match e.print() {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::FAILURE,
            };
        }
        Err(_) => return refuse(MSG_INVALID_ARGUMENTS, 2),
    };
    let keys_args = match &cli.mode {
        Mode::Select { keys, .. } | Mode::Verify { keys, .. } => Some(keys),
        _ => None,
    };
    if keys_args
        .and_then(|k| k.policy_commit.as_deref())
        .is_some_and(|c| !valid_commit(c))
    {
        return refuse(MSG_INVALID_ARGUMENTS, 2);
    }
    match run(&cli.mode) {
        Ok(text) => print(&text),
        Err(e) => refuse(&e.to_string(), 1),
    }
}

fn run(mode: &Mode) -> Result<String, TrustError> {
    match mode {
        Mode::Select {
            version,
            keys,
            signing,
        } => {
            let trust = load(keys.keys.as_deref())?;
            let signer = if *signing {
                trust.eligible_signer(version)?
            } else {
                trust.signer_for(version)?
            };
            result(&signer, keys.policy_commit.as_deref())
        }
        Mode::Verify {
            version,
            manifest,
            signature,
            keys,
        } => {
            let trust = load(keys.keys.as_deref())?;
            let manifest = read_bounded(manifest, MANIFEST_MAX, "the manifest")?;
            let signature = read_bounded(signature, RELEASE_SIG_MAX, "the signature")?;
            let signer = trust.verify_release(version, &manifest, &signature)?;
            result(&signer, keys.policy_commit.as_deref())
        }
        Mode::Render { keys } => load(keys.as_deref())?.trust_section(),
        Mode::Versions { keys } => Ok(load(keys.as_deref())?.versions_txt()),
        Mode::Records { keys } => release_keys::generated_records(keys),
    }
}

/// The authenticated trust of `dir`, or the historical root alone.
fn load(dir: Option<&Path>) -> Result<ReleaseKeys, TrustError> {
    match dir {
        Some(dir) => ReleaseKeys::load(dir),
        None => Ok(ReleaseKeys::historical()),
    }
}

/// The JSON line of `signer`.
fn result(signer: &Signer, policy_commit: Option<&str>) -> Result<String, TrustError> {
    let out = TrustResult {
        fingerprint: &signer.fingerprint,
        policy_commit,
        signer_line: &signer.signer_line,
        transition_sha256: signer.transition_sha256.as_deref(),
        version: &signer.version,
    };
    serde_json::to_string(&out)
        .map(|json| format!("{json}\n"))
        .map_err(|_| TrustError::Render("the result".to_string()))
}

/// At most `max + 1` bytes of `path`: one byte over the bound is enough for the refusal.
fn read_bounded(path: &Path, max: u64, what: &str) -> Result<Vec<u8>, TrustError> {
    let file = std::fs::File::open(path).map_err(|_| TrustError::Read(what.to_string()))?;
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| TrustError::Read(what.to_string()))?;
    Ok(bytes)
}

/// Whether `commit` is 40 lowercase hexadecimal digits.
fn valid_commit(commit: &str) -> bool {
    commit.len() == 40
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Writes `text` to stdout.
fn print(text: &str) -> ExitCode {
    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// Writes `message` and LF to stderr and exits with `code`.
fn refuse(message: &str, code: u8) -> ExitCode {
    let _ = writeln!(std::io::stderr().lock(), "{message}");
    ExitCode::from(code)
}
