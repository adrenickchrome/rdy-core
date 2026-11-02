//! The pack coverage section and the decision field `needs_content` (cc2).
use rdy_schema::*;

fn standard() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

/// A pack `p` that extends `rdy-standard` and adds `section`; the first error of either step.
fn try_pack_with(section: &str) -> Result<Pack, SchemaError> {
    let mut set = PackSet::with_builtin();
    set.add_yaml(&format!(
        "name: p\nversion: \"1\"\nextends: rdy-standard\n{section}"
    ))?;
    set.resolve("p")
}

fn pack_with(section: &str) -> Pack {
    try_pack_with(section).unwrap()
}

fn child_of_parent(parent: &str, child: &str) -> Pack {
    let mut set = PackSet::with_builtin();
    set.add_yaml(&format!(
        "name: base\nversion: \"1\"\nextends: rdy-standard\n{parent}"
    ))
    .unwrap();
    set.add_yaml(&format!(
        "name: child\nversion: \"1\"\nextends: base\n{child}"
    ))
    .unwrap();
    set.resolve("child").unwrap()
}

#[test]
fn cc2_defaults_without_a_coverage_section() {
    let p = standard();
    for d in CoverageDimension::ALL {
        assert_eq!(p.coverage_weight(d), 1, "{d:?}");
    }
    assert_eq!((p.dismissed_weight(), p.drift_threshold()), (50, 60));
}

#[test]
fn cc2_a_pack_sets_weights_and_the_two_numbers() {
    let p = pack_with(
        "coverage:\n  weights: {governed: 2, connected: 0}\n  dismissed_weight: 25\n  drift_threshold: 70\n",
    );
    assert_eq!(p.coverage_weight(CoverageDimension::Governed), 2);
    assert_eq!(p.coverage_weight(CoverageDimension::Connected), 0);
    assert_eq!(p.coverage_weight(CoverageDimension::Grounded), 1);
    assert_eq!((p.dismissed_weight(), p.drift_threshold()), (25, 70));
}

#[test]
fn cc2_out_of_range_numbers_are_refused_with_the_exact_text() {
    for (bad, text) in [
        (
            "coverage:\n  dismissed_weight: 101\n",
            "coverage.dismissed_weight is 101; it must be 0 to 100",
        ),
        (
            "coverage:\n  drift_threshold: 0\n",
            "coverage.drift_threshold is 0; it must be 1 to 100",
        ),
        (
            "coverage:\n  weights: {grounded: 0, consistent: 0, current: 0, connected: 0, complete: 0, governed: 0}\n",
            "coverage.weights are all 0; at least one dimension needs a weight",
        ),
    ] {
        match try_pack_with(bad) {
            Err(SchemaError::Invalid(msg)) => assert_eq!(msg, text, "{bad}"),
            other => panic!("{bad}: expected SchemaError::Invalid, got {other:?}"),
        }
    }
}

#[test]
fn cc2_unknown_keys_are_refused() {
    for bad in [
        "coverage:\n  weights: {groundd: 1}\n",
        "coverage:\n  dismised_weight: 10\n",
    ] {
        assert!(try_pack_with(bad).is_err(), "{bad}");
    }
}

#[test]
fn cc2_boundary_values_are_accepted() {
    for ok in [
        "coverage:\n  dismissed_weight: 0\n",
        "coverage:\n  dismissed_weight: 100\n",
        "coverage:\n  drift_threshold: 1\n",
        "coverage:\n  drift_threshold: 100\n",
        "coverage:\n  weights: {grounded: 0, consistent: 0, current: 0, connected: 0, complete: 0, governed: 1}\n",
    ] {
        assert!(try_pack_with(ok).is_ok(), "{ok}");
    }
}

#[test]
fn cc2_a_child_pack_merges_weights_by_key() {
    let p = child_of_parent(
        "coverage:\n  weights: {governed: 3}\n  dismissed_weight: 40\n",
        "coverage:\n  weights: {grounded: 2}\n",
    );
    assert_eq!(p.coverage_weight(CoverageDimension::Governed), 3);
    assert_eq!(p.coverage_weight(CoverageDimension::Grounded), 2);
    assert_eq!(p.dismissed_weight(), 40);
}

#[test]
fn cc2_a_decision_takes_needs_content() {
    let p = standard();
    let f = &p.block_types["decision"].fields["needs_content"];
    assert_eq!((f.kind, f.required), (FieldKind::Bool, false));
}

/// Final review M8 (task 2): a weight has no upper bound; u32::MAX is taken as written (the
/// rollup sums in u64, cc9_a_weight_of_u32_max_does_not_overflow).
#[test]
fn cc2_a_weight_of_u32_max_is_accepted() {
    let p = pack_with("coverage:\n  weights: {grounded: 4294967295}\n");
    assert_eq!(p.coverage_weight(CoverageDimension::Grounded), u32::MAX);
    assert_eq!(p.coverage_weight(CoverageDimension::Consistent), 1);
}

// ---- nb3: coverage.max_docs_per_level (D-YXBXH, O6) ----

#[test]
fn nb3_level_setting_defaults_inherits_and_overrides() {
    assert_eq!(standard().max_docs_per_level(), 8);
    assert_eq!(
        pack_with("coverage:\n  max_docs_per_level: 12\n").max_docs_per_level(),
        12
    );
    // the parent's value is inherited when the child sets another coverage key
    let inherits = child_of_parent(
        "coverage:\n  max_docs_per_level: 20\n",
        "coverage:\n  weights: {grounded: 2}\n",
    );
    assert_eq!(inherits.max_docs_per_level(), 20);
    // a present child value overrides
    let overrides = child_of_parent(
        "coverage:\n  max_docs_per_level: 20\n",
        "coverage:\n  max_docs_per_level: 3\n",
    );
    assert_eq!(overrides.max_docs_per_level(), 3);
    // both ends are accepted
    for n in [1, 100_000] {
        let p = pack_with(&format!("coverage:\n  max_docs_per_level: {n}\n"));
        assert_eq!(p.max_docs_per_level(), n);
    }
    // outside the range: the exact text
    for n in [0u32, 100_001] {
        let bad = format!("coverage:\n  max_docs_per_level: {n}\n");
        match try_pack_with(&bad) {
            Err(SchemaError::Invalid(msg)) => assert_eq!(
                msg,
                format!("coverage.max_docs_per_level is {n}; it must be 1 to 100000"),
                "{bad}"
            ),
            other => panic!("{bad}: expected SchemaError::Invalid, got {other:?}"),
        }
    }
    // the typed parser refuses a negative, a fraction, a string and an unknown key
    for bad in [
        "coverage:\n  max_docs_per_level: -1\n",
        "coverage:\n  max_docs_per_level: 2.5\n",
        "coverage:\n  max_docs_per_level: \"8\"\n",
        "coverage:\n  max_docs_per_level: 4294967296\n",
        "coverage:\n  max_docs_per_levl: 8\n",
    ] {
        assert!(try_pack_with(bad).is_err(), "{bad}");
    }
}
