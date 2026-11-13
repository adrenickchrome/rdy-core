//! The Windows side of `sys`, one child module for each part: `pipe` makes the transport,
//! `identity` reads the account at its other end, `agent` reaches the caller's SSH agent,
//! `run_state` names the project's pipe and reads and writes the files of its run directory,
//! `process` starts and looks at processes, and `stop` holds the project's stop event and ends a
//! daemon under proof. The parts still to come have no stand-in here, so this crate does not build
//! for Windows until the last of them exists.

mod agent;
mod identity;
mod pipe;
mod process;
mod run_state;
mod stop;
pub use agent::*;
pub use identity::*;
pub use pipe::*;
pub use process::*;
pub use run_state::*;
pub use stop::*;

// Local hooks

/// The text a hook launcher's shell line names `path` by. Git for Windows runs `commit-msg`
/// through its own `sh`, which takes a drive path with forward slashes (`C:/Program
/// Files/rdy/rdy.exe`) as the native program and passes it on unchanged as an argument. A plain
/// drive path loses its `\\?\` prefix first; `None` for a path that is not Unicode or that needs
/// the prefix, which that shell cannot name.
pub fn hook_sh_path(path: &std::path::Path) -> Option<String> {
    let text = rdy_store::pio::plain_drive_path(path.to_str()?);
    if text.starts_with(r"\\?\") {
        return None;
    }
    Some(text.replace('\\', "/"))
}

/// The text a harness's exec-form settings name `path` by: the native path, a plain drive path
/// without its `\\?\` prefix. The harness starts `rdy.exe` directly, with no shell. `None` for a
/// path that is not Unicode.
pub fn hook_exec_path(path: &std::path::Path) -> Option<String> {
    Some(rdy_store::pio::plain_drive_path(path.to_str()?).to_string())
}

/// Windows has no executable bit: Git for Windows runs an extensionless `commit-msg` through its
/// own shell. Nothing is written.
pub fn make_launcher_executable(
    dir: &std::fs::File,
    name: &std::ffi::OsStr,
) -> std::io::Result<()> {
    let _ = (dir, name);
    Ok(())
}

/// Whether a launcher may be run: always here, where no mode decides it.
pub fn launcher_is_executable(meta: &std::fs::Metadata) -> bool {
    let _ = meta;
    true
}

/// The allocator's large-block threshold: nothing to fix here (the Linux glibc step, TASK-2307).
/// Returns false: no setting was applied.
pub fn fix_malloc_threshold() -> bool {
    false
}

/// The bytes the allocator holds in blocks with their own mapping: not read here.
pub fn malloc_mapped_bytes() -> Option<usize> {
    None
}
