//! `qa-release-readme`: rebuilds the repository README from the checked export
//! `release/readme.md`, its sidecar and `assets/readme/`, with the generated parts from the
//! release targets and the authenticated release-key records. `--check` compares the README with
//! the same assembly and writes nothing. See `rdy_qa::release_readme`.
use clap::Parser;
use rdy_qa::release_readme::{self, Mode, ReadmeFailure};
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "qa-release-readme",
    about = "Assemble the release README from the checked export, or check it with --check"
)]
struct Args {
    /// The repository root: it holds Cargo.toml, release-keys/, release/readme.md,
    /// release/readme.sha256, assets/readme/ and README.md.
    #[arg(long, value_name = "DIR")]
    root: String,
    /// The release version, three numbers; it must equal the workspace version.
    #[arg(long, value_name = "V")]
    version: String,
    /// Compare README.md with the assembled text and write nothing.
    #[arg(long)]
    check: bool,
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
        Err(_) => return refuse(ReadmeFailure::Arguments),
    };
    if !release_readme::valid_version(&args.version) {
        return refuse(ReadmeFailure::Arguments);
    }
    let mode = if args.check { Mode::Check } else { Mode::Write };
    match release_readme::run(Path::new(&args.root), &args.version, mode) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => refuse(failure),
    }
}

/// Writes the failure's one line and LF to stderr and exits with its code.
fn refuse(failure: ReadmeFailure) -> ExitCode {
    let _ = writeln!(std::io::stderr().lock(), "{}", failure.message());
    ExitCode::from(failure.exit_code())
}
