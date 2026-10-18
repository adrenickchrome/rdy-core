//! pub10 (D-5CVWB, D-0VCQ5, D-KQ4CA, D-WDKWQ): `rdy render --profile` and `rdy export --profile`
//! and the chat tools `rdy_render` and `rdy_export` write one local output from the profile's
//! selected view. The same request gives the same files by either path; a profile that cannot be
//! resolved never falls back to the full source (RULING F2); a local output records nothing and
//! authorizes nothing (D-WDKWQ). Without a profile the outputs keep their existing behaviour.
use rdy_engine::Rdy;
use rdy_qa::daemon::{Served, short_tempdir};
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;

/// The ids the seed made, by name.
pub(crate) struct Seed {
    /// The id of the selected doc `chosen` (a doc, not a profile).
    pub chosen: String,
}

/// A folder `area` with a doc `chosen` that refers to the doc `outdoc` outside it, a folder
/// `pubdocs` with the clean doc `clean`, and the org profile `orgp` (selects `area`), the public
/// profile `openp` (selects `pubdocs`, clean) and the public profile `blockedp` (selects `area`,
/// which refers outside). All three profiles are tagged `v1.0.0`; the org profile `late` and a
/// later edit of `clean` come after the tag, and a draft view `draftv` holds an unlanded edit.
pub(crate) fn seed(dir: &Path) -> Seed {
    let mut r = Rdy::open(dir).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area", "title": "Area"}},
            {"op": "create", "type": "doc", "id": "$chosen", "parent": "$area", "fields": {"name": "chosen", "title": "Chosen page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$cs", "parent": "$chosen", "fields": {"title": "Chosen section"}},
            {"op": "create", "type": "prose", "parent": "$cs", "body": "Chosen prose words."},
            {"op": "create", "type": "term", "parent": "$cs", "fields": {"name": "Chosen term"}, "body": "Chosen term body."},
            {"op": "create", "type": "question", "parent": "$cs", "fields": {"prompt": "Chosen question prompt?"}},
            {"op": "create", "type": "folder", "id": "$pubdocs", "fields": {"name": "pubdocs", "title": "Public docs"}},
            {"op": "create", "type": "doc", "id": "$clean", "parent": "$pubdocs", "fields": {"name": "clean", "title": "Clean page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$clean", "body": "Clean prose words."},
            {"op": "create", "type": "doc", "id": "$out", "fields": {"name": "outdoc", "title": "OUTCANARY doc", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$out", "body": "OUTCANARY words."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let id = |k: &str| w.created[k].clone();
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": id("$cs"), "body": format!("See {{{{{}}}}}.", id("$out"))},
            {"op": "create", "type": "publication_profile", "parent": id("$admin"),
             "fields": {"key": "orgp", "title": "Title of orgp", "description": "Description of orgp.", "audience": "org", "select": [id("$area")]}},
            {"op": "create", "type": "publication_profile", "parent": id("$admin"),
             "fields": {"key": "openp", "title": "Title of openp", "description": "Description of openp.", "audience": "public", "select": [id("$pubdocs")]}},
            {"op": "create", "type": "publication_profile", "parent": id("$admin"),
             "fields": {"key": "blockedp", "title": "Title of blockedp", "description": "Description of blockedp.", "audience": "public", "select": [id("$area")]}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": id("$clean"), "body": "Clean later words."},
            {"op": "create", "type": "publication_profile", "parent": id("$admin"),
             "fields": {"key": "late", "title": "Title of late", "audience": "org", "select": [id("$pubdocs")]}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    write(
        &mut r,
        "draftv",
        json!([{"op": "create", "type": "prose", "parent": id("$clean"), "body": "DRAFTCANARY words."}]),
    )
    .unwrap();
    r.close().unwrap();
    Seed {
        chosen: id("$chosen"),
    }
}

pub(crate) fn chat(s: &Served) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:p", "run": "p", "role": "writer"}),
    )
    .ok();
    c
}

/// Every file under `p` (or `p` itself, when it is a file), by its path relative to `p`.
pub(crate) fn files_of(p: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    if p.is_file() {
        out.insert(String::new(), std::fs::read(p).unwrap());
        return out;
    }
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let f = e.unwrap().path();
            if f.is_dir() {
                stack.push(f);
            } else {
                let rel = f
                    .strip_prefix(p)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&f).unwrap());
            }
        }
    }
    out
}

/// Every entry (file or directory) directly or deeply under `dir`, relative, sorted.
fn entries(dir: &Path) -> Vec<String> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p.clone());
            }
            out.push(p.strip_prefix(dir).unwrap().to_string_lossy().into_owned());
        }
    }
    out.sort();
    out
}

fn without_out(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.remove("out");
    }
    v
}

fn arg(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// The code, message and fix of a tool error.
fn refused(r: Reply) -> (String, String, String) {
    let Reply::ToolError(e) = r else {
        panic!("expected a tool error, got {r:?}")
    };
    let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

/// A fingerprint of the log, the refs and every file outside `.rdy` (the index and the run
/// directory are not the record).
fn fingerprint(dir: &Path) -> BTreeMap<String, u64> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, u64>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let path = e.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .to_string();
            if rel == ".rdy/run" || rel.starts_with(".rdy/run/") || rel.contains("index.redb") {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                let mut h = DefaultHasher::new();
                bytes.hash(&mut h);
                out.insert(rel, h.finish());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    assert!(
        out.keys().any(|k| k.starts_with(".rdy/store/segments/")),
        "the log is in the print: {out:?}"
    );
    out
}

/// The four print outputs: (format, layout, name).
const PRINTS: [(&str, Option<&str>, &str); 4] = [
    ("pdf", None, "p.pdf"),
    ("html", None, "p.html"),
    ("md", None, "md"),
    ("md", Some("mkdocs"), "mk"),
];

#[test]
fn pub10_render_and_export_profile_match_between_chat_and_cli() {
    let s = Served::new();
    seed(s.dir.path());
    let mut c = chat(&s);
    let scratch = short_tempdir();
    for scope in ["canon", "tag:v1.0.0"] {
        for profile in ["orgp", "openp"] {
            let tag = format!("{profile}-{}", scope.replace(':', "-"));
            // the static site
            let (cli_dir, tool_dir) = (
                scratch.path().join(format!("cli-site-{tag}")),
                scratch.path().join(format!("mcp-site-{tag}")),
            );
            let (ok, cv) = s.json(&[
                "--json",
                "render",
                "--profile",
                profile,
                "--scope",
                scope,
                "--out",
                arg(&cli_dir),
            ]);
            assert!(ok, "{tag}: {cv}");
            let tv = c
                .call(
                    "rdy_render",
                    json!({"out": arg(&tool_dir), "profile": profile, "scope": scope}),
                )
                .ok();
            assert_eq!(cv, tv, "{tag}: the same render summary");
            let (a, b) = (files_of(&cli_dir), files_of(&tool_dir));
            assert!(a.contains_key("index.html"), "{tag}: {:?}", a.keys());
            assert!(a == b, "{tag}: the CLI and the tool write the same site");

            // every print format
            for (format, layout, name) in PRINTS {
                let (cli, tool) = (
                    scratch.path().join(format!("cli-{tag}-{name}")),
                    scratch.path().join(format!("mcp-{tag}-{name}")),
                );
                let mut args = vec![
                    "--json",
                    "export",
                    "--profile",
                    profile,
                    "--scope",
                    scope,
                    "--format",
                    format,
                    "--out",
                    arg(&cli),
                ];
                let mut chat_args = json!({"out": arg(&tool), "format": format, "profile": profile, "scope": scope});
                if let Some(l) = layout {
                    args.extend(["--layout", l]);
                    chat_args["layout"] = json!(l);
                }
                let (ok, cv) = s.json(&args);
                assert!(ok, "{tag} {name}: {cv}");
                let tv = c.call("rdy_export", chat_args).ok();
                assert_eq!(without_out(cv), without_out(tv), "{tag} {name}");
                let (a, b) = (files_of(&cli), files_of(&tool));
                assert!(!a.is_empty(), "{tag} {name}");
                assert!(a == b, "{tag} {name}: the same bytes by either path");
            }
        }
    }
}

/// The org profile's warning comes once in each reply and once in the CLI text, with the words
/// the existing warning formatting gives.
#[test]
fn pub10_org_warning_is_returned_exactly_once() {
    let s = Served::new();
    seed(s.dir.path());
    let mut c = chat(&s);
    let scratch = short_tempdir();
    let cli = scratch.path().join("cli");
    let (ok, cv) = s.json(&["--json", "render", "--profile", "orgp", "--out", arg(&cli)]);
    assert!(ok, "{cv}");
    let list = cv["warnings"].as_array().expect("warnings");
    assert_eq!(list.len(), 1, "{cv}");
    let word = list[0].as_str().unwrap();
    assert!(word.starts_with("Org profile has "), "{word}");
    let tv = c
        .call(
            "rdy_render",
            json!({"out": arg(&scratch.path().join("tool")), "profile": "orgp"}),
        )
        .ok();
    assert_eq!(tv["warnings"], cv["warnings"]);
    // the text line is printed once
    let text = String::from_utf8(
        s.rdy(&[
            "render",
            "--profile",
            "orgp",
            "--out",
            arg(&scratch.path().join("text")),
        ])
        .stdout,
    )
    .unwrap();
    assert_eq!(text.matches(word).count(), 1, "{text}");
    for (format, _, name) in PRINTS.iter().take(3) {
        let out = scratch.path().join(format!("w-{name}"));
        let (ok, v) = s.json(&[
            "--json",
            "export",
            "--profile",
            "orgp",
            "--format",
            format,
            "--out",
            arg(&out),
        ]);
        assert!(ok, "{v}");
        assert_eq!(v["warnings"].as_array().unwrap().len(), 1, "{format}: {v}");
        assert_eq!(v["warnings"][0], word, "{format}");
    }
}

#[test]
fn pub10_profile_never_falls_back_to_full_source() {
    let s = Served::new();
    let seeded = seed(s.dir.path());
    let mut c = chat(&s);
    let scratch = short_tempdir();
    // (profile, scope): a misspelt id and key, an empty name, a doc where a profile belongs, a
    // profile that did not exist at the tag, and scopes a profile does not take
    let cases: Vec<(String, &str)> = vec![
        ("orgq".into(), "canon"),
        ("p-ZZZZZ".into(), "canon"),
        (String::new(), "canon"),
        (seeded.chosen.clone(), "canon"),
        ("late".into(), "tag:v1.0.0"),
        ("orgp".into(), "tag:v9.9.9"),
        ("orgp".into(), "view:draftv"),
        ("orgp".into(), "not-a-scope"),
        ("orgp".into(), "inflight"),
    ];
    let before = entries(scratch.path());
    for (i, (profile, scope)) in cases.iter().enumerate() {
        let what = format!("{profile:?} at {scope}");
        let site = scratch.path().join(format!("site-{i}"));
        let (ok, cv) = s.json(&[
            "--json",
            "render",
            "--profile",
            profile,
            "--scope",
            scope,
            "--out",
            arg(&site),
        ]);
        assert!(!ok, "render {what}: {cv}");
        let (code, message, fix) = refused(c.call(
            "rdy_render",
            json!({"out": arg(&site), "profile": profile, "scope": scope}),
        ));
        assert_eq!(cv["code"], code.as_str(), "render {what}");
        assert_eq!(cv["message"], message.as_str(), "render {what}");
        assert_eq!(cv["fix"], fix.as_str(), "render {what}");
        assert!(code.starts_with("E_"), "{what}: {code}");
        for format in ["pdf", "html", "md"] {
            let out = scratch.path().join(format!("print-{i}-{format}"));
            let (ok, cv) = s.json(&[
                "--json",
                "export",
                "--profile",
                profile,
                "--scope",
                scope,
                "--format",
                format,
                "--out",
                arg(&out),
            ]);
            assert!(!ok, "export {format} {what}: {cv}");
            let (code, message, fix) = refused(c.call(
                "rdy_export",
                json!({"out": arg(&out), "format": format, "profile": profile, "scope": scope}),
            ));
            assert_eq!(cv["code"], code.as_str(), "export {format} {what}");
            assert_eq!(cv["message"], message.as_str(), "export {format} {what}");
            assert_eq!(cv["fix"], fix.as_str(), "export {format} {what}");
        }
    }
    // not one refusal wrote a file, a directory or a staging leftover
    assert_eq!(entries(scratch.path()), before);
    // the blocked public profile refuses the same way, and the full source is not the answer
    let blocked = scratch.path().join("blocked");
    let (ok, cv) = s.json(&[
        "--json",
        "render",
        "--profile",
        "blockedp",
        "--out",
        arg(&blocked),
    ]);
    assert!(!ok, "{cv}");
    assert_eq!(cv["code"], "E_PUBLICATION_REFERENCE", "{cv}");
    assert!(!blocked.exists());
    let (code, ..) = refused(c.call(
        "rdy_render",
        json!({"out": arg(&blocked), "profile": "blockedp"}),
    ));
    assert_eq!(code, "E_PUBLICATION_REFERENCE");
    assert_eq!(entries(scratch.path()), before);
}

#[test]
fn pub10_local_output_has_no_publication_side_effect() {
    let s = Served::new();
    seed(s.dir.path());
    let mut c = chat(&s);
    // warm the daemon first: starting it is not an output
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok, "{v}");
    let scratch = short_tempdir();
    let before = fingerprint(s.dir.path());
    let config = std::fs::read(s.dir.path().join(".rdy.yaml")).unwrap();

    let site = scratch.path().join("site");
    let (ok, cv) = s.json(&["--json", "render", "--profile", "orgp", "--out", arg(&site)]);
    assert!(ok, "{cv}");
    // the summary keeps its existing keys: no record, receipt or release list
    let keys: Vec<&str> = cv.as_object().unwrap().keys().map(String::as_str).collect();
    assert!(
        keys.iter()
            .all(|k| ["pages", "assets", "redirects", "warnings"].contains(k)),
        "{keys:?}"
    );
    let tv = c
        .call(
            "rdy_render",
            json!({"out": arg(&scratch.path().join("site2")), "profile": "orgp"}),
        )
        .ok();
    assert_eq!(cv, tv);
    for (format, layout, name) in PRINTS {
        let out = scratch.path().join(name);
        let mut args = vec![
            "--json",
            "export",
            "--profile",
            "orgp",
            "--format",
            format,
            "--out",
            arg(&out),
        ];
        if let Some(l) = layout {
            args.extend(["--layout", l]);
        }
        let (ok, v) = s.json(&args);
        assert!(ok, "{name}: {v}");
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert!(
            keys.iter().all(|k| [
                "out", "format", "docs", "folders", "assets", "pages", "bytes", "warnings"
            ]
            .contains(k)),
            "{name}: {keys:?}"
        );
    }

    // refs, log and config are as they were; the project holds no new file
    assert_eq!(fingerprint(s.dir.path()), before, "refs and log");
    assert_eq!(
        std::fs::read(s.dir.path().join(".rdy.yaml")).unwrap(),
        config
    );
    // only the requested outputs appeared (no staging directory is left beside them)
    let mut want = vec!["md", "mk", "p.html", "p.pdf", "site", "site2"];
    let names: Vec<String> = std::fs::read_dir(scratch.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let mut names = names;
    names.sort();
    want.sort();
    assert_eq!(names, want);
    // the project directory holds the files of the seed and the store, nothing else
    let in_project: Vec<String> = fingerprint(s.dir.path())
        .into_keys()
        .filter(|k| !k.starts_with(".rdy/"))
        .collect();
    assert_eq!(in_project, vec![".rdy.yaml".to_string()], "{in_project:?}");

    // no call to action: no approval, answer, assignment or queue control on any page or agent
    // file, and the ordinary question prose remains
    let files = files_of(&site);
    let mut pages = 0;
    for (path, bytes) in &files {
        if !(path.ends_with(".html")
            || path.ends_with(".txt")
            || path.ends_with("search-index.json"))
        {
            continue;
        }
        pages += 1;
        let text = String::from_utf8_lossy(bytes).to_lowercase();
        for word in [
            "approve",
            "rdy_answer",
            "assign",
            "queue",
            "<form",
            "<textarea",
        ] {
            assert!(!text.contains(word), "{path}: {word}");
        }
    }
    assert!(pages > 3, "{:?}", files.keys());
    let questions = String::from_utf8(files["questions.html"].clone()).unwrap();
    assert!(questions.contains("Chosen question prompt?"), "{questions}");
}

#[test]
fn pub10_unprofiled_outputs_keep_their_existing_behavior() {
    let s = Served::new();
    seed(s.dir.path());
    let mut c = chat(&s);
    let scratch = short_tempdir();
    // two stored profiles exist; with none asked for, canon is the full source
    let site = scratch.path().join("site");
    let (ok, cv) = s.json(&["--json", "render", "--out", arg(&site)]);
    assert!(ok, "{cv}");
    let tv = c
        .call(
            "rdy_render",
            json!({"out": arg(&scratch.path().join("site-chat"))}),
        )
        .ok();
    assert_eq!(cv, tv, "the same summary by either path");
    let all = files_of(&site);
    assert!(all == files_of(&scratch.path().join("site-chat")));
    let text = |files: &BTreeMap<String, Vec<u8>>, word: &str| {
        files
            .iter()
            .any(|(p, b)| p.contains(word) || String::from_utf8_lossy(b).contains(word))
    };
    // the whole source: the doc no profile selects, and the admin doc that holds the profiles
    for word in [
        "OUTCANARY",
        "Admin page",
        "Chosen prose words.",
        "Clean later words.",
    ] {
        assert!(text(&all, word), "{word}");
    }
    assert!(!text(&all, "DRAFTCANARY"), "a draft is not canon");

    // a version and a draft view read as they did
    let tagged = scratch.path().join("tagged");
    let (ok, v) = s.json(&[
        "--json",
        "render",
        "--scope",
        "tag:v1.0.0",
        "--out",
        arg(&tagged),
    ]);
    assert!(ok, "{v}");
    let tagged = files_of(&tagged);
    assert!(text(&tagged, "OUTCANARY") && !text(&tagged, "Clean later words."));
    let draft = scratch.path().join("draft");
    let (ok, v) = s.json(&[
        "--json",
        "render",
        "--scope",
        "view:draftv",
        "--out",
        arg(&draft),
    ]);
    assert!(ok, "{v}");
    let draft_tool = scratch.path().join("draft-chat");
    let tv = c
        .call(
            "rdy_render",
            json!({"out": arg(&draft_tool), "scope": "view:draftv"}),
        )
        .ok();
    assert_eq!(v, tv);
    let (draft_files, tool_files) = (files_of(&draft), files_of(&draft_tool));
    assert!(text(&draft_files, "DRAFTCANARY") && text(&draft_files, "OUTCANARY"));
    assert!(draft_files == tool_files);

    // every print format keeps the full source and matches between the paths
    for (format, layout, name) in PRINTS {
        let (cli, tool) = (
            scratch.path().join(format!("cli-{name}")),
            scratch.path().join(format!("mcp-{name}")),
        );
        let mut args = vec!["--json", "export", "--format", format, "--out", arg(&cli)];
        let mut chat_args = json!({"out": arg(&tool), "format": format});
        if let Some(l) = layout {
            args.extend(["--layout", l]);
            chat_args["layout"] = json!(l);
        }
        let (ok, cv) = s.json(&args);
        assert!(ok, "{name}: {cv}");
        let tv = c.call("rdy_export", chat_args).ok();
        assert_eq!(without_out(cv), without_out(tv), "{name}");
        let (a, b) = (files_of(&cli), files_of(&tool));
        assert!(a == b, "{name}");
        if format != "pdf" {
            assert!(text(&a, "OUTCANARY"), "{name}: the full source");
        }
    }
    // the text of both commands is the one they had
    let out = scratch.path().join("text-site");
    let t = String::from_utf8(s.rdy(&["render", "--out", arg(&out)]).stdout).unwrap();
    assert!(
        t.starts_with("rendered ") && t.contains(" pages, ") && t.contains(" permalinks and "),
        "{t}"
    );
}
