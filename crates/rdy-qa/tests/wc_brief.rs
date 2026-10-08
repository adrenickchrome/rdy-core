//! wcbr1: rdy brief on a file, a directory or a block prints the briefing and records its token; a
//! human commit to stable content briefs itself (Q6c, D-TT7W8); an agent commit must cite a
//! recorded token, and its refusal records a fresh one (amendment §5.5, D-4G9DE, D-8DZSC).
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::wc::*;
use serde_json::{Value, json};

/// A doc `spec` created stable in one batch (no briefing needed: its parents are new) and landed.
/// Returns its prose's id.
fn stable_doc(s: &Served) -> String {
    let w = store_write(
        s,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Stable text."}
        ]),
    );
    store_land(s, "work");
    w.created["$p"].clone()
}

fn agent(s: &Served, args: &[&str]) -> (bool, Value) {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    s.json_as("agent:ed", Some("r1"), &a)
}

#[test]
fn wcbr1_brief_on_a_file_a_dir_or_a_block_records_its_token() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let (okk, v) = json(&s, &["brief", "guides/intro.yaml"]);
    assert!(okk, "{v}");
    assert_eq!(v["recorded"], true);
    let token = v["base"]["token"].as_str().unwrap().to_string();
    let briefs = read(&s, ".rdy/wc/briefs.yaml");
    assert!(
        briefs.contains(&token) && briefs.contains(&seed.claim),
        "{briefs}"
    );
    let (okk, v) = json(&s, &["brief", "guides"]);
    assert!(okk, "{v}");
    assert!(
        v["base"]["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"] == seed.prose.as_str()),
        "{v}"
    );
    let (okk, v) = json(&s, &["brief", seed.note.as_str()]);
    assert!(
        okk && v["base"]["blocks"][0]["id"] == seed.note.as_str(),
        "{v}"
    );
    let text = ok(&s, &["brief", "notes.yaml"]);
    assert!(
        text.trim_end().ends_with("block(s)") && text.contains("token recorded for "),
        "{text}"
    );
    let o = s.rdy(&["brief", "notes.yaml", "--code", "x.rs"]);
    assert!(
        !o.status.success(),
        "a target and --code together are refused"
    );
    let o = s.rdy(&["brief"]);
    assert!(!o.status.success(), "one of them is required");
}

#[test]
fn wcbr1_a_human_commit_to_stable_content_briefs_itself() {
    let (s, _seed) = seeded();
    let p = stable_doc(&s);
    ok(&s, &["checkout"]);
    set_body(&s, "spec.yaml", &p, "Changed stable text.");
    ok(&s, &["add", "spec.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "edit stable"]);
    assert!(okk, "{v}");
    assert!(v["briefing"]["base"]["token"].is_string(), "{v}");
    set_body(&s, "spec.yaml", &p, "Again.");
    ok(&s, &["add", "spec.yaml"]);
    let text = ok(&s, &["commit", "-m", "again"]);
    assert!(
        text.starts_with("briefing (cited by this commit):"),
        "{text}"
    );
    assert_eq!(store_block(&s, "work", &p).body, "Again.");
}

#[test]
fn wcbr1_an_agent_commit_needs_a_recorded_token_and_a_refusal_records_one() {
    let (s, _seed) = seeded();
    let p = stable_doc(&s);
    let (okk, v) = agent(&s, &["checkout"]);
    assert!(okk && v["view"] == "r1", "{v}");
    set_body(&s, "spec.yaml", &p, "Agent edit.");
    assert!(agent(&s, &["add", "spec.yaml"]).0);
    let (okk, v) = agent(&s, &["commit", "-m", "blind"]);
    assert!(!okk && v["code"] == E_BRIEF_REQUIRED, "{v}");
    let token = v["briefing"]["base"]["token"]
        .as_str()
        .expect("a fresh briefing")
        .to_string();
    assert!(
        read(&s, ".rdy/wc/briefs.yaml").contains(&token),
        "the refusal records it"
    );
    let (okk, v) = agent(&s, &["commit", "-m", "briefed"]);
    assert!(okk, "{v}");
    assert_eq!(store_block(&s, "r1", &p).body, "Agent edit.");
}

#[test]
fn wcbr1_an_agent_brief_then_commit_goes_through_first_time() {
    let (s, _seed) = seeded();
    let p = stable_doc(&s);
    assert!(agent(&s, &["checkout"]).0);
    let (okk, v) = agent(&s, &["brief", "spec.yaml"]);
    assert!(okk && v["recorded"] == true, "{v}");
    set_body(&s, "spec.yaml", &p, "Briefed first.");
    assert!(agent(&s, &["add", "spec.yaml"]).0);
    let (okk, v) = agent(&s, &["commit", "-m", "briefed"]);
    assert!(okk, "{v}");
}

#[test]
fn wcbr1_an_agent_commit_without_the_daemon_is_refused_up_front() {
    let (s, _seed) = seeded();
    let p = stable_doc(&s);
    assert!(agent(&s, &["checkout"]).0);
    set_body(&s, "spec.yaml", &p, "No daemon.");
    assert!(agent(&s, &["add", "spec.yaml"]).0);
    let (okk, v) = agent(&s, &["--no-daemon", "commit", "-m", "x"]);
    assert!(!okk && v["code"] == E_BAD_INPUT, "{v}");
    assert!(
        v["message"].as_str().unwrap().contains("need the daemon"),
        "{v}"
    );
    assert!(v["fix"].as_str().unwrap().contains("--no-daemon"), "{v}");
}

#[test]
fn wcbr1_a_human_commit_of_drafts_prints_the_briefing_only_when_it_says_something() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    // a draft doc nothing governs: one line names the cited token
    set_body(&s, "notes.yaml", &seed.note, "Draft edit.");
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "draft"]);
    assert!(okk, "{v}");
    assert!(
        v["briefing"]["base"]["token"].is_string(),
        "the JSON keeps it: {v}"
    );
    set_body(&s, "notes.yaml", &seed.note, "Draft again.");
    ok(&s, &["add", "notes.yaml"]);
    let text = ok(&s, &["commit", "-m", "draft again"]);
    assert!(
        !text.contains("briefing (cited by this commit):")
            && text.starts_with("briefing cited (token "),
        "{text}"
    );
    assert_eq!(
        text.lines().filter(|l| l.contains("briefing")).count(),
        1,
        "{text}"
    );
    // a draft block a decision governs: the briefing has something to say
    set_body(&s, "guides/intro.yaml", &seed.claim, "Claims still hold.");
    ok(&s, &["add", "guides/intro.yaml"]);
    let text = ok(&s, &["commit", "-m", "governed"]);
    assert!(
        text.starts_with("briefing (cited by this commit):") && text.contains(&seed.decision),
        "{text}"
    );
}
