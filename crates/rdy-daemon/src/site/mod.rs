//! The live site: the read-only HTTP side of a daemon, serving the project's pages.

pub mod addr;
pub mod guard;
pub mod http;
pub mod live;
pub mod profiles;
pub mod server;
pub mod sse;

pub use profiles::ensure_profile_site;
pub use server::SiteServer;

use rdy_engine::engine::SiteWarning;
use std::net::{Ipv4Addr, SocketAddr};

/// Where the site listens, as the daemon's start found it (`Shared::site_info`, set once before
/// `ready`).
#[derive(Clone, Debug)]
pub struct SiteInfo {
    /// The loopback address, `http://127.0.0.1:<port>/`, or why no loopback port could be bound
    /// (the daemon then runs on without a site).
    pub url: Result<String, String>,
    /// The LAN address, while `serve.site_lan` is on and its address is bound.
    pub lan_url: Option<String>,
    /// The LAN warning: anyone on the network can read the site, or why it stays on this
    /// machine although `serve.site_lan` is on; with its fix.
    pub warning: Option<SiteWarning>,
    /// Every address the site is bound to.
    pub addrs: Vec<SocketAddr>,
}

/// Test hook: stands in for `addr::lan_address()`, the private address of the default route.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanOverride {
    Addr(Ipv4Addr),
    NoneFound,
}

/// The `site:` lines the daemon logs at its start, from `info`.
pub fn log_lines(info: &SiteInfo) -> Vec<String> {
    let mut lines = vec![match &info.url {
        Ok(url) => format!("site: {url}"),
        Err(reason) => format!(
            "site: could not listen on 127.0.0.1 ({reason}); the site is off until the daemon restarts"
        ),
    }];
    if let Some(w) = &info.warning {
        lines.push(match info.lan_url {
            Some(_) => format!("site: warning: {}; fix: {}", w.message, w.fix),
            None => format!("site: {}; fix: {}", w.message, w.fix),
        });
    }
    lines
}
