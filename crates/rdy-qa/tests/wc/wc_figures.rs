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

#[test]
fn msw11_blobs_are_never_written_through_a_linked_blobs_dir() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::create_dir_all(at(&s, "guides/img")).unwrap();
    std::fs::write(at(&s, "guides/img/chart.png"), b"\x89PNG chart").unwrap();
    add_figure(&s, &seed.section, "img/chart.png", "");
    ok(&s, &["commit", "-m", "a figure"]);
    std::fs::remove_dir_all(at(&s, "_blobs")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), at(&s, "_blobs")).unwrap();
    let _ = json(&s, &["checkout", "--force"]);
    assert_eq!(
        std::fs::read_dir(outside.path()).unwrap().count(),
        0,
        "a blob was written outside the working copy"
    );
}

#[test]
fn msw11_a_torn_blob_file_is_repaired_by_checkout() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::create_dir_all(at(&s, "guides/img")).unwrap();
    std::fs::write(at(&s, "guides/img/chart.png"), b"\x89PNG chart").unwrap();
    add_figure(&s, &seed.section, "img/chart.png", "");
    ok(&s, &["commit", "-m", "a figure"]);
    let fig = entries(&s, "guides/intro.yaml")
        .into_iter()
        .find(|e| e["type"].as_str() == Some("figure"))
        .unwrap();
    let hash = fig["fields"]["blob"].as_str().unwrap().to_string();
    let path = at(&s, &format!("_blobs/{hash}.png"));
    std::fs::write(&path, b"\x89PN").unwrap();
    ok(&s, &["checkout", "--force"]);
    assert_eq!(std::fs::read(&path).unwrap(), b"\x89PNG chart");
}

/// Commits one figure and returns the blob's working-copy path and its temp-file name.
fn msw11_committed_blob(s: &Served, section: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    std::fs::create_dir_all(at(s, "guides/img")).unwrap();
    std::fs::write(at(s, "guides/img/chart.png"), b"\x89PNG chart").unwrap();
    add_figure(s, section, "img/chart.png", "");
    ok(s, &["commit", "-m", "a figure"]);
    let fig = entries(s, "guides/intro.yaml")
        .into_iter()
        .find(|e| e["type"].as_str() == Some("figure"))
        .unwrap();
    let hash = fig["fields"]["blob"].as_str().unwrap().to_string();
    (
        at(s, &format!("_blobs/{hash}.png")),
        at(s, &format!("_blobs/.{hash}.png.tmp")),
    )
}

#[test]
fn msw11_checkout_never_writes_a_blob_through_a_link_at_its_temp_name() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let (blob, tmp) = msw11_committed_blob(&s, &seed.section);
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim.txt");
    std::fs::write(&victim, b"keep me").unwrap();
    std::fs::remove_file(&blob).unwrap();
    std::os::unix::fs::symlink(&victim, &tmp).unwrap();
    ok(&s, &["checkout", "--force"]);
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
    assert!(std::fs::symlink_metadata(&blob).unwrap().is_file());
    assert_eq!(std::fs::read(&blob).unwrap(), b"\x89PNG chart");
}

#[test]
fn msw11_commit_never_writes_a_blob_through_a_link_at_its_temp_name() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let (blob, tmp) = msw11_committed_blob(&s, &seed.section);
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim2.txt");
    std::fs::write(&victim, b"keep me").unwrap();
    std::fs::remove_file(&blob).unwrap();
    std::os::unix::fs::symlink(&victim, &tmp).unwrap();
    // a second figure with the same bytes uploads to the same blob name
    std::fs::write(at(&s, "guides/img/again.png"), b"\x89PNG chart").unwrap();
    add_figure(&s, &seed.section, "img/again.png", "");
    ok(&s, &["commit", "-m", "the same figure again"]);
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
    assert!(std::fs::symlink_metadata(&blob).unwrap().is_file());
    assert_eq!(std::fs::read(&blob).unwrap(), b"\x89PNG chart");
}

#[test]
fn hd5_a_custom_asset_types_image_path_becomes_a_blob_hash() {
    let (s, seed) = seeded();
    rdy_qa::packs::use_pack(
        s.dir.path(),
        "blobs",
        rdy_qa::packs::BLOB_PACK,
        "serve:\n  idle_timeout: 60\n",
    );
    ok(&s, &["checkout"]);
    std::fs::create_dir_all(at(&s, "guides/img")).unwrap();
    std::fs::write(at(&s, "guides/img/shot.png"), b"\x89PNG shot").unwrap();
    let mut text = read(&s, "guides/intro.yaml");
    text.push_str(&format!(
        "- type: shot\n  parent: {}\n  fields:\n    blob: img/shot.png\n",
        seed.section
    ));
    std::fs::write(at(&s, "guides/intro.yaml"), text).unwrap();
    ok(&s, &["add", "guides/intro.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "a shot"]);
    assert!(okk, "{v}");
    let shot = entries(&s, "guides/intro.yaml")
        .into_iter()
        .find(|e| e["type"].as_str() == Some("shot"))
        .unwrap();
    let hash = shot["fields"]["blob"].as_str().unwrap().to_string();
    assert_eq!(hash.len(), 64, "{shot:?}");
    assert_eq!(shot["fields"]["media_type"].as_str(), Some("image/png"));
    assert_eq!(
        std::fs::read(at(&s, &format!("_blobs/{hash}.png"))).unwrap(),
        b"\x89PNG shot"
    );
}

/// guard: a type without the asset trait keeps a path in `blob` as text: no blob is stored and
/// nothing is written to `_blobs/`.
#[test]
fn hd5_a_non_asset_types_blob_path_stays_text() {
    let (s, seed) = seeded();
    rdy_qa::packs::use_pack(
        s.dir.path(),
        "blobs",
        rdy_qa::packs::BLOB_PACK,
        "serve:\n  idle_timeout: 60\n",
    );
    ok(&s, &["checkout"]);
    std::fs::create_dir_all(at(&s, "guides/img")).unwrap();
    std::fs::write(at(&s, "guides/img/chart.png"), b"\x89PNG chart").unwrap();
    add_figure(
        &s,
        &seed.section,
        "img/chart.png",
        "    media_type: image/png\n",
    );
    let (okk, v) = json(&s, &["commit", "-m", "a figure without the asset trait"]);
    assert!(okk, "{v}");
    let fig = entries(&s, "guides/intro.yaml")
        .into_iter()
        .find(|e| e["type"].as_str() == Some("figure"))
        .unwrap();
    assert_eq!(fig["fields"]["blob"].as_str(), Some("img/chart.png"));
    let blobs = std::fs::read_dir(at(&s, "_blobs"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(blobs, 0, "nothing in _blobs/");
}

/// The image diagnostics name the entry's type: a custom asset type gets its own name, and the
/// media_type fix speaks of the entry, not of a figure.
#[test]
fn hd5_image_diagnostics_name_the_entrys_type() {
    let (s, seed) = seeded();
    rdy_qa::packs::use_pack(
        s.dir.path(),
        "blobs",
        rdy_qa::packs::BLOB_PACK,
        "serve:\n  idle_timeout: 60\n",
    );
    ok(&s, &["checkout"]);
    std::fs::create_dir_all(at(&s, "guides/img")).unwrap();
    std::fs::write(at(&s, "guides/img/shot.raw"), b"raw").unwrap();
    let mut text = read(&s, "guides/intro.yaml");
    text.push_str(&format!(
        "- type: shot\n  parent: {sec}\n  fields:\n    blob: img/none.png\n\
         - type: shot\n  parent: {sec}\n  fields:\n    blob: img/shot.raw\n",
        sec = seed.section
    ));
    std::fs::write(at(&s, "guides/intro.yaml"), text).unwrap();
    ok(&s, &["add", "guides/intro.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "two shots"]);
    assert!(!okk && v["code"] == E_WC_INVALID, "{v}");
    let diags = v["diagnostics"].as_array().unwrap();
    let find = |tail: &str| {
        diags
            .iter()
            .find(|d| d["message"].as_str().unwrap().ends_with(tail))
            .unwrap_or_else(|| panic!("no diagnostic ending {tail:?}: {v}"))
    };
    let missing = find(": no image at img/none.png");
    assert!(
        missing["message"].as_str().unwrap().starts_with("shot sh-"),
        "{missing}"
    );
    let untyped = find(": set media_type for img/shot.raw");
    assert!(
        untyped["message"].as_str().unwrap().starts_with("shot sh-"),
        "{untyped}"
    );
    assert_eq!(
        untyped["fix"], "add media_type, e.g. image/png, to the entry's fields",
        "{untyped}"
    );
}
