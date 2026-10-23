//! `qa-release-sources`: copies the exact crate file of each MPL-2.0-only library that the
//! release binaries link from Cargo's registry cache into a new directory, checks each against
//! `Cargo.lock`, and prints the release notes part that names them. With `--check-notes`, it
//! first checks a release notes file against that part and checks its changes part (D-7PQEF). See
//! `rdy_qa::release_notices::sources_tool`.
use std::ffi::OsString;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    ExitCode::from(rdy_qa::release_notices::sources_tool(
        &args,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    ))
}
