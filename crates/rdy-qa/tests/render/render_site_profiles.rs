//! spv4 (D-CWC91, D-DK03K, D-0VCQ5, D-3AVJH, D-VRW0A, D-F9Z60, D-QYCRB, D-42YMF, D-PX5B0,
//! D-W40KZ, D-BMX8P): a publication profile on the live site. A profile route under a scope and
//! the profile-only site read the profile's selected state alone: every page, the navigation, the
//! search payload, the permalinks, the trail, the glossary, the figures, the llms files and the
//! review. A review compares the profile's selected state in each side's own revision. The live
//! site warns of a blocked public profile while every durable output still refuses it. The
//! neutral chip counts the landings since the tag that change what the profile shows.
#[path = "../../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::site_scope::{ScopeRef, proposal_ref};
use rdy_engine::site_target::SiteTarget;
use rdy_engine::{E_PUBLICATION_REFERENCE, ProposalInfo, Rdy, ReadState};
use rdy_model::redaction::RedactTarget;
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::golden::tiny;
use rdy_render::live::{ProfileBuild, ProfileHistoryMemo, build_profile_live};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// The notice a page writes in place of removed content.
const NOTICE_HTML: &str = "<p class=\"redacted\">Content removed.</p>";
/// The fix of a blocked public profile's refusal, and of its live warning.
const PUBLIC_FIX: &str =
    "Include the target docs or remove these references. Run the publication check again.";

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

/// Proposes `view` as the writer under `title`.
fn propose(r: &mut Rdy, view: &str, title: &str) -> ProposalInfo {
    let summary = rdy_qa::intake::summary_for(r, view);
    r.propose(&writer(), view, title, summary)
        .unwrap_or_else(|e| panic!("propose {view}: {e}"))
}

/// One profile's create op under `parent`.
fn profile_op(
    key: &str,
    title: &str,
    audience: &str,
    parent: &BlockId,
    select: &[&BlockId],
) -> Value {
    json!({"op": "create", "type": "publication_profile", "id": format!("${key}"),
        "parent": parent.to_string(),
        "fields": {"key": key, "title": title, "description": format!("About {title}."),
            "audience": audience,
            "select": select.iter().map(|s| s.to_string()).collect::<Vec<_>>()}})
}

/// The ids of [`world`].
struct World {
    dir: tempfile::TempDir,
    r: Rdy,
    ids: BTreeMap<String, BlockId>,
    admin: BlockId,
    /// The `userguide` profile (org, title `User guide`, the `guide` folder).
    guide: BlockId,
    /// The figure's blob, in the intro doc.
    fig_png: Vec<u8>,
    fig_blob: Hash,
    /// The secret doc's figure blob.
    secret_blob: Hash,
}

/// The id `k` of [`World::ids`].
fn id(ids: &BTreeMap<String, BlockId>, k: &str) -> BlockId {
    ids[k].clone()
}

/// A project named `Tiny`: a folder `guide` (docs `intro` and `setup`), a top-level `secret` doc
/// full of canaries, a top-level `extra` doc, two decisions homed in the intro section (one
/// governs intro's prose, one setup's), and an `admin` doc holding the org profile `userguide`
/// (title `User guide`) that selects the folder.
fn world() -> World {
    let (dir, mut r) = tiny();
    let fig_png = b"\x89PNG\r\n\x1a\nFIGBYTES-intro-figure".to_vec();
    let secret_png = b"\x89PNG\r\n\x1a\nSECRETBYTES-figure".to_vec();
    let fig_blob = r.store_mut().put_blob(&fig_png).unwrap();
    let secret_blob = r.store_mut().put_blob(&secret_png).unwrap();
    let mut ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "description": "Guide folder."}},
            {"op": "create", "type": "doc", "id": "$intro", "parent": "$guide", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$is", "parent": "$intro", "fields": {"title": "Intro section"}},
            {"op": "create", "type": "prose", "id": "$ip", "parent": "$is", "body": "Intro words."},
            {"op": "create", "type": "figure", "id": "$if", "parent": "$is", "fields": {"blob": fig_blob.to_string(), "media_type": "image/png", "alt": "FIGALT intro"}},
            {"op": "create", "type": "doc", "id": "$setup", "parent": "$guide", "fields": {"name": "setup", "title": "Setup page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$sp", "parent": "$setup", "body": "Setup words."},
            {"op": "create", "type": "doc", "id": "$secret", "fields": {"name": "secret", "title": "SECRETCANARY page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$ss", "parent": "$secret", "fields": {"title": "SECRETCANARY section"}},
            {"op": "create", "type": "prose", "id": "$xp", "parent": "$ss", "body": "SECRETCANARY words."},
            {"op": "create", "type": "term", "id": "$xt", "parent": "$ss", "fields": {"name": "SECRETCANARY term"}, "body": "SECRETCANARY term body."},
            {"op": "create", "type": "question", "id": "$xq", "parent": "$ss", "fields": {"prompt": "SECRETCANARY question?"}},
            {"op": "create", "type": "figure", "id": "$xf", "parent": "$ss", "fields": {"blob": secret_blob.to_string(), "media_type": "image/png", "alt": "SECRETCANARY alt"}},
            {"op": "create", "type": "doc", "id": "$extra", "fields": {"name": "extra", "title": "Extra page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ep", "parent": "$extra", "body": "Extra words."}
        ]),
    );
    for (statement, affects) in [("Intro ruling.", "$ip"), ("Setup ruling.", "$sp")] {
        r.decide(
            &writer(),
            "w1",
            rdy_engine::DecisionInput {
                parent: ids["$is"].to_string(),
                statement: statement.into(),
                affects: vec![ids[affects].to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    }
    land(&mut r, "w1").unwrap();
    let admin = put(
        &mut r,
        "admin",
        json!([{"op": "create", "type": "doc", "id": "$admin",
            "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}]),
    )["$admin"]
        .clone();
    let p = put(
        &mut r,
        "admin",
        json!([profile_op(
            "userguide",
            "User guide",
            "org",
            &admin,
            &[&ids["$guide"]]
        )]),
    );
    land(&mut r, "admin").unwrap();
    let guide = p["$userguide"].clone();
    ids.insert("$userguide".into(), guide.clone());
    World {
        dir,
        r,
        ids,
        admin,
        guide,
        fig_png,
        fig_blob,
        secret_blob,
    }
}

/// Adds profiles under the admin doc and lands them; their ids by `$key`.
fn add_profiles(w: &mut World, ops: Vec<Value>) -> BTreeMap<String, BlockId> {
    let ids = put(&mut w.r, "admin", Value::Array(ops));
    land(&mut w.r, "admin").unwrap();
    ids
}

fn profile(scope: ScopeRef, key: &str) -> SiteTarget {
    SiteTarget::Profile {
        scope,
        key: key.into(),
    }
}

fn only(key: &str) -> SiteTarget {
    SiteTarget::ProfileOnly { key: key.into() }
}

fn proposal(p: &ProposalInfo) -> ScopeRef {
    ScopeRef::Proposal(proposal_ref(&p.r#ref).unwrap())
}

/// The live build of `target`, with a fresh history memo.
fn build(st: &ReadState, target: &SiteTarget) -> ProfileBuild {
    build_profile_live(st, target, &mut ProfileHistoryMemo::default())
        .unwrap_or_else(|e| panic!("{target:?}: {e}"))
}

/// A file of a build, as text.
fn file(b: &ProfileBuild, path: &str) -> String {
    String::from_utf8(
        b.live
            .files
            .get(path)
            .unwrap_or_else(|| panic!("no file {path}: {:?}", b.live.files.keys()))
            .clone(),
    )
    .unwrap()
}

/// Every path whose name or bytes hold `word`.
fn holding(files: &BTreeMap<String, Vec<u8>>, word: &[u8]) -> Vec<String> {
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
fn assert_absent(b: &ProfileBuild, word: &str) {
    assert_eq!(
        holding(&b.live.files, word.as_bytes()),
        Vec::<String>::new(),
        "{word}"
    );
}

/// The colophon paragraph of an HTML page.
fn colophon(html: &str) -> &str {
    let at = html.find("<p class=\"colophon\">").expect("a colophon");
    let end = html[at..].find("</p>").unwrap() + at + 4;
    &html[at..end]
}

/// The head lines of an llms file, up to the first blank line after the colophon.
fn llms_head(text: &str) -> &str {
    let mut cut = 0;
    for _ in 0..3 {
        cut += text[cut..].find("\n\n").unwrap() + 2;
    }
    &text[..cut]
}

/// The review bar's words on `html`, if it has one.
fn review_bar(html: &str) -> Option<&str> {
    let at = html.find("<p class=\"review-title\" title=\"")?;
    let rest = &html[at..];
    let start = rest.find('>').unwrap() + 1;
    let end = rest.find("</p>").unwrap();
    Some(&rest[start..end])
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
fn spv4_every_live_surface_is_selected() {
    let mut w = world();
    // a selected reference to excluded content reads in neutral words
    put_land(
        &mut w.r,
        "w2",
        json!([{"op": "create", "type": "prose", "parent": id(&w.ids, "$is").to_string(),
            "body": format!("See {{{{{}}}}} and {{{{{}}}}}.", id(&w.ids, "$xp"), id(&w.ids, "$xt"))}]),
    );
    // a review that edits selected and excluded content alike
    put(
        &mut w.r,
        "work",
        json!([
            {"op": "update", "id": id(&w.ids, "$ip").to_string(), "body": "Intro words, NEWWORDS."},
            {"op": "update", "id": id(&w.ids, "$xp").to_string(), "body": "SECRETCANARY edited words."},
            {"op": "create", "type": "prose", "parent": id(&w.ids, "$setup").to_string(), "body": "unused"}
        ]),
    );
    let st = w.r.read();
    let canon = build(&st, &profile(ScopeRef::Canon, "userguide"));
    let view = build(&st, &profile(ScopeRef::View("work".into()), "userguide"));
    let only_site = build(&st, &only("userguide"));

    let mut excluded: Vec<String> = [
        "$secret", "$ss", "$xp", "$xt", "$xq", "$xf", "$extra", "$ep",
    ]
    .iter()
    .map(|k| id(&w.ids, k).to_string())
    .collect();
    excluded.push(w.admin.to_string());
    excluded.push(w.guide.to_string());
    excluded.push(w.secret_blob.to_string());
    for b in [&canon, &view, &only_site] {
        for word in [
            "SECRETCANARY",
            "SECRETBYTES",
            "Admin page",
            "Extra page",
            "Extra words",
            "Tiny",
        ]
        .into_iter()
        .map(String::from)
        .chain(excluded.iter().cloned())
        {
            assert_absent(b, &word);
        }
        // the selected content is there, its excluded references in neutral words
        let intro = file(b, "guide/intro.html");
        assert!(
            intro.contains("See unavailable reference and unavailable reference."),
            "{intro}"
        );
        assert!(file(b, "guide/setup.html").contains("Setup words."));
        assert!(file(b, "index.html").contains("<h1>User guide</h1>"));
        assert!(file(b, "search-index.json").contains("Setup words."));
        assert!(file(b, "llms-full.txt").contains("Setup words."));
        assert!(
            !holding(&b.live.files, &w.fig_png).is_empty(),
            "the selected figure"
        );
        assert_eq!(
            b.live.warnings.first().map(String::as_str),
            Some(
                "Org profile has 2 references outside its selection. Review these references before you share the output."
            )
        );
        assert_eq!(b.profile.id.as_ref(), Some(&w.guide));
    }
    // the review shows its selected changes only, on the pages and in its summary
    let changes = file(&view, "changes.html");
    assert!(changes.contains("NEWWORDS"), "{changes}");
    assert!(file(&view, "guide/intro.html").contains("NEWWORDS"));
    assert!(
        !holding(&view.live.files, b"unused").is_empty(),
        "setup's new prose"
    );
    assert_eq!(view.changes, Some("changes.html"));
    assert_eq!(canon.changes, None);
    // the menu opens with the route's context
    assert!(
        view.live
            .header
            .contains("href=\"/~versions/?scope=view%3Awork&amp;profile=userguide\" data-menu"),
        "{}",
        view.live.header
    );
    assert!(
        canon
            .live
            .header
            .contains("href=\"/~versions/?scope=canon&amp;profile=userguide\" data-menu"),
        "{}",
        canon.live.header
    );
    // no menu, pill or review on the profile-only site
    assert_eq!(only_site.live.header, "");
    for (path, bytes) in &only_site.live.files {
        if path.ends_with(".html") {
            let html = String::from_utf8_lossy(bytes);
            assert!(
                !html.contains("data-menu") && !html.contains("class=\"pill"),
                "{path}"
            );
        }
    }
}

// --- 2. a proposal-only profile -----------------------------------------------------------------

#[test]
fn spv4_proposal_only_profile_reviews_against_empty() {
    let mut w = world();
    let admin = w.admin.clone();
    let folder = id(&w.ids, "$guide");
    put(
        &mut w.r,
        "draft",
        json!([profile_op(
            "newguide",
            "New guide",
            "org",
            &admin,
            &[&folder]
        )]),
    );
    let p = propose(&mut w.r, "draft", "Guide draft");
    let st = w.r.read();
    let b = build(&st, &profile(proposal(&p), "newguide"));
    // every selected page is New against the empty selection: the folder, two docs and their
    // four visible blocks (the section, two prose and the figure; the decisions never count)
    let index = file(&b, "index.html");
    assert_eq!(
        colophon(&index),
        "<p class=\"colophon\"><span data-rdy-label>New guide. · proposal: Guide draft, 7 changes, not landed</span></p>"
    );
    assert_eq!(
        review_bar(&index),
        Some("Reviewing proposal · Guide draft · by qa · as New guide")
    );
    assert!(
        b.live.header.contains(
            "title=\"7 changes, not landed\" aria-label=\"7 changes, not landed\">+7</a>"
        ),
        "{}",
        b.live.header
    );
    let changes = file(&b, "changes.html");
    for words in ["Intro words.", "Setup words.", "Intro section"] {
        assert!(changes.contains(words), "{words}: {changes}");
    }
    assert_eq!(
        changes.matches("class=\"chg-entry chg-entry-new\"").count(),
        7,
        "{changes}"
    );
    assert_eq!(
        changes.matches("class=\"chg-entry ").count(),
        7,
        "{changes}"
    );
    for page in ["guide/intro.html", "guide/setup.html"] {
        assert!(file(&b, page).contains("Show changes"), "{page}");
    }
    assert_eq!(b.profile.title, "New guide");
}

// --- 3. selection changes -----------------------------------------------------------------------

#[test]
fn spv4_selection_changes_review_each_own_revision() {
    let mut w = world();
    let (admin, secret) = (w.admin.clone(), id(&w.ids, "$secret"));
    let other = add_profiles(
        &mut w,
        vec![profile_op(
            "handbook",
            "Handbook",
            "org",
            &admin,
            &[&secret],
        )],
    )["$handbook"]
        .clone();
    // a setup block whose reference only the old selection resolves
    let setup = id(&w.ids, "$setup");
    put_land(
        &mut w.r,
        "w2",
        json!([{"op": "create", "type": "prose", "parent": setup.to_string(),
            "body": format!("Setup sees {{{{{setup}}}}}.")}]),
    );
    // the view renames userguide's key to the key another profile holds in its base, changes its
    // selection, and edits a doc it no longer selects
    put(
        &mut w.r,
        "work",
        json!([
            {"op": "update", "id": other.to_string(), "fields": {"key": "spare"}},
            {"op": "update", "id": w.guide.to_string(), "fields": {"key": "handbook",
                "select": [id(&w.ids, "$intro").to_string(), id(&w.ids, "$extra").to_string()]}},
            {"op": "update", "id": id(&w.ids, "$sp").to_string(), "body": "Setup NEWCANARY words."}
        ]),
    );
    let st = w.r.read();
    let b = build(&st, &profile(ScopeRef::View("work".into()), "handbook"));
    assert_eq!(
        b.profile.id.as_ref(),
        Some(&w.guide),
        "the current key's profile"
    );
    // the other profile's selection never shows, in either revision; the old text of a page that
    // left reads from the old selected state, never from the view's full source
    for word in ["SECRETCANARY", "SECRETBYTES", "NEWCANARY", "Handbook"] {
        assert_absent(&b, word);
    }
    let index = file(&b, "index.html");
    // setup left (the page and its two prose), extra entered (the page and its prose)
    assert_eq!(
        colophon(&index),
        "<p class=\"colophon\"><span data-rdy-label>User guide. · view: work, 5 changes, not landed</span></p>"
    );
    let changes = file(&b, "changes.html");
    assert!(changes.contains("Setup words."), "{changes}");
    // the removed text's reference reads through the old selected state, which holds its target
    assert!(changes.contains("Setup sees Setup page."), "{changes}");
    assert!(changes.contains("Extra words."), "{changes}");
    assert!(
        changes.contains("Setup page") && changes.contains("Extra page"),
        "{changes}"
    );
    assert!(!b.live.files.contains_key("guide/setup.html"));
    assert!(file(&b, "extra.html").contains("Extra words."));
    // a removed page never links to the view's page of a doc the profile no longer selects
    assert!(!changes.contains("href=\"guide/setup.html"), "{changes}");
    assert!(!index.contains("href=\"guide/setup.html"), "{index}");

    // the base revision of the key alone: the other profile, unchanged
    let base = build(&st, &profile(ScopeRef::Canon, "handbook"));
    assert_eq!(base.profile.id.as_ref(), Some(&other));
    assert!(!holding(&base.live.files, b"SECRETCANARY").is_empty());
}

// --- 4. live warns, output refuses --------------------------------------------------------------

#[test]
fn spv4_live_public_warns_while_output_still_refuses() {
    let mut w = world();
    let (admin, setup, intro, xp) = (
        w.admin.clone(),
        id(&w.ids, "$setup"),
        id(&w.ids, "$intro"),
        id(&w.ids, "$xp"),
    );
    // each selected doc references one excluded block: setup has no figure, intro has one
    put_land(
        &mut w.r,
        "w2",
        json!([
            {"op": "create", "type": "prose", "parent": setup.to_string(), "body": format!("Setup sees {{{{{xp}}}}}.")},
            {"op": "create", "type": "prose", "parent": id(&w.ids, "$is").to_string(), "body": format!("Intro sees {{{{{xp}}}}}.")}
        ]),
    );
    add_profiles(
        &mut w,
        vec![
            profile_op("pubzero", "Public zero", "public", &admin, &[&setup]),
            profile_op("pubfig", "Public figure", "public", &admin, &[&intro]),
        ],
    );
    let st = w.r.read();
    // intro's section also homes the decision that governs setup's prose: a second reference
    for (key, n) in [("pubzero", 1), ("pubfig", 2)] {
        let check = rdy_engine::publication::check(&st, key, Some("canon"), None).unwrap();
        assert_eq!(check["references"]["status"], "blocked", "{key}");
        let message = format!("Public profile has {n} references outside its selection.");
        for target in [profile(ScopeRef::Canon, key), only(key)] {
            let b = build(&st, &target);
            assert_eq!(
                b.live.warnings,
                vec![format!("{message} {PUBLIC_FIX}")],
                "{key}"
            );
            assert!(file(&b, "index.html").contains(&format!("<h1>{}</h1>", b.profile.title)));
            assert_absent(&b, "SECRETCANARY");
            let figures = holding(&b.live.files, &w.fig_png);
            match key {
                "pubfig" => assert!(!figures.is_empty(), "the available selected figure"),
                _ => assert!(figures.is_empty()),
            }
        }
        // every durable output still refuses before it writes anything
        let out = w.dir.path().join(format!("{key}-site"));
        let e = rdy_render::render_profile_site(&st, "canon", key, &out).unwrap_err();
        assert_eq!(e.code, E_PUBLICATION_REFERENCE, "{e}");
        assert!(!out.exists());
        let e = rdy_render::build_profile_site(&st, "canon", key)
            .err()
            .expect("refused");
        assert_eq!(e.code, E_PUBLICATION_REFERENCE, "{e}");
        for (format, name) in [
            (rdy_render::print::Format::Pdf, "out.pdf"),
            (rdy_render::print::Format::Html, "out.html"),
            (rdy_render::print::Format::Md, "out-md"),
        ] {
            let out = w.dir.path().join(format!("{key}-{name}"));
            let e = rdy_render::print::export_profile(&st, "canon", key, &out, format, None)
                .unwrap_err();
            assert_eq!(e.code, E_PUBLICATION_REFERENCE, "{key} {name}: {e}");
            assert_eq!(
                (e.message.as_str(), e.fix.as_str()),
                (message.as_str(), PUBLIC_FIX)
            );
            assert!(!out.exists(), "{key} {name}");
        }
    }
}

// --- 5. labels and summary links ----------------------------------------------------------------

#[test]
fn spv4_profile_labels_and_summary_links_agree() {
    let mut w = world();
    let st = w.r.read();
    // canon, unversioned
    let canon = build(&st, &profile(ScopeRef::Canon, "userguide"));
    let index = file(&canon, "index.html");
    assert_eq!(
        colophon(&index),
        "<p class=\"colophon\"><span data-rdy-label>User guide.</span></p>"
    );
    assert_eq!(canon.live.label, "User guide.");
    for path in ["llms.txt", "llms-full.txt"] {
        assert_eq!(
            llms_head(&file(&canon, path)),
            "# User guide\n\n> About User guide.\n\nUser guide.\n\n",
            "{path}"
        );
    }
    assert_eq!(
        canon.live.header,
        "<a class=\"pill\" href=\"/~versions/?scope=canon&amp;profile=userguide\" data-menu aria-haspopup=\"dialog\" aria-expanded=\"false\"><span class=\"pill-text\">unversioned · User guide</span></a>"
    );
    assert_eq!(review_bar(&index), None);
    drop(st);

    // a tag, and canon at the tag
    w.r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let st = w.r.read();
    let tagged = build(&st, &profile(ScopeRef::Tag("v1.0.0".into()), "userguide"));
    assert_eq!(
        colophon(&file(&tagged, "guide/intro.html")),
        "<p class=\"colophon\"><span data-rdy-label>A snapshot of User guide at v1.0.0.</span></p>"
    );
    assert_eq!(
        llms_head(&file(&tagged, "llms.txt")),
        "# User guide\n\n> About User guide.\n\nA snapshot of User guide at v1.0.0.\n\n"
    );
    assert!(
        tagged.live.header.contains("<span class=\"pill-text\">v1.0.0 · User guide</span> <span class=\"snapshot\">snapshot</span></a>"),
        "{}",
        tagged.live.header
    );
    assert!(
        tagged
            .live
            .header
            .contains("href=\"/~versions/?scope=tag%3Av1.0.0&amp;profile=userguide\"")
    );
    assert!(!tagged.live.header.contains("chip"));
    let canon = build(&st, &profile(ScopeRef::Canon, "userguide"));
    assert!(
        canon
            .live
            .header
            .contains("<span class=\"pill-text\">v1.0.0 · User guide</span></a>"),
        "{}",
        canon.live.header
    );
    assert_eq!(canon.live.label, "User guide.");
    drop(st);

    // a view and a proposal version of it
    put(
        &mut w.r,
        "work",
        json!([{"op": "update", "id": id(&w.ids, "$ip").to_string(), "body": "Intro words, edited."}]),
    );
    let p = propose(&mut w.r, "work", "Work edits");
    let st = w.r.read();
    for (scope, label, bar, root) in [
        (
            ScopeRef::View("work".into()),
            "User guide. · view: work, 1 change, not landed",
            "Reviewing view · work · by qa · as User guide",
            "/~view/work/~profile/userguide/".to_string(),
        ),
        (
            proposal(&p),
            "User guide. · proposal: Work edits, 1 change, not landed",
            "Reviewing proposal · Work edits · by qa · as User guide",
            format!(
                "/~proposal/{}/~profile/userguide/",
                proposal_ref(&p.r#ref).unwrap()
            ),
        ),
    ] {
        let b = build(&st, &profile(scope.clone(), "userguide"));
        let index = file(&b, "index.html");
        assert_eq!(
            colophon(&index),
            format!("<p class=\"colophon\"><span data-rdy-label>{label}</span></p>")
        );
        assert_eq!(b.live.label, label);
        for path in ["llms.txt", "llms-full.txt"] {
            assert_eq!(
                llms_head(&file(&b, path)),
                format!("# User guide\n\n> About User guide.\n\n{label}\n\n"),
                "{path}"
            );
            assert!(
                !file(&b, path).contains("<del"),
                "{path}: llms keeps the current text"
            );
        }
        assert_eq!(review_bar(&index), Some(bar), "{scope:?}");
        assert!(
            b.live.header.contains(&format!(
                "<a class=\"chip chip-review\" href=\"{root}changes.html\" title=\"1 change, not landed\" aria-label=\"1 change, not landed\">+1</a>"
            )),
            "{}",
            b.live.header
        );
        assert!(index.contains("Show changes"));
        assert_eq!(b.changes, Some("changes.html"));
    }

    // the implicit profile: the project's title
    let implicit = build(&st, &profile(ScopeRef::Canon, "implicit:org"));
    assert_eq!(implicit.live.label, "Tiny.");
    assert!(
        implicit
            .live
            .header
            .contains("<span class=\"pill-text\">v1.0.0 · Tiny</span>")
    );
    assert!(implicit.live.header.contains("profile=implicit%3Aorg"));
    assert_eq!(implicit.profile.id, None);

    // the profile-only site: the title alone
    for key in ["userguide", "implicit:org"] {
        let b = build(&st, &only(key));
        let title = b.profile.title.clone();
        let index = file(&b, "index.html");
        assert_eq!(
            colophon(&index),
            format!("<p class=\"colophon\"><span data-rdy-label>{title}.</span></p>")
        );
        let head = match key {
            "userguide" => "# User guide\n\n> About User guide.\n\nUser guide.\n\n",
            _ => "# Tiny\n\nTiny.\n\n",
        };
        for path in ["llms.txt", "llms-full.txt"] {
            assert!(file(&b, path).starts_with(head), "{key} {path}");
        }
        assert_eq!(b.live.header, "");
        assert!(!index.contains("data-rdy-version") && !index.contains("class=\"pill"));
        assert_eq!(review_bar(&index), None);
        assert!(!b.live.files.contains_key("changes.html"));
        assert!(!b.live.files.contains_key("changes-since.html"));
        assert_eq!(b.changes, None);
    }
    drop(st);

    // a selected doc that owns a summary's name keeps its page; no generated link names it
    let (admin, folder) = (w.admin.clone(), id(&w.ids, "$guide"));
    let docs = put_land(
        &mut w.r,
        "w3",
        json!([
            {"op": "create", "type": "doc", "id": "$changes", "fields": {"name": "changes", "title": "Changes doc", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$changes", "body": "Authored changes."},
            {"op": "create", "type": "doc", "id": "$since", "fields": {"name": "changes-since", "title": "Since doc", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$since", "body": "Authored since."}
        ]),
    );
    add_profiles(
        &mut w,
        vec![profile_op(
            "named",
            "Named",
            "org",
            &admin,
            &[&folder, &docs["$changes"], &docs["$since"]],
        )],
    );
    put(
        &mut w.r,
        "work",
        json!([{"op": "update", "id": id(&w.ids, "$sp").to_string(), "body": "Setup words, edited."}]),
    );
    let st = w.r.read();
    for target in [
        profile(ScopeRef::View("work".into()), "named"),
        profile(ScopeRef::Canon, "named"),
        only("named"),
    ] {
        let b = build(&st, &target);
        assert!(
            file(&b, "changes.html").contains("Authored changes."),
            "{target:?}"
        );
        assert!(
            file(&b, "changes-since.html").contains("Authored since."),
            "{target:?}"
        );
        assert_eq!(b.changes, None, "{target:?}");
        assert!(!b.live.header.contains("changes.html"), "{}", b.live.header);
        assert!(
            !b.live.header.contains("changes-since.html"),
            "{}",
            b.live.header
        );
    }
    let view = build(&st, &profile(ScopeRef::View("work".into()), "named"));
    assert!(
        view.live
            .header
            .contains("<span class=\"chip chip-review\""),
        "{}",
        view.live.header
    );
    assert!(
        view.live.header.contains("<span class=\"chip chip-since\""),
        "{}",
        view.live.header
    );
    assert!(view.live.warnings.contains(&"the top-level doc named changes has the page changes.html: the change summary of this view or proposal is left out. Rename the doc to publish it.".to_string()), "{:?}", view.live.warnings);
}

// --- 6. the neutral chip ------------------------------------------------------------------------

/// The since chip of a build's header: its text and hover; none when there is none.
fn since_chip(b: &ProfileBuild) -> Option<(String, String)> {
    let h = &b.live.header;
    let at = h.find("class=\"chip chip-since\"")?;
    let rest = &h[at..];
    let t = rest.find("title=\"").unwrap() + 7;
    let hover = &rest[t..t + rest[t..].find('"').unwrap()];
    let gt = rest.find('>').unwrap() + 1;
    let text = &rest[gt..gt + rest[gt..].find('<').unwrap()];
    Some((text.to_string(), hover.to_string()))
}

/// The since page's first sentence.
fn since_sentence(b: &ProfileBuild) -> String {
    let page = file(b, "changes-since.html");
    let at = page.find(" since v1.0.0 changed").expect("the sentence");
    let start = page[..at].rfind('>').unwrap() + 1;
    let end = at + page[at..].find('<').unwrap();
    page[start..end].to_string()
}

#[test]
fn spv4_selected_since_counts_landings() {
    let mut w = world();
    w.r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let admin = w.admin.clone();
    let intro = id(&w.ids, "$intro");
    // 1: excluded content only
    put_land(
        &mut w.r,
        "l1",
        json!([{"op": "update", "id": id(&w.ids, "$xp").to_string(), "body": "SECRETCANARY one."}]),
    );
    // 2: selected content
    put_land(
        &mut w.r,
        "l2",
        json!([{"op": "update", "id": id(&w.ids, "$ip").to_string(), "body": "Intro words, two."}]),
    );
    // 3: a selection edit, and a profile that did not exist before
    put(
        &mut w.r,
        "l3",
        json!([
            {"op": "update", "id": w.guide.to_string(), "fields": {"select": [id(&w.ids, "$guide").to_string(), id(&w.ids, "$extra").to_string()]}},
            profile_op("late", "Late", "org", &admin, &[&intro])
        ]),
    );
    land(&mut w.r, "l3").unwrap();
    // 4: profile metadata alone
    put_land(
        &mut w.r,
        "l4",
        json!([{"op": "update", "id": w.guide.to_string(), "fields": {"description": "New words about the guide."}}]),
    );
    // 5 and 6: an edit and its revert
    put_land(
        &mut w.r,
        "l5",
        json!([{"op": "update", "id": id(&w.ids, "$sp").to_string(), "body": "Setup words, five."}]),
    );
    put_land(
        &mut w.r,
        "l6",
        json!([{"op": "update", "id": id(&w.ids, "$sp").to_string(), "body": "Setup words."}]),
    );
    // 7: excluded content again
    put_land(
        &mut w.r,
        "l7",
        json!([{"op": "update", "id": id(&w.ids, "$xp").to_string(), "body": "SECRETCANARY seven."}]),
    );
    let st = w.r.read();

    // userguide: landings 2, 3, 5 and 6; the since page compares the tag with canon
    let b = build(&st, &profile(ScopeRef::Canon, "userguide"));
    assert_eq!(
        since_chip(&b),
        Some(("+4".into(), "4 landings since v1.0.0".into()))
    );
    assert!(
        b.live
            .header
            .contains("href=\"/~profile/userguide/changes-since.html\""),
        "{}",
        b.live.header
    );
    assert_eq!(
        since_sentence(&b),
        "4 landings since v1.0.0 changed 3 blocks in 2 docs."
    );
    assert_absent(&b, "SECRETCANARY");
    // the profile that did not exist at the tag: only the landing that made it
    let late = build(&st, &profile(ScopeRef::Canon, "late"));
    assert_eq!(
        since_chip(&late),
        Some(("+1".into(), "1 landing since v1.0.0".into()))
    );
    assert_eq!(
        since_sentence(&late),
        "1 landing since v1.0.0 changed 5 blocks in 1 doc."
    );
    // the implicit profile, explicitly: every landing a reader sees changed (1, 2, 5, 6, 7)
    let implicit = build(&st, &profile(ScopeRef::Canon, "implicit:org"));
    assert_eq!(
        since_chip(&implicit),
        Some(("+5".into(), "5 landings since v1.0.0".into()))
    );
    // a review route reads canon's selected history for this chip
    put(
        &mut w.r,
        "work",
        json!([{"op": "update", "id": id(&w.ids, "$sp").to_string(), "body": "Setup words, review."}]),
    );
    let st = w.r.read();
    let view = build(&st, &profile(ScopeRef::View("work".into()), "userguide"));
    assert_eq!(
        since_chip(&view),
        Some(("+4".into(), "4 landings since v1.0.0".into()))
    );
    // a tag and the profile-only site count nothing
    for target in [
        profile(ScopeRef::Tag("v1.0.0".into()), "userguide"),
        only("userguide"),
    ] {
        let b = build(&st, &target);
        assert_eq!(since_chip(&b), None, "{target:?}");
        assert!(
            !b.live.files.contains_key("changes-since.html"),
            "{target:?}"
        );
    }

    // an edit and its revert: two landings, and no net block on the since page
    let mut w = world();
    w.r.tag(&approver(), "v1.0.0", None, None).unwrap();
    put_land(
        &mut w.r,
        "l1",
        json!([{"op": "update", "id": id(&w.ids, "$sp").to_string(), "body": "Setup words, edited."}]),
    );
    put_land(
        &mut w.r,
        "l2",
        json!([{"op": "update", "id": id(&w.ids, "$sp").to_string(), "body": "Setup words."}]),
    );
    let st = w.r.read();
    let b = build(&st, &profile(ScopeRef::Canon, "userguide"));
    assert_eq!(
        since_chip(&b),
        Some(("+2".into(), "2 landings since v1.0.0".into()))
    );
    assert_eq!(
        since_sentence(&b),
        "2 landings since v1.0.0 changed no blocks."
    );
    // no landing that changes what it shows: no chip and no page
    let mut w = world();
    w.r.tag(&approver(), "v1.0.0", None, None).unwrap();
    put_land(
        &mut w.r,
        "l1",
        json!([{"op": "update", "id": id(&w.ids, "$xp").to_string(), "body": "SECRETCANARY one."}]),
    );
    let b = build(&w.r.read(), &profile(ScopeRef::Canon, "userguide"));
    assert_eq!(since_chip(&b), None);
    assert!(!b.live.files.contains_key("changes-since.html"));
}

// --- 7. the profile-only site -------------------------------------------------------------------

#[test]
fn spv4_live_only_keeps_trail_and_redaction_rules() {
    let mut w = world();
    let (admin, folder, setup) = (w.admin.clone(), id(&w.ids, "$guide"), id(&w.ids, "$setup"));
    let mut explicit = profile_op("pubtrail", "Public trail", "public", &admin, &[&folder]);
    explicit["fields"]["trail"] = json!(true);
    add_profiles(
        &mut w,
        vec![
            profile_op("pubguide", "Public guide", "public", &admin, &[&folder]),
            explicit,
            profile_op("setuponly", "Setup only", "org", &admin, &[&setup]),
        ],
    );
    let st = w.r.read();
    let trail = "data-rdy=\"trail\"";
    for (key, shown) in [
        ("userguide", true),
        ("implicit:org", true),
        ("pubguide", false),
        ("pubtrail", true),
    ] {
        let b = build(&st, &only(key));
        let intro = file(&b, "guide/intro.html");
        assert_eq!(intro.contains(trail), shown, "{key}");
        assert_eq!(intro.contains("Intro ruling."), shown, "{key}");
        assert_eq!(b.live.warnings, Vec::<String>::new(), "{key}");
    }
    // the governed doc without the decision's home: no trail and no decision text
    let b = build(&st, &only("setuponly"));
    assert_absent(&b, "Setup ruling.");
    assert_absent(&b, trail);
    assert!(file(&b, "guide/setup.html").contains("Setup words."));
    drop(st);

    // a review that removes the figure, then a redaction of the figure's blob
    put(
        &mut w.r,
        "work",
        json!([{"op": "delete", "id": id(&w.ids, "$if").to_string()}]),
    );
    let before = build(
        &w.r.read(),
        &profile(ScopeRef::View("work".into()), "userguide"),
    );
    assert!(file(&before, "changes.html").contains("FIGALT"));
    let fig_blob = w.fig_blob;
    let World { dir, r, .. } = w;
    r.close().unwrap();
    let (r, request) = redact(dir.path(), vec![RedactTarget::Blob(fig_blob)]);
    let st = r.read();
    let canon = build(&st, &only("userguide"));
    let view = build(&st, &profile(ScopeRef::View("work".into()), "userguide"));
    for b in [&canon, &view] {
        assert_absent(b, "FIGBYTES");
        assert_absent(b, "FIGALT");
        assert_absent(b, &fig_blob.to_string());
        assert_absent(b, &request.to_string());
    }
    assert_eq!(
        file(&canon, "guide/intro.html")
            .matches(NOTICE_HTML)
            .count(),
        1
    );
    // the old side's removed figure reads as the notice on its removed line
    let intro = file(&view, "guide/intro.html");
    assert_eq!(intro.matches(NOTICE_HTML).count(), 1, "{intro}");
    assert!(file(&view, "changes.html").contains("Content removed."));
}
