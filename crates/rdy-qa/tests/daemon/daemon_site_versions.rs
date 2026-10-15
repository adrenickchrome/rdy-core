//! sr8: the versions page of the live site (D-2F29G; RULING F5): one page at `/~versions/`, built
//! on its own key, so a view commit rebuilds it and never canon, and recounts only that view. It
//! is never a read scope.
use rdy_daemon::Shared;
use rdy_daemon::site::SiteInfo;
use rdy_daemon::site::guard::ScopeRef;
use rdy_daemon::site::live::*;
use rdy_engine::engine::write_typed;
use rdy_engine::{Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::daemon::Hosted;
use rdy_qa::golden::{land, tiny};
use rdy_qa::http;
use rdy_render::live::LiveBuild;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc::error::TryRecvError;

const C: ScopeRef = ScopeRef::Canon;
const V: ScopeRef = ScopeRef::Versions;

fn shared(h: &Hosted) -> Arc<Shared> {
    Arc::clone(h.run.as_ref().unwrap().shared())
}

/// The site's loopback URL, `http://127.0.0.1:<port>/`.
fn url(h: &Hosted) -> String {
    let info: SiteInfo = shared(h)
        .site_info
        .get()
        .cloned()
        .expect("the site info is set before ready");
    info.url.expect("the site listens")
}

/// Two landed docs, Alpha and Beta, each with one prose block.
fn two_docs() -> Hosted {
    let (dir, mut r) = tiny();
    for t in ["Alpha", "Beta"] {
        rdy_qa::golden::write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": t.to_lowercase(), "title": t, "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
            {"op": "create", "type": "prose", "parent": "$s", "body": format!("{t} first.")}
        ])).unwrap();
    }
    land(&mut r, "work").unwrap();
    drop(r);
    Hosted::start_in(dir)
}

/// The prose block of the doc titled `title`, in canon.
fn prose_of(r: &Rdy, title: &str) -> BlockId {
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let doc = m
        .iter()
        .find(|b| b.field_str("title") == Some(title))
        .unwrap()
        .id
        .clone();
    let sec = m.children(Some(&doc))[0].id.clone();
    m.children(Some(&sec))[0].id.clone()
}

/// Edits the prose of the doc titled `title` in `view`, on the daemon's writer.
fn edit(sh: &Arc<Shared>, view: &'static str, title: &'static str, body: &'static str) {
    write_typed(&*sh.engine(), move |r| {
        let id = prose_of(r, title);
        rdy_qa::golden::write(
            r,
            view,
            json!([{"op": "update", "id": id.to_string(), "body": body}]),
        )
        .map(|_| Value::Null)
    })
    .unwrap();
}

fn sub(sh: &Arc<Shared>, scope: &ScopeRef, page: &str, b: &LiveBuild) -> Subscription {
    sh.site
        .subscribe(sh, scope, page, &b.revs[page], &b.label_rev)
        .unwrap()
}

fn empty(s: &mut Subscription) -> bool {
    s.rx.try_recv() == Err(TryRecvError::Empty)
}

#[test]
fn sr8_the_versions_page_is_served() {
    let (dir, r, _ids) = rdy_qa::golden::project();
    drop(r);
    let h = Hosted::start_in(dir);
    let url = url(&h);
    let got = http::get(&format!("{url}~versions/"));
    assert_eq!(got.status, 200);
    let body = String::from_utf8(got.body).unwrap();
    for h2 in ["<h2>Versions</h2>", "<h2>Proposals</h2>", "<h2>Views</h2>"] {
        assert!(body.contains(h2), "{h2} in {body}");
    }
    assert!(
        body.contains("<a href=\"/~view/editing/\">editing</a>"),
        "{body}"
    );
    assert!(
        body.contains("data-events=\"/_rdy/events?scope=versions&amp;page=index.html"),
        "the page follows its own stream: {body}"
    );
}

#[test]
fn sr8_a_view_commit_rebuilds_the_versions_page_and_not_canon() {
    let h = two_docs();
    let sh = shared(&h);
    // the view exists before the pages open
    edit(&sh, "editing", "Alpha", "Alpha, edited.");
    let bc = sh.site.refresh(&sh, &C).unwrap();
    let bv = sh.site.refresh(&sh, &V).unwrap();
    assert!(
        String::from_utf8_lossy(&bv.files["index.html"]).contains("1 doc changed"),
        "the view changes one doc"
    );
    let mut home = sub(&sh, &C, "index.html", &bc);
    let mut alpha = sub(&sh, &C, "alpha.html", &bc);
    let mut versions = sub(&sh, &V, "index.html", &bv);
    for s in [&mut home, &mut alpha, &mut versions] {
        assert!(empty(s), "a current page gets nothing on subscribe");
    }
    let before = sh.site.builds();
    // the view now changes a second doc: the menu shows another count
    edit(&sh, "editing", "Beta", "Beta, edited.");
    sh.site.refresh(&sh, &C).unwrap();
    let after = sh.site.refresh(&sh, &V).unwrap();
    assert!(
        String::from_utf8_lossy(&after.files["index.html"]).contains("2 docs changed"),
        "the menu counts the new doc"
    );
    assert_eq!(versions.rx.try_recv(), Ok(LiveEvent::Reload));
    assert!(
        empty(&mut home) && empty(&mut alpha),
        "canon's pages get nothing"
    );
    assert_eq!(
        sh.site.builds(),
        before + 1,
        "the versions page built; canon's inputs did not change"
    );
}

#[test]
fn sr8_a_view_commit_recounts_one_view() {
    let h = two_docs();
    let sh = shared(&h);
    edit(&sh, "editing", "Alpha", "Alpha, edited.");
    edit(&sh, "other", "Beta", "Beta, edited.");
    let counted: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let calls = Arc::new(AtomicUsize::new(0));
    {
        let (counted, calls) = (Arc::clone(&counted), Arc::clone(&calls));
        sh.site.set_docs_changed(Box::new(move |st, scope| {
            calls.fetch_add(1, Ordering::SeqCst);
            counted.lock().unwrap().push(format!("{scope:?}"));
            rdy_render::menu::docs_changed(st, scope)
        }));
    }
    sh.site.refresh(&sh, &V).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "each view counted once");
    counted.lock().unwrap().clear();
    edit(&sh, "editing", "Alpha", "Alpha, edited again.");
    sh.site.refresh(&sh, &V).unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "one view commit recounts one view: {:?}",
        counted.lock().unwrap()
    );
    assert_eq!(
        *counted.lock().unwrap(),
        [format!("{:?}", Scope::View("editing".into()))]
    );
    // a refresh with nothing new counts nothing
    sh.site.refresh(&sh, &V).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn sr8_the_versions_page_is_never_a_read_scope() {
    let h = two_docs();
    let sh = shared(&h);
    let e = rdy_engine::engine::site_reply(&*sh.engine(), Some("versions")).unwrap_err();
    assert_eq!(e.code, rdy_engine::E_NOT_FOUND, "{e:?}");
    assert_eq!(e.message, "no view \"versions\"", "{e:?}");
    assert!(resolve(&sh.state.load_full(), &V).is_err());
}
