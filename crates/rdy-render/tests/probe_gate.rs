//! Ruling task-13-round1: the render-entry probe exposes nothing unless the `probe` feature is on.
//!
//! The renderer is built with and without the feature, so the test reads the declarations: a
//! public `probe` module must sit behind `cfg(feature = "probe")`, and the feature-off stand-in
//! must be private with a no-op `hit()` only.

fn lines(src: &str) -> Vec<&str> {
    src.lines().map(str::trim).collect()
}

#[test]
fn hpb13_the_public_probe_module_exists_only_with_the_probe_feature() {
    let lib = include_str!("../src/lib.rs");
    let l = lines(lib);
    let pubs: Vec<usize> = l
        .iter()
        .enumerate()
        .filter(|(_, s)| s.starts_with("pub mod probe"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(pubs.len(), 1, "one public probe module declaration");
    assert_eq!(
        l[pubs[0] - 1],
        "#[cfg(feature = \"probe\")]",
        "the public probe module is gated on the probe feature"
    );
    // the feature-off stand-in is private: `mod probe` with a no-op hit and no public item
    let off = l
        .iter()
        .position(|s| *s == "#[cfg(not(feature = \"probe\"))]")
        .expect("a feature-off stand-in");
    assert!(
        l[off + 1].starts_with("mod probe"),
        "the stand-in is a private module: {}",
        l[off + 1]
    );
    let body: String = l[off + 1..].join("\n");
    let stand_in = &body[..body.find("\n}").expect("the stand-in ends")];
    assert!(
        !stand_in.contains("pub fn") && !stand_in.contains("pub mod"),
        "{stand_in}"
    );
    assert!(stand_in.contains("fn hit() {}"), "{stand_in}");
}

#[test]
fn hpb13_probe_rs_has_one_gate_in_lib_rs_and_none_of_its_own() {
    // the whole file is behind the one gate above, so it holds no feature switch that could
    // leave a public item in a feature-off build
    let src = include_str!("../src/probe.rs");
    assert!(!src.contains("cfg("), "probe.rs holds no cfg of its own");
}
