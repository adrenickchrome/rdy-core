//! sr2: the live site's tag route (D-FV6G9, D-BHR1Z F5): a tag serves its frozen site, an
//! unknown tag is not found, and the versions page is served.
use rdy_daemon::site::SiteInfo;
use rdy_qa::daemon::Hosted;
use rdy_qa::golden;
use rdy_qa::http;
use std::sync::Arc;

/// A host over the golden project (tagged `v1.0.0`, with landings and a view after it).
fn host() -> Hosted {
    let (dir, r, _ids) = golden::project();
    drop(r);
    Hosted::start_in(dir)
}

/// The site's loopback URL, `http://127.0.0.1:<port>/`.
fn url(h: &Hosted) -> String {
    let info: SiteInfo = Arc::clone(h.run.as_ref().unwrap().shared())
        .site_info
        .get()
        .cloned()
        .expect("the site info is set before ready");
    info.url.expect("the site listens")
}

#[test]
fn sr2_a_tag_page_is_served_frozen() {
    let h = host();
    let url = url(&h);
    let r = http::get(&format!("{url}~tag/v1.0.0/index.html"));
    assert_eq!(r.status, 200);
    let body = String::from_utf8(r.body).unwrap();
    assert!(body.contains("snapshot"), "{body}");
    let dir = http::get(&format!("{url}~tag/v1.0.0/"));
    assert_eq!(dir.status, 200);
}

#[test]
fn sr2_an_unknown_tag_is_not_found() {
    let h = host();
    let url = url(&h);
    for t in ["v9.9.9", "nosuch", "v1.0.0x"] {
        let r = http::get(&format!("{url}~tag/{t}/index.html"));
        assert_eq!(r.status, 404, "{t}");
    }
}

/// Task 8 builds the page (F5): both of its paths serve it.
#[test]
fn sr8_the_versions_page_is_served() {
    let h = host();
    let url = url(&h);
    assert_eq!(http::get(&format!("{url}~versions/")).status, 200);
    assert_eq!(http::get(&format!("{url}~versions/index.html")).status, 200);
}
