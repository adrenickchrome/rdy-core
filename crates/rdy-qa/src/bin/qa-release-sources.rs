//! `qa-release-sources`: copies the exact crate file of each MPL-2.0-only library that the
//! release binaries link from Cargo's registry cache into a new directory, checks each against
//! `Cargo.lock`, and prints the release notes part that names them. With `--check-notes`, it
//! first checks a release notes file against that part. See
//! `rdy_qa::release_notices::release_sources`.
use clap::Parser;
use rdy_qa::release_install::{valid_path, valid_version};
use rdy_qa::release_notices::{self, MSG_INVALID_ARGUMENTS};
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "qa-release-sources",
    about = "Stage the MPL-2.0 crate files of a release and print its third-party notes part"
)]
struct Args {
    /// The plain release version, `0.<minor>.0`.
    #[arg(long, value_name = "V")]
    version: String,
    /// The output directory (an absolute path that does not exist yet).
    #[arg(long, value_name = "DIR")]
    out: String,
    /// A release notes file (an absolute path) to check before anything is staged.
    #[arg(long = "check-notes", value_name = "FILE")]
    check_notes: Option<String>,
}

fn main() -> ExitCode {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(e) if e.kind() == clap::error::ErrorKind::DisplayHelp => {
            return match e.print() {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::FAILURE,
            };
        }
        Err(_) => return refuse(MSG_INVALID_ARGUMENTS),
    };
    let out = Path::new(&args.out);
    let notes = args.check_notes.as_deref().map(Path::new);
    if !(valid_version(&args.version) && valid_path(out) && notes.is_none_or(valid_path)) {
        return refuse(MSG_INVALID_ARGUMENTS);
    }
    if let Some(notes) = notes
        && let Err(e) = release_notices::check_release_file(&args.version, notes)
    {
        return refuse(&e);
    }
    match release_notices::release_sources(&args.version, out) {
        Ok(part) => {
            let mut stdout = std::io::stdout().lock();
            match stdout
                .write_all(part.as_bytes())
                .and_then(|()| stdout.flush())
            {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::FAILURE,
            }
        }
        Err(e) => refuse(&e),
    }
}

/// Writes `message` and LF to stderr; invalid arguments exit 2, every other failure 1.
fn refuse(message: &str) -> ExitCode {
    let _ = writeln!(std::io::stderr().lock(), "{message}");
    ExitCode::from(if message == MSG_INVALID_ARGUMENTS {
        2
    } else {
        1
    })
}
