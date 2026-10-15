//! ls5: the live state of the site: a build per scope on change, and the events a change sends
//! to each open page (D-N5NSN, D-FV6G9; the founder addition of 2026-10-12: a write reloads each
//! open page it changes, and no other).
use rdy_daemon::Shared;
use rdy_daemon::site::guard::ScopeRef;
use rdy_daemon::site::live::*;
use rdy_engine::engine::write_typed;
use rdy_engine::tensions::*;
use rdy_engine::{Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::daemon::Hosted;
use rdy_qa::golden::{approver, land, retired_reference, tiny, writer};
use rdy_render::live::LiveBuild;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::error::TryRecvError;

const C: ScopeRef = ScopeRef::Canon;
const OLD: &str = "0000000000000000";

fn d() -> ScopeRef {
    ScopeRef::View("draft".into())
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
fn sub(sh: &Arc<Shared>, scope: &ScopeRef, page: &str, b: &LiveBuild) -> Subscription {
    sh.site
        .subscribe(sh, scope, page, &b.revs[page], &b.label_rev)
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
    let bd = sh.site.refresh(&sh, &d()).unwrap();
    let mut ca = sub(&sh, &C, "alpha.html", &bc);
    let mut cb = sub(&sh, &C, "beta.html", &bc);
    let mut da = sub(&sh, &d(), "alpha.html", &bd);
    let mut db = sub(&sh, &d(), "beta.html", &bd);
    let before = sh.site.builds();
    // another block of Alpha: the view now changes two blocks (the site counts blocks, C9)
    on_writer(&sh, |r| {
        edit_prose(r, "draft", "Alpha", 0, "Draft two.", false)
    });
    sh.site.refresh(&sh, &C).unwrap();
    let after = sh.site.refresh(&sh, &d()).unwrap();
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
        before + 1,
        "the view built; canon's inputs did not change"
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
            .header
            .contains("<span class=\"pill-text\">v1.0.0</span>"),
        "{}",
        after.header
    );
    let label = LiveEvent::Label {
        text: "v1.0.0".into(),
        html: after.header.clone(),
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
        r.propose(&writer(), "draft", "Draft").unwrap()
    });
    let pref = p["ref"].as_str().unwrap().to_string();
    assert!(pref.ends_with("@1"), "guard: version 1: {pref}");
    let stable = pref.split('@').next().unwrap().to_string();
    let v1 = ScopeRef::Proposal(pref.clone());
    let latest = ScopeRef::Proposal(stable);
    let b1 = sh.site.refresh(&sh, &v1).unwrap();
    let bl = sh.site.refresh(&sh, &latest).unwrap();
    let mut s1 = sub(&sh, &v1, "index.html", &b1);
    let mut sl = sub(&sh, &latest, "index.html", &bl);
    let p2 = on_writer(&sh, |r| {
        // the first prose: the home page shows it
        edit_prose(r, "draft", "Beta", 0, "Beta drafted first.", false);
        r.propose(&writer(), "draft", "Draft").unwrap()
    });
    assert!(
        p2["ref"].as_str().unwrap().ends_with("@2"),
        "guard: version 2: {p2}"
    );
    sh.site.refresh(&sh, &v1).unwrap();
    sh.site.refresh(&sh, &latest).unwrap();
    assert!(empty(&mut s1), "a proposal version never changes");
    assert_eq!(sl.rx.try_recv(), Ok(LiveEvent::Reload));
}

#[test]
fn ls5_a_stale_revision_gets_reload_on_subscribe() {
    let h = host_two_docs();
    let sh = shared(&h);
    let b = sh.site.refresh(&sh, &C).unwrap();
    let mut stale = sh
        .site
        .subscribe(&sh, &C, "alpha.html", OLD, &b.label_rev)
        .unwrap();
    assert_eq!(stale.rx.try_recv(), Ok(LiveEvent::Reload));
    let mut label = sh
        .site
        .subscribe(&sh, &C, "alpha.html", &b.revs["alpha.html"], OLD)
        .unwrap();
    assert_eq!(
        label.rx.try_recv(),
        Ok(LiveEvent::Label {
            text: b.label.clone(),
            html: b.header.clone()
        })
    );
    let mut gone = sh
        .site
        .subscribe(&sh, &C, "gone.html", OLD, &b.label_rev)
        .unwrap();
    assert_eq!(gone.rx.try_recv(), Ok(LiveEvent::Reload));
}

#[test]
fn ls5_no_open_page_no_build() {
    let h = host_two_docs();
    let sh = shared(&h);
    let b = sh.site.refresh(&sh, &C).unwrap();
    let rev = b.revs["alpha.html"].clone();
    let builds = sh.site.builds();
    on_writer(&sh, |r| {
        edit_second_prose(r, "work", "Alpha", "Nobody reads this.", true)
    });
    assert_eq!(sh.site.step(&sh), 0, "no open page, no build");
    assert_eq!(sh.site.builds(), builds);
    let mut s = sh
        .site
        .subscribe(&sh, &C, "alpha.html", &rev, &b.label_rev)
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
    let refused = sh
        .site
        .subscribe(&sh, &C, "alpha.html", &b.revs["alpha.html"], &b.label_rev);
    assert!(matches!(refused, Err(SiteBusy)), "the 33rd is refused");
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
        b.revs.contains_key("glossary.html"),
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
        assert!(sh.site.refresh(&sh, &ScopeRef::View(name)).is_err());
    }
    assert_eq!(sh.site.scopes(), 0, "a failed refresh keeps no scope");
    let s = sh
        .site
        .subscribe(
            &sh,
            &ScopeRef::View("nowhere".into()),
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
    sh.site.refresh(&sh, &d()).unwrap();
    assert_eq!(alpha.rx.try_recv(), Ok(LiveEvent::Reload));
}
