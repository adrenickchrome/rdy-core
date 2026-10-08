//! wcfg1: a figure enters through a file: commit stores the image as a blob, copies it to
//! _blobs/<hash>.<ext> and rewrites the reference to the hash (spec §12.1, D-7BKK5).
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::wc::*;

fn add_figure(s: &Served, section: &str, blob: &str, extra: &str) {
    let mut text = read(s, "guides/intro.yaml");
    text.push_str(&format!("- type: figure\n  parent: {section}\n  fields:\n    blob: {blob}\n    alt: A chart\n{extra}"));
    std::fs::write(at(s, "guides/intro.yaml"), text).unwrap();
    ok(s, &["add", "guides/intro.yaml"]);
}

#[test]
fn wcfg1_a_figure_path_becomes_a_blob_hash() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::create_dir_all(at(&s, "guides/img")).unwrap();
    std::fs::write(at(&s, "guides/img/chart.png"), b"\x89PNG chart").unwrap();
    add_figure(&s, &seed.section, "img/chart.png", "");
    let (okk, v) = json(&s, &["commit", "-m", "a figure"]);
    assert!(okk, "{v}");
    let fig = entries(&s, "guides/intro.yaml")
        .into_iter()
        .find(|e| e["type"].as_str() == Some("figure"))
        .unwrap();
    let hash = fig["fields"]["blob"].as_str().unwrap().to_string();
    assert_eq!(hash.len(), 64, "{fig:?}");
    assert_eq!(fig["fields"]["media_type"].as_str(), Some("image/png"));
    assert_eq!(
        std::fs::read(at(&s, &format!("_blobs/{hash}.png"))).unwrap(),
        b"\x89PNG chart"
    );
}

#[test]
fn wcfg1_a_missing_outside_or_untyped_image_is_e_wc_invalid() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    add_figure(&s, &seed.section, "img/none.png", "");
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(!okk && v["code"] == E_WC_INVALID, "{v}");
    assert!(
        v["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("img/none.png"),
        "{v}"
    );
    ok(&s, &["checkout", "--force"]);
    add_figure(
        &s,
        &seed.section,
        "../../../etc/hosts",
        "    media_type: text/plain\n",
    );
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(
        !okk && v["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("outside the working copy"),
        "{v}"
    );
    ok(&s, &["checkout", "--force"]);
    std::fs::write(at(&s, "guides/pic.bmp"), b"BM").unwrap();
    add_figure(&s, &seed.section, "pic.bmp", "");
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(
        !okk && v["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("set media_type"),
        "{v}"
    );
}
