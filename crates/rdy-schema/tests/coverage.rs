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
fn cc2_out_of_range_and_unknown_keys_are_refused() {
    for bad in [
        "coverage:\n  dismissed_weight: 101\n",
        "coverage:\n  drift_threshold: 0\n",
        "coverage:\n  weights: {grounded: 0, consistent: 0, current: 0, connected: 0, complete: 0, governed: 0}\n",
        "coverage:\n  weights: {groundd: 1}\n",
        "coverage:\n  dismised_weight: 10\n",
    ] {
        assert!(try_pack_with(bad).is_err(), "{bad}");
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
