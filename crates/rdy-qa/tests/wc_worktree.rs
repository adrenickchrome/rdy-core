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
}
