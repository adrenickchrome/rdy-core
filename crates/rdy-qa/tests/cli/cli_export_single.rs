//! rfr3 (D-ESHXM, D-6PR3Y, D-H1FRY): `rdy export --format md --layout single --out <file>
//! [--assets <dir>]` prints `exported 1 doc as md to <path> (<n> bytes)` and one `warning:` line
//! per warning, with the prefix once; `--json` gives `{out, format, layout, docs, folders,
//! assets, pages, bytes, warnings}`. Its refusals keep their words, and the existing layout and
//! format refusals are unchanged; `--assets` applies to the single file only.
use rdy_engine::Rdy;
use rdy_model::BlockId;
use rdy_qa::daemon::{Served, short_tempdir};
use rdy_qa::fixtures::{land, write};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// A served project: doc `Guide` with a figure, a section that refs a block of its collapsed
/// section, and doc `Other`; profile `onep` selects the guide.
pub(crate) fn served() -> (Served, BTreeMap<String, BlockId>) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let blob = r
        .store_mut()
        .put_blob(b"\x89PNG\r\n\x1a\nsingle image bytes")
        .unwrap();
    let w = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$fold", "parent": "$g", "fields": {"title": "Folded", "collapsed": true}},
            {"op": "create", "type": "prose", "id": "$hid", "parent": "$fold", "body": "Folded words."},
            {"op": "create", "type": "section", "id": "$s", "parent": "$g", "fields": {"title": "Use"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "See {{$hid|the fold}}, {{$fold}} and {{$s}}."},
            {"op": "create", "type": "figure", "id": "$f", "parent": "$s", "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "A picture"}},
            {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "Other", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$o", "body": "Other words."}
        ]),
    )
    .unwrap();
    land(&mut r, "w1").unwrap();
    let ids: BTreeMap<String, BlockId> = w
        .created
        .iter()
        .map(|(k, v)| (k.clone(), BlockId::parse(v).unwrap()))
        .collect();
    let p = write(
        &mut r,
        "w2",
        json!([{"op": "create", "type": "publication_profile", "parent": ids["$o"].to_string(),
            "fields": {"key": "onep", "title": "One", "audience": "org", "select": [ids["$g"].to_string()]}}]),
    )
    .unwrap();
    land(&mut r, "w2").unwrap();
    drop(p);
    r.close().unwrap();
    (s, ids)
}

/// Every entry under `dir`, by its path relative to it, with a file's bytes.
pub(crate) fn entries(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let rel = p
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if std::fs::symlink_metadata(&p).unwrap().is_dir() {
                stack.push(p.clone());
                out.insert(rel + "/", None);
            } else {
                out.insert(rel, std::fs::read(&p).ok());
            }
        }
    }
    out
}

fn warning(ids: &BTreeMap<String, BlockId>, target: &str) -> String {
    format!(
        "warning: guide: {}: the ref to {} is not in this file; it is written as its label",
        ids["$p"], ids[target]
    )
}

#[test]
fn rfr3_cli_single_has_exact_text_json_and_errors() {
    let (s, ids) = served();
    let scratch = short_tempdir();
    let base = scratch.path();
    let path = |n: &str| base.join(n).to_str().unwrap().to_string();

    // JSON: the result's keys and values
    let a = path("a.md");
    let (ok, v) = s.json(&[
        "--json",
        "export",
        "--out",
        &a,
        "--format",
        "md",
        "--layout",
        "single",
        "--profile",
        "onep",
        "--assets",
        "img",
    ]);
    assert!(ok, "{v}");
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "assets", "bytes", "docs", "folders", "format", "layout", "out", "pages", "warnings"
        ]
    );
    let file = std::fs::read(&a).unwrap();
    assert_eq!(
        v,
        json!({"out": a, "format": "md", "layout": "single", "docs": 1, "folders": 0,
            "assets": 1, "pages": null, "bytes": file.len(),
            "warnings": [warning(&ids, "$hid"), warning(&ids, "$fold")]})
    );
    // the file and its one image
    let text = String::from_utf8(file).unwrap();
    assert!(text.starts_with("# Guide\n"), "{text}");
    assert!(
        text.contains("See the fold, Folded and [Use](#use)."),
        "{text}"
    );
    assert!(text.contains("![A picture](img/"), "{text}");
    assert_eq!(
        entries(base)
            .keys()
            .filter(|k| k.starts_with("img/"))
            .count(),
        2,
        "the folder and its image"
    );

    // text: one line, then each warning with its prefix once, in the reply's order
    let b = path("b.md");
    let out = s.rdy(&[
        "export",
        "--out",
        &b,
        "--format",
        "md",
        "--layout",
        "single",
        "--profile",
        "onep",
        "--assets",
        "img2",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let n = std::fs::metadata(&b).unwrap().len();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim_end(),
        format!(
            "exported 1 doc as md to {b} ({n} bytes)\n{}\n{}",
            warning(&ids, "$hid"),
            warning(&ids, "$fold")
        )
    );

    // the refusals: code, message and fix, and nothing written
    let before = entries(base);
    let refuse = |args: &[&str], code: &str, message: &str, fix: &str| {
        let mut all = vec!["--json", "export"];
        all.extend_from_slice(args);
        let (ok, v) = s.json(&all);
        assert!(!ok, "{args:?}: {v}");
        assert_eq!(
            (&v["code"], &v["message"], &v["fix"]),
            (&json!(code), &json!(message), &json!(fix)),
            "{args:?}"
        );
        assert_eq!(entries(base), before, "{args:?}: nothing written");
        // the text reply names the same message and fix
        let mut all = vec!["export"];
        all.extend_from_slice(args);
        let out = s.rdy(&all);
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains(message) && err.contains(fix),
            "{args:?}: {err}"
        );
    };
    let c = path("c.md");
    let single = |more: &[&'static str]| -> Vec<String> {
        let mut v: Vec<String> = ["--out", &c, "--format", "md", "--layout", "single"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        v.extend(more.iter().map(|s| s.to_string()));
        v
    };
    let run = |more: &[&'static str], code: &str, message: &str, fix: &str| {
        let args = single(more);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        refuse(&args, code, message, fix);
    };
    // the selection holds two docs
    run(
        &["--assets", "img3"],
        "E_BAD_INPUT",
        "a single-file Markdown export takes one doc; the selection holds 2",
        "choose a profile that selects one doc",
    );
    // a figure with no assets folder
    run(
        &["--profile", "onep"],
        "E_BAD_INPUT",
        &format!("figure {} needs --assets <dir>", ids["$f"]),
        "pass --assets with a folder beside the file",
    );
    // every D-6PR3Y path refusal
    std::fs::write(base.join("afile"), b"f").unwrap();
    std::fs::create_dir(base.join("full")).unwrap();
    std::fs::write(base.join("full/x"), b"x").unwrap();
    let before_paths = entries(base);
    let refuse_paths = |assets: &'static str, message: String, fix: &str| {
        let args = single(&["--profile", "onep", "--assets", assets]);
        let mut all = vec!["--json".to_string(), "export".to_string()];
        all.extend(args);
        let all: Vec<&str> = all.iter().map(String::as_str).collect();
        let (ok, v) = s.json(&all);
        assert!(!ok, "{assets}: {v}");
        assert_eq!(
            (&v["code"], &v["message"], &v["fix"]),
            (&json!("E_BAD_INPUT"), &json!(message), &json!(fix)),
            "{assets}"
        );
        assert_eq!(entries(base), before_paths, "{assets}: nothing written");
    };
    for bad in [
        "/abs", "..", "../x", "a/../b", ".", "c.md", "c.md/x", "afile/x",
    ] {
        refuse_paths(
            bad,
            format!("--assets {bad} must be a folder below the folder of --out"),
            "pass a relative folder path with no .. part, such as assets",
        );
    }
    refuse_paths(
        "afile",
        "afile is a file: --assets names a folder".into(),
        "pass a folder path such as assets",
    );
    refuse_paths(
        "full",
        "full is not empty".into(),
        "choose an empty directory (or one that does not exist yet) so no stale files mix into the export",
    );
    // an output that is there
    let (ok, v) = s.json(&[
        "--json",
        "export",
        "--out",
        &a,
        "--format",
        "md",
        "--layout",
        "single",
        "--profile",
        "onep",
        "--assets",
        "img4",
    ]);
    assert!(!ok);
    assert_eq!(
        (&v["code"], &v["message"], &v["fix"]),
        (
            &json!("E_ALREADY_EXISTS"),
            &json!(format!("{a} exists; rdy export never overwrites a file")),
            &json!("pick another path, or remove the file")
        )
    );
    assert!(!base.join("img4").exists());

    // the layout refusal of a file format is unchanged and comes first; then D-H1FRY
    let before = entries(base);
    for format in ["pdf", "html"] {
        let o = path(&format!("l.{format}"));
        for layout in ["single", "mkdocs"] {
            let (ok, v) = s.json(&[
                "--json", "export", "--out", &o, "--format", format, "--layout", layout,
                "--assets", "x",
            ]);
            assert!(!ok);
            assert_eq!(
                (&v["code"], &v["message"], &v["fix"]),
                (
                    &json!("E_BAD_INPUT"),
                    &json!(format!("--layout {layout} applies to the md format only")),
                    &json!("drop --layout, or export with --format md")
                ),
                "{format} {layout}"
            );
        }
    }
    for args in [
        vec!["--format", "pdf"],
        vec!["--format", "html"],
        vec!["--format", "md"],
        vec!["--format", "md", "--layout", "mkdocs"],
        vec!["--format", "md", "--layout", "techdocs"],
    ] {
        let o = path("h.out");
        let mut all = vec!["--json", "export", "--out", o.as_str(), "--assets", "x"];
        all.extend(args.iter());
        let (ok, v) = s.json(&all);
        assert!(!ok, "{args:?}");
        assert_eq!(
            (&v["code"], &v["message"], &v["fix"]),
            (
                &json!("E_BAD_INPUT"),
                &json!("--assets applies to --format md --layout single only"),
                &json!("drop --assets, or export with --format md --layout single")
            ),
            "{args:?}"
        );
    }
    assert_eq!(entries(base), before);
    // a layout outside the list is refused by the parser
    let out = s.rdy(&[
        "export",
        "--out",
        &path("z.md"),
        "--format",
        "md",
        "--layout",
        "one",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("single"));

    // the other replies gain no layout
    let (ok, v) = s.json(&["--json", "export", "--out", &path("tree"), "--format", "md"]);
    assert!(ok, "{v}");
    assert!(v.get("layout").is_none(), "{v}");
    let _: Value = v;
}
