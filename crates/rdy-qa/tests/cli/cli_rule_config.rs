//! rtc1: `rules_from_chat` is a policy field, never a configuration key (D-ZSRBN). At every
//! configuration level it is an unknown key: it warns with `W_CONFIG_UNKNOWN`, and rdy keeps
//! going and gives it no effect.
use rdy_engine::config_levels::*;
use rdy_engine::*;
use std::path::{Path, PathBuf};

fn proj() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    let p = d.path().to_path_buf();
    (d, p)
}

fn append(p: &Path, rel: &str, text: &str) {
    let f = p.join(rel);
    let mut t = std::fs::read_to_string(&f).unwrap_or_default();
    t.push_str(text);
    std::fs::write(f, t).unwrap();
}

fn user(text: &str) -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("config.yaml");
    std::fs::write(&f, text).unwrap();
    (d, f)
}

/// The `(message, fix)` of each `W_CONFIG_UNKNOWN` warning, in order.
fn unknown(e: &Effective) -> Vec<(String, String)> {
    e.warnings
        .iter()
        .filter(|w| w.code == "W_CONFIG_UNKNOWN")
        .map(|w| (w.message.clone(), w.fix.clone()))
        .collect()
}

/// Guard: the key is unknown today and stays unknown. The config key tables do not change.
#[test]
fn rtc1_config_rule_setting_warns_and_has_no_effect() {
    // the baseline: the same project with no such key
    let (_d, p) = proj();
    let (_u, none) = user("views:\n  idle_days: 3\n");
    let base = effective(&p, Some(&none)).unwrap();
    assert!(unknown(&base).is_empty(), "{:?}", base.warnings);
    let base_keys: Vec<String> = base.origins.iter().map(|o| o.key.clone()).collect();

    // every level, with the key at the top, nested under a known section and nested under an
    // unknown one, and with close typos of the key
    let cases: [(&str, &str, &str); 9] = [
        ("project", "rules_from_chat: card\n", "rules_from_chat"),
        (
            "project",
            "policy:\n  rules_from_chat: card\n",
            "policy.rules_from_chat",
        ),
        ("project", "rules_from_cat: card\n", "rules_from_cat"),
        ("local", "rules_from_chat: card\n", "rules_from_chat"),
        (
            "local",
            "instructions:\n  rules_from_chat: land\n",
            "instructions.rules_from_chat",
        ),
        ("local", "rules_form_chat: land\n", "rules_form_chat"),
        ("user", "rules_from_chat: card\n", "rules_from_chat"),
        (
            "user",
            "views:\n  rules_from_chat: card\n",
            "views.rules_from_chat",
        ),
        ("user", "rule_from_chat: card\n", "rule_from_chat"),
    ];
    for (level, text, key) in cases {
        let (_d, p) = proj();
        let (_u, uf) = user(if level == "user" {
            text
        } else {
            "views:\n  idle_days: 3\n"
        });
        let (file, label) = match level {
            "project" => {
                append(&p, PROJECT_FILE, text);
                (PROJECT_FILE.to_string(), "project")
            }
            "local" => {
                append(&p, LOCAL_FILE, text);
                (LOCAL_FILE.to_string(), "local")
            }
            _ => (uf.display().to_string(), "user"),
        };
        let e = effective(&p, Some(&uf)).unwrap_or_else(|e| panic!("{level} {text:?}: {e}"));
        // the exact existing warning: one per key, naming the file and the level, no suggestion
        assert_eq!(
            unknown(&e),
            vec![(
                format!("unknown key {key} in {file} ({label} level); rdy ignores it"),
                format!("remove {key} from {file}, or check its spelling")
            )],
            "{level} {text:?}"
        );
        // no effect: it is no origin row, and every key and value is as before
        let keys: Vec<String> = e.origins.iter().map(|o| o.key.clone()).collect();
        assert!(!keys.iter().any(|k| k.contains("rules_f")), "{keys:?}");
        assert_eq!(keys, base_keys, "{level} {text:?}");
        assert!(Rdy::open(&p).is_ok(), "open accepts it: {level} {text:?}");
    }

    // beside a real typo the other warning keeps its suggestion
    let (_d, p) = proj();
    append(
        &p,
        PROJECT_FILE,
        "decision:\n  home: docs\nrules_from_chat: card\n",
    );
    let found = unknown(&effective(&p, None).unwrap());
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found
            .iter()
            .any(|w| w.0.contains("decision.home") && w.1.contains("decisions.home"))
    );
    assert!(
        found
            .iter()
            .any(|w| w.0.contains("unknown key rules_from_chat"))
    );
}
