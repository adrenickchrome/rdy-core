//! `qa-release-sample`: checks the frozen Tally sample with an explicit `rdy` binary, or builds
//! it once before publication. `check` prints the receipt as one JSON object; `build` prints the
//! freeze record it wrote. See `rdy_qa::release_sample` for the steps, the closed failures and
//! the receipt's keys.
use clap::{Parser, Subcommand};
use rdy_qa::release_sample::{self, BuildInput, Sample, SampleFailure};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "qa-release-sample",
    about = "Check the frozen Tally sample dump with an rdy binary, or build it"
)]
struct Args {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Load the frozen dump into a new directory, audit it, run the first session and verify it.
    Check {
        /// The rdy binary to check (absolute).
        #[arg(long, value_name = "FILE")]
        rdy: PathBuf,
        /// The frozen dump (absolute).
        #[arg(long, value_name = "FILE")]
        dump: PathBuf,
        /// Its freeze record (absolute).
        #[arg(long, value_name = "FILE")]
        freeze: PathBuf,
        /// The build list (absolute).
        #[arg(long, value_name = "FILE")]
        build_list: PathBuf,
        /// A new work directory (absolute; it must not exist).
        #[arg(long, value_name = "DIR")]
        work: PathBuf,
    },
    /// Build the baseline, freeze one dump and write its freeze record.
    Build {
        /// The rdy binary that builds (absolute).
        #[arg(long, value_name = "FILE")]
        rdy: PathBuf,
        /// The build list (absolute).
        #[arg(long, value_name = "FILE")]
        build_list: PathBuf,
        /// A new work directory (absolute; it must not exist).
        #[arg(long, value_name = "DIR")]
        work: PathBuf,
        /// The new dump file (absolute; it must not exist).
        #[arg(long, value_name = "FILE")]
        out: PathBuf,
        /// The new freeze record (absolute; it must not exist).
        #[arg(long, value_name = "FILE")]
        freeze: PathBuf,
        /// The 40-hex commit the builder runs from.
        #[arg(long, value_name = "COMMIT")]
        builder_revision: String,
    },
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
        Err(_) => return refuse(&SampleFailure::InvalidArguments),
    };
    let absolute = |paths: &[&Path]| paths.iter().all(|p| p.is_absolute());
    match args.command {
        Cmd::Check {
            rdy,
            dump,
            freeze,
            build_list,
            work,
        } => {
            if !absolute(&[&rdy, &dump, &freeze, &build_list, &work]) {
                return refuse(&SampleFailure::InvalidArguments);
            }
            let sample = Sample {
                rdy,
                dump,
                freeze,
                build_list,
                work,
            };
            match release_sample::check(&sample) {
                Ok(receipt) => print_json(&serde_json::Value::Object(receipt)),
                Err(f) => refuse(&f),
            }
        }
        Cmd::Build {
            rdy,
            build_list,
            work,
            out,
            freeze,
            builder_revision,
        } => {
            if !absolute(&[&rdy, &build_list, &work, &out, &freeze]) {
                return refuse(&SampleFailure::InvalidArguments);
            }
            let input = BuildInput {
                rdy,
                build_list,
                work,
                out,
                freeze,
                builder_revision,
            };
            match release_sample::build(&input) {
                Ok(record) => match serde_json::to_value(&record) {
                    Ok(v) => print_json(&v),
                    Err(_) => refuse(&SampleFailure::File),
                },
                Err(f) => refuse(&f),
            }
        }
    }
}

/// Writes `v` and LF to stdout.
fn print_json(v: &serde_json::Value) -> ExitCode {
    let mut stdout = std::io::stdout().lock();
    match writeln!(stdout, "{v}").and_then(|()| stdout.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// Writes the failure's text and LF to stderr and exits with its code.
fn refuse(f: &SampleFailure) -> ExitCode {
    let _ = writeln!(std::io::stderr().lock(), "{}", f.message());
    ExitCode::from(f.exit_code())
}
