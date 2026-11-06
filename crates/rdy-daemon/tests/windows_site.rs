//! The site's listeners and links on every platform (FW-XP1, Windows plan task 19): the full,
//! LAN and profile listeners are exclusive, site links are slash-based, every spelling of one
//! project gives one port, and the Host, Origin and LAN guards do not move.

use rdy_daemon::site::addr;
use rdy_daemon::site::guard::{Guard, Refusal};
use rdy_daemon::site::http::parse_head;
use rdy_daemon::site::{LanOverride, SiteServer};
use rdy_model::Hash;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use tokio::net::TcpSocket;

fn project() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(".rdy")).unwrap();
    d
}

/// A second bind on `at`, with and without reuse options: neither may take the address.
fn assert_second_bind_refused(at: SocketAddr) {
    for reuse in [false, true] {
        let socket = match at {
            SocketAddr::V4(_) => TcpSocket::new_v4().unwrap(),
            SocketAddr::V6(_) => TcpSocket::new_v6().unwrap(),
        };
        if reuse {
            socket.set_reuseaddr(true).unwrap();
        }
        let bound = socket.bind(at).and_then(|()| socket.listen(8).map(drop));
        assert!(
            bound.is_err(),
            "{at}: a second socket (reuseaddr {reuse}) must not take a live site address"
        );
    }
}

#[tokio::test]
async fn wx19_live_and_profile_listeners_are_exclusive() {
    let d = project();
    let root = d.path().canonicalize().unwrap();
    // 127.0.0.1 stands in as the LAN address: it is private to the test and always present
    let (server, info) =
        SiteServer::bind(&root, Some(LanOverride::Addr(Ipv4Addr::new(127, 0, 0, 1)))).await;
    let _server = server.expect("the site listens");
    assert!(info.addrs.contains(&SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        info.addrs[0].port()
    )));
    for at in &info.addrs {
        assert_second_bind_refused(*at);
    }
    // a second site of the same project meets the held port and takes the next candidate
    let (second, info2) = SiteServer::bind(&root, None).await;
    let _second = second.expect("the next port is free");
    assert_ne!(info2.addrs[0].port(), info.addrs[0].port());
    for at in &info2.addrs {
        assert_second_bind_refused(*at);
    }
    // a pre-bound port is skipped, never shared
    let other = project();
    let other_root = other.path().canonicalize().unwrap();
    let first = addr::candidates(None, &other_root)[0];
    let held = TcpSocket::new_v4().unwrap();
    if held
        .bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), first))
        .is_ok()
    {
        let _held = held.listen(8).unwrap();
        let (third, info3) = SiteServer::bind(&other_root, None).await;
        let _third = third.expect("a later candidate is free");
        assert_ne!(info3.addrs[0].port(), first);
    }
}

#[tokio::test]
async fn wx19_site_urls_never_contain_native_separators() {
    let d = project();
    let nested = d.path().join("a b").join("c");
    std::fs::create_dir_all(nested.join(".rdy")).unwrap();
    let root = nested.canonicalize().unwrap();
    let (server, info) = SiteServer::bind(&root, None).await;
    let _server = server;
    let url = info.url.clone().unwrap();
    let port = info.addrs[0].port();
    assert_eq!(url, format!("http://127.0.0.1:{port}/"));
    assert_eq!(addr::url("192.168.1.2", 50000), "http://192.168.1.2:50000/");
    for u in [url, addr::url("::1", 1)] {
        assert!(!u.contains('\\'), "{u}");
        assert!(!u.contains("%5C") && !u.contains("%5c"), "{u}");
    }
    for line in rdy_daemon::site::log_lines(&info) {
        assert!(!line.contains('\\'), "{line}");
    }
}

#[test]
fn wx19_canonical_aliases_choose_the_same_port() {
    let d = project();
    let real = d.path().join("real");
    std::fs::create_dir_all(real.join(".rdy")).unwrap();
    let dotted = real.join(".");
    let parent = real.join("..").join("real");
    for other in [&dotted, &parent] {
        assert_eq!(addr::derived_port(&real), addr::derived_port(other));
        assert_eq!(addr::candidates(None, &real), addr::candidates(None, other));
        assert_eq!(
            addr::profile_candidates(&real, "docs"),
            addr::profile_candidates(other, "docs")
        );
    }
    #[cfg(unix)]
    {
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(addr::derived_port(&real), addr::derived_port(&link));
    }
    #[cfg(windows)]
    {
        // a verbatim spelling and a differently-cased drive letter name one directory
        let canonical = real.canonicalize().unwrap();
        assert_eq!(addr::derived_port(&real), addr::derived_port(&canonical));
    }
    // the hash input: Unix keeps the path's own bytes, Windows its UTF-16LE units
    let probe = Path::new(if cfg!(windows) {
        r"C:\fixed\project"
    } else {
        "/fixed/project"
    });
    let bytes = rdy_daemon::sys::path_hash_bytes(probe);
    if cfg!(windows) {
        let want: Vec<u8> = r"C:\fixed\project"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(&*bytes, want.as_slice());
    } else {
        assert_eq!(&*bytes, b"/fixed/project");
        let h = Hash::of(&bytes).0;
        assert_eq!(
            PORT_OF_FIXED,
            addr::PORT_LOW + u16::from_le_bytes([h[0], h[1]]) % 16384
        );
    }
}

/// The source port of the Unix path `/fixed/project`, pinned before the seam moved.
const PORT_OF_FIXED: u16 = 50417;

fn request(target: &str, headers: &[(&str, &str)]) -> rdy_daemon::site::http::Request {
    let mut head = format!("GET {target} HTTP/1.1\r\n");
    for (n, v) in headers {
        head.push_str(&format!("{n}: {v}\r\n"));
    }
    head.push_str("\r\n");
    parse_head(head.as_bytes()).unwrap()
}

#[test]
fn wx19_host_origin_and_lan_guards_are_unchanged() {
    let lan = Ipv4Addr::new(192, 168, 1, 20);
    let g = Guard::full(51000, Some(lan));
    assert_eq!(
        g.hosts(),
        vec![
            "127.0.0.1:51000".to_string(),
            "localhost:51000".to_string(),
            "[::1]:51000".to_string(),
            "192.168.1.20:51000".to_string(),
        ]
    );
    assert!(
        g.check(&request("/", &[("Host", "127.0.0.1:51000")]))
            .is_ok()
    );
    assert!(
        g.check(&request("/", &[("Host", "192.168.1.20:51000")]))
            .is_ok()
    );
    for host in [
        "evil.example:51000",
        "127.0.0.1:51001",
        "192.168.1.21:51000",
    ] {
        assert_eq!(
            g.check(&request("/", &[("Host", host)])),
            Err(Refusal::Host),
            "{host}"
        );
    }
    assert_eq!(
        g.check(&request(
            "/",
            &[
                ("Host", "127.0.0.1:51000"),
                ("Origin", "http://evil.example")
            ]
        )),
        Err(Refusal::Origin)
    );
    assert!(
        g.check(&request(
            "/",
            &[
                ("Host", "127.0.0.1:51000"),
                ("Origin", "http://127.0.0.1:51000")
            ]
        ))
        .is_ok()
    );
    // a backslash or an encoded one never routes: logical paths are slash-based
    for target in ["/a%5Cb/", "/a\\b/", "/..%5C..%5Cx"] {
        assert!(
            g.check(&request(target, &[("Host", "127.0.0.1:51000")]))
                .is_err(),
            "{target}"
        );
    }
    // a LAN-less guard refuses the LAN host
    let local = Guard::full(51000, None);
    assert_eq!(
        local.check(&request("/", &[("Host", "192.168.1.20:51000")])),
        Err(Refusal::Host)
    );
}
