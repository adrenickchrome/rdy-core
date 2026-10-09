//! wcwt1: rdy worktree add makes an extra human checkout bound to its own view, ls lists it, rm
//! removes the checkout and never the view (spec §12.1, FW-V2: no agent form).
use rdy_engine::*;
use rdy_qa::wc::*;
use std::process::Command;

#[test]
fn wcwt1_add_ls_commit_inside_and_rm() {
    let (s, seed) = seeded();
    let side = tempfile::tempdir().unwrap();
    let dir = side.path().join("side");
    let d = dir.to_str().unwrap();
    let (okk, v) = json(&s, &["worktree", "add", d, "side"]);
    assert!(okk, "{v}");
    assert_eq!(v["name"], "side");
    assert!(dir.join(".rdy-worktree").exists() && dir.join("notes.yaml").exists());
    assert!(at(&s, ".rdy/worktrees/side/bound.yaml").exists());
    let (_, v) = json(&s, &["worktree", "ls"]);
    assert_eq!(v[0]["name"], "side");
    assert_eq!(v[0]["view"], "side");
    let run = |args: &[&str]| {
        Command::new(rdy_qa::fixtures::rdy_bin())
            .args(args)
            .current_dir(&dir)
            .env_remove("RDY_ACTOR")
            .env_remove("RDY_RUN")
            .envs(s.env_pairs())
            .output()
            .unwrap()
    };
    let mut list: Vec<serde_yaml_ng::Value> =
        serde_yaml_ng::from_str(&std::fs::read_to_string(dir.join("notes.yaml")).unwrap()).unwrap();
    let e = list
        .iter_mut()
        .find(|e| e["id"].as_str() == Some(seed.note.as_str()))
        .unwrap();
    e.as_mapping_mut()
        .unwrap()
        .insert("body".into(), "Side.".into());
    std::fs::write(
        dir.join("notes.yaml"),
        serde_yaml_ng::to_string(&list).unwrap(),
    )
    .unwrap();
    assert!(run(&["add", "notes.yaml"]).status.success());
    let o = run(&["commit", "-m", "side edit"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(store_block(&s, "side", &seed.note).body, "Side.");
    assert_eq!(
        store_block(&s, "work", &seed.note).body,
        "Note.",
        "the main view is untouched"
    );
    std::fs::write(dir.join("mine.txt"), "keep").unwrap();
    let (okk, v) = json(&s, &["worktree", "rm", d]);
    assert!(okk, "{v}");
    assert!(!dir.join("notes.yaml").exists() && !dir.join(".rdy-worktree").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join("mine.txt")).unwrap(),
        "keep"
    );
    assert!(!at(&s, ".rdy/worktrees/side").exists());
    assert_eq!(
        store_block(&s, "side", &seed.note).body,
        "Side.",
        "the view stays"
    );
}

#[test]
fn wcwt1_rm_refuses_a_dirty_worktree_and_add_refuses_a_full_dir() {
    let (s, seed) = seeded();
    let side = tempfile::tempdir().unwrap();
    let dir = side.path().join("w");
    let d = dir.to_str().unwrap();
    ok(&s, &["worktree", "add", d, "w"]);
    let mut list: Vec<serde_yaml_ng::Value> =
        serde_yaml_ng::from_str(&std::fs::read_to_string(dir.join("notes.yaml")).unwrap()).unwrap();
    list.iter_mut()
        .find(|e| e["id"].as_str() == Some(seed.note.as_str()))
        .unwrap()
        .as_mapping_mut()
        .unwrap()
        .insert("body".into(), "Dirty.".into());
    std::fs::write(
        dir.join("notes.yaml"),
        serde_yaml_ng::to_string(&list).unwrap(),
    )
    .unwrap();
    let (okk, v) = json(&s, &["worktree", "rm", d]);
    assert!(!okk && v["code"] == E_WC_DIRTY, "{v}");
    ok(&s, &["worktree", "rm", d, "--force"]);
    let full = side.path().join("full");
    std::fs::create_dir_all(&full).unwrap();
    std::fs::write(full.join("x.txt"), "x").unwrap();
    let (okk, v) = json(&s, &["worktree", "add", full.to_str().unwrap(), "w"]);
    assert!(!okk && v["code"] == E_BAD_INPUT, "{v}");
    let o = s.rdy(&["worktree", "add", d, "w", "--actor", "agent:x"]);
    assert!(!o.status.success(), "there is no agent form");
}

#[test]
fn wcwt1_names_are_sanitized_and_numbered() {
    let (s, _seed) = seeded();
    let side = tempfile::tempdir().unwrap();
    let one = side.path().join("one").join("a b");
    let two = side.path().join("two").join("a b");
    let (okk, v) = json(&s, &["worktree", "add", one.to_str().unwrap(), "side"]);
    assert!(okk && v["name"] == "a-b", "{v}");
    let (okk, v) = json(&s, &["worktree", "add", two.to_str().unwrap(), "side"]);
    assert!(okk && v["name"] == "a-b-2", "{v}");
    let (_, v) = json(&s, &["worktree", "ls"]);
    assert_eq!(v.as_array().unwrap().len(), 2, "{v}");
    let (okk, v) = json(&s, &["worktree", "rm", side.path().to_str().unwrap()]);
    assert!(!okk && v["code"] == E_BAD_INPUT, "not a worktree: {v}");
    ok(&s, &["worktree", "rm", one.to_str().unwrap()]);
    ok(&s, &["worktree", "rm", two.to_str().unwrap()]);
    assert!(!one.exists() && !two.exists(), "empty directories go too");
    // a one-character name that sanitizes to `-` must still read back from its pointer
    let odd = side.path().join("three").join("@");
    let (okk, v) = json(&s, &["worktree", "add", odd.to_str().unwrap(), "side"]);
    assert!(okk && v["name"] == "-", "{v}");
    ok(&s, &["worktree", "rm", odd.to_str().unwrap()]);
    assert!(!odd.exists(), "the odd-named worktree is removable");
}

#[test]
fn msw10_a_damaged_worktree_pointer_is_an_error_not_the_main_copy() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    let side = at(&s, "side");
    ok(&s, &["worktree", "add", side.to_str().unwrap(), "side"]);
    std::fs::write(side.join(".rdy-worktree"), "project: [\n").unwrap();
    let o = Command::new(rdy_qa::fixtures::rdy_bin())
        .args(["--json", "status"])
        .current_dir(&side)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .envs(s.env_pairs())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success(),
        "ran on the main working copy: {}",
        String::from_utf8_lossy(&o.stdout)
    );
    assert!(
        err.contains(".rdy-worktree") && err.contains(E_BAD_INPUT),
        "{err}"
    );
}

#[test]
fn msw16_rm_of_a_copied_worktree_dir_keeps_the_real_worktree() {
    let (s, _seed) = seeded();
    let tmp = tempfile::tempdir().unwrap();
    let side = tmp.path().join("side");
    let copy = tmp.path().join("side2");
    ok(&s, &["worktree", "add", side.to_str().unwrap(), "side"]);
    let o = Command::new("cp")
        .arg("-R")
        .arg(&side)
        .arg(&copy)
        .output()
        .unwrap();
    assert!(o.status.success());
    let (okk, v) = json(&s, &["worktree", "rm", copy.to_str().unwrap()]);
    assert!(!okk && v["code"] == E_BAD_INPUT, "{v}");
    assert!(
        at(&s, ".rdy/worktrees/side/bound.yaml").exists(),
        "the real worktree's state stays"
    );
    assert!(copy.join("notes.yaml").exists() && side.join("notes.yaml").exists());
}

/// `rdy --json <args>` run inside `dir` (no `--project`): whether it succeeded, and its JSON.
fn json_in(
    s: &rdy_qa::daemon::Served,
    dir: &std::path::Path,
    args: &[&str],
) -> (bool, serde_json::Value) {
    let o = Command::new(rdy_qa::fixtures::rdy_bin())
        .arg("--json")
        .args(args)
        .current_dir(dir)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .envs(s.env_pairs())
        .output()
        .unwrap();
    let okk = o.status.success();
    let text = String::from_utf8_lossy(if okk { &o.stdout } else { &o.stderr }).into_owned();
    let v = serde_json::from_str(text.trim())
        .or_else(|_| serde_json::from_str(text.trim().lines().last().unwrap_or("")))
        .unwrap_or(serde_json::Value::String(text));
    (okk, v)
}

fn same_dir(a: &str, b: &std::path::Path) -> bool {
    std::path::Path::new(a).canonicalize().ok() == b.canonicalize().ok()
}

/// msw16 fix round 1 (ruling): a moved worktree is still the worktree. A command run inside it
/// records its new directory, and rm removes it.
#[test]
fn msw16_a_moved_worktree_is_used_and_removed() {
    let (s, _seed) = seeded();
    let tmp = tempfile::tempdir().unwrap();
    let side = tmp.path().join("side");
    let moved = tmp.path().join("moved");
    ok(&s, &["worktree", "add", side.to_str().unwrap(), "side"]);
    std::fs::rename(&side, &moved).unwrap();
    let (okk, v) = json_in(&s, &moved, &["update"]);
    assert!(okk, "{v}");
    let (_, v) = json(&s, &["worktree", "ls"]);
    assert!(same_dir(v[0]["dir"].as_str().unwrap(), &moved), "{v}");
    let (okk, v) = json(&s, &["worktree", "rm", moved.to_str().unwrap()]);
    assert!(okk, "{v}");
    assert!(!at(&s, ".rdy/worktrees/side").exists());
    assert!(!moved.join("notes.yaml").exists() && !moved.join(".rdy-worktree").exists());
}

/// msw16 fix round 1 (ruling): a crash between the pointer write and the dir.txt write leaves a
/// worktree with no recorded directory; rm still removes it.
#[test]
fn msw16_rm_of_a_worktree_without_dir_txt_removes_it() {
    let (s, _seed) = seeded();
    let tmp = tempfile::tempdir().unwrap();
    let side = tmp.path().join("side");
    ok(&s, &["worktree", "add", side.to_str().unwrap(), "side"]);
    std::fs::remove_file(at(&s, ".rdy/worktrees/side/dir.txt")).unwrap();
    let (okk, v) = json(&s, &["worktree", "rm", side.to_str().unwrap()]);
    assert!(okk, "{v}");
    assert!(!at(&s, ".rdy/worktrees/side").exists());
    assert!(!side.join(".rdy-worktree").exists());
}

/// msw16 fix round 1 (ruling): a command run inside a copied worktree directory is refused and
/// names the real worktree; the real worktree and its view are untouched.
#[test]
fn msw16_a_command_in_a_copied_worktree_is_refused() {
    let (s, seed) = seeded();
    let tmp = tempfile::tempdir().unwrap();
    let side = tmp.path().join("side");
    let copy = tmp.path().join("side2");
    ok(&s, &["worktree", "add", side.to_str().unwrap(), "side"]);
    let o = Command::new("cp")
        .arg("-R")
        .arg(&side)
        .arg(&copy)
        .output()
        .unwrap();
    assert!(o.status.success());
    let before = std::fs::read_to_string(side.join("notes.yaml")).unwrap();
    let text = before.replace("Note.", "Copy.");
    assert_ne!(text, before);
    std::fs::write(copy.join("notes.yaml"), text).unwrap();
    for args in [
        &["add", "notes.yaml"][..],
        &["commit", "-m", "from the copy"][..],
    ] {
        let (okk, v) = json_in(&s, &copy, args);
        assert!(!okk && v["code"] == E_BAD_INPUT, "{args:?}: {v}");
        let m = v["message"].as_str().unwrap();
        assert!(
            m.contains("worktree side") && m.contains(side.file_name().unwrap().to_str().unwrap()),
            "{v}"
        );
    }
    assert_eq!(store_block(&s, "side", &seed.note).body, "Note.");
    assert!(!at(&s, ".rdy/worktrees/side/stage/files/notes.yaml").exists());
    assert_eq!(
        std::fs::read_to_string(side.join("notes.yaml")).unwrap(),
        before
    );
    let (_, v) = json(&s, &["worktree", "ls"]);
    assert!(same_dir(v[0]["dir"].as_str().unwrap(), &side), "{v}");
    let (okk, v) = json_in(&s, &side, &["status"]);
    assert!(okk, "{v}");
}
