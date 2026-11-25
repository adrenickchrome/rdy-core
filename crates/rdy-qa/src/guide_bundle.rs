//! The agent guide bundle (`release/agent-guide.json`) as the tests read it. rdy-design owns the
//! guide's words and freezes them at the design landing, after the code is gated; so a test never
//! holds a copy of a guide sentence. It checks that the binary serves the bundle it was built
//! with: each ruleset's rows, ids and text in bundle order, every rule id, and no empty ruleset.
use crate::mcp::McpClient;
use serde_json::{Value, json};

/// The bundle bytes this build compiled against: the file `rdy-engine`'s build embeds.
pub const BUNDLE: &str = include_str!("../../../release/agent-guide.json");

/// One rule of the bundle: its ruleset name (no `rdy:` prefix), id and text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub ruleset: String,
    pub id: String,
    pub text: String,
}

/// The names of the bundle's rulesets, in bundle order.
pub fn rulesets() -> Vec<String> {
    bundle()["rulesets"]
        .as_array()
        .expect("the bundle has rulesets")
        .iter()
        .map(|s| s["name"].as_str().expect("a ruleset name").to_string())
        .collect()
}

/// The rules of the ruleset `name`, in bundle order, each with the text that rdy delivers: the
/// last rule of a ruleset that owns tools carries its value line (`rdy_engine::kungfu`).
pub fn rules(name: &str) -> Vec<Rule> {
    let b = bundle();
    let set = b["rulesets"]
        .as_array()
        .expect("the bundle has rulesets")
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("the bundle has no ruleset {name}"));
    let rows = set["rows"].as_array().expect("a ruleset has rows");
    let last = rows.len().saturating_sub(1);
    rows.iter()
        .enumerate()
        .map(|(i, r)| {
            let text = r["text"].as_str().expect("a rule text");
            let text = match rdy_engine::kungfu::owner(name) {
                Some(o) if i == last => o.delivered(text),
                _ => text.to_string(),
            };
            Rule {
                ruleset: name.to_string(),
                id: r["id"].as_str().expect("a rule id").to_string(),
                text,
            }
        })
        .collect()
}

fn bundle() -> Value {
    serde_json::from_str(BUNDLE).expect("release/agent-guide.json is JSON")
}

/// The rdy rows that a real `rdy_instructions` call with `load: ["rdy:<name>", ...]` serves. It
/// claims a reader session when the connection has none.
pub fn served(c: &mut McpClient, sets: &[&str]) -> Vec<Rule> {
    let load: Vec<String> = sets.iter().map(|s| format!("rdy:{s}")).collect();
    let mut r = c.call("rdy_instructions", json!({"load": load}));
    if r.code() == Some("E_NO_SESSION") {
        c.call(
            "rdy_session",
            json!({"actor": "agent:guide", "role": "reader"}),
        )
        .ok();
        r = c.call("rdy_instructions", json!({"load": load}));
    }
    let v = r.ok();
    v["instructions"]
        .as_array()
        .expect("instructions")
        .iter()
        .filter(|i| i["layer"] == "rdy")
        .map(|i| Rule {
            ruleset: i["ruleset"]
                .as_str()
                .expect("a row's ruleset")
                .trim_start_matches("rdy:")
                .to_string(),
            id: i["id"].as_str().expect("a row id").to_string(),
            text: i["text"].as_str().expect("a row text").to_string(),
        })
        .collect()
}

/// Asserts that loading each ruleset of `sets` serves it exactly as the bundle holds it: the same
/// rules, ids and text, in bundle order, and at least one rule.
pub fn assert_served(c: &mut McpClient, sets: &[&str]) {
    let got = served(c, sets);
    for set in sets {
        let want = rules(set);
        assert!(!want.is_empty(), "the bundle's ruleset {set} is empty");
        let got: Vec<&Rule> = got.iter().filter(|r| r.ruleset == *set).collect();
        let got: Vec<Rule> = got.into_iter().cloned().collect();
        assert_eq!(got, want, "rdy:{set} is not served as the bundle holds it");
    }
}

/// [`assert_served`] from a fresh project and stdio server.
pub fn assert_served_fresh(sets: &[&str]) {
    let (dir, r) = crate::fixtures::project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    assert_served(&mut c, sets);
}
