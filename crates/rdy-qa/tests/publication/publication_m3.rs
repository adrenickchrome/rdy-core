//! mg6 (M3 gate, Task 6; D-0VCQ5, D-44NMT, D-7M0YF): the publication check and the profile
//! outputs, over the chat tools, then the command line.
//!
//! The fixture (`rdy_qa::publication_scenario`) has two selected docs, one excluded doc, a public
//! and an org profile, a selected and an excluded figure, a selected and an outside decision and
//! a missing target. Every excluded field and asset holds its own canary. Public blocks on the
//! counted references; org warns. A decision outside the selection is dropped and never counted;
//! a missing target still is. A link edge outside the selection is a `links_outside` row, never a
//! counted reference, and it keeps its drift effect. A blocked check is never waived: a repaired
//! copy is rendered instead.
use rdy_engine::{Rdy, publication};
use rdy_qa::daemon::{Served, short_tempdir};
use rdy_qa::mcp::McpClient;
use rdy_qa::publication_scenario::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;

/// A served project holding the fixture.
fn served() -> (Served, Scenario) {
    let s = Served::new();
    let sc = seed(s.dir.path());
    (s, sc)
}

/// A session of `role` on the served project, in `view` when one is given.
fn session(s: &Served, run: &str, role: &str, view: Option<&str>) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    let mut args = json!({"actor": format!("agent:{run}"), "run": run, "role": role});
    if let Some(v) = view {
        args["view"] = json!(v);
    }
    c.call("rdy_session", args).ok();
    c
}

/// The report's rows of one array, sorted for comparison with the fixture's.
fn sorted_rows(v: &Value) -> Vec<Value> {
    let mut rows = v.as_array().cloned().unwrap_or_default();
    rows.sort_by_key(Value::to_string);
    rows
}

fn expected(rows: &[Row]) -> Vec<Value> {
    let mut v: Vec<Value> = rows.iter().map(Row::json).collect();
    v.sort_by_key(Value::to_string);
    v
}

/// The ids a check's selection lists.
fn selection_ids(v: &Value) -> BTreeSet<String> {
    v["selection"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect()
}

/// A fingerprint of the log segments and of every file outside `.rdy`: a check adds nothing to
/// the record. A session may add its own start to the log, so a test takes its print after the
/// session has begun.
fn fingerprint(dir: &Path) -> BTreeMap<String, u64> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, u64>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let path = e.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .to_string();
            if path.is_dir() {
                walk(root, &path, out);
            } else if rel.starts_with(".rdy/") && !rel.starts_with(".rdy/store/segments/") {
                continue;
            } else if let Ok(bytes) = std::fs::read(&path) {
                let mut h = DefaultHasher::new();
                bytes.hash(&mut h);
                out.insert(rel, h.finish());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    assert!(
        out.keys().any(|k| k.starts_with(".rdy/store/segments/")),
        "the log is in the print: {out:?}"
    );
    out
}

fn text_of(files: &Files, path: &str) -> String {
    String::from_utf8_lossy(
        files
            .get(path)
            .unwrap_or_else(|| panic!("no {path}: {:?}", files.keys())),
    )
    .into_owned()
}

#[test]
fn mg6_check_matches_visible_refs_and_reports_hidden_links() {
    let (s, sc) = served();
    let mut c = session(&s, "p", "writer", Some("drift"));
    let (excluded, missing) = sc.counted();
    assert_eq!((excluded, missing), (7, 1), "the fixture's own count");

    for (profile, status, audience) in [("pubp", "blocked", "public"), ("orgp", "warning", "org")] {
        let v = c
            .call("rdy_publication_check", json!({"profile": profile}))
            .ok();
        assert_eq!(v["profile"]["audience"], audience);
        let refs = &v["references"];
        assert_eq!(refs["status"], status, "{profile}: {v}");
        assert_eq!(refs["excluded"], 7, "{profile}");
        assert_eq!(refs["missing"], 1, "{profile}");
        assert_eq!(refs["total"], 8, "{profile}");
        assert_eq!(refs["omitted_rows"], 0);
        // the counted rows are the fixture's, row for row
        assert_eq!(sorted_rows(&refs["rows"]), expected(&sc.rows), "{profile}");
        // the link edge that leaves the selection is its own row and counts apart
        assert_eq!(refs["links_outside"]["count"], 1, "{profile}");
        assert_eq!(
            sorted_rows(&refs["links_outside"]["rows"]),
            expected(&sc.link_rows),
            "{profile}"
        );
        for r in refs["rows"].as_array().unwrap() {
            assert!(
                !r["location"].as_str().unwrap().starts_with("/links/"),
                "a link edge is no counted reference: {r}"
            );
        }
        // a decision outside the selection is dropped, in every spelling the author used
        // (bare, `@key`, display words and an empty citation): it is no row and no link
        let named = refs.to_string();
        for d in &sc.dropped_decisions {
            assert!(!named.contains(d.as_str()), "{profile}: {d} in {named}");
        }
        assert!(!named.contains("out-rule"), "{profile}");
        // a missing target is still counted, as `missing`
        assert!(
            refs["rows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["target"] == MISSING_TARGET && r["classification"] == "missing"),
            "{profile}"
        );
        // public blocks with no warning; org warns with the engine's words
        match audience {
            "public" => assert_eq!(v["warnings"], json!([]), "{v}"),
            _ => {
                assert_eq!(v["warnings"].as_array().unwrap().len(), 1, "{v}");
                assert_eq!(v["warnings"][0]["code"], "W_PUBLICATION_REFERENCE");
                assert_eq!(
                    v["warnings"][0]["message"],
                    "Org profile has 8 references outside its selection."
                );
                assert_eq!(
                    v["warnings"][0]["fix"],
                    "Review these references before you share the output."
                );
            }
        }
    }

    // what the counted references read as in a site: the marker, or the author's display words
    let out = scratch();
    let site = out.path().join("site");
    c.call(
        "rdy_render",
        json!({"out": site.to_str().unwrap(), "profile": "orgp"}),
    )
    .ok();
    let files = tree(&site);
    let (alpha, bravo) = (text_of(&files, "alpha.html"), text_of(&files, "bravo.html"));
    let u = UNAVAILABLE;
    for want in [
        // three counted references in one block: two read as the marker, one as its display words
        format!("<p>Explicit Shown display words and bare {u} and typed {u}.</p>"),
        format!("Alpha section {u}</h2>"),
        format!("<td>Cell {u}</td>"),
        format!("Label {u}</th>"),
        format!("alt=\"Selected figure alt {u}\""),
        // the decision citations read as the author's words, with the empty citation gone
        "<p>The rule holds. Named the outside rule stays. Spaced too. Chosen ".to_string(),
    ] {
        assert!(alpha.contains(&want), "{want}\n{alpha}");
    }
    assert!(
        bravo.contains(&format!("<p>Missing {u} here.</p>")),
        "{bravo}"
    );
    let block = alpha
        .split("<p>")
        .find(|p| p.starts_with("Explicit "))
        .and_then(|p| p.split("</p>").next())
        .unwrap();
    let markers = block.matches(u).count() as u64;
    let display_words = 1;
    let pa2: u64 = sc
        .rows
        .iter()
        .filter(|r| r.source == sc.id("$pA2"))
        .map(|r| r.occurrences)
        .sum();
    assert_eq!(
        markers + display_words,
        pa2,
        "every counted reference is visible"
    );
    // the hidden edge shows nowhere
    for hidden in [sc.id("$xp"), "XPROSECANARY", "depends_on"] {
        assert_eq!(holding(&files, hidden.as_bytes()), Vec::<String>::new());
    }

    // the edge keeps its drift effect: editing its excluded target makes the source suspect,
    // and the check still reports the edge apart, with the selection unchanged
    let canon = c
        .call("rdy_publication_check", json!({"profile": "pubp"}))
        .ok();
    c.call(
        "rdy_write",
        json!({"ops": [{"op": "update", "id": sc.id("$xp"), "body": "XPROSECANARY words, edited."}]}),
    )
    .ok();
    let drift = c
        .call(
            "rdy_publication_check",
            json!({"profile": "pubp", "scope": "view:drift"}),
        )
        .ok();
    assert_eq!(drift["selection"]["hash"], canon["selection"]["hash"]);
    assert_eq!(
        drift["selection"]["identity"],
        canon["selection"]["identity"]
    );
    assert_eq!(drift["references"]["rows"], canon["references"]["rows"]);
    assert_eq!(
        drift["references"]["links_outside"],
        canon["references"]["links_outside"]
    );
    let sus = c.call("rdy_suspects", json!({"scope": "view:drift"})).ok();
    let items = sus["items"].as_array().unwrap_or_else(|| panic!("{sus}"));
    assert!(
        items
            .iter()
            .any(|i| i["block"] == sc.id("$cA") && i["target"] == sc.id("$xp")),
        "the claim is suspect after its hidden target changed: {sus}"
    );
    // the suspect is not a counted reference and not a leak
    assert!(!drift["references"].to_string().contains(sc.id("$decOut")));
}

#[test]
fn mg6_profile_outputs_keep_selected_canaries_and_no_excluded_canary() {
    let (s, sc) = served();
    let mut c = session(&s, "p", "writer", None);

    // the canaries are real: a full-source render holds them, so a clean profile proves a filter
    let full = scratch();
    let full_site = full.path().join("full");
    c.call("rdy_render", json!({"out": full_site.to_str().unwrap()}))
        .ok();
    let full_files = tree(&full_site);
    for word in ["XPROSECANARY", "XDOCCANARY", "OUTDECCANARY", "OUTQCANARY"] {
        assert!(!holding(&full_files, word.as_bytes()).is_empty(), "{word}");
    }
    assert!(!holding(&full_files, &sc.excluded_png).is_empty());

    // the blocked state first: every output is refused with the check's own words, and nothing is
    // written; the check is not waived to obtain output
    let out_pub = scratch();
    let blocked = drive(&mut c, "pubp", "canon", out_pub.path());
    assert_eq!(blocked.check["references"]["status"], "blocked");
    assert_eq!(blocked.refused.len(), 5, "{:?}", blocked.refused);
    for (name, (code, message, fix)) in &blocked.refused {
        assert_eq!(code, "E_PUBLICATION_REFERENCE", "{name}");
        assert_eq!(
            message, "Public profile has 8 references outside its selection.",
            "{name}"
        );
        assert_eq!(
            fix,
            "Include the target docs or remove these references. Run the publication check again.",
            "{name}"
        );
    }
    assert!(blocked.outputs.is_empty() && blocked.leftovers.is_empty());
    assert_eq!(tree(out_pub.path()).len(), 0, "nothing was written");
    assert_eq!(
        audit(&blocked, &sc, Stage::Source, None),
        Vec::<String>::new()
    );

    // the org profile warns and writes every output
    let out_org = scratch();
    let org = drive(&mut c, "orgp", "canon", out_org.path());
    assert!(org.refused.is_empty(), "{:?}", org.refused);
    assert_eq!(
        org.outputs.keys().map(String::as_str).collect::<Vec<_>>(),
        ["html", "md", "mkdocs", "pdf", "site"]
    );
    // the selected figure alone is an asset of the site, and it is embedded in the HTML file
    let pngs: Vec<&String> = org.outputs["site"]
        .keys()
        .filter(|p| p.starts_with("assets/") && p.ends_with(".png"))
        .collect();
    assert_eq!(pngs.len(), 1, "{pngs:?}");
    assert_eq!(org.outputs["site"][pngs[0]], sc.selected_png);
    assert!(pngs[0].contains(&sc.selected_blob));
    assert!(
        text_of(&org.outputs["html"], "out.html").contains(&base64(&sc.selected_png)),
        "the selected figure is embedded"
    );
    drop(c);
    s.stop();
    let org_print = print_text(s.dir.path(), "canon", "orgp");
    assert_eq!(
        audit(&org, &sc, Stage::Source, Some(&org_print)),
        Vec::<String>::new()
    );

    // the audit itself is not vacuous: one leaked canary in a file name, a file body, a Markdown
    // file, the single HTML file or the PDF text is found
    for (name, path, body) in [
        ("site", "leak.html", "XPROSECANARY"),
        ("site", "xdoccanaryname.html", "ok"),
        ("md", "leak.md", "OUTDECCANARY"),
        ("html", "out.html", "XTERMCANARY"),
    ] {
        let mut leaked = org.clone();
        leaked
            .outputs
            .get_mut(name)
            .unwrap()
            .entry(path.into())
            .and_modify(|b| b.extend_from_slice(body.as_bytes()))
            .or_insert_with(|| body.as_bytes().to_vec());
        let bad = audit(&leaked, &sc, Stage::Source, Some(&org_print));
        assert!(!bad.is_empty(), "{name}/{path} leaked and nothing noticed");
    }
    let bad = audit(
        &org,
        &sc,
        Stage::Source,
        Some(&format!("{org_print} XFIGCANARY")),
    );
    assert!(!bad.is_empty(), "the PDF text leaked and nothing noticed");

    // the repaired copy: the same counted references removed through the chat tools, the ready
    // selection held, then rendered at the landing that holds it and scanned
    let (s2, sc2) = served();
    let mut c2 = session(&s2, "fix", "writer", Some("fix"));
    let before = c2
        .call("rdy_publication_check", json!({"profile": "pubp"}))
        .ok();
    assert_eq!(before["references"]["status"], "blocked");
    let fixed = repair_through_mcp(&mut c2, &sc2, "fix");
    for check in [&fixed.ready, &fixed.landed] {
        let refs = &check["references"];
        assert_eq!(refs["status"], "clear", "{check}");
        assert_eq!(refs["total"], 0, "{check}");
        assert_eq!(check["warnings"], json!([]));
        // the source-only link stays in both states
        assert_eq!(refs["links_outside"]["count"], 1, "{check}");
        assert_eq!(
            sorted_rows(&refs["links_outside"]["rows"]),
            expected(&sc2.link_rows)
        );
    }
    // the held selection is the landed one
    assert_eq!(
        fixed.ready["selection"]["hash"],
        fixed.landed["selection"]["hash"]
    );
    assert_eq!(
        fixed.ready["selection"]["identity"],
        fixed.landed["selection"]["identity"]
    );
    assert_ne!(
        before["selection"]["hash"], fixed.landed["selection"]["hash"],
        "the repair changed what the selection holds"
    );
    let scope = format!("landing:{}", fixed.landing);
    let out_fixed = scratch();
    let ready = drive(&mut c2, "pubp", &scope, out_fixed.path());
    assert!(ready.refused.is_empty(), "{:?}", ready.refused);
    assert_eq!(ready.outputs.len(), 5);
    drop(c2);
    s2.stop();
    let ready_print = print_text(s2.dir.path(), &scope, "pubp");
    assert_eq!(
        audit(&ready, &sc2, Stage::Repaired, Some(&ready_print)),
        Vec::<String>::new()
    );
    // the citations stay as the author's words in the repaired outputs too
    assert!(
        text_of(&ready.outputs["md"], "alpha.md").contains(&sc2.citation_words),
        "the outside decision citations remain"
    );
    // a public profile shows no trail
    for (path, bytes) in &ready.outputs["site"] {
        if path.ends_with(".html") {
            assert!(!String::from_utf8_lossy(bytes).contains("data-rdy=\"trail\""));
        }
    }
}

#[test]
fn mg6_all_local_links_and_anchors_resolve() {
    // the checker on a small known set first: a silent empty scan cannot pass
    let mut known = Files::new();
    known.insert(
        "index.html".into(),
        br##"<a href="a%20b.html">ok</a><a href="sub/p.html#sec-one">ok</a><a href="https://example.com/x">ext</a><a href="#top">top</a>"##.to_vec(),
    );
    known.insert("a b.html".into(), br#"<h1 id="h">A</h1>"#.to_vec());
    known.insert(
        "sub/p.html".into(),
        br##"<h2 id="sec-one">S</h2><a href="../index.html">up</a><a href="#sec-one">self</a><img src="../pic.png">"##.to_vec(),
    );
    known.insert("pic.png".into(), vec![0, 1, 2]);
    known.insert(
        "notes.md".into(),
        b"See [a](sub/p.md) and ![i](pic.png).".to_vec(),
    );
    known.insert("sub/p.md".into(), b"# P".to_vec());
    let scan = scan_local_links(&known);
    assert_eq!(scan.broken, Vec::<String>::new());
    assert_eq!(scan.checked, 8, "{scan:?}");
    assert_eq!(scan_local_links(&Files::new()).checked, 0);
    for (what, file, html, why) in [
        (
            "a missing page",
            "index.html",
            r#"<a href="missing-page.html">x</a>"#,
            "no such file",
        ),
        (
            "a missing fragment",
            "index.html",
            r##"<a href="sub/p.html#nowhere">x</a>"##,
            "no id nowhere",
        ),
        (
            "a missing own fragment",
            "a b.html",
            r##"<a href="#nowhere">x</a>"##,
            "no id nowhere",
        ),
        (
            "a bad escape",
            "index.html",
            r#"<a href="a%ZZ.html">x</a>"#,
            "percent-encoding",
        ),
        (
            "a path above the root",
            "index.html",
            r#"<a href="../outside.html">x</a>"#,
            "leaves the output root",
        ),
        (
            "a missing image",
            "sub/p.html",
            r#"<img src="gone.png">"#,
            "no such file",
        ),
    ] {
        let mut broken = known.clone();
        broken
            .get_mut(file)
            .unwrap()
            .extend_from_slice(html.as_bytes());
        let scan = scan_local_links(&broken);
        assert_eq!(scan.broken.len(), 1, "{what}: {scan:?}");
        assert!(scan.broken[0].contains(why), "{what}: {scan:?}");
    }
    let mut broken = known.clone();
    broken
        .get_mut("notes.md")
        .unwrap()
        .extend_from_slice(b" And [gone](nowhere.md).");
    assert_eq!(scan_local_links(&broken).broken.len(), 1);

    // every produced surface of the org profile and of the repaired public profile
    let (s, _sc) = served();
    let mut c = session(&s, "p", "writer", None);
    let out = scratch();
    let org = drive(&mut c, "orgp", "canon", out.path());
    drop(c);
    s.stop();
    let (s2, sc2) = served();
    let mut c2 = session(&s2, "fix", "writer", Some("fix"));
    let fixed = repair_through_mcp(&mut c2, &sc2, "fix");
    let out2 = scratch();
    let ready = drive(
        &mut c2,
        "pubp",
        &format!("landing:{}", fixed.landing),
        out2.path(),
    );
    drop(c2);
    s2.stop();
    for (label, frozen) in [("org", &org), ("repaired public", &ready)] {
        assert_eq!(frozen.outputs.len(), 5, "{label}");
        for (name, files) in &frozen.outputs {
            let scan = scan_local_links(files);
            assert_eq!(scan.broken, Vec::<String>::new(), "{label}/{name}");
            let floor = match name.as_str() {
                "site" => 20,
                "md" | "mkdocs" => 2,
                "html" => 1,
                _ => 0,
            };
            assert!(
                scan.checked >= floor,
                "{label}/{name} checked only {}",
                scan.checked
            );
        }
        // one deliberately broken local reference in the real output fails the scan, for each
        // surface that has links
        for (name, path, add) in [
            (
                "site",
                "alpha.html",
                r#"<a href="missing-page.html#nowhere">x</a>"#,
            ),
            (
                "site",
                "bravo.html",
                r##"<a href="alpha.html#no-such-anchor">x</a>"##,
            ),
            ("html", "out.html", r##"<a href="#no-such-anchor">x</a>"##),
            ("md", "alpha.md", "[x](missing-page.md)"),
        ] {
            let mut files = frozen.outputs[name].clone();
            files
                .get_mut(path)
                .unwrap()
                .extend_from_slice(add.as_bytes());
            let scan = scan_local_links(&files);
            assert_eq!(scan.broken.len(), 1, "{label}/{name}/{path}: {scan:?}");
            assert!(scan.broken[0].starts_with(path), "{scan:?}");
        }
    }
    // the cross-doc and fragment links the author wrote: the page link stays in both states; the
    // fragment link to the section is a link only once the section's words hold no marker
    let bravo = text_of(&ready.outputs["site"], "bravo.html");
    assert!(bravo.contains("href=\"alpha.html\""), "{bravo}");
    assert!(
        bravo.contains("href=\"alpha.html#alpha-section\""),
        "{bravo}"
    );
    assert!(
        !bravo.contains("xdoccanaryname"),
        "no link to the excluded doc"
    );
    let source_bravo = text_of(&org.outputs["site"], "bravo.html");
    assert!(source_bravo.contains("href=\"alpha.html\""));
    assert!(
        !source_bravo.contains("alpha.html#alpha-section\""),
        "the source state's section anchor holds the marker, so the author's fragment link reads as text"
    );
}

#[test]
fn mg6_profile_check_is_read_only_and_has_cli_parity() {
    let (s, sc) = served();
    let out = scratch();

    // every session role may check; every reply is the CLI's, key for key and byte for byte
    let mut mcp: BTreeMap<String, Value> = BTreeMap::new();
    for (i, role) in ["reader", "writer", "reviewer"].into_iter().enumerate() {
        let mut c = session(&s, &format!("r{i}"), role, None);
        let before = fingerprint(s.dir.path());
        for profile in ["pubp", "orgp"] {
            for (scope, against) in [
                (None, None),
                (Some("tag:v1.0.0"), Some("canon")),
                (None, Some("tag:v1.0.0")),
            ] {
                let mut args = json!({"profile": profile});
                let mut cli = vec!["--json", "publication", "check", "--profile", profile];
                if let Some(x) = scope {
                    args["scope"] = json!(x);
                    cli.extend(["--scope", x]);
                }
                if let Some(x) = against {
                    args["against"] = json!(x);
                    cli.extend(["--against", x]);
                }
                let v = c.call("rdy_publication_check", args).ok();
                let (ok, cv) = s.json(&cli);
                assert!(ok, "{cv}");
                assert_eq!(v, cv, "{role} {profile} {scope:?} {against:?}");
                assert_eq!(v.to_string(), cv.to_string());
                mcp.insert(format!("{profile} {scope:?} {against:?}"), v);
            }
        }
        // the blocked check is a result, not an error
        let v = c
            .call("rdy_publication_check", json!({"profile": "pubp"}))
            .ok();
        assert_eq!(v["references"]["status"], "blocked", "{role}");
        assert_eq!(
            fingerprint(s.dir.path()),
            before,
            "{role}: no check wrote to the log or to a file"
        );
    }
    // the check works with no daemon and gives the same report
    s.stop();
    let before = fingerprint(s.dir.path());
    let (ok, nd) = s.json(&[
        "--json",
        "--no-daemon",
        "publication",
        "check",
        "--profile",
        "pubp",
    ]);
    assert!(ok, "{nd}");
    assert_eq!(nd, mcp["pubp None None"]);
    // the text, for the person: counted references and links outside, apart
    let o = s.rdy(&["publication", "check", "--profile", "pubp"]);
    let t = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(o.status.success(), "{t}");
    assert_eq!(
        t,
        "Publication profile pubp: blocked.\n15 blocks in 2 docs and 0 folders.\nReferences outside selection: 8.\nLinks outside selection: 1.\n"
    );
    let org_text =
        String::from_utf8_lossy(&s.rdy(&["publication", "check", "--profile", "orgp"]).stdout)
            .into_owned();
    assert!(
        org_text.starts_with("Publication profile orgp: warning.\n"),
        "{org_text}"
    );
    for word in ["approve", "publish", "authoriz", "answer"] {
        assert!(!t.to_lowercase().contains(word), "{t}");
    }
    s.stop();
    assert_eq!(
        fingerprint(s.dir.path()),
        before,
        "no check, with or without a daemon, wrote to the log or to a file"
    );

    // the outputs: the CLI refuses a blocked check with the chat tool's words and writes nothing,
    // and writes the org outputs the tools write, file for file
    let mut c = session(&s, "w", "writer", None);
    let blocked = out.path().join("cli-blocked");
    let (ok, e) = s.json(&[
        "--json",
        "render",
        "--profile",
        "pubp",
        "--out",
        blocked.to_str().unwrap(),
    ]);
    assert!(!ok, "{e}");
    let chat = out.path().join("chat-blocked");
    let refused = c.call(
        "rdy_render",
        json!({"out": chat.to_str().unwrap(), "profile": "pubp"}),
    );
    assert_eq!(refused.code(), Some("E_PUBLICATION_REFERENCE"));
    assert_eq!(e["code"], "E_PUBLICATION_REFERENCE", "{e}");
    assert!(!blocked.exists() && !chat.exists());
    let (cli_site, chat_site) = (out.path().join("cli-site"), out.path().join("chat-site"));
    let (ok, cv) = s.json(&[
        "--json",
        "render",
        "--profile",
        "orgp",
        "--out",
        cli_site.to_str().unwrap(),
    ]);
    assert!(ok, "{cv}");
    let v = c
        .call(
            "rdy_render",
            json!({"out": chat_site.to_str().unwrap(), "profile": "orgp"}),
        )
        .ok();
    assert_eq!(cv, v);
    assert!(tree(&cli_site) == tree(&chat_site), "the same site files");
    for (format, layout) in [("md", None), ("md", Some("mkdocs")), ("html", None)] {
        let name = format!("{format}-{}", layout.unwrap_or("plain"));
        let (a, b) = (
            out.path().join(format!("cli-{name}")),
            out.path().join(format!("chat-{name}")),
        );
        let mut args = vec![
            "--json",
            "export",
            "--format",
            format,
            "--profile",
            "orgp",
            "--out",
            a.to_str().unwrap(),
        ];
        let mut chat_args =
            json!({"out": b.to_str().unwrap(), "format": format, "profile": "orgp"});
        if let Some(l) = layout {
            args.extend(["--layout", l]);
            chat_args["layout"] = json!(l);
        }
        let (ok, mut cv) = s.json(&args);
        assert!(ok, "{name}: {cv}");
        let mut v = c.call("rdy_export", chat_args).ok();
        // the replies differ only in the path each wrote
        assert_eq!(cv["out"], a.to_str().unwrap());
        assert_eq!(v["out"], b.to_str().unwrap());
        cv["out"] = Value::Null;
        v["out"] = Value::Null;
        assert_eq!(cv, v, "{name}");
        // a single file is held under its own name, so compare the bytes in path order
        let (fa, fb) = (tree(&a), tree(&b));
        assert!(!fa.is_empty(), "{name}");
        assert!(
            fa.into_values().collect::<Vec<_>>() == fb.into_values().collect::<Vec<_>>(),
            "{name}"
        );
    }
    // and no output holds a canary
    for dir in [
        "cli-site",
        "chat-site",
        "cli-md-plain",
        "cli-md-mkdocs",
        "cli-html-plain",
    ] {
        let files = tree(&out.path().join(dir));
        for word in &sc.excluded_words {
            assert_eq!(
                holding(&files, word.as_bytes()),
                Vec::<String>::new(),
                "{dir}: {word}"
            );
        }
    }
    drop(c);
}

#[test]
fn mg6_profile_identity_survives_reindex_dump_and_clone() {
    let s = Served::new();
    let sc = seed(s.dir.path());
    let reports = |dir: &Path| -> Vec<Value> {
        let r = Rdy::open(dir).unwrap_or_else(|e| panic!("open {}: {e}", dir.display()));
        let mut out = vec![];
        for profile in ["pubp", "orgp"] {
            for scope in [None, Some("tag:v1.0.0")] {
                out.push(
                    publication::check(&r.read(), profile, scope, None)
                        .unwrap_or_else(|e| panic!("check {profile}: {e}")),
                );
            }
        }
        r.close().unwrap();
        out
    };
    // the site files of each profile at the tag, or the refusal's words
    let sites = |dir: &Path| -> Vec<Files> {
        let r = Rdy::open(dir).unwrap();
        let out = ["pubp", "orgp"]
            .iter()
            .map(
                |p| match rdy_render::build_profile_site(&r.read(), "tag:v1.0.0", p) {
                    Ok(site) => site.files,
                    Err(e) => BTreeMap::from([("refused".to_string(), e.to_string().into_bytes())]),
                },
            )
            .collect();
        r.close().unwrap();
        out
    };
    let before = reports(s.dir.path());
    let before_sites = sites(s.dir.path());
    // the profile and its selection are what the fixture built
    assert_eq!(before[1]["profile"]["id"], sc.profiles["pubp"].as_str());
    assert_eq!(before[3]["profile"]["id"], sc.profiles["orgp"].as_str());
    for v in &before {
        assert!(
            v["selection"]["identity"]
                .as_str()
                .is_some_and(|h| h.len() == 64)
        );
        assert!(v["profile"]["version"].is_string());
    }
    // the public site is refused before and after; the org site is written
    assert!(
        before_sites[0].contains_key("refused"),
        "{:?}",
        before_sites[0].keys()
    );
    assert!(before_sites[1].contains_key("alpha.html"));

    // a reindex rebuilds the index from the log
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        r.reindex().unwrap();
        r.close().unwrap();
    }
    assert_eq!(reports(s.dir.path()), before, "after a reindex");
    assert!(sites(s.dir.path()) == before_sites, "after a reindex");

    // a dump, then a load and a clone from it
    let scratch_dir = short_tempdir();
    let file = scratch_dir.path().join("p.rdydump");
    let o = s.rdy(&["dump", "--out", file.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    s.stop();
    let env = s.env_pairs();
    let loaded = scratch_dir.path().join("loaded");
    std::fs::create_dir(&loaded).unwrap();
    let out = rdy_qa::daemon::human_command(
        &loaded,
        &env,
        &["--no-daemon", "load", file.to_str().unwrap()],
    )
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cloned = scratch_dir.path().join("cloned");
    let out = rdy_qa::daemon::human_command(
        scratch_dir.path(),
        &env,
        &[
            "--no-daemon",
            "clone",
            file.to_str().unwrap(),
            cloned.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for (what, dir) in [("load", &loaded), ("clone", &cloned)] {
        assert_eq!(reports(dir), before, "after a {what}");
        assert!(
            sites(dir) == before_sites,
            "after a {what}: the same site files"
        );
    }
}

#[test]
fn mg6_root_supporting_blocks_follow_the_built_selection_rule() {
    let (s, sc) = served();
    let mut c = session(&s, "p", "writer", Some("rule"));
    for profile in ["pubp", "orgp"] {
        let v = c
            .call("rdy_publication_check", json!({"profile": profile}))
            .ok();
        // a decision with no doc that governs a selected claim is selected, and the question it
        // answers comes with it; a decision and a question that govern or answer nothing do not
        let ids = selection_ids(&v);
        assert_eq!(ids, sc.selected, "{profile}");
        assert_eq!(v["selection"]["blocks"], sc.selected.len());
        assert_eq!(
            (
                v["selection"]["docs"].as_u64(),
                v["selection"]["folders"].as_u64()
            ),
            (Some(2), Some(0))
        );
        assert_eq!(v["selection"]["attachments"], 1);
        assert!(ids.contains(sc.id("$decSel")) && ids.contains(sc.id("$qHop")));
        assert!(!ids.contains(sc.id("$decOut")) && !ids.contains(sc.id("$qOut")));
    }

    // in a view: the claim no longer names its decision, and a selected paragraph names the
    // outside decision instead. The rule follows the edges, not the blocks' names.
    c.call(
        "rdy_write",
        json!({"ops": [
            {"op": "update", "id": sc.id("$cA"), "links": {"decided_by": []}},
            {"op": "update", "id": sc.id("$pB1"), "links": {"decided_by": [{"target": sc.id("$decOut")}]}}
        ]}),
    )
    .ok();
    let v = c
        .call(
            "rdy_publication_check",
            json!({"profile": "orgp", "scope": "view:rule"}),
        )
        .ok();
    let got = selection_ids(&v);
    let mut want = sc.selected.clone();
    for k in ["$decSel", "$qHop"] {
        want.remove(sc.id(k));
    }
    want.insert(sc.id("$decOut").to_string());
    assert_eq!(got, want, "{v}");
    assert!(
        !got.contains(sc.id("$qOut")),
        "an unrelated root question stays out"
    );
    // the outside decision is now inside the selection: nothing names it as a reference outside
    assert!(!v["references"].to_string().contains(sc.id("$decOut")));
    // canon is untouched
    let canon = c
        .call("rdy_publication_check", json!({"profile": "orgp"}))
        .ok();
    assert_eq!(canon["selection"]["blocks"], sc.selected.len());
}
