//! pub6 (D-0VCQ5, D-48NYM, D-KDFZ3, D-X61DT, D-QWSTV): a named profile's static site reads one
//! filtered view of canon. Every page, the navigation, the permalinks, the trail, the glossary,
//! the questions, the search index, the figures, the llms files and every file name come from the
//! profile's selected state and its induced graph alone. Only the explicitly chosen tag's name and
//! its existing `redacted` badge show of the history (RULING F3). A reference outside the
//! selection reads as its author's display words or `unavailable reference`; an authored local
//! link leads only to a selected page and an anchor emitted on it. Placement replaces the whole
//! output; a public profile's refusal changes nothing.
#[path = "../../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::{E_PUBLICATION_REFERENCE, Rdy, ReadState, Scope};
use rdy_model::redaction::RedactTarget;
use rdy_model::{BlockId, Change, ChangeLinks, Hash, Op};
use rdy_qa::edit_loop::Spec;
use rdy_qa::fixtures::{approver, land, land_with_a_tie, raw, write, writer};
use rdy_qa::golden::{retired_reference, tiny};
use rdy_render::{SiteFiles, build_profile_site, build_site, render_profile_site};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// The words of a reference outside the selection with no display text.
const UNAVAILABLE: &str = "unavailable reference";
/// The notice a page writes in place of removed content.
const NOTICE_HTML: &str = "<p class=\"redacted\">Content removed.</p>";

// --- helpers ------------------------------------------------------------------------------------

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Writes `ops` to `view` as the writer; panics on a refusal.
fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, BlockId> {
    let w = write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e}"));
    w.created.iter().map(|(k, v)| (k.clone(), bid(v))).collect()
}

/// Writes then lands `ops` as one view.
fn put_land(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, BlockId> {
    let ids = put(r, view, ops);
    land(r, view).unwrap_or_else(|e| panic!("land {view}: {e}"));
    ids
}

/// One profile's create op under `parent`.
fn profile_op(key: &str, audience: &str, parent: &BlockId, select: &[&BlockId]) -> Value {
    json!({"op": "create", "type": "publication_profile", "id": format!("${key}"),
        "parent": parent.to_string(),
        "fields": {"key": key, "title": format!("Title of {key}"),
            "description": format!("Description of {key}."), "audience": audience,
            "select": select.iter().map(|s| s.to_string()).collect::<Vec<_>>()}})
}

/// Creates and lands an `admin` doc holding one profile per (key, audience, selection).
fn profiles(r: &mut Rdy, list: &[(&str, &str, Vec<&BlockId>)]) -> BTreeMap<String, BlockId> {
    let admin = put(
        r,
        "admin",
        json!([{"op": "create", "type": "doc", "id": "$admin",
            "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}]),
    )["$admin"]
        .clone();
    let ops: Vec<Value> = list
        .iter()
        .map(|(k, a, s)| profile_op(k, a, &admin, s))
        .collect();
    let mut ids = put(r, "admin", Value::Array(ops));
    land(r, "admin").expect("land the profiles");
    ids.insert("$admin".into(), admin);
    ids
}

/// The site of `profile` at `scope`, built in memory.
fn site(st: &ReadState, scope: &str, profile: &str) -> SiteFiles {
    build_profile_site(st, scope, profile).unwrap_or_else(|e| panic!("{profile} at {scope}: {e}"))
}

/// A file of a site, as text.
fn page(s: &SiteFiles, path: &str) -> String {
    String::from_utf8(
        s.files
            .get(path)
            .unwrap_or_else(|| panic!("no file {path}: {:?}", s.files.keys()))
            .clone(),
    )
    .unwrap()
}

/// Every path whose name or bytes hold `word`.
fn holding(s: &SiteFiles, word: &str) -> Vec<String> {
    holding_in(&s.files, word.as_bytes())
}

fn holding_in(files: &BTreeMap<String, Vec<u8>>, word: &[u8]) -> Vec<String> {
    files
        .iter()
        .filter(|(p, b)| {
            p.as_bytes().windows(word.len()).any(|w| w == word)
                || b.windows(word.len()).any(|w| w == word)
        })
        .map(|(p, _)| p.clone())
        .collect()
}

#[track_caller]
fn assert_absent(s: &SiteFiles, word: &str) {
    assert_eq!(holding(s, word), Vec::<String>::new(), "{word}");
}

/// [`assert_absent`] over every file but the theme's own assets (`assets/site.css`,
/// `assets/app.js`...), for a word the theme's code may use: a figure's file under `assets/` is
/// still checked.
#[track_caller]
fn assert_absent_pages(s: &SiteFiles, word: &str) {
    let theme = |p: &str| {
        p.starts_with("assets/")
            && !p
                .trim_start_matches("assets/")
                .chars()
                .take(64)
                .all(|c| c.is_ascii_hexdigit())
    };
    let found: Vec<String> = holding(s, word).into_iter().filter(|p| !theme(p)).collect();
    assert_eq!(found, Vec::<String>::new(), "{word}");
}

/// Every file under `dir`, by its path relative to `dir`, dotfiles included.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(at).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                out.insert(rel.replace('\\', "/"), std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    if dir.exists() {
        walk(dir, dir, &mut out);
    }
    out
}

/// The href of the one `<a>` whose whole text is `text` on `html`; none when `text` stands only
/// as plain text (in running text, or in an attribute such as a copied reference's words).
/// Panics when `text` is not on the page.
fn link_of(html: &str, text: &str) -> Option<String> {
    assert!(html.contains(text), "{text} is not on the page");
    let links: Vec<String> = html
        .match_indices(&format!(">{text}</a>"))
        .map(|(at, _)| {
            let tag = &html[html[..at].rfind('<').unwrap()..at];
            assert!(tag.starts_with("<a "), "{tag}");
            let h = tag.find("href=\"").expect("an href") + 6;
            tag[h..h + tag[h..].find('"').unwrap()].to_string()
        })
        .collect();
    assert!(links.len() <= 1, "{text}: {links:?}");
    links.into_iter().next()
}

/// An 8-bit RGB PNG of `w` × `h`, its one IDAT stored (no compression).
fn png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for b in data {
            c ^= u32::from(*b);
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let line: Vec<u8> = std::iter::once(0)
        .chain(rgb.iter().copied().cycle().take(3 * w as usize))
        .collect();
    let raw = line.repeat(h as usize);
    let (mut a, mut b) = (1u32, 0u32);
    for x in &raw {
        a = (a + u32::from(*x)) % 65521;
        b = (b + a) % 65521;
    }
    let mut z = vec![0x78, 0x01, 1];
    let len = raw.len() as u16;
    z.extend_from_slice(&len.to_le_bytes());
    z.extend_from_slice(&(!len).to_le_bytes());
    z.extend_from_slice(&raw);
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = vec![];
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Whether some file of `files` holds exactly `bytes`, or holds them inside it.
fn carries(files: &BTreeMap<String, Vec<u8>>, bytes: &[u8]) -> Vec<String> {
    holding_in(files, bytes)
}

/// Applies one approved redaction of `targets` to the closed project at `dir`, then opens it.
fn redact(dir: &Path, targets: Vec<RedactTarget>) -> (Rdy, Hash) {
    let f = authorized_batch_fixture(
        &dir.join(".rdy/store"),
        &[RequestSpec::new("redact", targets)],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    (Rdy::open(dir).unwrap(), f.requests[0].request)
}

// --- 1. every surface ---------------------------------------------------------------------------

#[test]
fn pub6_every_site_surface_uses_the_selection() {
    let (dir, mut r) = tiny();
    let chosen_png = png(5, 3, [10, 120, 200]);
    let sib_png = png(4, 7, [201, 33, 44]);
    let chosen_pdf = b"%PDF-1.4 chosen download bytes".to_vec();
    let sib_pdf = b"%PDF-1.4 SIBCANARY download bytes".to_vec();
    let chosen_blob = r.store_mut().put_blob(&chosen_png).unwrap();
    let sib_blob = r.store_mut().put_blob(&sib_png).unwrap();
    let chosen_dl = r.store_mut().put_blob(&chosen_pdf).unwrap();
    let sib_dl = r.store_mut().put_blob(&sib_pdf).unwrap();
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area", "title": "Area own title", "description": "Area own description."}},
            {"op": "create", "type": "doc", "id": "$chosen", "parent": "$area", "fields": {"name": "chosen", "title": "Chosen page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$cs", "parent": "$chosen", "fields": {"title": "Chosen section"}},
            {"op": "create", "type": "prose", "id": "$cp", "parent": "$cs", "body": "Chosen prose words."},
            {"op": "create", "type": "term", "id": "$ct", "parent": "$cs", "fields": {"name": "Chosen term"}, "body": "Chosen term body."},
            {"op": "create", "type": "question", "id": "$cq", "parent": "$cs", "fields": {"prompt": "Chosen question prompt?"}},
            {"op": "create", "type": "figure", "id": "$cf", "parent": "$cs", "fields": {"blob": chosen_blob.to_string(), "media_type": "image/png", "alt": "Chosen figure alt"}},
            {"op": "create", "type": "figure", "id": "$cd", "parent": "$cs", "fields": {"blob": chosen_dl.to_string(), "media_type": "application/pdf", "alt": "Chosen download"}},
            {"op": "create", "type": "doc", "id": "$sib", "parent": "$area", "fields": {"name": "sibcanaryname", "title": "SIBCANARY doc title", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$ss", "parent": "$sib", "fields": {"title": "SIBCANARY section"}},
            {"op": "create", "type": "prose", "id": "$sp", "parent": "$ss", "body": "SIBCANARY prose."},
            {"op": "create", "type": "term", "id": "$st", "parent": "$ss", "fields": {"name": "SIBCANARY term"}, "body": "SIBCANARY term body."},
            {"op": "create", "type": "question", "id": "$sq", "parent": "$ss", "fields": {"prompt": "SIBCANARY question?"}},
            {"op": "create", "type": "figure", "id": "$sf", "parent": "$ss", "fields": {"blob": sib_blob.to_string(), "media_type": "image/png", "alt": "SIBCANARY figure alt"}},
            {"op": "create", "type": "figure", "id": "$sd", "parent": "$ss", "fields": {"blob": sib_dl.to_string(), "media_type": "application/pdf", "alt": "SIBCANARY download"}},
            {"op": "create", "type": "folder", "id": "$sub", "parent": "$area", "fields": {"name": "subcanaryname", "title": "SUBCANARY folder"}},
            {"op": "create", "type": "doc", "id": "$subd", "parent": "$sub", "fields": {"name": "subdoc", "title": "SUBCANARY doc", "doc_type": "page"}},
            {"op": "create", "type": "folder", "id": "$far", "fields": {"name": "farcanaryname", "title": "FARCANARY folder", "description": "FARCANARY description."}},
            {"op": "create", "type": "doc", "id": "$fard", "parent": "$far", "fields": {"name": "faraway", "title": "FARCANARY doc", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$fard", "body": "FARCANARY words."}
        ]),
    );
    let i = |k: &str| ids[k].clone();
    r.decide(
        &writer(),
        "w1",
        rdy_engine::DecisionInput {
            parent: i("$cs").to_string(),
            statement: "Chosen ruling words.".into(),
            affects: vec![i("$cp").to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    let sib_decision = r
        .decide(
            &writer(),
            "w1",
            rdy_engine::DecisionInput {
                parent: i("$ss").to_string(),
                statement: "SIBCANARY ruling.".into(),
                affects: vec![i("$sp").to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let sib_decision = bid(&sib_decision.created["$decision"]);
    land(&mut r, "w1").unwrap();
    let p = profiles(&mut r, &[("surfacekey", "org", vec![&i("$chosen")])]);
    let st = r.read();

    // the canaries are real: the full source shows them
    let full = build_site(&st, &Scope::Canon, rdy_render::Mode::Static).unwrap();
    for word in ["SIBCANARY", "SUBCANARY", "FARCANARY", "Tiny"] {
        assert!(!holding(&full, word).is_empty(), "{word}");
    }

    let s = site(&st, "canon", "surfacekey");
    // no file name, page, permalink, hidden attribute, script data, search entry, llms line or
    // download holds an excluded block, its id, its file or the project's own name
    let mut excluded: Vec<String> = [
        "$sib", "$ss", "$sp", "$st", "$sq", "$sf", "$sd", "$sub", "$subd", "$far", "$fard",
    ]
    .iter()
    .map(|k| i(k).to_string())
    .collect();
    excluded.push(sib_decision.to_string());
    excluded.push(p["$admin"].to_string());
    excluded.push(p["$surfacekey"].to_string());
    for word in [
        "SIBCANARY",
        "SUBCANARY",
        "FARCANARY",
        "sibcanaryname",
        "subcanaryname",
        "farcanaryname",
        "Admin page",
        "Tiny",
        &sib_blob.to_string(),
        &sib_dl.to_string(),
    ]
    .into_iter()
    .chain(excluded.iter().map(String::as_str))
    {
        assert_absent(&s, word);
    }
    assert_eq!(carries(&s.files, &sib_png), Vec::<String>::new());
    assert_eq!(carries(&s.files, &sib_pdf), Vec::<String>::new());

    // the selection and its ancestor folder's own record remain
    let chosen = page(&s, "area/chosen.html");
    for want in [
        "Chosen prose words.",
        "Chosen term body.",
        "Chosen question prompt?",
        "Chosen ruling words.",
    ] {
        assert!(chosen.contains(want), "{want}");
    }
    let area = page(&s, "area/index.html");
    assert!(area.contains("Area own title") && area.contains("Area own description."));
    assert!(page(&s, "glossary.html").contains("Chosen term"));
    assert!(page(&s, "questions.html").contains("Chosen question prompt?"));
    let home = page(&s, "index.html");
    assert!(home.contains("Title of surfacekey") && home.contains("Description of surfacekey."));
    assert!(page(&s, "search-index.json").contains("Chosen prose words."));
    assert!(page(&s, "llms.txt").contains("Chosen page"));
    assert!(page(&s, "llms-full.txt").contains("Chosen prose words."));
    assert!(s.files.contains_key(&format!("go/{}.html", i("$cp"))));
    assert!(!s.files.contains_key(&format!("go/{}.html", i("$sp"))));
    assert_eq!(
        s.files.get(&format!("assets/{chosen_blob}.png")),
        Some(&chosen_png)
    );
    assert_eq!(carries(&s.files, &chosen_pdf).len(), 1, "the download");
    assert!(s.summary.warnings.is_empty(), "{:?}", s.summary.warnings);
    drop(st);
    drop(dir);
}

// --- 2. the trail -------------------------------------------------------------------------------

#[test]
fn pub6_trail_requires_the_decision_home_and_profile_permission() {
    let (_dir, mut r) = tiny();
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$gov", "fields": {"name": "governed", "title": "Governed page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$gs", "parent": "$gov", "fields": {"title": "Governed section"}},
            {"op": "create", "type": "prose", "id": "$gp", "parent": "$gs", "body": "Governed words."},
            {"op": "create", "type": "doc", "id": "$rul", "fields": {"name": "rulings", "title": "Rulings page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$rs", "parent": "$rul", "fields": {"title": "Rulings section"}}
        ]),
    );
    r.decide(
        &writer(),
        "w1",
        rdy_engine::DecisionInput {
            parent: ids["$rs"].to_string(),
            statement: "TRAILCANARY ruling.".into(),
            affects: vec![ids["$gp"].to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "w1").unwrap();
    let (gov, rul) = (&ids["$gov"], &ids["$rul"]);
    profiles(
        &mut r,
        &[
            ("orgnarrow", "org", vec![gov]),
            ("orgwide", "org", vec![gov, rul]),
            ("pubwide", "public", vec![gov, rul]),
        ],
    );
    let st = r.read();
    // the source shows the trail
    let full = build_site(&st, &Scope::Canon, rdy_render::Mode::Static).unwrap();
    assert!(page(&full, "governed.html").contains("TRAILCANARY ruling."));

    // the governed doc without the decision's home: no decision text anywhere, no trail
    let narrow = site(&st, "canon", "orgnarrow");
    assert_absent(&narrow, "TRAILCANARY");
    assert_absent_pages(&narrow, "data-rdy=\"trail\"");
    // with its home, an org profile shows the trail on the governed block
    let wide = site(&st, "canon", "orgwide");
    let gp = page(&wide, "governed.html");
    let trail = gp.find("data-rdy=\"trail\"").expect("the trail");
    assert!(gp[trail..].contains("TRAILCANARY ruling."), "{gp}");
    // a public profile's default: no trail markup, data or control on any page
    let public = site(&st, "canon", "pubwide");
    for word in [
        "data-rdy=\"trail\"",
        "class=\"why\"",
        "<summary>Why</summary>",
        "Ruled:",
    ] {
        assert_absent_pages(&public, word);
    }
    assert!(page(&public, "governed.html").contains("Governed words."));
}

// --- 3. history ---------------------------------------------------------------------------------

#[test]
fn pub6_profile_has_no_source_versions_or_since_history() {
    let (dir, mut r) = tiny();
    let ids = put_land(
        &mut r,
        "w0",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$np", "parent": "$n", "body": "OLDCANARY text."},
            {"op": "create", "type": "doc", "id": "$gone", "fields": {"name": "gone", "title": "Gone page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$gp", "parent": "$gone", "body": "REMOVEDCANARY words."}
        ]),
    );
    r.tag(
        &approver(),
        "v0.1.0",
        None,
        Some("TAGMSGCANARY zero".into()),
    )
    .unwrap();
    let mut more = put(
        &mut r,
        "w1",
        json!([
            {"op": "update", "id": ids["$np"].to_string(), "body": "Current text."},
            {"op": "create", "type": "doc", "id": "$cs", "fields": {"name": "changes-since", "title": "Ordinary since page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$cs", "body": "Selected since words."}
        ]),
    );
    land(&mut r, "w1").unwrap();
    let cs = more.remove("$cs").unwrap();
    profiles(&mut r, &[("hist", "org", vec![&ids["$n"], &cs])]);
    let tag = r
        .tag(&approver(), "v1.0.0", None, Some("TAGMSGCANARY one".into()))
        .unwrap();
    let landing = tag.landing.clone();
    put_land(
        &mut r,
        "w2",
        json!([{"op": "create", "type": "doc", "fields": {"name": "later", "title": "Later page", "doc_type": "page"}}]),
    );
    {
        let st = r.read();
        // the source shows its history
        let full = build_site(&st, &Scope::Canon, rdy_render::Mode::Static).unwrap();
        assert!(!holding(&full, "TAGMSGCANARY").is_empty());

        let canon = site(&st, "canon", "hist");
        let at_landing = site(&st, &format!("landing:{landing}"), "hist");
        let tagged = site(&st, "tag:v1.0.0", "hist");
        let bare = site(&st, "tag:1.0.0", "hist");
        assert_eq!(
            tagged.files, bare.files,
            "tag:1.0.0 is the stored tag v1.0.0"
        );
        for s in [&canon, &at_landing, &tagged] {
            for word in [
                "TAGMSGCANARY",
                "OLDCANARY",
                "REMOVEDCANARY",
                "v0.1.0",
                "unversioned",
                "update",
                "landing",
                "human:qa",
                "chip",
                "snapshot</span>",
                "#versions",
                "id=\"versions\"",
                "Versions",
                "No versions yet",
                "review-changes",
                "Changes in this",
                "changes since",
                "data-menu",
                "Later page",
                &landing,
            ] {
                assert_absent_pages(s, word);
            }
            // the selected doc named changes-since is an ordinary doc page
            let since = page(s, "changes-since.html");
            assert!(
                since.contains("Ordinary since page") && since.contains("Selected since words.")
            );
        }
        // canon and a bare landing at the tag's commit: no label and no empty wrapper
        for (s, name) in [(&canon, "canon"), (&at_landing, "landing")] {
            assert_absent(s, "v1.0.0");
            assert_absent_pages(s, "data-rdy-version");
            assert_absent_pages(s, "data-rdy-label");
            let home = page(s, "index.html");
            assert!(
                home.contains("<p class=\"colophon\">Title of hist.</p>"),
                "{name}: {home}"
            );
            assert!(
                page(s, "llms.txt").contains("\n\nTitle of hist.\n\n"),
                "{name}"
            );
            assert!(!s.label_at.contains_key("llms.txt"), "{name}");
        }
        // the chosen tag: exactly its stored name, a span with no link, list or message
        let home = page(&tagged, "index.html");
        assert!(
            home.contains("<span class=\"pill-text\">v1.0.0</span></span></span>"),
            "{home}"
        );
        assert!(
            home.contains("A snapshot of Title of hist at <span data-rdy-label>v1.0.0</span>.")
        );
        assert_absent_pages(&tagged, "<a class=\"pill\"");
        assert_absent_pages(&tagged, "redacted");
        assert!(page(&tagged, "llms.txt").contains("A snapshot of Title of hist at v1.0.0."));
    }

    // an affected chosen tag keeps only the existing redacted badge
    r.close().unwrap();
    let (r, request) = redact(
        dir.path(),
        vec![RedactTarget::AllVersions(ids["$gp"].clone())],
    );
    let st = r.read();
    let tagged = site(&st, "tag:v1.0.0", "hist");
    let home = page(&tagged, "index.html");
    assert!(
        home.contains(
            "<span class=\"pill-text\">v1.0.0</span></span> <span class=\"badge badge-redacted\">redacted</span></span>"
        ),
        "{home}"
    );
    for word in [
        &request.to_string(),
        "TAGMSGCANARY",
        "Content removed",
        "v0.1.0",
    ] {
        assert_absent_pages(&tagged, word);
    }
    assert_absent_pages(&site(&st, "canon", "hist"), "redacted");
}

// --- 4. reference words -------------------------------------------------------------------------

#[test]
fn pub6_reference_words_are_uniform_across_surfaces() {
    let (_dir, mut r) = tiny();
    let blob = r.store_mut().put_blob(&png(3, 3, [1, 2, 3])).unwrap();
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$hid", "fields": {"name": "hidden", "title": "HIDDENCANARY doc", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$hs", "parent": "$hid", "fields": {"title": "HIDDENCANARY section"}},
            {"op": "create", "type": "term", "id": "$ht", "parent": "$hs", "fields": {"name": "HIDDENCANARY term", "key": "HIDDEN-KEY"}, "body": "HIDDENCANARY definition."},
            {"op": "create", "type": "prose", "id": "$hp", "parent": "$hs", "body": "HIDDENCANARY prose."},
            {"op": "create", "type": "doc", "id": "$refs", "fields": {"name": "refs", "title": "Refs page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$rs", "parent": "$refs", "fields": {"title": "Heading {{$ht}}"}},
            {"op": "create", "type": "prose", "id": "$r1", "parent": "$rs", "body": "Explicit {{$hs|Shown display words}} and bare {{$hs}} and typed {{$ht|@key}}."},
            {"op": "create", "type": "table", "parent": "$rs", "fields": {"columns": ["Col"], "rows": [["Cell {{$hp}}"]]}},
            {"op": "create", "type": "figure", "parent": "$rs", "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "Alt {{$hp}}"}},
            {"op": "create", "type": "diagram", "parent": "$rs", "fields": {"lang": "mermaid", "alt": "Diagram alt {{$hp}}"}, "body": "graph TD\n  A[{{$hp}}] --> B[{{$hs|Diagram display}}]"},
            {"op": "create", "type": "term", "parent": "$rs", "fields": {"name": "Glossed term"}, "body": "Defined via {{$ht}}."},
            {"op": "create", "type": "question", "parent": "$rs", "fields": {"prompt": "Ask about {{$hp}}?"}},
            // fields read as written: a value and its unit, an alias, an entity's kind, a code
            // language, and a prose reference to the value, which reads as its value
            {"op": "create", "type": "value", "id": "$val", "parent": "$rs", "fields": {"name": "Budget", "value": "5 {{$hp}}", "unit": "per {{$hs|Unit words}}"}},
            {"op": "create", "type": "prose", "parent": "$rs", "body": "See VALREF {{$val}} now."},
            {"op": "create", "type": "term", "parent": "$rs", "fields": {"name": "Aliased term", "aliases": ["Alias {{$hp}}", "Shown {{$hp|Alias words}}"]}, "body": "Has aliases."},
            {"op": "create", "type": "entity", "parent": "$rs", "fields": {"name": "Kinded thing", "kind": "kind {{$hp}}"}, "body": "Has a kind."},
            {"op": "create", "type": "code", "parent": "$rs", "fields": {"lang": "{{$hp}}"}, "body": "x = 1"},
            // fix round 2: a reference's words stay verbatim in a kind and a code language; the
            // author's own words around them, and a field with no reference, keep their format
            {"op": "create", "type": "entity", "parent": "$rs", "fields": {"name": "Explicit kinded thing", "kind": "{{$hp|api_v2}}"}, "body": "Has an explicit kind."},
            {"op": "create", "type": "entity", "parent": "$rs", "fields": {"name": "Mixed kinded thing", "kind": "side_kind {{$hp|api_v2}}"}, "body": "Has a mixed kind."},
            {"op": "create", "type": "entity", "parent": "$rs", "fields": {"name": "Plain kinded thing", "kind": "api_v2"}, "body": "Has a plain kind."},
            {"op": "create", "type": "code", "parent": "$rs", "fields": {"lang": "{{$hp|python}}"}, "body": "y = 2"},
            {"op": "create", "type": "code", "parent": "$rs", "fields": {"lang": "python"}, "body": "z = 3"}
        ]),
    );
    land(&mut r, "w1").unwrap();
    // a reference to no block of the source at all, written raw (the write path refuses one)
    let missing = raw(
        "p-M1SS1",
        "prose",
        Some(ids["$rs"].as_str()),
        "zz",
        &[],
        "Missing {{p-ZZZZZ}} here.",
    );
    let change = Change {
        view: "raw".into(),
        ops: vec![Op::Create { block: missing }],
        deps: vec![],
        provenance: writer().provenance(),
        justification: None,
        message: "raw".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    r.store_mut().commit_to_view("raw", change).unwrap();
    r.invalidate();
    land_with_a_tie(&mut r, "raw");
    profiles(&mut r, &[("refsorg", "org", vec![&ids["$refs"]])]);
    let st = r.read();
    let resolved =
        rdy_engine::publication::resolve(&st, &Scope::Canon, "refsorg").expect("resolve");
    let count = resolved.references.blocked_count;
    assert!(count >= 12, "{count}");

    let s = site(&st, "canon", "refsorg");
    // the org warning comes first, in the source reply only
    assert_eq!(
        s.summary.warnings.first().map(String::as_str),
        Some(
            format!(
                "Org profile has {count} references outside its selection. Review these references before you share the output."
            )
            .as_str()
        )
    );
    assert_absent(&s, "Org profile has");
    for word in ["unresolved", "could not be resolved"] {
        assert_absent_pages(&s, word);
    }
    for word in [
        "HIDDENCANARY",
        "HIDDEN-KEY",
        "hidden-key",
        "p-ZZZZZ",
        &ids["$hid"].to_string(),
        &ids["$hs"].to_string(),
        &ids["$ht"].to_string(),
        &ids["$hp"].to_string(),
    ] {
        assert_absent(&s, word);
    }
    let refs = page(&s, "refs.html");
    let u = UNAVAILABLE;
    for want in [
        format!("<p>Explicit Shown display words and bare {u} and typed {u}.</p>"),
        format!(">Heading {u}</h2>"),
        format!("alt=\"Alt {u}\""),
        format!("<figcaption>Alt {u}</figcaption>"),
        format!("<td>Cell {u}</td>"),
        format!("<p class=\"visually-hidden\">Diagram alt {u}</p>"),
        format!("<figcaption>Diagram alt {u}</figcaption>"),
        format!("A[{u}] --&gt; B[Diagram display]"),
        format!("<p>Missing {u} here.</p>"),
        format!("Ask about {u}?"),
        format!("<p class=\"entity-value\">5 {u} per Unit words</p>"),
        format!(">5 {u} per Unit words</a> now."),
        format!("<p class=\"entity-aliases\">Also: Alias {u}, Shown Alias words</p>"),
        format!("<span class=\"marker\">Kind {u}</span>"),
        format!("<span class=\"lang\">{u}</span>"),
        format!("class=\"language-{u}\""),
        "<span class=\"marker\">api_v2</span>".to_string(),
        "<span class=\"marker\">Side kind api_v2</span>".to_string(),
        "<span class=\"marker\">Api v2</span>".to_string(),
        "<span class=\"lang\">python</span>".to_string(),
        "<span class=\"lang\">Python</span>".to_string(),
    ] {
        assert!(refs.contains(&want), "{want}\n{refs}");
    }
    // the marker words are exact on every surface: never capitalized or reworded
    assert_absent_pages(&s, "Unavailable reference");
    // a code block's label: a reference's words as written, else the language's own name
    for label in [
        format!("{u} code 1"),
        "python code 2".into(),
        "Python code 3".into(),
    ] {
        assert!(!holding(&s, &label).is_empty(), "{label}");
    }
    assert_absent_pages(&s, "Python code 2");
    // the drawing, its file and the readable source read the same selected words
    let at = refs.find("<span class=\"diagram-svg\"").expect("a drawing");
    let drawn = &refs[at..];
    let drawn = &drawn[..drawn.find("</svg>").unwrap()];
    assert!(
        drawn.contains(&format!(">{u}<")) && drawn.contains(">Diagram display<"),
        "{drawn}"
    );
    let files: Vec<&String> = s
        .files
        .keys()
        .filter(|p| p.starts_with("assets/diagrams/"))
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let file = page(&s, files[0]);
    assert!(file.contains(&format!(">{u}<")) && file.contains(">Diagram display<"));
    assert!(refs.contains(&format!(
        "<pre class=\"code\"><code>graph TD\n  A[{u}] --&gt; B[Diagram display]</code>"
    )));
    // the readable source and the copied reference's words
    assert_eq!(
        refs.matches(&format!("A[{u}] --&gt; B[Diagram display]"))
            .count(),
        2
    );
    assert!(page(&s, "glossary.html").contains(&format!("Defined via {u}.")));
    assert!(page(&s, "questions.html").contains(&format!("Ask about {u}?")));
    // the summaries: a copied reference's words, the home card, the search entries, llms
    assert!(
        refs.contains(&format!("bare {u} and typed {u}")),
        "data-ref"
    );
    assert!(page(&s, "search-index.json").contains(&format!("bare {u} and typed {u}")));
    assert!(
        page(&s, "llms-full.txt").contains(&format!("Explicit Shown display words and bare {u}"))
    );
    // a field read as written reads its references the same way on every surface
    let value = format!("5 {u} per Unit words");
    assert!(
        page(&s, "search-index.json").contains(&format!("Budget: {value}")),
        "search"
    );
    assert!(
        refs.contains(&format!("See VALREF {value} now.")),
        "data-ref"
    );
    assert!(page(&s, "llms-full.txt").contains(&value), "llms-full");
    assert!(
        holding(&s, &value).iter().any(|p| p.starts_with("go/")),
        "permalink"
    );
    // the source reads these fields as written, as before
    let full = build_site(&st, &Scope::Canon, rdy_render::Mode::Static).unwrap();
    let source = page(&full, "refs.html");
    let hp = ids["$hp"].to_string();
    for want in [
        format!(
            "<p class=\"entity-value\">5 {{{{{hp}}}}} per {{{{{}|Unit words}}}}</p>",
            ids["$hs"]
        ),
        format!("Also: Alias {{{{{hp}}}}}, Shown {{{{{hp}|Alias words}}}}"),
        format!("class=\"language-{{{{{hp}}}}}\""),
    ] {
        assert!(source.contains(&want), "{want}\n{source}");
    }
}

/// Fix round 1: the profile's own title and description read their references through the
/// selection, though the profile's home doc is excluded; the source keeps the project's name.
#[test]
fn pub6_profile_metadata_reads_references_through_the_selection() {
    let (_dir, mut r) = tiny();
    let ids = put_land(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$hid", "fields": {"name": "hidden", "title": "HIDDENCANARY doc", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$hp", "parent": "$hid", "body": "HIDDENCANARY prose."},
            {"op": "create", "type": "doc", "id": "$pub", "fields": {"name": "shown", "title": "Shown page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$pub", "body": "Shown words."}
        ]),
    );
    let (hp, shown) = (ids["$hp"].to_string(), &ids["$pub"]);
    let admin = put(
        &mut r,
        "admin",
        json!([{"op": "create", "type": "doc", "id": "$admin",
            "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}]),
    )["$admin"]
        .clone();
    let mut op = profile_op("meta", "org", &admin, &[shown]);
    op["fields"]["title"] = json!(format!("Guide {{{{{hp}}}}}"));
    op["fields"]["description"] = json!(format!("Description {{{{{hp}|Author words}}}}"));
    put(&mut r, "admin", json!([op]));
    land(&mut r, "admin").expect("land the profile");
    let st = r.read();
    let s = site(&st, "canon", "meta");
    for word in ["HIDDENCANARY", hp.as_str(), "Admin page", "{{"] {
        assert_absent_pages(&s, word);
    }
    let u = UNAVAILABLE;
    let (title, description) = (format!("Guide {u}"), "Description Author words");
    let home = page(&s, "index.html");
    assert!(home.contains(&title), "{home}");
    assert!(home.contains(description), "{home}");
    assert!(page(&s, "shown.html").contains(&title));
    for llms in ["llms.txt", "llms-full.txt"] {
        let t = page(&s, llms);
        assert!(
            t.starts_with(&format!("# {title}\n\n> {description}\n")),
            "{t}"
        );
    }
    // the source names the project, never the profile
    let full = build_site(&st, &Scope::Canon, rdy_render::Mode::Static).unwrap();
    assert!(page(&full, "llms.txt").starts_with("# Tiny\n"));
    assert_eq!(holding(&full, "Author words"), Vec::<String>::new());
}

/// Final fix round 1: the chosen profile's title and description count in the check (the
/// report, the public refusal and the org warning) though the profile's home doc is excluded;
/// the home doc stays out of the selection.
#[test]
fn pub6_profile_metadata_references_count_in_the_check() {
    let (dir, mut r) = tiny();
    let ids = put_land(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$hid", "fields": {"name": "hidden", "title": "HIDDENCANARY doc", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$hp", "parent": "$hid", "body": "HIDDENCANARY prose."},
            {"op": "create", "type": "doc", "id": "$pub", "fields": {"name": "shown", "title": "Shown page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$pub", "body": "Shown words."}
        ]),
    );
    let (hid, shown) = (ids["$hid"].to_string(), &ids["$pub"]);
    let admin = put(
        &mut r,
        "admin",
        json!([{"op": "create", "type": "doc", "id": "$admin",
            "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}]),
    )["$admin"]
        .clone();
    let mut public = profile_op("metap", "public", &admin, &[shown]);
    public["fields"]["title"] = json!(format!("Metadata {{{{{hid}}}}}"));
    let mut org = profile_op("metao", "org", &admin, &[shown]);
    org["fields"]["description"] = json!(format!("About {{{{{hid}|Author words}}}}."));
    let clean = profile_op("cleanp", "public", &admin, &[shown]);
    let pids = put(&mut r, "admin", json!([public, org, clean]));
    land(&mut r, "admin").expect("land the profiles");
    let st = r.read();
    for (key, field, status) in [
        ("metap", "/fields/title", "blocked"),
        ("metao", "/fields/description", "warning"),
    ] {
        let v = rdy_engine::publication::check(&st, key, Some("canon"), None).unwrap();
        let refs = &v["references"];
        assert_eq!(refs["status"], status, "{key}: {v}");
        assert_eq!(
            (&refs["excluded"], &refs["missing"], &refs["total"]),
            (&json!(1), &json!(0), &json!(1)),
            "{key}: {v}"
        );
        let text = v.to_string();
        assert!(text.contains(field), "{key}: {text}");
        assert!(
            text.contains(&pids[&format!("${key}")].to_string()),
            "{key}: {text}"
        );
        assert!(!text.contains(&admin.to_string()), "{key}: {text}");
        for leak in ["HIDDENCANARY", "Author words", "Admin page"] {
            assert!(!text.contains(leak), "{leak} in {key}: {text}");
        }
    }
    let v = rdy_engine::publication::check(&st, "cleanp", Some("canon"), None).unwrap();
    assert_eq!(v["references"]["status"], "clear", "{v}");
    // the public profile refuses before anything is written; the org profile warns
    let out = dir.path().join("metap-site");
    let e = render_profile_site(&st, "canon", "metap", &out).unwrap_err();
    assert_eq!(e.code, E_PUBLICATION_REFERENCE, "{e}");
    assert!(!out.exists());
    let e = rdy_render::print::export_profile(
        &st,
        "canon",
        "metap",
        &dir.path().join("metap.html"),
        rdy_render::print::Format::Html,
        None,
    )
    .unwrap_err();
    assert_eq!(e.code, E_PUBLICATION_REFERENCE, "{e}");
    assert!(!dir.path().join("metap.html").exists());
    let s = site(&st, "canon", "metao");
    assert_absent(&s, "HIDDENCANARY");
    assert!(page(&s, "index.html").contains("About Author words."));
    let summary = render_profile_site(&st, "canon", "metao", &dir.path().join("metao")).unwrap();
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| w.starts_with("Org profile has 1 references outside its selection.")),
        "{:?}",
        summary.warnings
    );
}

/// Fix round 1: a partial supersession's part reads its references through the selection, with
/// the author's display words kept; the source trail keeps its existing text.
#[test]
fn pub6_partial_supersession_reads_references_through_the_selection() {
    let (_dir, mut r) = tiny();
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$hid", "fields": {"name": "hidden", "title": "HIDDENCANARY doc", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$hp", "parent": "$hid", "body": "HIDDENCANARY prose."},
            {"op": "create", "type": "doc", "id": "$gov", "fields": {"name": "governed", "title": "Governed page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$gs", "parent": "$gov", "fields": {"title": "Governed section"}},
            {"op": "create", "type": "prose", "id": "$gp", "parent": "$gs", "body": "Governed words."}
        ]),
    );
    let hp = ids["$hp"].to_string();
    let old = r
        .decide(
            &writer(),
            "w1",
            rdy_engine::DecisionInput {
                parent: ids["$gs"].to_string(),
                statement: format!("Rule {{{{{hp}}}}} and clause {{{{{hp}|Clause words}}}}."),
                affects: vec![ids["$gp"].to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let old = old.created["$decision"].clone();
    land(&mut r, "w1").unwrap();
    for (view, part, statement) in [
        ("w2", format!("Rule {{{{{hp}}}}}"), "Replacement rule."),
        (
            "w3",
            format!("clause {{{{{hp}|Part words}}}}"),
            "Replacement clause.",
        ),
    ] {
        r.supersede(
            &writer(),
            view,
            &old,
            Some(part),
            rdy_engine::DecisionInput {
                parent: ids["$gs"].to_string(),
                statement: statement.into(),
                ..Default::default()
            },
        )
        .unwrap();
        land(&mut r, view).unwrap();
    }
    profiles(&mut r, &[("trailorg", "org", vec![&ids["$gov"]])]);
    let st = r.read();
    // the source trail reads the part as written, as before
    let full = build_site(&st, &Scope::Canon, rdy_render::Mode::Static).unwrap();
    assert!(
        page(&full, "governed.html").contains(&format!(
            "Partly superseded (Rule {{{{{hp}}}}}): Replacement rule."
        )),
        "source"
    );

    let s = site(&st, "canon", "trailorg");
    for word in ["HIDDENCANARY", hp.as_str()] {
        assert_absent(&s, word);
    }
    let u = UNAVAILABLE;
    let gov = page(&s, "governed.html");
    for want in [
        format!("Rule {u} and clause Clause words."),
        format!("Partly superseded (Rule {u}): Replacement rule."),
        "Partly superseded (clause Part words): Replacement clause.".to_string(),
    ] {
        assert!(gov.contains(&want), "{want}\n{gov}");
    }
}

// --- 5. authored local links --------------------------------------------------------------------

#[test]
fn pub6_authored_local_links_cannot_reach_excluded_output() {
    let (_dir, mut r) = tiny();
    let ok_2048 = format!("{}b.html", "./".repeat(1021));
    assert_eq!(ok_2048.len(), 2048);
    let long_2049 = format!("{ok_2048}#");
    // an external target is bounded the same way, in UTF-8 bytes
    let ext_2048 = format!("https://example.com/é{}", "a".repeat(2048 - 22));
    assert_eq!(ext_2048.len(), 2048);
    let ext_2049 = format!("{ext_2048}a");
    let body = [
        "[LINK01](b.html)",
        "[LINK02](b.html#target-section)",
        "[LINK03](/docs/b.html)",
        "[LINK04](b%2Ehtml)",
        "[LINK05](../secret.html)",
        "[LINK06](/secret.html)",
        "[LINK07](b.html#hidden-section)",
        "[LINK08](../../xnowhere.html)",
        "[LINK09](b.html?x=1)",
        "[LINK10](b%ZZ.html)",
        "[LINK11](../assets/site.css)",
        "[LINK12](https://example.com/x)",
        "[LINK13](mailto:a@example.com)",
        &format!("[LINK14]({ok_2048})"),
        &format!("[LINK15]({long_2049})"),
        "[LINK16](#own-section)",
        "[LINK17](../secret.html#hidden-section)",
        "[LINK18](../index.html)",
        &format!("[LINK19]({ext_2048})"),
        &format!("[LINK20]({ext_2049})"),
    ]
    .join(" ");
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$docs", "fields": {"name": "docs", "title": "Docs"}},
            {"op": "create", "type": "doc", "id": "$a", "parent": "$docs", "fields": {"name": "a", "title": "Page A", "doc_type": "page"}},
            {"op": "create", "type": "section", "parent": "$a", "id": "$as", "fields": {"title": "Own section"}},
            {"op": "create", "type": "prose", "parent": "$as", "body": body},
            {"op": "create", "type": "doc", "id": "$b", "parent": "$docs", "fields": {"name": "b", "title": "Page B", "doc_type": "page"}},
            {"op": "create", "type": "section", "parent": "$b", "fields": {"title": "Target section"}},
            {"op": "create", "type": "prose", "parent": "$b", "body": "B words."},
            {"op": "create", "type": "doc", "id": "$secret", "fields": {"name": "secret", "title": "Secret page", "doc_type": "page"}},
            {"op": "create", "type": "section", "parent": "$secret", "fields": {"title": "Hidden section"}}
        ]),
    );
    land(&mut r, "w1").unwrap();
    profiles(&mut r, &[("links", "public", vec![&ids["$docs"]])]);
    let st = r.read();
    // the source keeps its own rules
    let full = build_site(&st, &Scope::Canon, rdy_render::Mode::Static).unwrap();
    assert_eq!(
        link_of(&page(&full, "docs/a.html"), "LINK05").as_deref(),
        Some("../secret.html")
    );
    assert_eq!(
        link_of(&page(&full, "docs/a.html"), "LINK20").as_deref(),
        Some(ext_2049.as_str())
    );

    let s = site(&st, "canon", "links");
    let a = page(&s, "docs/a.html");
    for (text, want) in [
        ("LINK01", Some("../docs/b.html")),
        ("LINK02", Some("../docs/b.html#target-section")),
        ("LINK03", Some("../docs/b.html")),
        ("LINK04", Some("../docs/b.html")),
        ("LINK05", None),
        ("LINK06", None),
        ("LINK07", None),
        ("LINK08", None),
        ("LINK09", None),
        ("LINK10", None),
        ("LINK11", None),
        ("LINK12", Some("https://example.com/x")),
        ("LINK13", Some("mailto:a@example.com")),
        ("LINK14", Some("../docs/b.html")),
        ("LINK15", None),
        ("LINK16", Some("#own-section")),
        ("LINK17", None),
        ("LINK18", Some("../index.html")),
        ("LINK19", Some(ext_2048.as_str())),
        ("LINK20", None),
    ] {
        assert_eq!(link_of(&a, text).as_deref(), want, "{text}");
    }
    assert!(a.contains("<a href=\"https://example.com/x\" rel=\"noopener noreferrer\">LINK12</a>"));
    // no byte of the output names the excluded page, its anchor or a refused target
    for word in [
        "secret",
        "Secret page",
        "hidden-section",
        "xnowhere",
        "b%ZZ",
        "?x=1",
        "site.css\">LINK",
    ] {
        assert_absent_pages(&s, word);
    }
}

// --- 6. disagreements ---------------------------------------------------------------------------

const RETIRED: &str = "This page still points to something that was retired.";
const CONTRADICT: &str = "A reviewer found that these statements contradict each other.";
const ADVICE: &str = "ADVICECANARY A and C cannot both hold";

/// A judged contradiction between claims `a` and `c` of `Spec`, with raw advice and a reason.
fn contradiction(r: &mut Rdy, s: &Spec) {
    use rdy_model::*;
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let content = |id: &BlockId| m.get(id).unwrap().content_hash_with(r.store().tracking());
    let mut items = vec![
        PairItem {
            block: s.a.clone(),
            content: content(&s.a),
        },
        PairItem {
            block: s.c.clone(),
            content: content(&s.c),
        },
    ];
    items.sort();
    let provenance = Provenance {
        actor: "agent:r".into(),
        run: Some("r".into()),
        role: Role::Reviewer,
        at_ms: 1,
    };
    let review = Review {
        subject: ReviewSubject::Pair {
            key: pair_key(&items),
            items: items.clone(),
        },
        case_hash: Some(Hash([9; 32])),
        verdict: Verdict::Tension,
        reason: "REASONCANARY review reason".into(),
        citations: vec![],
        provenance: provenance.clone(),
    };
    let tension = SemanticTension {
        key: pair_key(&items),
        items,
        review: Hash::of(&postcard::to_allocvec(&review).unwrap()),
        advice: Some(ADVICE.into()),
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance,
    };
    drop(m);
    r.record_verdict(&review, Some(&tension)).unwrap();
}

#[test]
fn pub6_disagreements_require_every_selected_side() {
    let (_dir, mut r, spec) = retired_reference();
    contradiction(&mut r, &spec);
    let notes = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .children(None)
        .into_iter()
        .find(|b| b.field_str("name") == Some("notes"))
        .map(|b| b.id.clone())
        .expect("the notes doc");
    profiles(
        &mut r,
        &[
            ("notesonly", "org", vec![&notes]),
            ("speconly", "org", vec![&spec.doc]),
            ("both", "org", vec![&notes, &spec.doc]),
        ],
    );
    let st = r.read();
    // the source lists both entries
    let full = build_site(&st, &Scope::Canon, rdy_render::Mode::Static).unwrap();
    let page_full = page(&full, "disagreements.html");
    assert!(page_full.contains(RETIRED) && page_full.contains(CONTRADICT));

    // one side of the retired reference: no entry, no page, no card, no count
    let one = site(&st, "canon", "notesonly");
    assert!(!one.files.contains_key("disagreements.html"));
    for word in [
        RETIRED,
        CONTRADICT,
        "Where the docs disagree",
        "disagreements",
    ] {
        assert_absent_pages(&one, word);
    }
    // the other side alone: the contradiction (both its sides are in Spec) stays, the retired
    // reference does not
    let other = site(&st, "canon", "speconly");
    let d = page(&other, "disagreements.html");
    assert!(d.contains(CONTRADICT) && !d.contains(RETIRED), "{d}");
    // every side selected: both fixed sentences, in the source's order
    let both = site(&st, "canon", "both");
    let d = page(&both, "disagreements.html");
    assert!(d.contains(RETIRED) && d.contains(CONTRADICT), "{d}");
    let src_order = page_full.find(RETIRED) < page_full.find(CONTRADICT);
    assert_eq!(d.find(RETIRED) < d.find(CONTRADICT), src_order);
    // no action text, raw advice, reason or summary on any profile
    for s in [&one, &other, &both] {
        for word in [
            "ADVICECANARY",
            "REASONCANARY",
            "references deprecated",
            "agent:r",
            "Resolve",
            "Dismiss",
            "Approve",
        ] {
            assert_absent_pages(s, word);
        }
    }
}

// --- 7. placement -------------------------------------------------------------------------------

#[test]
fn pub6_replacement_removes_old_profile_files_and_refusal_preserves_output() {
    let (_dir, mut r) = tiny();
    let wide_png = png(6, 2, [90, 90, 10]);
    let blob = r.store_mut().put_blob(&wide_png).unwrap();
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$keep", "fields": {"name": "keep", "title": "Keep page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$keep", "body": "Kept words."},
            {"op": "create", "type": "doc", "id": "$wide", "fields": {"name": "wide", "title": "Wide page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$wp", "parent": "$wide", "body": "Wide words."},
            {"op": "create", "type": "figure", "parent": "$wide", "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "Wide figure"}},
            {"op": "create", "type": "doc", "id": "$leaky", "fields": {"name": "leaky", "title": "Leaky page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$leaky", "body": "Points at {{$wp}}."}
        ]),
    );
    land(&mut r, "w1").unwrap();
    profiles(
        &mut r,
        &[
            ("widep", "org", vec![&ids["$keep"], &ids["$wide"]]),
            ("narrowp", "org", vec![&ids["$keep"]]),
            ("blockedp", "public", vec![&ids["$leaky"]]),
        ],
    );
    let st = r.read();
    let parent = tempfile::tempdir().unwrap();
    let out = parent.path().join("site");
    render_profile_site(&st, "canon", "widep", &out).unwrap();
    let wide = tree(&out);
    assert!(wide.contains_key("wide.html") && wide.contains_key(".rdy-site"));
    assert!(wide.contains_key(&format!("assets/{blob}.png")));
    assert!(wide.contains_key(&format!("go/{}.html", ids["$wp"])));

    // the narrower selection at the same marked directory: no old path or blob stays
    render_profile_site(&st, "canon", "narrowp", &out).unwrap();
    let narrow = tree(&out);
    assert!(narrow.contains_key("keep.html") && narrow.contains_key(".rdy-site"));
    assert_eq!(holding_in(&narrow, b"Wide"), Vec::<String>::new());
    assert_eq!(
        holding_in(&narrow, blob.to_string().as_bytes()),
        Vec::<String>::new()
    );
    assert_eq!(carries(&narrow, &wide_png), Vec::<String>::new());
    assert!(!narrow.contains_key(&format!("go/{}.html", ids["$wp"])));

    // a blocked public profile changes nothing and stages nothing
    let before = std::fs::read_dir(parent.path()).unwrap().count();
    let e = render_profile_site(&st, "canon", "blockedp", &out).unwrap_err();
    assert_eq!(e.code, E_PUBLICATION_REFERENCE, "{e}");
    assert_eq!(
        e.message,
        "Public profile has 1 references outside its selection."
    );
    assert_eq!(tree(&out), narrow);
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), before);
    // nor at a new path
    let fresh = parent.path().join("fresh");
    let e = render_profile_site(&st, "canon", "blockedp", &fresh).unwrap_err();
    assert_eq!(e.code, E_PUBLICATION_REFERENCE);
    assert!(!fresh.exists());
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), before);
}

// --- 8. redaction -------------------------------------------------------------------------------

#[test]
fn pub6_redacted_selected_content_keeps_only_the_notice() {
    let (dir, mut r) = tiny();
    let fig_png = png(7, 5, [200, 30, 30]);
    let fig_blob = r.store_mut().put_blob(&fig_png).unwrap();
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$vault", "fields": {"name": "vault", "title": "VAULTCANARY title", "description": "VAULTCANARY description."}},
            {"op": "create", "type": "doc", "id": "$kept", "parent": "$vault", "fields": {"name": "kept", "title": "Kept page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$kept", "body": "Kept under the folder."},
            {"op": "create", "type": "doc", "id": "$plan", "fields": {"name": "plan", "title": "Plan page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$ps", "parent": "$plan", "fields": {"title": "Plan section"}},
            {"op": "create", "type": "prose", "id": "$secretp", "parent": "$ps", "body": "PROSECANARY body."},
            {"op": "create", "type": "prose", "parent": "$ps", "body": "Readable sibling."},
            {"op": "create", "type": "figure", "id": "$fig", "parent": "$ps", "fields": {"blob": fig_blob.to_string(), "media_type": "image/png", "alt": "FIGCANARY alt"}},
            {"op": "create", "type": "doc", "id": "$secret", "fields": {"name": "secret", "title": "DOCCANARY title", "doc_type": "page"}},
            {"op": "create", "type": "section", "parent": "$secret", "fields": {"title": "Kept child section"}},
            {"op": "create", "type": "prose", "parent": "$secret", "body": "Child text stays."}
        ]),
    );
    land(&mut r, "w1").unwrap();
    let i = |k: &str| ids[k].clone();
    let p = profiles(
        &mut r,
        &[
            (
                "vaultp",
                "org",
                vec![&i("$vault"), &i("$plan"), &i("$secret")],
            ),
            ("doomed", "org", vec![&i("$plan")]),
        ],
    );
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let prose_version = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(&i("$secretp"))
        .unwrap();
    let before = site(&r.read(), "canon", "vaultp");
    assert!(!holding(&before, "PROSECANARY").is_empty());
    assert!(!carries(&before.files, &fig_png).is_empty());
    r.close().unwrap();

    let (r, request) = redact(
        dir.path(),
        vec![
            RedactTarget::AllVersions(i("$vault")),
            RedactTarget::AllVersions(i("$secretp")),
            RedactTarget::AllVersions(i("$secret")),
            RedactTarget::Blob(fig_blob),
            RedactTarget::AllVersions(p["$doomed"].clone()),
        ],
    );
    let st = r.read();
    for scope in ["canon", "tag:v1.0.0"] {
        let s = site(&st, scope, "vaultp");
        // the profile's selection alone, under its own title
        assert_absent(&s, "Admin page");
        assert!(
            page(&s, "index.html").contains("<h1>Title of vaultp</h1>"),
            "{scope}"
        );
        for word in [
            "VAULTCANARY",
            "PROSECANARY",
            "DOCCANARY",
            "FIGCANARY",
            &fig_blob.to_string(),
            &request.to_string(),
        ] {
            assert_absent(&s, word);
        }
        assert_eq!(carries(&s.files, &fig_png), Vec::<String>::new(), "{scope}");
        let plan = page(&s, "plan.html");
        assert_eq!(plan.matches(NOTICE_HTML).count(), 2, "{scope}: {plan}");
        assert!(plan.contains("Readable sibling."));
        // the removed folder and doc keep their children's pages
        assert!(!holding(&s, "Kept under the folder.").is_empty(), "{scope}");
        assert!(!holding(&s, "Child text stays.").is_empty(), "{scope}");
    }
    // the historical tag: its pill and only the existing badge
    let tagged = page(&site(&st, "tag:v1.0.0", "vaultp"), "index.html");
    assert!(tagged.contains("<span class=\"badge badge-redacted\">redacted</span>"));

    // the source report keeps the original identity: the version and the removing request
    let resolved = rdy_engine::publication::resolve(&st, &Scope::Canon, "vaultp").unwrap();
    let entry = resolved
        .entries
        .iter()
        .find(|e| e.id == i("$secretp"))
        .expect("the removed prose is selected");
    assert_eq!(
        (entry.version, entry.redaction),
        (prose_version, Some(request))
    );
    let row = resolved
        .attachments
        .iter()
        .find(|a| a.block == i("$fig"))
        .expect("the figure's row");
    assert!(!row.available && row.blob == fig_blob);

    // a removed profile refuses canon and its old tag alike: no historical fallback
    let doomed = p["$doomed"].to_string();
    for scope in ["canon", "tag:v1.0.0"] {
        let e = build_profile_site(&st, scope, &doomed)
            .err()
            .expect("refused");
        assert_eq!(
            e.message,
            format!("Publication profile {doomed} has been redacted."),
            "{scope}"
        );
    }
}
