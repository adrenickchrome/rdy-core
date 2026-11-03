//! `qa-follower <full> <dir> [--then <increment>]...`: seeds a follower once, opens it once,
//! follows each increment in argument order, reads and checks it after each success, and prints
//! one JSON object. A refusal prints the engine's error as JSON on stderr and exits 1. A QA
//! command over the engine's follower: it opens no source project and promotes nothing.
use rdy_engine::RdyError;
use rdy_engine::dump_error::chain_too_long;
use rdy_engine::dump_stop::MAX_CHAIN_FILES;
use rdy_engine::follower::{self, FollowerCore};
use serde_json::json;
use std::ffi::OsString;
use std::path::PathBuf;

const USAGE: &str = "usage: qa-follower <full> <dir> [--then <increment>]...";

fn bad_arguments() -> RdyError {
    RdyError::new(
        "E_BAD_INPUT",
        "invalid qa-follower arguments",
        "pass a full dump, an empty follower directory and zero or more --then increment paths",
    )
}

/// One path argument: any text but the empty one, a leading dash included.
fn path(arg: Option<OsString>) -> Result<PathBuf, RdyError> {
    match arg {
        Some(a) if !a.is_empty() => Ok(PathBuf::from(a)),
        _ => Err(bad_arguments()),
    }
}

fn run(mut args: impl Iterator<Item = OsString>) -> Result<Option<serde_json::Value>, RdyError> {
    let first = args.next();
    if first.as_deref() == Some(std::ffi::OsStr::new("--help")) {
        if args.next().is_some() {
            return Err(bad_arguments());
        }
        println!("{USAGE}");
        return Ok(None);
    }
    let full = path(first)?;
    let dir = path(args.next())?;
    let mut then: Vec<PathBuf> = Vec::new();
    while let Some(key) = args.next() {
        if key != "--then" {
            return Err(bad_arguments());
        }
        let increment = path(args.next())?;
        if then.len() >= MAX_CHAIN_FILES {
            return Err(chain_too_long());
        }
        then.push(increment);
    }
    let mut end = follower::seed(&full, &dir)?;
    let mut core = FollowerCore::open(&dir)?;
    let (mut batches, mut frames) = (0u64, 0u64);
    for increment in &then {
        let got = core.follow(increment)?;
        batches += got.batches;
        frames += got.frames;
        end = got.end;
        core.read()?;
        core.fsck()?;
    }
    Ok(Some(json!({
        "repo_id": end.repo_id.to_string(),
        "end": end.end,
        "digest": end.digest.to_string(),
        "increments": then.len(),
        "batches": batches,
        "frames": frames,
    })))
}

fn main() {
    match run(std::env::args_os().skip(1)) {
        Ok(Some(report)) => println!("{report}"),
        Ok(None) => {}
        Err(e) => {
            eprintln!(
                "{}",
                serde_json::to_string(&e).unwrap_or_else(|_| e.code.to_string())
            );
            std::process::exit(1);
        }
    }
}
