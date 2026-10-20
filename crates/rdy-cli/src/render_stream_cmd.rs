//! hpb9: `rdy render-stream`, the internal render process (hub i-N0G2N, r-B0ZYE, r-7GS51;
//! D-KDFZ3, D-48NYM). It reads one render request on standard input and writes the artifact
//! packet on standard output (`rdy_render::process::run`). It runs before project discovery,
//! config reads and any daemon connection, takes no option of its own and refuses the global
//! ones, so nothing but its standard input decides its bytes.
//!
//! A refusal leaves standard output empty, exits 1 and writes one compact JSON line on standard
//! error (`rdy_render::process::error_line`). After an output failure the bytes already written
//! may be partial: the exit status 1 invalidates them. A success exits 0 and writes nothing on
//! standard error.
use rdy_engine::{E_BAD_INPUT, RdyError};
use rdy_render::process::{error_line, run};
use std::io::{BufReader, Write};
use std::process::ExitCode;

/// `E_BAD_INPUT`: a global option (`--json`, `--project`, `--no-daemon`) beside the command.
pub(crate) fn global_options() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "render-stream accepts only its standard input",
        "send the render request on standard input without global output or project options",
    )
}

/// Runs the process over standard input and output, or refuses `globals` (the global options
/// were given) before reading anything.
pub(crate) fn render_stream(globals: bool) -> ExitCode {
    let result = if globals {
        Err(global_options())
    } else {
        run(
            BufReader::new(std::io::stdin().lock()),
            std::io::stdout().lock(),
        )
        .map(drop)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(&error_line(&e));
            let _ = err.flush();
            ExitCode::FAILURE
        }
    }
}
