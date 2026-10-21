//! `qa-release-install`: checks a signed preview download and installs it into a new version
//! prefix, then runs the installed CLI and MCP paths. Prints the installed report as one JSON
//! object; see `rdy_qa::release_install::check`. `--print-commands` prints the guide instead.
use clap::Parser;
use rdy_qa::release_install::{self, InstallInput, MSG_INSTALL_INVALID_ARGUMENTS};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "qa-release-install",
    about = "Verify a signed rdy preview download, install it and smoke-test the installed binary"
)]
struct Args {
    /// The directory holding the archive, SHA256SUMS and SHA256SUMS.sig (absolute).
    #[arg(long, value_name = "DIR")]
    download: String,
    /// The trusted allowed-signers line, as the README and the release notes print it.
    #[arg(long, value_name = "LINE")]
    signer_line: String,
    /// The plain release version, `0.<minor>.0`.
    #[arg(long, value_name = "V")]
    version: String,
    /// The 40-hex commit from the build receipt.
    #[arg(long, value_name = "S")]
    commit: String,
    /// The target triple: aarch64-apple-darwin or x86_64-unknown-linux-gnu.
    #[arg(long, value_name = "TRIPLE")]
    target: String,
    /// A new directory to extract into (absolute; it must not exist).
    #[arg(long, value_name = "DIR")]
    unpack: String,
    /// The new version prefix, `.../rdy-V` (absolute; it must not exist).
    #[arg(long, value_name = "DIR")]
    prefix: String,
    /// The archive was downloaded with a browser on macOS.
    #[arg(long)]
    browser: bool,
    /// Print the install commands for these arguments and do nothing else.
    #[arg(long)]
    print_commands: bool,
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
        Err(_) => return refuse(MSG_INSTALL_INVALID_ARGUMENTS, 2),
    };
    let input = InstallInput {
        download: PathBuf::from(args.download),
        signer_line: args.signer_line,
        version: args.version,
        commit: args.commit,
        target: args.target,
        unpack: PathBuf::from(args.unpack),
        prefix: PathBuf::from(args.prefix),
        browser: args.browser,
    };
    // the grammar of every argument first: a failure here is an argument error
    let commands = match release_install::commands(&input) {
        Ok(commands) => commands,
        Err(_) => return refuse(MSG_INSTALL_INVALID_ARGUMENTS, 2),
    };
    if args.print_commands {
        return match release_install::render_commands(&commands) {
            Ok(block) => print(&format!("{block}\n")),
            Err(_) => refuse(MSG_INSTALL_INVALID_ARGUMENTS, 2),
        };
    }
    match release_install::check(&input) {
        Ok(report) => match serde_json::to_string(&report) {
            Ok(json) => print(&format!("{json}\n")),
            Err(_) => refuse(&release_install::InstallFailure::Input.message(), 1),
        },
        Err(failure) => refuse(&failure.message(), 1),
    }
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
