//! ha15: `rdy stream check <file|-> [--repo-id <id>]`, the CLI twin of `rdy_stream_check`
//! (D-D2NPN, D-KQ4CA, D-WDKWQ). It checks one upload batch without opening a store: it runs
//! before project discovery, config reads and any daemon connection, so it works in any
//! directory. The pure check is `rdy_engine::stream_check::check_batch`; this file only reads
//! the input and prints the report once the whole batch has passed.
use crate::output::print_error;
use rdy_daemon::ErrorReply;
use rdy_engine::stream_check::check_batch;
use rdy_engine::{E_BAD_INPUT, E_IO, RdyError};
use rdy_model::RepoId;
use serde_json::Value;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(clap::Subcommand)]
pub(crate) enum StreamCmd {
    /// Check one upload batch without opening a store, and print its record keys in order. Complete upload checks run when all batches are taken. The file is read from the current directory; - reads standard input.
    Check(CheckArgs),
}

#[derive(clap::Args)]
pub(crate) struct CheckArgs {
    /// The batch file, or - for standard input.
    file: PathBuf,
    /// Refuse a batch of another repo: 32 hex digits.
    #[arg(long)]
    repo_id: Option<String>,
}

/// Runs the command and prints its result as `main` prints any command's: JSON on stdout with
/// `--json`, else the one summary line (with the count of the batch's prerequisites, when it has
/// any). Nothing prints until the whole batch has passed.
pub(crate) fn run(cmd: StreamCmd, as_json: bool) -> ExitCode {
    let StreamCmd::Check(a) = cmd;
    match check(a) {
        Ok((v, text)) => {
            if as_json {
                println!("{v}");
            } else {
                println!("{text}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            print_error(&e, as_json);
            ExitCode::FAILURE
        }
    }
}

fn check(a: CheckArgs) -> Result<(Value, String), ErrorReply> {
    let repo = a.repo_id.as_deref().map(repo_id).transpose()?;
    let report = if a.file.as_os_str() == "-" {
        check_batch(BufReader::new(std::io::stdin().lock()), repo)
    } else {
        let file = std::fs::File::open(&a.file).map_err(|e| {
            RdyError::new(
                E_IO,
                format!("{}: {e}", a.file.display()),
                "check the path and its permissions",
            )
        })?;
        check_batch(BufReader::new(file), repo)
    }?;
    let text = format!(
        "checked {} records and {} bytes{}",
        report.records,
        report.bytes,
        needs(report.header.prerequisites.len())
    );
    let v = serde_json::to_value(&report).expect("a report serializes");
    Ok((v, text))
}

/// The summary's note of a batch's `n` prerequisites: none when it needs none.
fn needs(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => "; it needs 1 prerequisite from other batches or the holder".to_string(),
        n => format!("; it needs {n} prerequisites from other batches or the holder"),
    }
}

/// `--repo-id`: 32 hex digits, parsed as every repo id is.
fn repo_id(text: &str) -> Result<RepoId, RdyError> {
    text.parse::<RepoId>().map_err(|e| {
        RdyError::new(
            E_BAD_INPUT,
            e.to_string(),
            "pass a repo id with 32 hex digits",
        )
    })
}
