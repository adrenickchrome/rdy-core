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
use std::ffi::OsString;
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

/// Whether `args` (the program name first) run `render-stream`, and if so whether any argument
/// but the command word is given. The command word is found as clap finds it: an argument
/// starting with `-` is an option (`--project` takes the next argument as its value, unless it
/// is written `--project=<dir>`), and the first other argument is the command. So
/// `rdy render-stream --project`, `rdy --project=x render-stream` and `rdy render-stream --out x`
/// all reach the command's own refusal before clap could print its usage text.
pub(crate) fn invocation(args: &[OsString]) -> Option<bool> {
    let mut rest = args.iter().skip(1);
    while let Some(a) = rest.next() {
        if a == "--project" {
            rest.next();
        } else if !a.as_encoded_bytes().starts_with(b"-") {
            return (a == "render-stream").then_some(args.len() > 2);
        }
    }
    None
}

/// Runs the process over standard input and output, or refuses `globals` (an argument but the
/// command word was given) before reading anything.
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

#[cfg(test)]
mod tests {
    use super::invocation;

    fn of(args: &[&str]) -> Option<bool> {
        let args: Vec<_> = std::iter::once("rdy")
            .chain(args.iter().copied())
            .map(Into::into)
            .collect();
        invocation(&args)
    }

    #[test]
    fn hpb9_render_stream_is_found_before_clap_parses() {
        assert_eq!(of(&["render-stream"]), Some(false));
        for args in [
            &["render-stream", "--project"][..],
            &["render-stream", "--out", "x"],
            &["render-stream", "x"],
            &["--json", "render-stream"],
            &["--project", "dir", "render-stream"],
            &["--project=dir", "render-stream"],
            &["--bogus", "render-stream"],
        ] {
            assert_eq!(of(args), Some(true), "{args:?}");
        }
        // another command, or render-stream as a value: clap's
        for args in [
            &["show", "render-stream"][..],
            &["--project", "render-stream"],
            &["render", "--out", "x"],
            &[],
        ] {
            assert_eq!(of(args), None, "{args:?}");
        }
    }
}
