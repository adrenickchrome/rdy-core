//! wx26d: a message for a person or an agent shows a Windows drive path without the `\\?\`
//! prefix; a UNC path and a path that needs the prefix stay as they are.
use rdy_store::pio::{display_path, plain_drive_path};
use std::path::Path;

#[test]
fn wx26d_messages_show_a_drive_path_without_the_verbatim_prefix() {
    assert_eq!(
        plain_drive_path(r"\\?\C:\Users\me\proj\.rdy"),
        r"C:\Users\me\proj\.rdy"
    );
    assert_eq!(plain_drive_path(r"\\?\d:\x"), r"d:\x");
    assert_eq!(plain_drive_path(r"\\?\C:\"), r"C:\");
    // identity stays: a UNC path, a device path, a path that needs the prefix
    for keep in [
        r"\\?\UNC\srv\share\p",
        r"\\?\pipe\agent",
        r"\\?\Volume{1234}\x",
        r"\\?\C:\dir\trailing.\x",
        r"\\?\C:\dir\trailing \x",
        r"\\?\C:\dir\CON\x",
        r"\\?\C:\dir\nul.txt",
        r"\\?\C:\a\..\b",
        r"\\?\C:\a\.\b",
        r"\\?\C:\a/b",
        r"\\?\C:rel",
        r"C:\plain\already",
        "/unix/path",
    ] {
        assert_eq!(plain_drive_path(keep), keep, "{keep}");
    }
    let long = format!(r"\\?\C:\{}", "d".repeat(300));
    assert_eq!(plain_drive_path(&long), long);
    // the held-store refusal text of the daemon shows the plain form
    let shown = display_path(Path::new(r"\\?\C:\Users\me\proj\.rdy"));
    if cfg!(windows) {
        assert_eq!(shown, r"C:\Users\me\proj\.rdy");
    } else {
        assert_eq!(shown, r"\\?\C:\Users\me\proj\.rdy");
    }
}
