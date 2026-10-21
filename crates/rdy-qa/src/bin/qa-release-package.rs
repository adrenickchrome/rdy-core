//! `qa-release-package`: packages a release build of rdy as one native `.tar.gz`.
//! Prints the archive's absolute path; see `rdy_qa::release_install::package`.
use clap::Parser;
use rdy_qa::release_install::{self, MSG_INVALID_ARGUMENTS};
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "qa-release-package",
    about = "Package a release build of rdy as one native .tar.gz archive"
)]
struct Args {
    /// The rdy binary to package (an absolute path to a regular file).
    #[arg(long, value_name = "FILE")]
    binary: String,
    /// The plain release version, `0.<minor>.0`; the binary's `--version` must print it.
    #[arg(long, value_name = "V")]
    version: String,
    /// This machine's target triple: aarch64-apple-darwin or x86_64-unknown-linux-gnu.
    #[arg(long, value_name = "TRIPLE")]
    target: String,
    /// The output directory (an absolute path that does not exist yet).
    #[arg(long, value_name = "DIR")]
    out: String,
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
        Err(_) => return refuse(MSG_INVALID_ARGUMENTS, 2),
    };
    match release_install::package(
        Path::new(&args.binary),
        &args.version,
        &args.target,
        Path::new(&args.out),
    ) {
        Ok(archive) => {
            let mut stdout = std::io::stdout().lock();
            match writeln!(stdout, "{}", archive.display()).and_then(|()| stdout.flush()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::FAILURE,
            }
        }
        Err(e) => refuse(&e, if e == MSG_INVALID_ARGUMENTS { 2 } else { 1 }),
    }
}

/// Writes `message` and LF to stderr and exits with `code`.
fn refuse(message: &str, code: u8) -> ExitCode {
    let _ = writeln!(std::io::stderr().lock(), "{message}");
    ExitCode::from(code)
}
