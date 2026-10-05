//! FW-D5: the page JSON twin and llms.txt (json1).
use rdy_engine::Scope;
use rdy_qa::golden::*;

#[test]
fn json1_every_page_has_a_twin_whose_ids_match_the_pages_block_ids() {
    let (_d, mut r, ids) = decided_site();
    let out = tempfile::tempdir().unwrap();
    let s = rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let html_pages: Vec<_> = pages(out.path())
        .into_iter()
        .filter(|p| !p.to_string_lossy().contains("/go/"))
        .collect();
    assert_eq!(s.json, html_pages.len());
    for p in html_pages {
        let twin = p.with_extension("json");
        let j: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&twin).unwrap_or_else(|_| panic!("{}", twin.display())),
        )
        .unwrap();
        assert_eq!(j["schema"], "rdy-page/1");
        let h = std::fs::read_to_string(&p).unwrap();
        let mut in_page: Vec<String> = h
            .match_indices("data-rdy-block=\"")
            .map(|(i, _)| h[i + 16..].split('"').next().unwrap().to_string())
            .collect();
        let mut in_json: Vec<String> = j["ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        in_page.sort();
        in_json.sort();
        assert_eq!(in_page, in_json, "{}", p.display());
        assert!(h.contains(&format!(
            "<link rel=\"alternate\" type=\"application/json\" href=\"{}\">",
            twin.file_name().unwrap().to_str().unwrap()
        )));
    }
    let j: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.path().join("ledgers.json")).unwrap())
            .unwrap();
    let req = j["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == ids.requirement.to_string())
        .unwrap();
    assert_eq!(req["anchor"], "ledgers-are-atomic-when-settled");
    assert_eq!(req["type"], "requirement");
    assert!(
        req["governing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["id"] == ids.d1.to_string() && g["status"] == "live")
    );
    let d1 = j["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == ids.d1.to_string())
        .unwrap();
    assert_eq!(d1["supersedes"][0], ids.d0.to_string());
    assert_eq!(d1["decided_at"], "2021-02-08T10:00:00Z");
    assert_eq!(j["page"]["permalink"], format!("go/{}.html", ids.doc));
    assert_eq!(j["links"]["search"], "search-index.json");
}

/// The twin's half of the test below, which needs no sibling task.
#[test]
fn json1_the_version_block_carries_the_version_and_its_landing() {
    let (_d, mut r) = tiny_with_doc();
    r.tag(&approver(), "v1.0.0", None, Some("first".into()))
        .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let j: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.path().join("index.json")).unwrap())
            .unwrap();
    let v = &j["version"];
    assert_eq!(v["landing"].as_str().unwrap().len(), 64);
    assert_eq!(v["name"], "v1.0.0");
    assert_eq!(v["label"], "v1.0.0");
    assert_eq!(
        (
            v["landings_since"].as_u64(),
            v["frozen"].as_bool(),
            &v["view"]
        ),
        (Some(0), Some(false), &serde_json::Value::Null)
    );
    assert_eq!(
        (
            j["page"]["kind"].as_str(),
            &j["page"]["id"],
            &j["page"]["permalink"]
        ),
        (
            Some("home"),
            &serde_json::Value::Null,
            &serde_json::Value::Null
        )
    );
    assert!(
        j["blocks"].as_array().unwrap().is_empty()
            && j["ids"].as_array().unwrap().is_empty()
            && j["decisions"].as_array().unwrap().is_empty()
    );
}

#[test]
#[ignore = "SITE-BAR: Task 12 (until the home page stops showing the landing hash)"]
fn json1_the_version_block_carries_the_hash_the_html_never_shows() {
    let (_d, mut r) = tiny_with_doc();
    r.tag(&approver(), "v1.0.0", None, Some("first".into()))
        .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let j: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.path().join("index.json")).unwrap())
            .unwrap();
    let landing = j["version"]["landing"].as_str().unwrap();
    assert_eq!(landing.len(), 64);
    assert_eq!(j["version"]["name"], "v1.0.0");
    assert_eq!(j["version"]["label"], "v1.0.0");
    assert!(
        !std::fs::read_to_string(out.path().join("index.html"))
            .unwrap()
            .contains(&landing[..12])
    );
}

#[test]
fn json1_llms_txt_lists_pages_in_tree_order_with_the_reference_pages() {
    let (_d, mut r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let t = std::fs::read_to_string(out.path().join("llms.txt")).unwrap();
    assert!(t.starts_with("# Tree\n\n> "));
    let i = |s: &str| t.find(s).unwrap_or_else(|| panic!("{s} in {t}"));
    assert!(
        i("- [Intro](intro.html)") < i("- [Alpha](a/index.html)")
            && i("- [Alpha](a/index.html)") < i("  - [One](a/one.html)")
            && i("  - [One](a/one.html)") < i("- [Beta](b/index.html)")
    );
    assert!(
        t.contains("- [Glossary](glossary.html): 1 term")
            && t.contains("- [Open questions](questions.html): 1 question")
            && t.contains("- [Search index](search-index.json)")
    );
    assert!(!t.contains("http"));
}

/// Review Focus 5.
#[test]
fn json1_hostile_text_is_valid_json_on_every_page_file() {
    let (_d, mut r) = hostile();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    for p in pages(out.path()) {
        if p.to_string_lossy().contains("/go/") {
            continue;
        }
        let twin = p.with_extension("json");
        let text = std::fs::read_to_string(&twin).unwrap();
        let _: serde_json::Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", twin.display()));
    }
    let t = std::fs::read_to_string(out.path().join("llms.txt")).unwrap();
    assert!(
        !t.contains("](javascript:"),
        "a hostile title cannot become a link scheme: {t}"
    );
}

/// The keys of the JSON object that sits at `indent` spaces in the pretty-printed `text`,
/// in file order (a `Value` would have sorted them).
fn keys_at(text: &str, indent: usize, from: usize) -> Vec<String> {
    let pad = " ".repeat(indent);
    let mut keys = vec![];
    for line in text[from..].lines() {
        if let Some(rest) = line.strip_prefix(&pad)
            && let Some(rest) = rest.strip_prefix('"')
            && let Some((key, _)) = rest.split_once("\": ")
        {
            keys.push(key.to_string());
        }
        // the object's closing brace at one level up ends it
        if line.starts_with(&format!("{}}}", " ".repeat(indent - 2))) && !keys.is_empty() {
            break;
        }
    }
    keys
}

/// The brief fixes the order of the twin's keys: the file reads top to bottom like the spec.
#[test]
fn json1_the_twins_keys_come_in_the_documented_order() {
    let (_d, mut r, _ids) = decided_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let text = std::fs::read_to_string(out.path().join("ledgers.json")).unwrap();
    assert!(text.ends_with("}\n"));
    let top: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix("  \""))
        .filter_map(|l| l.split_once("\": ").map(|(k, _)| k.to_string()))
        .collect();
    assert_eq!(
        top,
        [
            "schema",
            "site",
            "version",
            "page",
            "ids",
            "blocks",
            "decisions",
            "links"
        ]
    );
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle} in {text}"))
    };
    assert_eq!(
        keys_at(&text, 4, at("  \"site\": {")),
        ["name", "description"]
    );
    assert_eq!(
        keys_at(&text, 4, at("  \"version\": {")),
        [
            "label",
            "name",
            "landing",
            "landings_since",
            "view",
            "frozen"
        ]
    );
    assert_eq!(
        keys_at(&text, 4, at("  \"page\": {")),
        [
            "kind",
            "id",
            "url",
            "title",
            "doc_type",
            "lifecycle",
            "permalink"
        ]
    );
    assert_eq!(
        keys_at(&text, 6, at("  \"blocks\": [")),
        [
            "id",
            "type",
            "anchor",
            "parent",
            "title",
            "summary",
            "lifecycle",
            "governing",
            "suspect",
            "presentational"
        ]
    );
    assert_eq!(
        keys_at(&text, 6, at("  \"decisions\": [")),
        [
            "id",
            "statement",
            "status",
            "decided_at",
            "authority_basis",
            "options",
            "supersedes",
            "revokes",
            "affects",
            "governs"
        ]
    );
    assert_eq!(
        keys_at(&text, 4, at("  \"links\": {")),
        ["prev", "next", "parent", "search", "index"]
    );
}

/// A decision that affects nothing and that nothing names (what `rdy decide` writes by
/// default) governs its parent section: the section's twin lists it, so an agent reading the
/// page sees every decision the `go/<D>.html` permalink leads to.
#[test]
fn json1_a_parent_only_decision_governs_its_section_in_the_twin() {
    let (_d, mut r) = hostile();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let j: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.path().join("evil/page.json")).unwrap())
            .unwrap();
    let section = j["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "section")
        .unwrap();
    let governing = section["governing"].as_array().unwrap();
    assert_eq!(governing.len(), 1, "{section}");
    let d = governing[0]["id"].as_str().unwrap();
    assert_eq!(governing[0]["status"], "live");
    let decisions = j["decisions"].as_array().unwrap();
    let dj = decisions
        .iter()
        .find(|x| x["id"] == d)
        .unwrap_or_else(|| panic!("{d} in {decisions:?}"));
    assert_eq!(dj["governs"], serde_json::json!([section["id"]]));
    assert_eq!(dj["affects"], serde_json::json!([]));
}
