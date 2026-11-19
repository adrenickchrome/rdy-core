//! ap3 (D-ZWP7Q, D-WB22B, D-T5S9A; M3c O18): `coverage.gate` is a project setting, `off` or
//! `no_lower`, default `off`. A value outside the list warns and reads as `off`; only the
//! project config sets it; the gate settings feed the assessment's config digest.
use rdy_engine::config_levels::{GivenFiles, KEY_LEVELS, Level, effective, effective_from};
use rdy_engine::readiness::assessment::{GateConfig, config_digest};
use rdy_engine::{CoverageGate, E_CONFIG, Rdy};
use rdy_model::Hash;
use serde_yaml_ng::Value;

const KEY: &str = "coverage.gate";

fn effective_of(yaml: &str) -> rdy_engine::config_levels::Effective {
    let d = tempfile::tempdir().unwrap();
    effective_from(
        d.path(),
        None,
        GivenFiles {
            project: yaml.as_bytes(),
            local: None,
        },
    )
    .unwrap()
}

fn origin(e: &rdy_engine::config_levels::Effective) -> (Value, Level) {
    let o = e.origins.iter().find(|o| o.key == KEY).expect("origin");
    (o.value.clone(), o.level)
}

#[test]
fn ap3_coverage_gate_is_off_by_default() {
    // the key sits right after `coverage.target`, project level only, default `off`
    let keys: Vec<&str> = KEY_LEVELS.iter().map(|r| r.key).collect();
    let at = keys.iter().position(|k| *k == "coverage.target").unwrap();
    assert_eq!(keys[at + 1], KEY);
    let rule = &KEY_LEVELS[at + 1];
    assert_eq!(rule.levels, &[Level::Project]);
    assert_eq!(rule.default, Some("off"));

    assert_eq!(CoverageGate::Off.as_str(), "off");
    assert_eq!(CoverageGate::NoLower.as_str(), "no_lower");

    // unset: the default level, no warning
    let e = effective_of("pack: rdy-standard\n");
    assert_eq!(origin(&e), (Value::from("off"), Level::Default));
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);

    // a new project and an open project with a `coverage:` section that sets no gate read `off`
    let d = tempfile::tempdir().unwrap();
    let r = Rdy::init(d.path(), "rdy-standard").unwrap();
    assert_eq!(r.read().coverage_gate(), CoverageGate::Off);
    drop(r);
    std::fs::write(
        d.path().join(".rdy.yaml"),
        "pack: rdy-standard\ncoverage:\n  target: 60\n",
    )
    .unwrap();
    let r = Rdy::open(d.path()).unwrap();
    assert_eq!(r.read().coverage_gate(), CoverageGate::Off);
    assert_eq!(r.read().coverage_target(), 60);
}

#[test]
fn ap3_no_lower_reads_from_rdy_yaml() {
    let e = effective_of("pack: rdy-standard\ncoverage:\n  gate: no_lower\n");
    assert_eq!(origin(&e), (Value::from("no_lower"), Level::Project));
    assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    let e = effective_of("pack: rdy-standard\ncoverage:\n  gate: off\n");
    assert_eq!(origin(&e), (Value::from("off"), Level::Project));

    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    std::fs::write(
        d.path().join(".rdy.yaml"),
        "pack: rdy-standard\ncoverage:\n  gate: no_lower\n",
    )
    .unwrap();
    let r = Rdy::open(d.path()).unwrap();
    assert_eq!(r.read().coverage_gate(), CoverageGate::NoLower);
    // the other coverage key keeps its default
    assert_eq!(r.read().coverage_target(), 80);
    // the gate settings a readiness read takes
    let g = r.read().gate_config();
    assert_eq!(g.coverage_gate, CoverageGate::NoLower);
    assert!(!g.require_up_to_date);
}

#[test]
fn ap3_another_value_warns_and_uses_off() {
    for bad in ["sometimes", "No_Lower", "on", "\"\""] {
        let e = effective_of(&format!("pack: rdy-standard\ncoverage:\n  gate: {bad}\n"));
        assert_eq!(e.warnings.len(), 1, "{bad:?}: {:?}", e.warnings);
        let w = &e.warnings[0];
        assert_eq!(w.code, "W_CONFIG_UNKNOWN", "{bad:?}");
        assert_eq!(
            w.message, "coverage.gate must be off or no_lower; using off",
            "{bad:?}"
        );
        assert_eq!(w.fix, "set coverage.gate to off or no_lower", "{bad:?}");
        assert_eq!(origin(&e).0, Value::from("off"), "{bad:?}");
    }
    // the project opens and reads `off`
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    std::fs::write(
        d.path().join(".rdy.yaml"),
        "pack: rdy-standard\ncoverage:\n  gate: sometimes\n",
    )
    .unwrap();
    let r = Rdy::open(d.path()).unwrap();
    assert_eq!(r.read().coverage_gate(), CoverageGate::Off);

    // a value that is not a string is the existing type refusal
    std::fs::write(
        d.path().join(".rdy.yaml"),
        "pack: rdy-standard\ncoverage:\n  gate: 5\n",
    )
    .unwrap();
    let err = effective(d.path(), None).unwrap_err();
    assert_eq!(err.code, E_CONFIG);
    assert!(
        err.message
            .starts_with("coverage.gate in .rdy.yaml: invalid type"),
        "{}",
        err.message
    );
}

#[test]
fn ap3_coverage_gate_in_local_yaml_warns_and_uses_off() {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    std::fs::create_dir_all(d.path().join(".rdy")).unwrap();
    std::fs::write(
        d.path().join(".rdy/local.yaml"),
        "coverage:\n  gate: no_lower\n",
    )
    .unwrap();
    let e = effective(d.path(), None).unwrap();
    assert_eq!(origin(&e), (Value::from("off"), Level::Default));
    assert_eq!(e.warnings.len(), 1, "{:?}", e.warnings);
    let w = &e.warnings[0];
    assert_eq!(w.code, "W_CONFIG_UNKNOWN");
    assert_eq!(
        w.message,
        "coverage.gate in .rdy/local.yaml belongs in .rdy.yaml; rdy ignores it here"
    );
    assert_eq!(w.fix, "move coverage.gate to .rdy.yaml");
    let r = Rdy::open(d.path()).unwrap();
    assert_eq!(r.read().coverage_gate(), CoverageGate::Off);
}

#[test]
fn ap3_config_digest_follows_o18() {
    let g = |require_up_to_date, coverage_gate| GateConfig {
        require_up_to_date,
        coverage_gate,
    };
    let off = config_digest(&g(false, CoverageGate::Off));
    let no_lower = config_digest(&g(false, CoverageGate::NoLower));
    assert_ne!(off, no_lower, "the gate moves the digest");
    assert_ne!(
        off,
        config_digest(&g(true, CoverageGate::Off)),
        "require_up_to_date still moves it"
    );
    assert_ne!(
        no_lower,
        config_digest(&g(true, CoverageGate::NoLower)),
        "require_up_to_date still moves it"
    );
    // the bytes: the domain, then the two keys in this order
    for (require, gate, word) in [
        (false, CoverageGate::Off, "off"),
        (true, CoverageGate::Off, "off"),
        (false, CoverageGate::NoLower, "no_lower"),
        (true, CoverageGate::NoLower, "no_lower"),
    ] {
        let want = format!(
            "rdy-assessment-config-1\0{{\"coverage_gate\":\"{word}\",\"require_up_to_date\":{require}}}"
        );
        assert_eq!(
            config_digest(&g(require, gate)),
            Hash::of(want.as_bytes()),
            "{word} {require}"
        );
    }
}
