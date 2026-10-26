//! The Release 1 contract of the command line (D-TXFHR: the contract surfaces include the CLI's
//! commands, flags, positional arguments and defaults). This test-only module walks the real clap
//! `Command` tree, hidden commands and hidden flags included, and compares it with the `cli.tree`
//! member of the release contract fixture (`rdy-qa/fixtures/contracts/release-1.json`). It adds no
//! command, flag or production code path. The rest of the fixture, and the runtime facts of the
//! `cli` member, belong to the rdy-qa `release` area (`release_contract.rs`).
//!
//! Declared order is part of the contract: a command's flags and arguments are listed in the order
//! the parser declares them, and its subcommands in declaration order. Objects are written with
//! sorted keys. To write a candidate baseline, run this test with `RDY_CONTRACT_CAPTURE=1`: it
//! replaces only the `cli.tree` member and keeps every other member of the file.
use super::Cli;
use clap::{Arg, ArgAction, Command, CommandFactory};
use serde_json::{Map, Value, json};
use std::path::PathBuf;

/// The fixture the rdy-qa `release` area also reads.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rdy-qa/fixtures/contracts/release-1.json")
}

/// The fixture's text; the baseline-absent failure names the capture switch.
fn read_fixture() -> Value {
    let path = fixture_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "the release contract baseline {} is absent ({e}); capture it with RDY_CONTRACT_CAPTURE=1",
            path.display()
        )
    });
    serde_json::from_str(&text).expect("the release contract fixture is JSON")
}

/// The action of an argument, as its stable word.
fn action_word(a: &ArgAction) -> &'static str {
    match a {
        ArgAction::Set => "set",
        ArgAction::Append => "append",
        ArgAction::SetTrue => "set_true",
        ArgAction::SetFalse => "set_false",
        ArgAction::Count => "count",
        ArgAction::Help => "help",
        ArgAction::HelpShort => "help_short",
        ArgAction::HelpLong => "help_long",
        ArgAction::Version => "version",
        _ => "other",
    }
}

fn strings<'a>(items: impl IntoIterator<Item = &'a str>) -> Value {
    Value::Array(items.into_iter().map(|s| json!(s)).collect())
}

/// One argument: its id, how it is written, its arity, its defaults and its choices. A member
/// that holds its empty or false value is left out, so the fixture lists only what is set.
fn arg_json(cmd: &Command, a: &Arg) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(a.get_id().as_str()));
    if a.is_positional() {
        m.insert("positional".into(), json!(true));
    }
    if let Some(i) = a.get_index() {
        m.insert("index".into(), json!(i));
    }
    if let Some(l) = a.get_long() {
        m.insert("long".into(), json!(l));
    }
    if let Some(s) = a.get_short() {
        m.insert("short".into(), json!(s.to_string()));
    }
    let aliases: Vec<&str> = a.get_all_aliases().unwrap_or_default();
    if !aliases.is_empty() {
        m.insert("long_aliases".into(), strings(aliases));
    }
    m.insert("action".into(), json!(action_word(a.get_action())));
    for (name, set) in [
        ("required", a.is_required_set()),
        ("global", a.is_global_set()),
        ("hidden", a.is_hide_set()),
        ("last", a.is_last_set()),
        ("allow_hyphen_values", a.is_allow_hyphen_values_set()),
    ] {
        if set {
            m.insert(name.into(), json!(true));
        }
    }
    if let Some(n) = a.get_num_args() {
        m.insert("num_args".into(), json!(n.to_string()));
    }
    if let Some(names) = a.get_value_names() {
        m.insert(
            "value_names".into(),
            strings(names.iter().map(|n| n.as_str())),
        );
    }
    if let Some(d) = a.get_value_delimiter() {
        m.insert("value_delimiter".into(), json!(d.to_string()));
    }
    let defaults: Vec<String> = a
        .get_default_values()
        .iter()
        .map(|v| v.to_string_lossy().into_owned())
        .collect();
    if !defaults.is_empty() {
        m.insert("defaults".into(), json!(defaults));
    }
    let choices: Vec<String> = a
        .get_possible_values()
        .iter()
        .map(|v| v.get_name().to_string())
        .collect();
    if !choices.is_empty() {
        m.insert("choices".into(), json!(choices));
    }
    // the arguments this one cannot be used with
    let mut conflicts: Vec<String> = cmd
        .get_arg_conflicts_with(a)
        .iter()
        .map(|c| c.get_id().as_str().to_string())
        .collect();
    conflicts.sort();
    if !conflicts.is_empty() {
        m.insert("conflicts_with".into(), json!(conflicts));
    }
    Value::Object(m)
}

/// One command: its names, its arguments in declared order, its groups and its subcommands in
/// declared order.
fn command_json(cmd: &Command) -> Value {
    let mut m = Map::new();
    m.insert("name".into(), json!(cmd.get_name()));
    let aliases: Vec<&str> = cmd.get_all_aliases().collect();
    if !aliases.is_empty() {
        m.insert("aliases".into(), strings(aliases));
    }
    if cmd.is_hide_set() {
        m.insert("hidden".into(), json!(true));
    }
    m.insert(
        "args".into(),
        Value::Array(cmd.get_arguments().map(|a| arg_json(cmd, a)).collect()),
    );
    let groups: Vec<Value> = cmd
        .get_groups()
        .map(|g| {
            let mut g = g.clone();
            let mut members: Vec<&str> = g.get_args().map(|a| a.as_str()).collect();
            members.sort();
            json!({
                "id": g.get_id().as_str(),
                "args": members,
                "multiple": g.is_multiple(),
                "required": g.is_required_set(),
            })
        })
        .collect();
    if !groups.is_empty() {
        m.insert("groups".into(), Value::Array(groups));
    }
    if cmd.is_subcommand_required_set() {
        m.insert("subcommand_required".into(), json!(true));
    }
    // the `help` subcommand is clap's own, added to every command that has subcommands
    m.insert(
        "subcommands".into(),
        Value::Array(
            cmd.get_subcommands()
                .filter(|c| c.get_name() != "help")
                .map(command_json)
                .collect(),
        ),
    );
    Value::Object(m)
}

/// The complete command tree of the real parser, built the way `clap` builds it before a parse,
/// so the generated help and version arguments and the propagated global flags are in it.
pub(crate) fn tree() -> Value {
    let mut cmd = Cli::command();
    cmd.build();
    let mut v = command_json(&cmd);
    // the version is the producer's, not a contract surface (cli.runtime normalizes it the same way)
    v["version"] = json!("<version>");
    v["about"] = json!(cmd.get_about().map(|s| s.to_string()).unwrap_or_default());
    v
}

/// Every command path of `tree`, root first: `rdy`, `rdy view`, `rdy view create`, and so on.
fn paths(t: &Value, prefix: &str, out: &mut Vec<String>) {
    let name = t["name"].as_str().unwrap_or_default();
    let here = if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix} {name}")
    };
    out.push(here.clone());
    for s in t["subcommands"].as_array().into_iter().flatten() {
        paths(s, &here, out);
    }
}

/// Writes `tree` into the fixture's `cli.tree`, keeping every other member.
fn capture(tree: &Value) {
    let path = fixture_path();
    let mut doc: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    if !doc["cli"].is_object() {
        doc["cli"] = json!({});
    }
    doc["cli"]["tree"] = tree.clone();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("create the fixture directory");
    }
    let text = serde_json::to_string_pretty(&doc).expect("encode the fixture") + "\n";
    std::fs::write(&path, text).expect("write the fixture");
}

/// ok14: the command tree of the real clap parser, with every hidden flag and default, equals the
/// `cli.tree` member of the release contract fixture. Nothing in the tree may be missing from the
/// fixture and nothing in the fixture may be missing from the tree.
#[test]
fn ok14_cli_contract_from_clap_is_complete() {
    let actual = tree();
    if std::env::var_os("RDY_CONTRACT_CAPTURE").is_some() {
        capture(&actual);
    }
    let fixture = read_fixture();
    let want = &fixture["cli"]["tree"];
    assert!(
        want.is_object(),
        "the fixture has no cli.tree member: capture it with RDY_CONTRACT_CAPTURE=1"
    );
    // the paths first, so a missing or added command is named
    let (mut have, mut listed) = (vec![], vec![]);
    paths(&actual, "", &mut have);
    paths(want, "", &mut listed);
    let added: Vec<&String> = have.iter().filter(|p| !listed.contains(p)).collect();
    let dropped: Vec<&String> = listed.iter().filter(|p| !have.contains(p)).collect();
    assert!(
        added.is_empty() && dropped.is_empty(),
        "commands added to the parser: {added:?}; commands dropped from it: {dropped:?}"
    );
    // the contract has substance: the parser has many commands and the walk reaches nested ones
    assert!(
        have.len() > 60,
        "the tree holds only {} commands",
        have.len()
    );
    assert!(
        have.iter().any(|p| p == "rdy view new"),
        "the walk reaches nested subcommands: {have:?}"
    );
    assert_eq!(
        &actual, want,
        "the command tree changed: a public change needs an upgrade or deprecation path (D-TXFHR)"
    );
}

/// The producer's version is not a contract surface: `cli.runtime` records it as `<version>`, and
/// the tree must too, or the release minor and the `(preview)` mark fail the tree comparison.
#[test]
fn ok14_cli_tree_leaves_out_the_producer_version() {
    let t = tree();
    assert_eq!(t["version"], "<version>", "the tree names the version");
    let text = t.to_string();
    let version = env!("CARGO_PKG_VERSION");
    assert!(
        !text.contains(version),
        "the tree holds the producer version {version}"
    );
}
