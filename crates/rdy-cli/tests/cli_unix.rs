#![cfg(unix)]
// Unix only: the editor is a shell script made executable with a file mode. A Windows editor is a
// program or a command line; edit_cmd.rs holds its counterparts (wx20_editor_receives_one_literal_native_path).
mod common;
use common::*;

#[test]
fn edit_runs_editor_with_args_and_failure_changes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, id) = project_with_term();
    let script = dir.path().join("ed.sh");
    std::fs::write(&script, "#!/bin/sh\n[ \"$1\" = \"--flag\" ] && shift\nsed 's/^body: .*/body: Edited by script/' \"$1\" > \"$1.new\" && mv \"$1.new\" \"$1\"\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let editor = format!("{} --flag", script.display());
    let (ok, _, err) = rdy_env(dir.path(), &editor, &["edit", &id, "--view", "work"]);
    assert!(ok, "{err}");
    assert_eq!(
        json(dir.path(), &["show", &id, "--scope", "work"])["body"],
        "Edited by script"
    );

    let (dir2, id2) = project_with_term();
    let (ok, _, _) = rdy_env(dir2.path(), "false", &["edit", &id2, "--view", "work"]);
    assert!(!ok);
    assert_eq!(
        json(dir2.path(), &["show", &id2, "--scope", "work"])["body"],
        "Balances."
    );
}

#[test]
fn wx20_failed_edit_keeps_its_recovery_file() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, id) = project_with_term();
    let script = dir.path().join("bad.sh");
    // the editor saves a file the write refuses
    std::fs::write(&script, "#!/bin/sh\nprintf 'type: [unclosed\\n' > \"$1\"\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (ok, _, err) = rdy_env(
        dir.path(),
        script.to_str().unwrap(),
        &["edit", &id, "--view", "work"],
    );
    assert!(!ok);
    let marker = "your edit is saved at ";
    let at = err
        .find(marker)
        .unwrap_or_else(|| panic!("no recovery path: {err}"))
        + marker.len();
    let path = err[at..].split(':').next().unwrap().trim();
    let kept = std::fs::read_to_string(path).unwrap();
    assert!(kept.contains("type: [unclosed"), "{kept}");
    std::fs::remove_file(path).unwrap();
}
