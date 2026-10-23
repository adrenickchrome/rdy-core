//! rf7 (D-VPATZ): the site's Why trail finds a block's governing decisions through the one graph
//! route query. A decision that affects an entity governs each block that describes the entity
//! (`describes/affects`), next to `decided_by`, `implements` and `affects`. The trail stays prose
//! for readers, keeps its lineage, escaping and selection rules, and a finding or a source
//! stays supporting: no page, no navigation entry, no search or llms text.
use rdy_engine::site_target::SiteTarget;
use rdy_engine::{DecisionInput, Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::fixtures::{land, project, write, writer};
use rdy_qa::golden::{EVIL, tiny};
use rdy_render::live::{ProfileBuild, ProfileHistoryMemo, build_profile_live};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn decide(r: &mut Rdy, parent: &str, statement: &str, affects: &[&str], depends_on: &[&str]) {
    r.decide(
        &writer(),
        "work",
        DecisionInput {
            parent: parent.into(),
            statement: statement.into(),
            affects: affects.iter().map(|s| s.to_string()).collect(),
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("decide {statement}: {e}"));
}

/// The trail of block `id` on `html`: the text of its `Why` details, if it has one. A block's
/// wrapper runs up to the next block's wrapper.
fn trail_of(html: &str, id: &str) -> Option<String> {
    let at = html.find(&format!("data-rdy-block=\"{id}\""))?;
    let rest = &html[at..];
    let end = rest[1..]
        .find("data-rdy-block=\"")
        .map_or(rest.len(), |e| e + 1);
    let own = &rest[..end];
    let from = own.find("<details class=\"why\">")?;
    let to = own[from..].find("</details>")?;
    Some(own[from..from + to].to_string())
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Every file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                todo.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn rf7_site_why_includes_affected_described_entities() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Ledger"}},
        {"op": "create", "type": "entity", "id": "$e1", "parent": "$s", "fields": {"name": "Ledger service"}},
        {"op": "create", "type": "entity", "id": "$e2", "parent": "$s", "fields": {"name": "Billing service"}},
        {"op": "create", "type": "entity", "id": "$e3", "parent": "$s", "fields": {"name": "Audit service"}},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "Entries are append-only.",
         "links": {"describes": [{"target": "$e1"}, {"target": "$e3"}]}},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Billing runs nightly.",
         "links": {"describes": [{"target": "$e2"}]}},
        {"op": "create", "type": "claim", "id": "$c3", "parent": "$s", "body": "Nothing links this claim."},
        {"op": "create", "type": "claim", "id": "$c4", "parent": "$s", "body": "The Ledger service closes daily."}
    ]))
    .unwrap();
    let id = |k: &str| w.created[k].clone();
    decide(
        &mut r,
        &id("$s"),
        "Ledger entries are never rewritten.",
        &[&id("$e1"), &id("$e3")],
        &[],
    );
    decide(&mut r, &id("$s"), "Billing is nightly.", &[&id("$e2")], &[]);
    // homed beside the entities, but it affects nothing
    decide(&mut r, &id("$s"), "Home-only ruling.", &[], &[]);
    // names an entity and references a claim in its words, with no link
    decide(
        &mut r,
        &id("$s"),
        &format!("Mention of Ledger service and {{{{{}}}}} only.", id("$c1")),
        &[],
        &[],
    );
    land(&mut r, "work").unwrap();

    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = read(out.path(), "ops.html");

    let c1 = trail_of(&h, &id("$c1")).expect("the described claim has a trail");
    assert_eq!(
        c1.matches("Ruled: Ledger entries are never rewritten.")
            .count(),
        1,
        "once, though two entities lead to it: {c1}"
    );
    assert_eq!(c1.matches("<li").count(), 1, "{c1}");
    for other in ["Billing is nightly", "Home-only ruling", "Mention of"] {
        assert!(!c1.contains(other), "{other}: {c1}");
    }
    let c2 = trail_of(&h, &id("$c2")).expect("the second claim has a trail");
    assert!(c2.contains("Ruled: Billing is nightly."), "{c2}");
    assert!(!c2.contains("Ledger entries"), "{c2}");
    for k in ["$c3", "$c4"] {
        assert_eq!(trail_of(&h, &id(k)), None, "{k} has no governing decision");
    }
}

/// A world with a guide folder that two profiles select and a `secret` doc they do not. The
/// guide claim describes a guide entity and the secret doc's entity.
struct Guide {
    _dir: tempfile::TempDir,
    r: Rdy,
    claim: BlockId,
}

fn guide() -> Guide {
    let (dir, mut r) = tiny();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$g", "fields": {"name": "guide", "title": "Guide", "description": "Guide folder."}},
        {"op": "create", "type": "doc", "id": "$intro", "parent": "$g", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$is", "parent": "$intro", "fields": {"title": "Intro section"}},
        {"op": "create", "type": "entity", "id": "$ge", "parent": "$is", "fields": {"name": EVIL}},
        {"op": "create", "type": "doc", "id": "$secret", "fields": {"name": "secret", "title": "SECRETCANARY page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$ss", "parent": "$secret", "fields": {"title": "SECRETCANARY section"}},
        {"op": "create", "type": "entity", "id": "$se", "parent": "$ss", "fields": {"name": "SECRETCANARY entity"}},
        {"op": "create", "type": "finding", "id": "$sf", "parent": "$ss", "fields": {"statement": "SECRETCANARY finding.", "evidence": [{"quote": "SECRETCANARY quote."}]}},
        {"op": "create", "type": "claim", "id": "$gc", "parent": "$is", "body": "Guide words.",
         "links": {"describes": [{"target": "$ge"}, {"target": "$se"}]}}
    ]))
    .unwrap();
    let id = |k: &str| w.created[k].clone();
    decide(&mut r, &id("$is"), EVIL, &[&id("$ge")], &[]);
    decide(
        &mut r,
        &id("$ss"),
        "SECRETCANARY ruling.",
        &[&id("$ge")],
        &[],
    );
    decide(
        &mut r,
        &id("$is"),
        "Crosses the selection.",
        &[&id("$se")],
        &[&id("$sf")],
    );
    land(&mut r, "work").unwrap();
    let admin = write(&mut r, "admin", json!([
        {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
    ]))
    .unwrap()
    .created["$a"]
        .clone();
    let profile = |key: &str, audience: &str, extra: Value| {
        let mut fields = json!({"key": key, "title": format!("{key} profile"),
            "description": format!("About {key}."), "audience": audience,
            "select": [id("$g")]});
        for (k, v) in extra.as_object().unwrap() {
            fields[k] = v.clone();
        }
        json!({"op": "create", "type": "publication_profile", "id": format!("${key}"),
            "parent": admin, "fields": fields})
    };
    write(
        &mut r,
        "admin",
        json!([
            profile("orgguide", "org", json!({})),
            profile("pubtrail", "public", json!({"trail": true})),
            profile("pubguide", "public", json!({}))
        ]),
    )
    .unwrap();
    land(&mut r, "admin").unwrap();
    Guide {
        _dir: dir,
        r,
        claim: BlockId::parse(&id("$gc")).unwrap(),
    }
}

fn build(r: &Rdy, key: &str) -> ProfileBuild {
    let target = SiteTarget::ProfileOnly { key: key.into() };
    build_profile_live(&r.read(), &target, &mut ProfileHistoryMemo::default())
        .unwrap_or_else(|e| panic!("{key}: {e}"))
}

fn text_files(b: &ProfileBuild) -> BTreeMap<String, String> {
    b.live
        .files
        .iter()
        .map(|(p, v)| (p.clone(), String::from_utf8_lossy(v).into_owned()))
        .collect()
}

#[test]
fn rf7_profile_trail_does_not_leak_excluded_supporting_text() {
    let g = guide();
    let claim = g.claim.to_string();
    for (key, trail_shown) in [("orgguide", true), ("pubtrail", true), ("pubguide", false)] {
        let b = build(&g.r, key);
        let files = text_files(&b);
        let intro = &files["guide/intro.html"];
        let trail = trail_of(intro, &claim);
        if trail_shown {
            let t = trail.unwrap_or_else(|| panic!("{key}: no trail\n{intro}"));
            // the selected decision, hostile as written, reads escaped
            assert!(
                t.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
                "{key}: {t}"
            );
            assert_eq!(t.matches("<li").count(), 1, "{key}: {t}");
            assert!(!t.contains("Crosses the selection"), "{key}: {t}");
        } else {
            assert_eq!(trail, None, "{key}");
        }
        for (path, text) in &files {
            for word in ["SECRETCANARY", "Crosses the selection"] {
                assert!(!text.contains(word), "{key}: {path} holds {word}");
            }
            // a page holds the hostile words only escaped (the search index is JSON)
            if path.ends_with(".html") {
                for word in ["<script>alert(1)", "onerror=alert(2)>"] {
                    assert!(!text.contains(word), "{key}: {path} holds {word}");
                }
            }
        }
        // the links that leave the selection are warned of, without their words
        assert!(
            b.live.warnings.iter().all(|w| !w.contains("SECRETCANARY")),
            "{key}: {:?}",
            b.live.warnings
        );
    }
}

#[test]
fn rf7_supporting_types_get_no_site_or_working_page() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Ledger"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger service"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Entries are append-only.",
         "links": {"describes": [{"target": "$e"}]}},
        {"op": "create", "type": "finding", "id": "$f", "parent": "$s",
         "fields": {"statement": "FINDINGCANARY statement.", "evidence": [{"quote": "FINDINGCANARY quote."}]}},
        {"op": "create", "type": "source", "id": "$x", "parent": "$s",
         "fields": {"title": "SOURCECANARY title", "kind": "release", "locator": "SOURCECANARY locator"}}
    ]))
    .unwrap();
    let id = |k: &str| w.created[k].clone();
    decide(
        &mut r,
        &id("$s"),
        "Entries stay as written.",
        &[&id("$e")],
        &[&id("$f")],
    );
    land(&mut r, "work").unwrap();

    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let files = files_under(out.path());
    assert!(!files.is_empty());
    for path in &files {
        let name = path.strip_prefix(out.path()).unwrap().to_string_lossy();
        for k in ["$f", "$x"] {
            let bid = id(k);
            if name.contains(&bid) {
                // the one file a supporting block gets is its permalink, which leads to its
                // doc's page (url1_), like a rationale's
                let text = std::fs::read_to_string(path).unwrap();
                assert!(
                    name == format!("go/{bid}.html")
                        && text.contains("content=\"0; url=../ops.html\""),
                    "a page of its own for {k}: {name}"
                );
            }
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for word in ["FINDINGCANARY", "SOURCECANARY"] {
            assert!(!text.contains(word), "{name} holds {word}");
        }
        for k in ["$f", "$x"] {
            assert!(
                !text.contains(&format!("data-rdy-block=\"{}\"", id(k))),
                "{name} holds a block for {k}"
            );
        }
    }
    // the governed claim still reads its decision through the entity
    let h = read(out.path(), "ops.html");
    let t = trail_of(&h, &id("$c")).expect("a trail");
    assert!(t.contains("Ruled: Entries stay as written."), "{t}");

    // the working copy (the projection `rdy checkout` writes) gives neither type a page or a
    // file, a projected block, or a word, id or link in any file it does write
    let wc = r.read().wc_project(&Scope::Canon, None).unwrap();
    assert!(!wc.files.is_empty());
    for k in ["$f", "$x"] {
        let bid = id(k);
        assert!(!wc.blocks.contains_key(&bid), "a projected block for {k}");
        for f in &wc.files {
            assert!(f.id != bid, "a working-copy file for {k}: {}", f.path);
            assert!(!f.path.contains(&bid), "{} is named for {k}", f.path);
            assert!(!f.text.contains(&bid), "{} names {k}:\n{}", f.path, f.text);
        }
    }
    for f in &wc.files {
        for word in ["FINDINGCANARY", "SOURCECANARY"] {
            assert!(!f.text.contains(word), "{} holds {word}", f.path);
        }
    }
    // the claim, the entity and the decision are projected
    for k in ["$c", "$e"] {
        assert!(wc.blocks.contains_key(&id(k)), "{k} is not projected");
    }
    assert!(
        wc.files
            .iter()
            .any(|f| f.text.contains("Entries are append-only.")),
        "the working copy lost the claim"
    );
}

/// New defect, found by the pin above: a finding's or a source's permalink page read as the
/// supporting words (its label was the start of its text).
#[test]
fn rf7_supporting_permalink_reads_as_its_doc() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "finding", "id": "$f", "parent": "$o",
         "fields": {"statement": "FINDINGCANARY statement.", "evidence": [{"quote": "FINDINGCANARY quote."}]}},
        {"op": "create", "type": "source", "id": "$x", "parent": "$o",
         "fields": {"title": "SOURCECANARY title", "kind": "release", "locator": "SOURCECANARY locator"}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    for k in ["$f", "$x"] {
        let h = read(out.path(), &format!("go/{}.html", w.created[k]));
        assert!(h.contains("content=\"0; url=../ops.html\""), "{h}");
        assert!(
            !h.contains("CANARY") && h.contains(">Ops</a>"),
            "{k}: the permalink reads as the doc: {h}"
        );
    }
}
