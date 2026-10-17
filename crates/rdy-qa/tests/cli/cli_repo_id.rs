//! ec10: `rdy status` names the repo id and, for a clone or a fork, where it came from (plan
//! Export and clone; D-DAGNB: "in `rdy --json status`"; D-C8C70; D-KQ4CA).
use rdy_engine::Rdy;
use rdy_engine::clone::{CloneSource, clone};
use rdy_qa::daemon::{Served, human_command, short_tempdir};
use rdy_qa::golden::{FIXTURE_DIR, approver, land};
use serde_json::Value;
use std::path::Path;

/// An open project with the golden corpus imported and landed, tagged when `tag` names one.
fn source(tag: Option<&str>) -> (Served, Rdy) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    r.import(&approver(), "import", Path::new(FIXTURE_DIR))
        .unwrap();
    land(&mut r, "import").unwrap();
    if let Some(t) = tag {
        r.tag(&approver(), t, None, None).unwrap();
    }
    (s, r)
}

/// `rdy status` (or `rdy --json status`) in `dir`, in process, as the person of `s`.
fn status_in(s: &Served, dir: &Path, json: bool) -> String {
    let mut args = vec!["--no-daemon"];
    if json {
        args.push("--json");
    }
    args.push("status");
    let out = human_command(dir, &s.env_pairs(), &args).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

#[test]
fn ec10_status_json_names_the_repo_id() {
    let s = Served::new();
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok, "{v}");
    let id = v["repo_id"].as_str().unwrap_or_else(|| panic!("{v}"));
    assert_eq!(id.len(), 32, "{id}");
    assert!(
        id.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{id}"
    );
    assert!(
        v.get("origin").is_none(),
        "a new project has no origin: {v}"
    );
    assert!(v.get("forked_from").is_none(), "{v}");
    let (_, again) = s.json(&["--json", "status"]);
    assert_eq!(again["repo_id"], v["repo_id"], "the id is stable");
}

#[test]
fn ec10_status_text_names_the_repo_and_its_origin() {
    let scratch = short_tempdir();

    // a project that is not a clone: the repo line, then the canon line
    let plain = Served::new();
    let text = status_in(&plain, plain.dir.path(), false);
    let l = lines(&text);
    assert_eq!(l[0], "pack rdy-standard", "{text}");
    let id = l[1]
        .strip_prefix("repo ")
        .unwrap_or_else(|| panic!("{text}"));
    assert_eq!(id.len(), 32, "{text}");
    assert!(l[2].starts_with("canon: "), "{text}");

    // a clone of a tagged project: the origin names the version
    let (s, r) = source(Some("v1.0.0"));
    let source_id = r.read().repo_id().unwrap().to_string();
    let head = r.read().canon_head().unwrap().to_string();
    let to = scratch.path().join("tagged");
    clone(CloneSource::State(&r.read()), &to, false).unwrap();
    let text = status_in(&s, &to, false);
    let l = lines(&text);
    assert_eq!(l[0], "pack rdy-standard", "{text}");
    assert_eq!(l[1], format!("repo {source_id}"), "a clone keeps the id");
    assert_eq!(
        l[2],
        format!("cloned from {source_id} at version v1.0.0"),
        "{text}"
    );
    assert!(l[3].starts_with("canon: "), "{text}");

    // no version: the landing, 12 hex digits
    let (s2, r2) = source(None);
    let id2 = r2.read().repo_id().unwrap().to_string();
    let head2 = r2.read().canon_head().unwrap().to_string();
    let to2 = scratch.path().join("landed");
    clone(CloneSource::State(&r2.read()), &to2, false).unwrap();
    let text = status_in(&s2, &to2, false);
    let l = lines(&text);
    assert_eq!(
        l[2],
        format!("cloned from {id2} at landing {}", &head2[..12]),
        "{text}"
    );
    assert_eq!(l[2].rsplit(' ').next().unwrap().len(), 12, "{text}");
    assert!(head.len() > 12, "the hash is longer than the shown prefix");

    // neither a version nor a landing
    let empty = Served::new();
    let eid = {
        let r = Rdy::open(empty.dir.path()).unwrap();
        let id = r.read().repo_id().unwrap().to_string();
        let to3 = scratch.path().join("bare");
        clone(CloneSource::State(&r.read()), &to3, false).unwrap();
        id
    };
    let text = status_in(&empty, &scratch.path().join("bare"), false);
    let l = lines(&text);
    assert_eq!(l[2], format!("cloned from {eid}"), "{text}");

    // the same in JSON: tag and landing are null when absent
    let json: Value =
        serde_json::from_str(&status_in(&empty, &scratch.path().join("bare"), true)).unwrap();
    assert_eq!(
        json["origin"],
        serde_json::json!({"repo_id": eid, "tag": null, "landing": null}),
        "{json}"
    );
    assert_eq!(json["repo_id"], eid);
    let json: Value = serde_json::from_str(&status_in(&s, &to, true)).unwrap();
    assert_eq!(json["origin"]["tag"], "v1.0.0", "{json}");
    assert_eq!(json["origin"]["landing"], head, "{json}");
}

#[test]
fn ec10_status_of_a_fork_says_forked_from() {
    let scratch = short_tempdir();
    let (s, r) = source(Some("v1.0.0"));
    let source_id = r.read().repo_id().unwrap().to_string();
    let to = scratch.path().join("fork");
    clone(CloneSource::State(&r.read()), &to, true).unwrap();

    let text = status_in(&s, &to, false);
    let l = lines(&text);
    let new_id = l[1]
        .strip_prefix("repo ")
        .unwrap_or_else(|| panic!("{text}"));
    assert_ne!(new_id, source_id, "a fork takes a new id: {text}");
    assert_eq!(
        l[2],
        format!("forked from {source_id} at version v1.0.0"),
        "{text}"
    );

    let json: Value = serde_json::from_str(&status_in(&s, &to, true)).unwrap();
    assert_eq!(json["repo_id"], new_id, "{json}");
    assert_eq!(json["forked_from"]["repo_id"], source_id, "{json}");
    assert!(json.get("origin").is_none(), "{json}");
}
