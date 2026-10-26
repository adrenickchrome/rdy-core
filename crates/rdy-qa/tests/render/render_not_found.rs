//! dgm10 (D-94BT2, D-E8P8J; system/site n-REYZ9, n-3S9SC, tb-QCTS0; reference/cli tb-JYKPD):
//! `rdy render` writes the site's own not-found page, `404.html`, rooted at the project's
//! `site.base_path`, for the full source and for `--profile`. Its summary counts no extra page.
//! An authored top-level doc named 404 keeps its page, and the render's last warning line says so.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

/// The generated page's marker on its html element.
const MARKER: &str = "<html lang=\"en\" data-rdy-kind=\"not-found\">";
const KEPT: &str = "the top-level doc named 404 has the page 404.html: the site's not-found page is left out. Rename the doc to publish it.";

/// A project named `Lost project`, with `site.base_path` = `base`, holding `ops`, landed; its
/// store closed.
fn seeded(
    base: &str,
    ops: Value,
) -> (
    tempfile::TempDir,
    std::collections::BTreeMap<String, String>,
) {
    let (dir, r) = project();
    drop(r);
    std::fs::write(
        dir.path().join(".rdy.yaml"),
        format!("pack: rdy-standard\nname: \"Lost project\"\nsite:\n  base_path: {base}\n"),
    )
    .unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let ids = write(&mut r, "work", ops).unwrap().created;
    land(&mut r, "work").unwrap();
    (dir, ids.into_iter().collect())
}

/// `rdy render` of `project` into `out`, with `extra` arguments: success, stdout, stderr.
fn render(project: &Path, out: &Path, extra: &[&str]) -> (bool, String, String) {
    let (_cfg, env) = human_env("human:qa");
    let mut args = vec![
        "--no-daemon",
        "--project",
        project.to_str().unwrap(),
        "render",
        "--out",
        out.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    let o = Command::new(rdy_bin())
        .args(&args)
        .env("RDY_ACTOR", "human:qa")
        .envs(env)
        .output()
        .unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into(),
        String::from_utf8_lossy(&o.stderr).into(),
    )
}

fn guide() -> Value {
    json!([
        {"op": "create", "type": "doc", "id": "$other", "fields": {"name": "other", "title": "Other page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$os", "parent": "$other", "fields": {"title": "Other section"}},
        {"op": "create", "type": "prose", "parent": "$os", "body": "Other words."},
        {"op": "create", "type": "folder", "id": "$guide", "fields": {"name": "guide", "title": "Guide"}},
        {"op": "create", "type": "doc", "id": "$intro", "parent": "$guide", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$intro", "fields": {"title": "Start here"}},
        {"op": "create", "type": "prose", "parent": "$s", "body": "Intro words."}
    ])
}

/// The number of HTML pages under `dir` but the permalinks.
fn pages(dir: &Path) -> usize {
    rdy_qa::golden::pages(dir)
        .into_iter()
        .filter(|p| !p.strip_prefix(dir).unwrap().starts_with("go"))
        .count()
}

#[test]
fn dgm10_cli_render_writes_the_themed_404_at_the_base() {
    let (dir, _ids) = seeded("/docs/", guide());
    let out = tempfile::tempdir().unwrap();
    let site = out.path().join("site");
    let (ok, stdout, stderr) = render(dir.path(), &site, &[]);
    assert!(ok, "{stdout} {stderr}");
    let html = std::fs::read_to_string(site.join("404.html")).unwrap();
    assert!(html.contains(MARKER), "{html}");
    assert!(html.contains("<title>Page not found · Lost project</title>"));
    assert!(html.contains("<meta name=\"robots\" content=\"noindex\">"));
    assert!(
        html.contains("<link rel=\"stylesheet\" href=\"/docs/assets/site.css\">")
            && html.contains("<script src=\"/docs/assets/app.js\" defer></script>")
            && html.contains("data-index=\"/docs/search-index.json\"")
            && html.contains("<a href=\"/docs/guide/intro.html\">Intro page</a>")
            && html.contains("go to the <a href=\"/docs/index.html\">home page</a>."),
        "{html}"
    );
    // the summary counts the documentation pages alone, and warns of nothing
    let n = pages(&site);
    assert!(
        stdout.starts_with(&format!("rendered {} pages, ", n - 1)),
        "{stdout} ({n} html files)"
    );
    assert!(!stdout.contains("warning"), "{stdout}");
    // an ordinary page keeps its relative links
    let other = std::fs::read_to_string(site.join("other.html")).unwrap();
    assert!(!other.contains("/docs/") && !other.contains("not-found"));
}

#[test]
fn dgm10_cli_render_profile_writes_the_selected_404() {
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$hid", "fields": {"name": "hidden", "title": "HIDDENCANARY doc", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$hs", "parent": "$hid", "fields": {"title": "Hidden section"}},
        {"op": "create", "type": "prose", "parent": "$hs", "body": "Hidden words."},
        {"op": "create", "type": "doc", "id": "$shown", "fields": {"name": "shown", "title": "Shown page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$ss", "parent": "$shown", "fields": {"title": "Shown section"}},
        {"op": "create", "type": "prose", "parent": "$ss", "body": "Shown words."}
    ]);
    let (dir, ids) = seeded("/", ops);
    let mut r = Rdy::open(dir.path()).unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "parent": ids["$hid"],
            "fields": {"key": "shownorg", "title": "Shown site", "audience": "org", "select": [ids["$shown"]]}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    drop(r);
    let out = tempfile::tempdir().unwrap();
    let site = out.path().join("site");
    let (ok, stdout, stderr) = render(dir.path(), &site, &["--profile", "shownorg"]);
    assert!(ok, "{stdout} {stderr}");
    let html = std::fs::read_to_string(site.join("404.html")).unwrap();
    assert!(html.contains(MARKER), "{html}");
    assert!(html.contains("<title>Page not found · Shown site</title>"));
    assert!(
        html.contains("<a href=\"/shown.html\">Shown page</a>"),
        "{html}"
    );
    for word in ["HIDDENCANARY", "Lost project", "hidden.html"] {
        assert!(!html.contains(word), "{word}");
    }
    assert!(!stdout.contains(KEPT), "{stdout}");
}

#[test]
fn dgm10_cli_render_keeps_an_authored_404_and_says_so_once() {
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$lost", "fields": {"name": "404", "title": "Authored lost page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$ls", "parent": "$lost", "fields": {"title": "Where to go"}},
        {"op": "create", "type": "prose", "parent": "$ls", "body": "AUTHOREDCANARY words."}
    ]);
    let (dir, _ids) = seeded("/", ops);
    let out = tempfile::tempdir().unwrap();
    let site = out.path().join("site");
    let (ok, stdout, stderr) = render(dir.path(), &site, &[]);
    assert!(ok, "{stdout} {stderr}");
    let html = std::fs::read_to_string(site.join("404.html")).unwrap();
    assert!(html.contains("AUTHOREDCANARY words.") && !html.contains("data-rdy-kind"));
    assert_eq!(stdout.matches(KEPT).count(), 1, "{stdout}");
    assert!(
        stdout.trim_end().ends_with(&format!("warning: {KEPT}")),
        "{stdout}"
    );
    // the authored page is a documentation page: the summary counts it
    assert!(
        stdout.starts_with(&format!("rendered {} pages, ", pages(&site))),
        "{stdout}"
    );
}
