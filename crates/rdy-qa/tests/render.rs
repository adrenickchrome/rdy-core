use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;
use std::path::{Path, PathBuf};

const EVIL: &str = "<script>alert(1)</script>\"'><img src=x onerror=alert(2)>&amp;";

fn hostile_site() -> (tempfile::TempDir, tempfile::TempDir) {
    let (d, mut r) = project();
    let blob = r.store_mut().put_blob(b"png").unwrap().to_string();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "evil", "title": EVIL, "description": EVIL}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "page", "title": EVIL, "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": EVIL}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": EVIL}, "body": EVIL},
        {"op": "create", "type": "claim", "parent": "$s", "body": format!("{EVIL} {{{{$t}}}}")},
        {"op": "create", "type": "table", "parent": "$s", "fields": {"columns": [EVIL], "rows": [[EVIL]]}},
        {"op": "create", "type": "flow", "parent": "$s", "fields": {"name": EVIL, "steps": [EVIL]}},
        {"op": "create", "type": "diagram", "parent": "$s", "fields": {"lang": "mermaid", "alt": EVIL}, "body": format!("graph TD\n A[{EVIL}]")},
        {"op": "create", "type": "code", "parent": "$s", "fields": {"lang": EVIL}, "body": EVIL},
        {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob, "media_type": "image/png", "alt": EVIL}}
    ])).unwrap();
    r.decide(
        &writer(),
        "work",
        DecisionInput {
            parent: res.created["$s"].clone(),
            statement: EVIL.into(),
            options: vec![EVIL.into()],
            body: EVIL.into(),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v0.1.0", None, Some(EVIL.into()))
        .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    (d, out)
}

fn pages(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p)
            } else if p.extension().is_some_and(|x| x == "html") {
                out.push(p)
            }
        }
    }
    out
}

#[test]
fn render_hostile_text_is_escaped_on_every_page() {
    let (_d, site) = hostile_site();
    for p in pages(site.path()) {
        let html = std::fs::read_to_string(&p).unwrap();
        for raw in [
            "<script>alert(1)",
            "onerror=alert(2)>",
            "\"'><img",
            "<img src=x",
        ] {
            assert!(!html.contains(raw), "{}: raw {raw:?}", p.display());
        }
        for (i, _) in html.match_indices("<script") {
            assert!(
                html[i..].starts_with("<script type=\"module\">import mermaid from \"https://"),
                "{}: unexpected <script",
                p.display()
            );
        }
        for h in attrs(&html, "href") {
            assert!(
                !h.to_ascii_lowercase().starts_with("javascript:"),
                "{}: {h}",
                p.display()
            );
        }
    }
}

/// Every attribute value of `attr` in `html`.
fn attrs<'a>(html: &'a str, attr: &str) -> Vec<&'a str> {
    let key = format!("{attr}=\"");
    let mut out = vec![];
    let mut rest = html;
    while let Some(i) = rest.find(&key) {
        rest = &rest[i + key.len()..];
        if let Some(j) = rest.find('"') {
            out.push(&rest[..j]);
            rest = &rest[j..];
        }
    }
    out
}

/// Decodes the HTML entities a browser would decode in an attribute value.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(j) = rest.find(';') else { break };
        let ent = &rest[1..j];
        let ch = match ent {
            "amp" => Some('&'),
            "quot" => Some('"'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[j + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[derive(Default, Debug)]
struct LinkStats {
    pages: usize,
    max_depth: usize,
    anchors: usize,
    assets: usize,
    decisions: usize,
}

/// Checks every link in the site; returns the broken ones and what was covered.
fn check_links(site: &Path) -> (Vec<String>, LinkStats) {
    let mut broken = vec![];
    let mut st = LinkStats::default();
    for p in pages(site) {
        let html = std::fs::read_to_string(&p).unwrap();
        st.pages += 1;
        st.max_depth = st
            .max_depth
            .max(p.strip_prefix(site).unwrap().components().count() - 1);
        for link in attrs(&html, "href").into_iter().chain(attrs(&html, "src")) {
            if link.starts_with("http://") || link.starts_with("https://") || link.is_empty() {
                continue;
            }
            let link = unescape(link);
            let (file, anchor) = match link.split_once('#') {
                Some((f, a)) => (f.to_string(), Some(a.to_string())),
                None => (link.clone(), None),
            };
            if anchor.is_some() {
                st.anchors += 1;
            }
            if file.contains("_assets/") {
                st.assets += 1;
            }
            if file.contains("_decision/") {
                st.decisions += 1;
            }
            let mut target = if file.is_empty() {
                p.clone()
            } else {
                p.parent().unwrap().join(&file)
            };
            if !target.exists() {
                broken.push(format!("{} → {link}", p.display()));
                continue;
            }
            if target.is_dir() {
                target = target.join("index.html");
                if !target.exists() {
                    broken.push(format!(
                        "{} → {link} (directory without index.html)",
                        p.display()
                    ));
                    continue;
                }
            }
            if let Some(a) = anchor {
                let t = std::fs::read_to_string(&target).unwrap_or_default();
                if !t.contains(&format!("id=\"{a}\"")) {
                    broken.push(format!("{} → {link} (no anchor)", p.display()));
                }
            }
        }
    }
    (broken, st)
}

#[test]
fn render_every_link_resolves() {
    let (_d, site) = hostile_site();
    let (broken, _) = check_links(site.path());
    assert!(broken.is_empty(), "{broken:#?}");
}

#[test]
fn render_every_link_resolves_on_a_rich_site() {
    let (_d, mut r) = project();
    let blob = r.store_mut().put_blob(b"png").unwrap().to_string();
    let mut ops = vec![
        json!({"op": "create", "type": "folder", "id": "$fa", "fields": {"name": "a", "title": "A"}}),
        json!({"op": "create", "type": "folder", "id": "$fb", "parent": "$fa", "fields": {"name": "b", "title": "B"}}),
        json!({"op": "create", "type": "folder", "id": "$fc", "parent": "$fb", "fields": {"name": "c", "title": "C"}}),
    ];
    let folders = ["$fa", "$fb", "$fc"];
    for (fi, f) in folders.iter().enumerate() {
        for d in 0..3 {
            let (doc, sec, term) = (
                format!("$d{fi}{d}"),
                format!("$s{fi}{d}"),
                format!("$t{fi}{d}"),
            );
            ops.push(json!({"op": "create", "type": "doc", "id": doc, "parent": f, "fields": {"title": format!("Doc {fi}{d}"), "doc_type": "glossary"}}));
            ops.push(json!({"op": "create", "type": "section", "id": sec, "parent": doc, "fields": {"title": "Terms"}}));
            ops.push(json!({"op": "create", "type": "term", "id": term, "parent": sec, "fields": {"name": format!("Term{fi}{d}")}, "body": "A term."}));
        }
    }
    for fi in 0..3 {
        for d in 0..3 {
            let (sec, term) = (format!("$s{fi}{d}"), format!("$t{fi}{d}"));
            // cross-doc refs to other folders and depths
            let other = format!("$t{}{}", (fi + 1) % 3, d);
            let far = format!("$t{}{}", (fi + 2) % 3, (d + 1) % 3);
            ops.push(json!({"op": "create", "type": "claim", "parent": sec, "body": format!("Uses {{{{{other}}}}} and {{{{{far}}}}} and {{{{{term}}}}}.")}));
        }
    }
    ops.push(json!({"op": "create", "type": "figure", "parent": "$s22", "fields": {"blob": blob, "media_type": "image/png", "alt": "pic"}}));
    ops.push(json!({"op": "create", "type": "diagram", "parent": "$s22", "fields": {"lang": "mermaid", "alt": "d"}, "body": "graph TD\n A[{{$t00}}] --> B"}));
    let res = write(&mut r, "work", json!(ops)).unwrap();
    let sec = |k: &str| res.created[k].clone();
    let dec = |statement: &str, parent: String, affects: Vec<String>| DecisionInput {
        parent,
        statement: statement.into(),
        options: vec!["yes".into(), "no".into()],
        affects,
        body: "Because.".into(),
        ..Default::default()
    };
    let d1 = r
        .decide(
            &writer(),
            "work",
            dec("First", sec("$s00"), vec![sec("$t22")]),
        )
        .unwrap();
    let d1_id = d1.created.values().next().expect("decision id").clone();
    r.decide(
        &writer(),
        "work",
        dec("Second", sec("$s11"), vec![sec("$t00")]),
    )
    .unwrap();
    r.supersede(
        &writer(),
        "work",
        &d1_id,
        None,
        dec("Third supersedes first", sec("$s22"), vec![sec("$t11")]),
    )
    .unwrap();
    let landing = land(&mut r, "work").unwrap();
    let v = r.tag(&approver(), "v1.0.0", None, None).unwrap();
    // unlanded changes in a view
    write(&mut r, "draft", json!([
        {"op": "create", "type": "doc", "parent": sec("$fc"), "fields": {"title": "Draft doc", "doc_type": "glossary"}},
    ])).unwrap();
    let _ = landing;
    let mut scopes = vec![
        ("canon", Scope::Canon),
        ("view", Scope::View("draft".into())),
        ("tag", Scope::At(v.landing.parse().unwrap())),
    ];
    for (name, scope) in scopes.drain(..) {
        let out = tempfile::tempdir().unwrap();
        rdy_render::render_site(&mut r, &scope, out.path()).unwrap();
        let (broken, st) = check_links(out.path());
        assert!(broken.is_empty(), "{name}: {broken:#?}");
        assert!(st.pages >= 10, "{name}: {st:?}");
        assert!(st.max_depth >= 3, "{name}: {st:?}");
        assert!(st.anchors >= 5, "{name}: {st:?}");
        assert!(st.assets >= 1, "{name}: {st:?}");
        assert!(st.decisions >= 1, "{name}: {st:?}");
    }
}
