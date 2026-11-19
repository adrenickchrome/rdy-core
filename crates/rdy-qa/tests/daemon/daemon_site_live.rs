//! ls5: the live state of the site: a build per scope on change, and the events a change sends
//! to each open page (D-N5NSN, D-FV6G9; the founder addition of 2026-10-12: a write reloads each
//! open page it changes, and no other).
use rdy_daemon::Shared;
use rdy_daemon::site::guard::{ScopeRef, SiteTarget};
use rdy_daemon::site::live::*;
use rdy_engine::engine::write_typed;
use rdy_engine::tensions::*;
use rdy_engine::{Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::daemon::Hosted;
use rdy_qa::golden::{approver, land, retired_reference, tiny, writer};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::error::TryRecvError;

const C: SiteTarget = SiteTarget::Source(ScopeRef::Canon);
const OLD: &str = "0000000000000000";

fn d() -> SiteTarget {
    SiteTarget::Source(ScopeRef::View("draft".into()))
}

/// Two landed docs, Alpha and Beta, each with one section of two prose blocks (as `render_live`).
fn two_docs() -> (tempfile::TempDir, Rdy) {
    let (d, mut r) = tiny();
    for t in ["Alpha", "Beta"] {
        rdy_qa::golden::write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": t.to_lowercase(), "title": t, "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
            {"op": "create", "type": "prose", "parent": "$s", "body": format!("{t} first.")},
            {"op": "create", "type": "prose", "parent": "$s", "body": format!("{t} second.")}
        ])).unwrap();
    }
    land(&mut r, "work").unwrap();
    (d, r)
}

/// The section of the doc titled `title`.
fn section_of(r: &Rdy, title: &str) -> BlockId {
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let doc = m
        .iter()
        .find(|b| b.field_str("title") == Some(title))
        .unwrap()
        .id
        .clone();
    m.children(Some(&doc))[0].id.clone()
}

/// Writes an edit of prose block `nth` of the doc titled `title` into `view`, and lands the
/// view when `land`.
fn edit_prose(r: &mut Rdy, view: &str, title: &str, nth: usize, body: &str, landed: bool) {
    let sec = section_of(r, title);
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let block = m.children(Some(&sec))[nth].id.clone();
    rdy_qa::golden::write(
        r,
        view,
        json!([
            {"op": "update", "id": block.to_string(), "body": body}
        ]),
    )
    .unwrap();
    if landed {
        land(r, view).unwrap();
    }
}

/// An edit of the second prose block (the home page shows only the first).
fn edit_second_prose(r: &mut Rdy, view: &str, title: &str, body: &str, landed: bool) {
    edit_prose(r, view, title, 1, body, landed);
}

/// A host over the closed project of `dir`.
fn host(dir: tempfile::TempDir, r: Rdy) -> Hosted {
    drop(r);
    Hosted::start_in(dir)
}

fn host_two_docs() -> Hosted {
    let (dir, r) = two_docs();
    host(dir, r)
}

fn shared(h: &Hosted) -> Arc<Shared> {
    Arc::clone(h.run.as_ref().unwrap().shared())
}

/// Runs `f` on the daemon's writer.
fn on_writer<T: serde::Serialize + 'static>(
    sh: &Arc<Shared>,
    f: impl FnOnce(&mut Rdy) -> T + Send + 'static,
) -> Value {
    write_typed(&*sh.engine(), move |r| Ok(f(r))).unwrap()
}

/// A stream of `page` of `scope` that has the page as `b` built it.
fn sub(sh: &Arc<Shared>, scope: &SiteTarget, page: &str, b: &SiteBuild) -> Subscription {
    sh.site
        .subscribe(sh, scope, page, &b.live.revs[page], &b.live.label_rev)
        .unwrap()
}

/// The file `path` of review target `t` as a request gets it: rendered on request, never
/// through a whole build.
#[track_caller]
fn review(sh: &Arc<Shared>, t: &SiteTarget, path: &str) -> ReviewPage {
    sh.site.review_page(sh, t, path).unwrap().unwrap()
}

/// A stream of `path` of review target `t` that has the page as `p` served it.
fn review_sub(sh: &Arc<Shared>, t: &SiteTarget, path: &str, p: &ReviewPage) -> Subscription {
    sh.site
        .subscribe(sh, t, path, p.rev.as_deref().unwrap(), &p.label_rev)
        .unwrap()
}

fn empty(s: &mut Subscription) -> bool {
    s.rx.try_recv() == Err(TryRecvError::Empty)
}

#[test]
fn ls5_a_body_edit_reloads_only_its_page() {
    let h = host_two_docs();
    let sh = shared(&h);
    let b = sh.site.refresh(&sh, &C).unwrap();
    let mut alpha = sub(&sh, &C, "alpha.html", &b);
    let mut beta = sub(&sh, &C, "beta.html", &b);
    let mut home = sub(&sh, &C, "index.html", &b);
    for s in [&mut alpha, &mut beta, &mut home] {
        assert!(empty(s), "a current page gets nothing on subscribe");
    }
    on_writer(&sh, |r| {
        edit_second_prose(r, "work", "Alpha", "A new second paragraph.", true)
    });
    sh.site.refresh(&sh, &C).unwrap();
    assert_eq!(alpha.rx.try_recv(), Ok(LiveEvent::Reload));
    assert!(empty(&mut beta), "beta's bytes did not change");
    assert!(empty(&mut home), "home shows only the first prose");
}

#[test]
fn ls5_a_view_commit_reaches_only_that_view() {
    let h = host_two_docs();
    let sh = shared(&h);
    // the view exists before the pages open
    on_writer(&sh, |r| {
        edit_second_prose(r, "draft", "Alpha", "Draft one.", false)
    });
    let bc = sh.site.refresh(&sh, &C).unwrap();
    let (pa, pb) = (
        review(&sh, &d(), "alpha.html"),
        review(&sh, &d(), "beta.html"),
    );
    let mut ca = sub(&sh, &C, "alpha.html", &bc);
    let mut cb = sub(&sh, &C, "beta.html", &bc);
    let mut da = review_sub(&sh, &d(), "alpha.html", &pa);
    let mut db = review_sub(&sh, &d(), "beta.html", &pb);
    let (before, renders) = (sh.site.builds(), sh.site.page_renders());
    // another block of Alpha: the view now changes two blocks (the site counts blocks, C9)
    on_writer(&sh, |r| {
        edit_prose(r, "draft", "Alpha", 0, "Draft two.", false)
    });
    sh.site.refresh(&sh, &C).unwrap();
    let after = review(&sh, &d(), "beta.html");
    assert_eq!(da.rx.try_recv(), Ok(LiveEvent::Reload));
    match db.rx.try_recv() {
        Ok(LiveEvent::Label { text, html }) => {
            assert!(
                text.ends_with(" · view: draft, 2 changes, not landed"),
                "{text}"
            );
            assert!(html.contains(">+2</a>") && html == after.header, "{html}");
        }
        other => panic!("the view's other page gets the new label and header: {other:?}"),
    }
    assert!(
        empty(&mut ca) && empty(&mut cb),
        "canon's pages get nothing"
    );
    assert_eq!(
        sh.site.builds(),
        before,
        "canon's inputs did not change, and the view has no whole build"
    );
    assert_eq!(
        sh.site.page_renders(),
        renders + 2,
        "the view's two open pages rendered once each"
    );
}

#[test]
fn ls5_a_tag_sends_label_not_reload() {
    let h = host_two_docs();
    let sh = shared(&h);
    let b = sh.site.refresh(&sh, &C).unwrap();
    let mut alpha = sub(&sh, &C, "alpha.html", &b);
    let mut beta = sub(&sh, &C, "beta.html", &b);
    let mut home = sub(&sh, &C, "index.html", &b);
    on_writer(&sh, |r| {
        r.tag(&approver(), "v1.0.0", None, None).unwrap();
    });
    let after = sh.site.refresh(&sh, &C).unwrap();
    assert!(
        after
            .live
            .header
            .contains("<span class=\"pill-text\">v1.0.0</span>"),
        "{}",
        after.live.header
    );
    let label = LiveEvent::Label {
        text: "v1.0.0".into(),
        html: after.live.header.clone(),
    };
    assert_eq!(alpha.rx.try_recv(), Ok(label.clone()));
    assert_eq!(beta.rx.try_recv(), Ok(label));
    assert_eq!(
        home.rx.try_recv(),
        Ok(LiveEvent::Reload),
        "home lists the versions"
    );
}

#[test]
fn ls5_a_proposal_version_holds_still_and_its_latest_follows() {
    let h = host_two_docs();
    let sh = shared(&h);
    let p = on_writer(&sh, |r| {
        edit_second_prose(r, "draft", "Alpha", "Draft one.", false);
        r.propose(&writer(), "draft", "Draft", None).unwrap()
    });
    let pref = p["ref"].as_str().unwrap().to_string();
    assert!(pref.ends_with("@1"), "guard: version 1: {pref}");
    let stable = pref.split('@').next().unwrap().to_string();
    let v1 = SiteTarget::Source(ScopeRef::Proposal(pref.clone()));
    let latest = SiteTarget::Source(ScopeRef::Proposal(stable));
    let b1 = review(&sh, &v1, "index.html");
    let bl = review(&sh, &latest, "index.html");
    let mut s1 = review_sub(&sh, &v1, "index.html", &b1);
    let mut sl = review_sub(&sh, &latest, "index.html", &bl);
    let renders = sh.site.page_renders();
    let p2 = on_writer(&sh, |r| {
        // the first prose: the home page shows it
        edit_prose(r, "draft", "Beta", 0, "Beta drafted first.", false);
        r.propose(&writer(), "draft", "Draft", None).unwrap()
    });
    assert!(
        p2["ref"].as_str().unwrap().ends_with("@2"),
        "guard: version 2: {p2}"
    );
    // the worker may render the latest alias's open page before or after the reads below: its
    // reload is the fan-out of the new state, so the one render is counted from before the
    // propose, and the reads follow the reload
    assert_eq!(sl.rx.blocking_recv(), Some(LiveEvent::Reload));
    review(&sh, &v1, "index.html");
    review(&sh, &latest, "index.html");
    assert_eq!(
        sh.site.page_renders(),
        renders + 1,
        "only the latest alias's open page rendered"
    );
    assert!(empty(&mut s1), "a proposal version never changes");
    assert!(empty(&mut sl), "one reload");
}

#[test]
fn ls5_a_stale_revision_gets_reload_on_subscribe() {
    let h = host_two_docs();
    let sh = shared(&h);
    let b = sh.site.refresh(&sh, &C).unwrap();
    let mut stale = sh
        .site
        .subscribe(&sh, &C, "alpha.html", OLD, &b.live.label_rev)
        .unwrap();
    assert_eq!(stale.rx.try_recv(), Ok(LiveEvent::Reload));
    let mut label = sh
        .site
        .subscribe(&sh, &C, "alpha.html", &b.live.revs["alpha.html"], OLD)
        .unwrap();
    assert_eq!(
        label.rx.try_recv(),
        Ok(LiveEvent::Label {
            text: b.live.label.clone(),
            html: b.live.header.clone()
        })
    );
    let mut gone = sh
        .site
        .subscribe(&sh, &C, "gone.html", OLD, &b.live.label_rev)
        .unwrap();
    assert_eq!(gone.rx.try_recv(), Ok(LiveEvent::Reload));
}

#[test]
fn ls5_no_open_page_no_build() {
    let h = host_two_docs();
    let sh = shared(&h);
    let b = sh.site.refresh(&sh, &C).unwrap();
    let rev = b.live.revs["alpha.html"].clone();
    let builds = sh.site.builds();
    on_writer(&sh, |r| {
        edit_second_prose(r, "work", "Alpha", "Nobody reads this.", true)
    });
    assert_eq!(sh.site.step(&sh), 0, "no open page, no build");
    assert_eq!(sh.site.builds(), builds);
    let mut s = sh
        .site
        .subscribe(&sh, &C, "alpha.html", &rev, &b.live.label_rev)
        .unwrap();
    assert_eq!(sh.site.builds(), builds + 1, "the subscribe built once");
    assert_eq!(s.rx.try_recv(), Ok(LiveEvent::Reload));
}

#[test]
fn ls5_a_full_queue_drops_the_stream() {
    let h = host_two_docs();
    let sh = shared(&h);
    let b = sh.site.refresh(&sh, &C).unwrap();
    let mut alpha = sub(&sh, &C, "alpha.html", &b);
    for i in 0..=STREAM_QUEUE {
        on_writer(&sh, move |r| {
            edit_second_prose(r, "work", "Alpha", &format!("Edit {i}."), true)
        });
        sh.site.refresh(&sh, &C).unwrap();
    }
    assert_eq!(sh.site.streams(), 0, "the full stream was dropped");
    for _ in 0..STREAM_QUEUE {
        assert_eq!(alpha.rx.try_recv(), Ok(LiveEvent::Reload));
    }
    assert_eq!(alpha.rx.try_recv(), Err(TryRecvError::Disconnected));
}

#[test]
fn ls5_the_streams_are_capped() {
    let h = host_two_docs();
    let sh = shared(&h);
    let b = sh.site.refresh(&sh, &C).unwrap();
    let mut open: Vec<Subscription> = (0..MAX_STREAMS)
        .map(|_| sub(&sh, &C, "alpha.html", &b))
        .collect();
    assert_eq!(sh.site.streams(), MAX_STREAMS);
    let refused = sh.site.subscribe(
        &sh,
        &C,
        "alpha.html",
        &b.live.revs["alpha.html"],
        &b.live.label_rev,
    );
    assert!(
        matches!(refused, Err(NoStream::Busy)),
        "the 33rd is refused"
    );
    drop(open.pop());
    assert_eq!(sh.site.streams(), MAX_STREAMS - 1);
    open.push(sub(&sh, &C, "alpha.html", &b));
    assert_eq!(sh.site.streams(), MAX_STREAMS);
}

#[test]
fn ls5_the_writer_wakes_the_worker() {
    let h = host_two_docs();
    let sh = shared(&h);
    let b = sh.site.refresh(&sh, &C).unwrap();
    let mut alpha = sub(&sh, &C, "alpha.html", &b);
    on_writer(&sh, |r| {
        edit_second_prose(r, "work", "Alpha", "The worker builds this.", true)
    });
    let rt = Hosted::rt();
    let got =
        rt.block_on(async { tokio::time::timeout(Duration::from_secs(10), alpha.rx.recv()).await });
    assert_eq!(
        got,
        Ok(Some(LiveEvent::Reload)),
        "the worker built on its own"
    );
}

#[test]
fn ls5_a_new_term_reloads_the_glossary() {
    let h = host_two_docs();
    let sh = shared(&h);
    let term = |title: &'static str, name: &'static str, body: &'static str| {
        move |r: &mut Rdy| {
            let sec = section_of(r, title);
            rdy_qa::golden::write(r, "work", json!([
                {"op": "create", "type": "term", "parent": sec.to_string(), "fields": {"name": name}, "body": body}
            ]))
            .unwrap();
            land(r, "work").unwrap();
        }
    };
    on_writer(&sh, term("Alpha", "Ledger", "Balances."));
    let b = sh.site.refresh(&sh, &C).unwrap();
    assert!(
        b.live.revs.contains_key("glossary.html"),
        "guard: the glossary exists"
    );
    let mut glossary = sub(&sh, &C, "glossary.html", &b);
    let mut alpha = sub(&sh, &C, "alpha.html", &b);
    let mut beta = sub(&sh, &C, "beta.html", &b);
    on_writer(&sh, term("Beta", "Balance", "What is left."));
    sh.site.refresh(&sh, &C).unwrap();
    assert_eq!(glossary.rx.try_recv(), Ok(LiveEvent::Reload));
    assert_eq!(beta.rx.try_recv(), Ok(LiveEvent::Reload));
    assert!(empty(&mut alpha), "alpha's bytes did not change");
}

#[test]
fn ls5_a_dismissal_reloads_the_disagreements_page() {
    let (dir, r, _s) = retired_reference();
    let h = host(dir, r);
    let sh = shared(&h);
    let b = sh.site.refresh(&sh, &C).unwrap();
    let mut page = sub(&sh, &C, "disagreements.html", &b);
    let mut home = sub(&sh, &C, "index.html", &b);
    on_writer(&sh, |r| {
        let t = {
            let set = r.read().tensions().unwrap();
            set.iter()
                .find(|t| t.kind == TensionKind::DeprecatedReference)
                .cloned()
                .expect("guard: the canon holds the tension")
        };
        r.dismiss_tension(&writer(), &tension_label(&t.id), "fine as it is")
            .unwrap();
    });
    sh.site.refresh(&sh, &C).unwrap();
    assert_eq!(
        page.rx.try_recv(),
        Ok(LiveEvent::Reload),
        "the page is gone"
    );
    assert_eq!(
        home.rx.try_recv(),
        Ok(LiveEvent::Reload),
        "home no longer links it"
    );
}

/// Fix round 1, finding 3: a scope that does not resolve keeps no state, so request-chosen
/// names cannot grow the daemon's memory; a stream of such a scope takes its entry with it.
#[test]
fn ls5_an_unknown_scope_leaves_nothing_behind() {
    let h = host_two_docs();
    let sh = shared(&h);
    for i in 0..1000 {
        let name = format!("{}{i}", "x".repeat(4000));
        let target = SiteTarget::Source(ScopeRef::View(name));
        assert!(sh.site.refresh(&sh, &target).is_err());
    }
    assert_eq!(sh.site.scopes(), 0, "a failed refresh keeps no scope");
    let s = sh
        .site
        .subscribe(
            &sh,
            &SiteTarget::Source(ScopeRef::View("nowhere".into())),
            "index.html",
            OLD,
            OLD,
        )
        .unwrap();
    assert_eq!(sh.site.scopes(), 1, "the stream holds its scope");
    drop(s);
    assert_eq!(
        sh.site.scopes(),
        0,
        "the last stream takes its scope with it"
    );
}

/// Fix round 1, finding 4 (Best effort, then exact): a page whose stream opened while its scope
/// did not build is compared with the scope's first build, as a subscribe compares it.
#[test]
fn ls5_a_page_opened_before_its_scope_builds_gets_reload() {
    let h = host_two_docs();
    let sh = shared(&h);
    // no view `draft` yet: the stream opens with nothing to compare
    let mut alpha = sh
        .site
        .subscribe(&sh, &d(), "alpha.html", OLD, OLD)
        .unwrap();
    assert!(empty(&mut alpha), "no build, no comparison");
    on_writer(&sh, |r| {
        edit_second_prose(r, "draft", "Alpha", "Draft one.", false)
    });
    let renders = sh.site.page_renders();
    review(&sh, &d(), "alpha.html");
    assert_eq!(
        sh.site.page_renders(),
        renders + 1,
        "the open page rendered once"
    );
    assert_eq!(alpha.rx.try_recv(), Ok(LiveEvent::Reload));
}

/// slb1 (TASK-2307): the test cap on the bytes of inactive builds, small so a few builds of
/// [`figures_host`] pass it.
const SLB1_CAP: u64 = 16 * 1024 * 1024;

/// A host over a project with one landed doc, Figures, of 8 figures of 1 MiB each (distinct
/// bytes), and tags `v1.0.0`..`v1.9.0`, each after the landing of one more note. Each source
/// build holds about 8 MiB of figure bytes. Canon and tags are eager targets: each is one whole
/// build.
fn figures_host() -> Hosted {
    let (dir, mut r) = tiny();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "figures", "title": "Figures", "doc_type": "page"}}),
        json!({"op": "create", "type": "prose", "parent": "$d", "body": "Figures first."}),
    ];
    for i in 0..8u8 {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend((0..1024 * 1024u32).map(|n| (n as u8) ^ i.wrapping_mul(37)));
        png.push(i);
        let blob = r.store_mut().put_blob(&png).unwrap();
        ops.push(json!({"op": "create", "type": "figure", "parent": "$d", "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": format!("Figure {i}")}}));
    }
    rdy_qa::golden::write(&mut r, "work", Value::Array(ops)).unwrap();
    land(&mut r, "work").unwrap();
    for v in 0..10 {
        rdy_qa::golden::write(
            &mut r,
            &format!("v{v}"),
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("note{v}"), "title": format!("Note {v}"), "doc_type": "page"}},
                {"op": "create", "type": "prose", "parent": "$d", "body": format!("View {v}.")}
            ]),
        )
        .unwrap();
        land(&mut r, &format!("v{v}")).unwrap();
        r.tag(&approver(), &format!("v1.{v}.0"), None, None)
            .unwrap();
    }
    host(dir, r)
}

fn tag(v: usize) -> SiteTarget {
    SiteTarget::Source(ScopeRef::Tag(format!("v1.{v}.0")))
}

/// slb1: inactive builds are dropped, least recently used first, past the byte cap, not only
/// past the count: canon and 10 tags of 8 MiB each stay under a 16 MiB cap.
#[test]
fn slb1_inactive_builds_stay_under_the_byte_cap() {
    let h = figures_host();
    let sh = shared(&h);
    sh.site.set_inactive_build_cap(Some(SLB1_CAP));
    let one = sh.site.refresh(&sh, &C).unwrap();
    let size: u64 = one.live.files.values().map(|f| f.len() as u64).sum();
    assert!(size >= 8 * 1024 * 1024, "a build holds the figures: {size}");
    drop(one);
    for v in 0..10 {
        sh.site.refresh(&sh, &tag(v)).unwrap();
        assert!(
            sh.site.held_inactive_bytes() <= SLB1_CAP,
            "after tag {v}: {} inactive bytes held",
            sh.site.held_inactive_bytes()
        );
    }
    assert!(
        (1..=2).contains(&sh.site.held_builds()),
        "{} builds held",
        sh.site.held_builds()
    );
    assert_eq!(sh.site.held_bytes(), sh.site.held_inactive_bytes());
}

/// TASK-2307: a build holds each figure in a buffer of the figure's length, so the bytes the cap
/// counts are the bytes the build holds. A figure of more than one store chunk was held in a
/// buffer of up to twice its length, which the cap did not count.
#[test]
fn slb1_a_build_holds_each_figure_at_its_length() {
    let h = figures_host();
    let sh = shared(&h);
    let b = sh.site.refresh(&sh, &C).unwrap();
    let figures: Vec<(&String, &Vec<u8>)> = b
        .live
        .files
        .iter()
        .filter(|(_, f)| f.starts_with(b"\x89PNG"))
        .collect();
    assert_eq!(figures.len(), 8, "{:?}", b.live.files.keys());
    for (path, f) in figures {
        assert!(f.len() > 1024 * 1024, "{path}: {} bytes", f.len());
        assert_eq!(f.capacity(), f.len(), "{path} is held at its length");
    }
}

/// slb1: a build dropped by the byte cap is built again on its next request, to the same bytes.
#[test]
fn slb1_a_dropped_build_is_rebuilt_on_its_next_request() {
    let h = figures_host();
    let sh = shared(&h);
    sh.site.set_inactive_build_cap(Some(SLB1_CAP));
    let first = sh.site.refresh(&sh, &C).unwrap();
    let (files, revs) = (first.live.files.clone(), first.live.revs.clone());
    drop(first);
    for v in 0..3 {
        sh.site.refresh(&sh, &tag(v)).unwrap();
    }
    let builds = sh.site.builds();
    let again = sh.site.refresh(&sh, &C).unwrap();
    assert_eq!(
        sh.site.builds(),
        builds + 1,
        "canon was dropped and builds again"
    );
    assert_eq!(again.live.revs, revs);
    assert!(
        again.live.files == files,
        "the rebuilt site serves the same bytes"
    );
}

/// The bytes the site's cap counts for build `b` (as the daemon's `held_size`).
fn counted(b: &SiteBuild) -> u64 {
    b.live
        .files
        .iter()
        .map(|(k, v)| k.len() as u64 + v.len() as u64 + 64)
        .sum()
}

/// A landing that changes the Figures page of [`figures_host`] (its prose), and no figure.
fn edit_figures_prose(r: &mut Rdy, body: &str) {
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let doc = m
        .iter()
        .find(|b| b.field_str("title") == Some("Figures"))
        .unwrap()
        .id
        .clone();
    let prose = m
        .children(Some(&doc))
        .iter()
        .find(|b| b.body == "Figures first.")
        .unwrap()
        .id
        .clone();
    rdy_qa::golden::write(
        r,
        "work",
        json!([{"op": "update", "id": prose.to_string(), "body": body}]),
    )
    .unwrap();
    land(r, "work").unwrap();
}

/// The builds of targets with an open page count in the byte cap: past it, builds are dropped
/// least recently used first whether a page is open or not, keeping the most recently used one;
/// every stream stays open.
#[test]
fn slb1_open_targets_builds_count_in_the_byte_cap() {
    let h = figures_host();
    let sh = shared(&h);
    let one = sh.site.refresh(&sh, &C).unwrap();
    let size = counted(&one);
    drop(one);
    // room for two builds and not three
    let cap = size * 5 / 2;
    sh.site.set_inactive_build_cap(Some(cap));
    let mut subs = Vec::new();
    for t in [C, tag(0), tag(1), tag(2)] {
        let b = sh.site.refresh(&sh, &t).unwrap();
        subs.push(sub(&sh, &t, "index.html", &b));
        assert!(
            sh.site.held_bytes() <= cap,
            "{} bytes held in {} builds",
            sh.site.held_bytes(),
            sh.site.held_builds()
        );
    }
    assert_eq!(sh.site.held_builds(), 2, "the cap holds two builds");
    assert_eq!(sh.site.streams(), 4, "every page stays open");
    assert_eq!(sh.site.scopes(), 4, "every open target keeps its slot");
    // a cap below one build keeps only the most recently used build, over the cap
    sh.site.set_inactive_build_cap(Some(size / 2));
    assert_eq!(sh.site.held_builds(), 1);
    assert!(sh.site.held_bytes() > size / 2);
    let builds = sh.site.builds();
    sh.site.refresh(&sh, &tag(2)).unwrap();
    assert_eq!(sh.site.builds(), builds, "the most recent build is kept");
    assert_eq!(sh.site.streams(), 4);
}

/// An open target whose build the byte cap dropped is built again on its next request, and its
/// page's stream, compared with the revision the page opened at, gets nothing while its page is
/// unchanged and `Reload` once a landing changes it.
#[test]
fn slb1_an_evicted_open_target_rebuilds_and_its_page_still_hears() {
    let h = figures_host();
    let sh = shared(&h);
    sh.site.set_inactive_build_cap(Some(SLB1_CAP));
    let b = sh.site.refresh(&sh, &C).unwrap();
    assert!(counted(&b) > SLB1_CAP / 2, "one build fits, two do not");
    let mut figures = sub(&sh, &C, "figures.html", &b);
    drop(b);
    let mut others = Vec::new();
    for v in 0..2 {
        let b = sh.site.refresh(&sh, &tag(v)).unwrap();
        others.push(sub(&sh, &tag(v), "index.html", &b));
    }
    assert_eq!(sh.site.held_builds(), 1, "canon's build was dropped");
    assert_eq!(sh.site.streams(), 3, "canon's page is still open");
    // the next request builds canon again; its page did not change
    let builds = sh.site.builds();
    sh.site.refresh(&sh, &C).unwrap();
    assert_eq!(sh.site.builds(), builds + 1, "canon builds again");
    assert!(empty(&mut figures), "an unchanged page gets nothing");
    // canon's build goes again, then a landing changes its page
    sh.site.refresh(&sh, &tag(0)).unwrap();
    assert_eq!(sh.site.held_builds(), 1);
    on_writer(&sh, |r| edit_figures_prose(r, "Figures changed."));
    // the worker may build canon first; after this refresh a build of the landing has fanned out
    sh.site.refresh(&sh, &C).unwrap();
    assert_eq!(figures.rx.try_recv(), Ok(LiveEvent::Reload));
}

/// A page whose close makes the caps drop a build gives the allocator's free memory back once;
/// a close that drops no build gives nothing back.
#[test]
fn slb1_closing_a_page_that_drops_a_build_gives_memory_back() {
    let h = figures_host();
    let sh = shared(&h);
    let mut subs = Vec::new();
    for t in std::iter::once(C).chain((0..9).map(tag)) {
        let b = sh.site.refresh(&sh, &t).unwrap();
        subs.push(sub(&sh, &t, "index.html", &b));
    }
    assert_eq!(sh.site.held_builds(), 10, "ten builds fit the default cap");
    let builds = sh.site.builds();
    // closed oldest first: past MAX_SCOPES inactive targets, each close drops one build
    for (i, s) in subs.into_iter().enumerate() {
        let before = sh.site.memory_releases();
        drop(s);
        let dropped = i >= MAX_SCOPES;
        assert_eq!(
            sh.site.memory_releases(),
            before + u64::from(dropped),
            "close {i}"
        );
        assert_eq!(
            sh.site.held_builds(),
            10 - (i + 1).saturating_sub(MAX_SCOPES),
            "close {i}"
        );
    }
    assert_eq!(sh.site.builds(), builds, "no close builds");
}

/// slb1: after a landing rebuilds an open target, the old build is released: no slot, worker
/// or fan-out keeps it.
#[test]
fn slb1_a_landing_rebuild_releases_the_old_build() {
    let h = figures_host();
    let sh = shared(&h);
    let b = sh.site.refresh(&sh, &C).unwrap();
    let _home = sub(&sh, &C, "index.html", &b);
    let old = Arc::downgrade(&b);
    drop(b);
    assert_eq!(sh.site.held_builds(), 1);
    on_writer(&sh, |r| {
        rdy_qa::golden::write(
            r,
            "late",
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "late", "title": "Late", "doc_type": "page"}},
                {"op": "create", "type": "prose", "parent": "$d", "body": "Late."}
            ]),
        )
        .unwrap();
        land(r, "late").unwrap();
    });
    let new = sh.site.refresh(&sh, &C).unwrap();
    assert!(
        new.live.files.contains_key("late.html"),
        "{:?}",
        new.live.files.keys()
    );
    drop(new);
    // the worker may still be in its step for the landing; it holds no build once it is done
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while old.strong_count() > 0 && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(old.strong_count(), 0, "the old build is released");
    assert_eq!(sh.site.held_builds(), 1, "one build of canon is held");
}

/// slb1 (review F1): builds kept by open streams become inactive when their last stream closes,
/// and the byte cap holds from then on, with no new build: a cache hit finds them dropped.
#[test]
fn slb1_closing_the_last_streams_enforces_the_byte_cap() {
    let h = figures_host();
    let sh = shared(&h);
    sh.site.set_inactive_build_cap(Some(SLB1_CAP));
    let mut subs = Vec::new();
    for t in [C, tag(0), tag(1), tag(2)] {
        let b = sh.site.refresh(&sh, &t).unwrap();
        subs.push(sub(&sh, &t, "index.html", &b));
    }
    assert_eq!(
        sh.site.held_builds(),
        1,
        "open targets' builds count in the cap"
    );
    assert_eq!(sh.site.streams(), 4);
    drop(subs);
    let builds = sh.site.builds();
    sh.site.refresh(&sh, &tag(2)).unwrap();
    assert_eq!(sh.site.builds(), builds, "a cache hit");
    assert!(
        sh.site.held_inactive_bytes() <= SLB1_CAP || sh.site.held_builds() == 1,
        "{} inactive bytes in {} builds",
        sh.site.held_inactive_bytes(),
        sh.site.held_builds()
    );
    assert_eq!(sh.site.held_bytes(), sh.site.held_inactive_bytes());
}
