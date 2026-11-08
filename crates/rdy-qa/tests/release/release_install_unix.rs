#![cfg(unix)]
// Unix only: a token that is not UTF-8 is made with OsStrExt for the guide renderers' refusal.
//! The raw Unix-byte case of the guide renderers' refusal of tokens that are not text.
use rdy_qa::release_install::{self, GuideCommand, MSG_INVALID_COMMAND};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

#[test]
fn pre3_render_refuses_non_utf8_bytes() {
    let command = |program: &OsStr| GuideCommand {
        program: program.to_os_string(),
        args: vec![],
        cwd: PathBuf::from("/tmp"),
        stdin: None,
        stdout: None,
        ps: None,
    };
    let bad = OsStr::from_bytes(b"a\xffb");
    for render in [
        release_install::render_commands,
        release_install::render_powershell,
    ] {
        assert_eq!(
            render(&[command(bad)]),
            Err(MSG_INVALID_COMMAND.to_string())
        );
        let mut argument = command(OsStr::new("printf"));
        argument.args = vec![bad.to_os_string()];
        assert_eq!(render(&[argument]), Err(MSG_INVALID_COMMAND.to_string()));
        let mut redirected = command(OsStr::new("printf"));
        redirected.stdout = Some(PathBuf::from(bad));
        assert_eq!(render(&[redirected]), Err(MSG_INVALID_COMMAND.to_string()));
    }
}
