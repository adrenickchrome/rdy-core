//! The QA test pack's areas and tensions, and hostile sizes (qapack1).
use rdy_qa::packs::*;
use rdy_schema::*;

#[test]
fn qapack1_the_qa_pack_declares_three_areas_and_one_override() {
    let (_dir, r) = qa_project();
    let p = r.pack();
    assert_eq!(p.name, "rdy-qa");
    assert_eq!(
        p.areas.keys().cloned().collect::<Vec<_>>(),
        vec!["glossary", "identity", "payments"]
    );
    assert_eq!(p.areas_for_path("wallets/transactions"), vec!["payments"]);
    assert_eq!(
        p.areas_for_path("identity/users/glossary"),
        vec!["glossary", "identity"]
    );
    assert_eq!(
        p.tension_severity(TensionKind::UnjustifiedChange),
        (TensionSeverity::Blocking, true)
    );
    assert_eq!(
        p.tension_severity(TensionKind::Fork),
        (TensionSeverity::Blocking, false)
    );
    assert!(
        p.block_types.contains_key("claim"),
        "still extends rdy-standard"
    );
}

#[test]
fn qapack1_a_thousand_areas_with_a_hundred_globs_each_resolve_and_answer() {
    let mut yaml = String::from("name: big\nversion: \"1\"\nextends: rdy-standard\nareas:\n");
    for a in 0..1000 {
        yaml.push_str(&format!("  area{a}:\n    paths: ["));
        for g in 0..100 {
            yaml.push_str(&format!("'f{a}/**/d{g}', "));
        }
        yaml.push_str("]\n");
    }
    // how fast: the perf tier's `pack_resolve_1000_areas` and `areas_lookup_100k_globs`
    let mut set = PackSet::with_builtin();
    set.add_yaml(&yaml).unwrap();
    let p = set.resolve("big").unwrap();
    assert_eq!(p.areas_for_path("f999/x/y/d99"), vec!["area999"]);
}

#[test]
fn qapack1_pathological_globs_in_a_pack_are_bounded() {
    let mut set = PackSet::with_builtin();
    let pat = format!("{}/a", vec!["**"; 60].join("/"));
    // 100 bare stars would be `BadDoubleStar`; interleave `?` to keep 50 stars in one segment.
    set.add_yaml(&format!("name: p\nversion: \"1\"\nextends: rdy-standard\nareas:\n  a: {{ paths: ['{pat}', '{}a'] }}\n", "*?".repeat(50))).unwrap();
    let p = set.resolve("p").unwrap();
    let path = vec!["b"; MAX_PATH_SEGMENTS].join("/");
    // how fast: the perf tier's `areas_lookup_pathological`
    for _ in 0..100 {
        assert!(p.areas_for_path(&path).is_empty());
    }
    // bounded: the matcher's steps stay polynomial in the pattern and the path, whatever the
    // stars (a backtracking matcher takes exponentially many on these)
    let segs = |x: &str| x.split('/').count();
    for g in [pat.clone(), format!("{}a", "*?".repeat(50))] {
        let glob = Glob::compile(&g).unwrap();
        let (covered, steps) = glob.covers_counting(&path);
        assert!(!covered, "{g}");
        let bound = ((g.len() + segs(&g) + 1) * (path.len() + segs(&path) + 1)) as u64;
        assert!(steps <= bound, "{g}: {steps} steps, bound {bound}");
    }
    // and on a long single name against the starred segment
    let name = "b".repeat(200);
    let g = format!("{}a", "*?".repeat(50));
    let (_, steps) = Glob::compile(&g).unwrap().covers_counting(&name);
    let bound = ((g.len() + 2) * (name.len() + 2)) as u64;
    assert!(steps <= bound, "{steps} steps, bound {bound}");
    let long = "a".repeat(MAX_GLOB_LEN + 1);
    set.add_yaml(&format!(
        "name: q\nversion: \"1\"\nextends: rdy-standard\nareas:\n  a: {{ paths: ['{long}'] }}\n"
    ))
    .unwrap();
    let e = set.resolve("q").unwrap_err();
    assert!(e.to_string().contains("at most 256 bytes"), "{e}");
}
