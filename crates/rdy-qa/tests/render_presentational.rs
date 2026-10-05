//! A presentational block renders without marker, pill or governing row (pres1).
use rdy_engine::Scope;
use rdy_qa::golden::*;

#[test]
fn pres1_presentational_blocks_render_plainly_and_are_flagged_in_json() {
    let (_d, mut r, ids) = presentational_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("t.html")).unwrap();
    let i = h
        .find(&format!("data-rdy-block=\"{}\"", ids.aside))
        .unwrap();
    let b = &h[i..h.len().min(i + 1200)];
    assert!(
        b.contains("<p>Decorative.</p>")
            && !b.contains("class=\"marker\"")
            && !b.contains("<details class=\"why\">")
            && !b.contains("<dt>Governed by</dt>"),
        "{b}"
    );
    let j: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.path().join("t.json")).unwrap()).unwrap();
    let a = j["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == ids.aside.to_string())
        .unwrap();
    assert_eq!(a["presentational"], true);
}
