//! wx29a: the working copy on a native path and native text. A path typed the way the platform
//! writes it names the same logical doc, and a checkout whose files an editor or `core.autocrlf`
//! gave CR LF line ends commits, writes back and exports as the same LF text. Blob bytes stay as
//! they are. On Unix these run with `/` as the separator and prove the same contract.
use rdy_qa::wc::*;
use std::path::MAIN_SEPARATOR;

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
