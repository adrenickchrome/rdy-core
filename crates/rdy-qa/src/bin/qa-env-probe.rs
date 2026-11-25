//! The gate's probe runner. The gate sets it as Cargo's runner for one `cargo test` call, so
//! Cargo starts it in place of each test binary, with the binary's path first. It writes the
//! working directory and environment that Cargo gives that binary to one file in the directory
//! that `RDY_QA_RUNNER_PROBE` names, then exits 0. It never starts the binary.
use rdy_qa::gate_steps::{RUNNER_PROBE_VAR, runner_record};
use std::path::Path;

fn main() {
    let Some(dir) = std::env::var_os(RUNNER_PROBE_VAR) else {
        eprintln!("qa-env-probe: {RUNNER_PROBE_VAR} names no directory; only the gate runs this");
        std::process::exit(2);
    };
    let Some(exe) = std::env::args().nth(1) else {
        eprintln!("qa-env-probe: no test binary given");
        std::process::exit(2);
    };
    // the binary's file name holds Cargo's hash, so it is unique in the build
    let name = Path::new(&exe)
        .file_name()
        .map_or_else(|| "binary".into(), |n| n.to_string_lossy().into_owned());
    let file = Path::new(&dir).join(format!("{name}.json"));
    if let Err(e) = std::fs::write(&file, runner_record(&exe)) {
        eprintln!("qa-env-probe: cannot write {}: {e}", file.display());
        std::process::exit(1);
    }
}
