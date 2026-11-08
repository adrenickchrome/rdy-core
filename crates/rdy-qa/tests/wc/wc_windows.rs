//! wx29a: the working copy on a native path and native text. A path typed the way the platform
//! writes it names the same logical doc, and a checkout whose files an editor or `core.autocrlf`
//! gave CR LF line ends commits, writes back and exports as the same LF text. Blob bytes stay as
//! they are. On Unix these run with `/` as the separator and prove the same contract.
//!
//! wx29b: a working copy the system refuses to change, or one with a link where a tracked file
//! or folder goes. A refusal costs no edit and no byte, and a link is never gone through.
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::platform;
use rdy_qa::skip::{Why, skip};
use rdy_qa::wc::*;
use rdy_store::fileid::FileId;
use serde_json::json;
use std::path::{MAIN_SEPARATOR, Path};

/// `guides<sep>intro.yaml` with the platform's own separator, and on Windows also the
/// backslash typed by hand.
fn native_forms() -> Vec<String> {
    let mut forms = vec![format!("guides{MAIN_SEPARATOR}intro.yaml")];
    if cfg!(windows) {
        forms.push("guides\\intro.yaml".to_string());
        forms.push("guides\\".to_string());
    }
    forms
}

#[test]
fn wx29a_backslash_native_paths_keep_logical_doc_names() {
    for form in native_forms() {
        let (s, seed) = seeded();
        ok(&s, &["checkout"]);
        set_body(&s, "guides/intro.yaml", &seed.prose, "Native.");
        ok(&s, &["add", &form]);
        let (okk, v) = json(&s, &["status"]);
        assert!(okk, "{form}: {v}");
        let staged = v["working_copy"]["staged"].as_array().unwrap();
        let paths: Vec<&str> = staged.iter().map(|e| e["path"].as_str().unwrap()).collect();
        assert_eq!(
            paths,
            vec!["guides/intro.yaml"],
            "{form}: the logical name has `/`, never the native separator: {v}"
        );
        let (okk, v) = json(&s, &["commit", "-m", "native"]);
        assert!(okk, "{form}: {v}");
        assert_eq!(
            store_block(&s, "work", &seed.prose).body,
            "Native.",
            "{form}"
        );
        let (_, v) = json(&s, &["status"]);
        for part in ["staged", "unstaged"] {
            assert!(
                v["working_copy"][part].as_array().unwrap().is_empty(),
                "{form}: {v}"
            );
        }
    }
}

#[test]
fn wx29a_crlf_checkout_commit_export_round_trips() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    // an editor or `core.autocrlf` rewrites the checked-out file with CR LF line ends
    set_body(&s, "guides/intro.yaml", &seed.prose, "Crlf edit.");
    let lf = read(&s, "guides/intro.yaml");
    assert!(!lf.contains('\r'), "rdy writes LF");
    std::fs::write(at(&s, "guides/intro.yaml"), lf.replace('\n', "\r\n")).unwrap();
    ok(&s, &["add", "guides/intro.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "crlf"]);
    assert!(okk, "{v}");
    assert_eq!(store_block(&s, "work", &seed.prose).body, "Crlf edit.");
    assert!(
        !read(&s, "guides/intro.yaml").contains('\r'),
        "the write-back is LF"
    );
    let (_, v) = json(&s, &["status"]);
    assert!(
        v["working_copy"]["unstaged"].as_array().unwrap().is_empty(),
        "{v}"
    );
    // the export holds the same LF text
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("md");
    ok(
        &s,
        &[
            "export",
            "--out",
            dest.to_str().unwrap(),
            "--format",
            "md",
            "--scope",
            "view:work",
        ],
    );
    let mut seen = false;
    let mut stack = vec![dest];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "md") {
                let t = std::fs::read(&p).unwrap();
                assert!(!t.contains(&b'\r'), "{p:?} has a carriage return");
                seen |= String::from_utf8_lossy(&t).contains("Crlf edit.");
            }
        }
    }
    assert!(seen, "the export shows the edit");
}

/// The bytes of `rels` in the project, or `None` where a file is gone. A comparison of two of
/// these is the promise that a refused operation changed nothing.
fn bytes_of(s: &Served, rels: &[&str]) -> Vec<(String, Option<Vec<u8>>)> {
    rels.iter()
        .map(|rel| (rel.to_string(), std::fs::read(at(s, rel)).ok()))
        .collect()
}

const WX29B: &str = "wx29b_acl_and_reparse_failures_leave_working_copy_intact";

/// Reading through a link gives its target's data, or the system's redirection guard refuses it
/// (`ERROR_UNTRUSTED_MOUNT_POINT`, 448: a standard user's link on the Windows 11 build box). Any
/// other error fails the test. Which of the two happens does not change what the test proves:
/// identity is asserted from the entry itself.
fn reads_through_or_448(through: &Path, want: &[u8]) {
    match std::fs::read(through) {
        Ok(bytes) => assert_eq!(bytes, want, "{}", through.display()),
        Err(e) => assert_eq!(e.raw_os_error(), Some(448), "{}: {e}", through.display()),
    }
}

/// The entry `link` is still a link, with an identity of its own.
fn still_a_link(link: &Path, target: &Path) {
    assert!(
        std::fs::symlink_metadata(link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "{} was replaced",
        link.display()
    );
    assert_ne!(
        FileId::lstat(link).unwrap(),
        FileId::lstat(target).unwrap(),
        "{}",
        link.display()
    );
}

/// A folder the account cannot write in: the update stops before it changes a file, and it
/// leaves no marker. Every file keeps its bytes, and the update finishes once the folder is
/// writable.
fn a_read_only_folder_stops_an_update_before_any_change() -> Option<()> {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "notes.yaml", &seed.note, "Kept edit.");
    store_write(
        &s,
        "work",
        json!([{"op": "create", "type": "doc", "parent": seed.folder, "fields": {"name": "zz", "title": "Zz", "doc_type": "page"}}]),
    );
    let tracked = ["notes.yaml", "guides/intro.yaml", "guides/_folder.yaml"];
    let before = bytes_of(&s, &tracked);
    let guard = read_only_dir(&at(&s, "guides"))?;
    let (okk, v) = json(&s, &["update"]);
    drop(guard);
    assert!(!okk && v["code"] == E_IO, "{v}");
    assert_eq!(bytes_of(&s, &tracked), before, "a refused update wrote");
    assert!(!at(&s, "guides/zz.yaml").exists());
    let (_, v) = json(&s, &["status"]);
    assert!(
        v["working_copy"]["interrupted"].is_null(),
        "no change was made, so nothing is interrupted: {v}"
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    assert!(at(&s, "guides/zz.yaml").exists());
    assert_eq!(
        entry(&s, "notes.yaml", &seed.note)["body"].as_str(),
        Some("Kept edit.")
    );
    Some(())
}

/// A root the account cannot write in: the update stops part way. Status says so, no edit is
/// lost, and `rdy update` finishes it with the store unchanged by the failure.
fn a_read_only_root_interrupts_an_update_that_can_finish() -> Option<()> {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "guides/intro.yaml", &seed.prose, "Kept edit.");
    store_write(
        &s,
        "work",
        json!([
            {"op": "update", "id": seed.claim, "body": "Theirs claim."},
            {"op": "update", "id": seed.note, "body": "Theirs."}
        ]),
    );
    let notes = read(&s, "notes.yaml");
    let guard = read_only_dir(s.dir.path())?;
    let (okk, v) = json(&s, &["update"]);
    drop(guard);
    assert!(!okk && v["code"] == E_IO, "{v}");
    assert_eq!(
        read(&s, "notes.yaml"),
        notes,
        "the refused file keeps its bytes"
    );
    let (okk, v) = json(&s, &["status"]);
    assert!(
        okk && v["working_copy"]["interrupted"]["code"] == E_WC_DIRTY,
        "{v}"
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    assert_eq!(
        entry(&s, "guides/intro.yaml", &seed.prose)["body"].as_str(),
        Some("Kept edit.")
    );
    assert_eq!(
        entry(&s, "notes.yaml", &seed.note)["body"].as_str(),
        Some("Theirs.")
    );
    Some(())
}

/// The working copy's own state cannot be written: a checkout changes nothing, and it works once
/// the directory is writable. The save after the files are written fails too: the checkout is
/// then interrupted, it is refused until forced, and forcing it finishes it.
fn a_read_only_state_directory_stops_or_interrupts_a_checkout() -> Option<()> {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    store_write(
        &s,
        "other",
        json!([{"op": "update", "id": seed.section, "fields": {"title": "Other start"}}]),
    );
    let tracked = ["notes.yaml", "guides/intro.yaml", "guides/_folder.yaml"];
    let before = bytes_of(&s, &tracked);
    let guard = read_only_dir(&at(&s, ".rdy/wc"))?;
    let (okk, v) = json(&s, &["checkout", "other"]);
    drop(guard);
    assert!(!okk && v["code"] == E_IO, "{v}");
    assert_eq!(bytes_of(&s, &tracked), before, "a refused checkout wrote");
    assert!(read(&s, ".rdy/wc/bound.yaml").contains("view: work"));
    let guard = read_only_dir(&at(&s, ".rdy/wc/stage"))?;
    let (okk, v) = json(&s, &["checkout", "other"]);
    drop(guard);
    assert!(!okk && v["code"] == E_IO, "{v}");
    let (okk, v) = json(&s, &["checkout"]);
    assert!(!okk && v["code"] == E_WC_DIRTY, "{v}");
    ok(&s, &["checkout", "other", "--force"]);
    assert!(read(&s, ".rdy/wc/bound.yaml").contains("view: other"));
    assert!(read(&s, "guides/intro.yaml").contains("Other start"));
    Some(())
}

/// A second name for a folder at a tracked folder's place (a junction on Windows, a symbolic link
/// on Unix) is never read, written or removed through. Taking the link away restores everything.
fn a_directory_link_at_a_tracked_folder_is_never_gone_through() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let outside = tempfile::tempdir().unwrap();
    let moved = outside.path().join("moved");
    std::fs::rename(at(&s, "guides"), &moved).unwrap();
    platform::dir_alias(&moved, &at(&s, "guides")).unwrap();
    still_a_link(&at(&s, "guides"), &moved);
    let folder = std::fs::read(moved.join("_folder.yaml")).unwrap();
    reads_through_or_448(&at(&s, "guides/_folder.yaml"), &folder);
    std::fs::write(moved.join("intro.yaml"), "not rdy's\n").unwrap();
    store_write(
        &s,
        "other",
        json!([{"op": "update", "id": seed.folder, "fields": {"name": "manuals"}}]),
    );
    let (okk, v) = json(&s, &["checkout", "other"]);
    assert!(!okk && v["code"] == E_WC_DIRTY, "{v}");
    let msg = v["message"].as_str().unwrap();
    assert!(
        msg.contains("guides/_folder.yaml") && msg.contains("guides/intro.yaml"),
        "{v}"
    );
    let (okk, v) = json(&s, &["checkout", "other", "--force"]);
    assert!(okk, "{v}");
    still_a_link(&at(&s, "guides"), &moved);
    assert_eq!(std::fs::read(moved.join("_folder.yaml")).unwrap(), folder);
    assert_eq!(
        std::fs::read(moved.join("intro.yaml")).unwrap(),
        b"not rdy's\n"
    );
    assert!(at(&s, "manuals/intro.yaml").is_file());
    platform::remove_dir_link(&at(&s, "guides")).unwrap();
    ok(&s, &["checkout", "other"]);
    assert_eq!(
        std::fs::read(moved.join("intro.yaml")).unwrap(),
        b"not rdy's\n"
    );
}

/// A link to a file at a tracked file's place: a checkout calls it dirty, an update does not
/// write through it, and the file it names keeps its bytes.
fn a_file_link_at_a_tracked_file_is_never_written_through() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("kept.yaml");
    std::fs::write(&target, "kept\n").unwrap();
    std::fs::remove_file(at(&s, "notes.yaml")).unwrap();
    platform::symlink(&target, &at(&s, "notes.yaml")).unwrap();
    still_a_link(&at(&s, "notes.yaml"), &target);
    reads_through_or_448(&at(&s, "notes.yaml"), b"kept\n");
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.note, "body": "Theirs."}]),
    );
    let (okk, v) = json(&s, &["checkout"]);
    assert!(!okk && v["code"] == E_WC_DIRTY, "{v}");
    assert!(v["message"].as_str().unwrap().contains("notes.yaml"), "{v}");
    let _ = json(&s, &["update"]);
    assert_eq!(std::fs::read(&target).unwrap(), b"kept\n");
    let (okk, v) = json(&s, &["checkout", "--force"]);
    assert!(okk, "{v}");
    assert_eq!(std::fs::read(&target).unwrap(), b"kept\n");
    assert!(
        std::fs::symlink_metadata(at(&s, "notes.yaml"))
            .unwrap()
            .is_file(),
        "a forced checkout puts the regular file back"
    );
    assert_eq!(
        entry(&s, "notes.yaml", &seed.note)["body"].as_str(),
        Some("Theirs.")
    );
}

/// A second name for a folder at `_blobs`: a blob is never written outside the working copy,
/// and a figure the store holds comes back once the link is gone.
fn a_directory_link_at_blobs_takes_no_blob() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::create_dir_all(at(&s, "guides/img")).unwrap();
    std::fs::write(at(&s, "guides/img/chart.png"), b"\x89PNG chart").unwrap();
    crate::wc_figures::add_figure(&s, &seed.section, "img/chart.png", "");
    ok(&s, &["commit", "-m", "a figure"]);
    std::fs::remove_dir_all(at(&s, "_blobs")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    platform::dir_alias(outside.path(), &at(&s, "_blobs")).unwrap();
    let _ = json(&s, &["checkout", "--force"]);
    assert_eq!(
        std::fs::read_dir(outside.path()).unwrap().count(),
        0,
        "a blob was written outside the working copy"
    );
    platform::remove_dir_link(&at(&s, "_blobs")).unwrap();
    ok(&s, &["checkout", "--force"]);
    let blobs: Vec<_> = std::fs::read_dir(at(&s, "_blobs"))
        .unwrap()
        .map(|e| std::fs::read(e.unwrap().path()).unwrap())
        .collect();
    assert_eq!(blobs, vec![b"\x89PNG chart".to_vec()]);
}

#[test]
fn wx29b_acl_and_reparse_failures_leave_working_copy_intact() {
    a_directory_link_at_a_tracked_folder_is_never_gone_through();
    a_file_link_at_a_tracked_file_is_never_written_through();
    a_directory_link_at_blobs_takes_no_blob();
    let refused = a_read_only_folder_stops_an_update_before_any_change()
        .and_then(|()| a_read_only_root_interrupts_an_update_that_can_finish())
        .and_then(|()| a_read_only_state_directory_stops_or_interrupts_a_checkout());
    if refused.is_none() {
        skip(WX29B, Why::Prerequisite, READ_ONLY_REASON);
    }
}

/// Names only Windows has. A name that is not Unicode stays an untracked file through every
/// command. A name the system refuses, and a device name, are paths that do not exist. A name the
/// system would store under another spelling is not taken for the tracked file. A device name
/// that the system now takes as a file never reaches the store.
#[cfg(windows)]
#[test]
fn wx29b_native_names_leave_working_copy_intact() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let tracked = ["notes.yaml", "guides/intro.yaml", "guides/_folder.yaml"];
    let before = bytes_of(&s, &tracked);
    let mut odd = platform::not_unicode("odd");
    odd.push(".yaml");
    for dir in ["", "guides"] {
        std::fs::write(at(&s, dir).join(&odd), "odd\n").unwrap();
    }
    let (okk, v) = json(&s, &["status"]);
    assert!(okk, "{v}");
    let untracked = v["working_copy"]["untracked"].as_array().unwrap();
    assert_eq!(untracked.len(), 2, "{v}");
    assert!(
        untracked
            .iter()
            .all(|u| u["reason"] == "the name is not UTF-8"),
        "{v}"
    );

    let mut refused: Vec<String> = platform::native_invalid_names()
        .iter()
        .map(|n| format!("{}.yaml", n.to_str().unwrap()))
        .collect();
    refused.push(platform::device_name().to_string());
    for rel in &refused {
        let (okk, v) = json(&s, &["add", rel]);
        assert!(!okk && v["code"] == E_NOT_FOUND, "{rel}: {v}");
        assert_eq!(v["message"], format!("{rel} does not exist"), "{v}");
    }
    // the two endings the system drops from a name
    for rel in ["notes.yaml.", "notes.yaml "] {
        let (okk, v) = json(&s, &["add", rel]);
        assert!(!okk && v["code"] == E_BAD_INPUT, "{rel:?}: {v}");
    }

    put(
        &s,
        "aux.yaml",
        "- type: doc\n  fields:\n    title: Aux\n    doc_type: page\n",
    );
    ok(&s, &["add", "aux.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "a device name"]);
    assert!(!okk && v["code"] == E_WC_INVALID, "{v}");
    assert_eq!(v["diagnostics"][0]["code"], E_BAD_NAME, "{v}");
    std::fs::remove_file(at(&s, "aux.yaml")).unwrap();

    assert_eq!(bytes_of(&s, &tracked), before);
    let (okk, v) = json(&s, &["checkout", "--force"]);
    assert!(okk, "{v}");
    for dir in ["", "guides"] {
        assert_eq!(std::fs::read(at(&s, dir).join(&odd)).unwrap(), b"odd\n");
    }
    // the working copy still works
    set_body(&s, "notes.yaml", &seed.note, "Still works.");
    ok(&s, &["add", "notes.yaml"]);
    ok(&s, &["commit", "-m", "with odd names beside it"]);
    assert_eq!(store_block(&s, "work", &seed.note).body, "Still works.");
}

/// The read-only attribute is what a copy from elsewhere often carries. The system refuses a
/// rename over such a file, though the directory is writable: an update fails and keeps the
/// file's bytes. It finishes once the attribute is off, and that takes away the temp file the
/// failed rename left, as every replacement does.
#[cfg(windows)]
#[test]
fn wx29b_a_read_only_attribute_keeps_the_file_until_it_is_cleared() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.note, "body": "Theirs."}]),
    );
    let path = at(&s, "notes.yaml");
    let before = std::fs::read(&path).unwrap();
    let set = |on: bool| {
        let mut perm = std::fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(on);
        std::fs::set_permissions(&path, perm).unwrap();
    };
    set(true);
    let (okk, v) = json(&s, &["update"]);
    assert!(!okk && v["code"] == E_IO, "{v}");
    assert!(
        v["message"].as_str().unwrap().contains("(os error 5)"),
        "{v}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    set(false);
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    assert_eq!(
        entry(&s, "notes.yaml", &seed.note)["body"].as_str(),
        Some("Theirs.")
    );
    assert!(!at(&s, ".notes.yaml.tmp").exists(), "a temp file stayed");
}

/// A reparse point that is no link, here a socket: the system calls it a file and refuses to open
/// it. It stays untracked and in place, and it stops no other file.
#[cfg(windows)]
#[test]
fn wx29b_a_reparse_point_of_no_link_is_untracked_and_kept() {
    use platform::{ReparseKind, reparse_kind};
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let sock = at(&s, "sock.yaml");
    platform::socket_entry(&sock).unwrap();
    let kind = reparse_kind(&sock).unwrap();
    assert!(matches!(kind, Some(ReparseKind::Other(_))), "{kind:?}");
    assert_eq!(std::fs::read(&sock).unwrap_err().raw_os_error(), Some(1920));
    let (okk, v) = json(&s, &["status"]);
    let untracked = &v["working_copy"]["untracked"];
    assert!(okk && untracked[0]["path"] == "sock.yaml", "{v}");
    let reason = untracked[0]["reason"].as_str().unwrap();
    assert!(reason.starts_with("cannot read the file"), "{v}");
    let (okk, v) = json(&s, &["add", "sock.yaml"]);
    assert!(!okk && v["code"] == E_WC_INVALID, "{v}");
    set_body(&s, "notes.yaml", &seed.note, "Beside a socket.");
    ok(&s, &["add", "notes.yaml"]);
    ok(&s, &["commit", "-m", "beside a socket"]);
    assert_eq!(store_block(&s, "work", &seed.note).body, "Beside a socket.");
    let (okk, v) = json(&s, &["checkout", "--force"]);
    assert!(okk, "{v}");
    assert_eq!(reparse_kind(&sock).unwrap(), kind, "the entry is kept");
}

/// A copy of a tree keeps a link as a link with the same target, for a directory and for a file,
/// and it never goes through either. A directory link stays one though following it can be
/// refused (448).
#[test]
fn wx29b_a_copied_tree_keeps_links_as_links() {
    let t = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let kept = outside.path().join("kept.txt");
    std::fs::write(&kept, "kept\n").unwrap();
    let from = t.path().join("from");
    std::fs::create_dir(&from).unwrap();
    std::fs::write(from.join("plain.yaml"), "plain\r\n").unwrap();
    platform::symlink(outside.path(), &from.join("dirlink")).unwrap();
    platform::symlink(&kept, &from.join("filelink")).unwrap();
    let to = t.path().join("to");
    copy_tree(&from, &to).unwrap();
    assert_eq!(std::fs::read(to.join("plain.yaml")).unwrap(), b"plain\r\n");
    for name in ["dirlink", "filelink"] {
        let (old, new) = (from.join(name), to.join(name));
        assert_eq!(
            std::fs::read_link(&new).unwrap(),
            std::fs::read_link(&old).unwrap(),
            "{name}"
        );
        still_a_link(&new, &old);
    }
    reads_through_or_448(&to.join("filelink"), b"kept\n");
    reads_through_or_448(&to.join("dirlink").join("kept.txt"), b"kept\n");
    #[cfg(windows)]
    {
        use platform::{ReparseKind, reparse_kind};
        for name in ["dirlink", "filelink"] {
            let kind = reparse_kind(&to.join(name)).unwrap();
            assert_eq!(kind, Some(ReparseKind::Symlink), "{name}");
        }
        use std::os::windows::fs::FileTypeExt;
        let meta = std::fs::symlink_metadata(to.join("dirlink")).unwrap();
        assert!(
            meta.file_type().is_symlink_dir(),
            "a directory link stays one"
        );
    }
}
