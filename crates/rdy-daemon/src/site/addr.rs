//! Where the live site listens: the port derived from the project path, the candidates a busy
//! port falls back to, and the private LAN address of the default route.

use crate::layout::RunDir;
use rdy_model::Hash;
use std::net::{Ipv4Addr, UdpSocket};
use std::path::Path;

/// The first port of the range a derived port falls in (the dynamic range, 49152 to 65535).
pub const PORT_LOW: u16 = 49152;
/// How many ports a start tries: the derived port and the next 15.
pub const TRIES: u16 = 16;
/// How many ports the range holds.
const SPAN: u16 = 16384;

/// The port for project `root`: a hash of the canonical root that the short socket hashes
/// (D-YMWEM), so every spelling of one project gives one port, as it gives one socket.
pub fn derived_port(root: &Path) -> u16 {
    let canonical = RunDir::of_project(root).root();
    let h = Hash::of(crate::sys::path_bytes(&canonical)).0;
    PORT_LOW + u16::from_le_bytes([h[0], h[1]]) % SPAN
}

/// The ports to try in order: the setting alone, or the derived port and the next 15, wrapping
/// inside 49152 to 65535.
pub fn candidates(setting: Option<u16>, root: &Path) -> Vec<u16> {
    if let Some(port) = setting {
        return vec![port];
    }
    window(derived_port(root) - PORT_LOW)
}

/// The ports to try for the listener of profile `canonical_key` (a folded key, or
/// `implicit:org`) of project `root`, in order (D-DK03K). The hash input is
/// `rdy-site-profile-v1\0`, the byte length of the canonical root as a little-endian u64, the
/// root's bytes, the byte length of the key as a little-endian u64, then the key's bytes. The
/// first port is `49152 + le_u16(hash[0..2]) % 16384`, then 15 more, wrapping. `serve.site_port`
/// pins the source listener only: it has no say here.
pub fn profile_candidates(root: &Path, canonical_key: &str) -> Vec<u16> {
    let canonical = RunDir::of_project(root).root();
    let root_bytes = crate::sys::path_bytes(&canonical);
    let mut input =
        Vec::with_capacity(PROFILE_DOMAIN.len() + 16 + root_bytes.len() + canonical_key.len());
    input.extend_from_slice(PROFILE_DOMAIN);
    input.extend_from_slice(&(root_bytes.len() as u64).to_le_bytes());
    input.extend_from_slice(root_bytes);
    input.extend_from_slice(&(canonical_key.len() as u64).to_le_bytes());
    input.extend_from_slice(canonical_key.as_bytes());
    let h = Hash::of(&input).0;
    window(u16::from_le_bytes([h[0], h[1]]) % SPAN)
}

/// The domain of the profile hash: it keeps a profile's ports apart from the source's.
const PROFILE_DOMAIN: &[u8] = b"rdy-site-profile-v1\0";

/// [`TRIES`] ports from offset `start` of the range, wrapping inside it.
fn window(start: u16) -> Vec<u16> {
    (0..TRIES).map(|i| PORT_LOW + (start + i) % SPAN).collect()
}

/// Whether `ip` is in `10.0.0.0/8`, `172.16.0.0/12` or `192.168.0.0/16`. The config check in
/// rdy-engine uses the same std method, since rdy-engine cannot depend on this crate.
pub fn is_private(ip: Ipv4Addr) -> bool {
    ip.is_private()
}

/// The private IPv4 address of the default route, else `None`. A UDP connect sends no packet;
/// `192.0.2.0/24` is the documentation range. Any error gives `None`.
pub fn lan_address() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if is_private(ip) => Some(ip),
        _ => None,
    }
}

/// The site's address for `host` and `port`.
pub fn url(host: &str, port: u16) -> String {
    format!("http://{host}:{port}/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::path::Path;

    #[test]
    fn ls4_candidates_stay_in_the_dynamic_range() {
        let root = Path::new("/some/project");
        let c = candidates(None, root);
        assert_eq!(c.len(), 16);
        assert_eq!(c[0], derived_port(root));
        assert!(c.iter().all(|p| *p >= PORT_LOW));
        assert_eq!(candidates(None, root), c, "stable for one path");
        assert_ne!(derived_port(Path::new("/other/project")), c[0]);
        assert_eq!(candidates(Some(4567), root), vec![4567]);
    }

    #[test]
    fn ls4_candidates_wrap_at_the_top() {
        for i in 0..2000u32 {
            let p = format!("/p/{i}");
            let c = candidates(None, Path::new(&p));
            for w in c.windows(2) {
                assert!(
                    w[0].checked_add(1) == Some(w[1]) || (w[0] == 65535 && w[1] == PORT_LOW),
                    "{c:?}"
                );
            }
        }
    }

    #[test]
    fn ls4_only_private_addresses_count_as_lan() {
        for ip in [
            [10, 0, 0, 5],
            [172, 16, 0, 1],
            [172, 31, 255, 254],
            [192, 168, 1, 20],
        ] {
            assert!(is_private(Ipv4Addr::from(ip)), "{ip:?}");
        }
        for ip in [
            [172, 32, 0, 1],
            [8, 8, 8, 8],
            [127, 0, 0, 1],
            [169, 254, 1, 1],
            [100, 64, 0, 1],
            [0, 0, 0, 0],
        ] {
            assert!(!is_private(Ipv4Addr::from(ip)), "{ip:?}");
        }
        assert_eq!(url("127.0.0.1", 51000), "http://127.0.0.1:51000/");
        // guard: never panics, whatever the machine's network
        let _ = lan_address();
    }

    #[test]
    fn ls4_two_spellings_give_one_port() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            derived_port(&real),
            derived_port(&link),
            "one canonical root, one port"
        );
    }

    /// The profile ports, built by hand from the recipe: no helper of the module is used.
    fn recipe(root_bytes: &[u8], key: &str) -> Vec<u16> {
        let mut input = b"rdy-site-profile-v1\0".to_vec();
        input.extend_from_slice(&(root_bytes.len() as u64).to_le_bytes());
        input.extend_from_slice(root_bytes);
        input.extend_from_slice(&(key.len() as u64).to_le_bytes());
        input.extend_from_slice(key.as_bytes());
        let h = Hash::of(&input).0;
        let start = u32::from(u16::from_le_bytes([h[0], h[1]])) % 16384;
        (0..16u32)
            .map(|i| (49152 + (start + i) % 16384) as u16)
            .collect()
    }

    #[test]
    fn spv3_profile_ports_match_the_domain_separated_recipe() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real");
        std::fs::create_dir(&real).unwrap();
        // a second spelling of the same directory, with no OS-specific call
        let link = real.join(".");
        let canonical = RunDir::of_project(&real).root();
        let bytes = crate::sys::path_bytes(&canonical).to_vec();
        for key in ["docs", "a.b-c_d", "implicit:org", "x"] {
            let want = recipe(&bytes, key);
            assert_eq!(want.len(), 16);
            assert_eq!(profile_candidates(&real, key), want, "{key}");
            assert_eq!(
                profile_candidates(&link, key),
                want,
                "{key}: a second spelling of the root"
            );
        }
        // the key and the root both feed the hash, and the domain keeps it apart from the
        // source's. Fixed inputs with known first ports: the hash is cut to 16384 offsets, so two
        // arbitrary inputs may share a window, and a random root could fail a correct build
        let source = |root: &[u8]| {
            let h = Hash::of(root).0;
            49152 + u16::from_le_bytes([h[0], h[1]]) % 16384
        };
        assert_eq!(recipe(b"/fixed/project", "docs")[0], 60127);
        assert_eq!(recipe(b"/fixed/project", "doc")[0], 58185);
        assert_eq!(recipe(b"/fixed/other", "docs")[0], 50814);
        assert_eq!(source(b"/fixed/project"), 50417);
        // equivalent folded keys: the caller folds, and one folded key gives one list
        assert_eq!(
            profile_candidates(&real, &rdy_model::key::fold("Docs")),
            profile_candidates(&real, "docs")
        );
        // the length prefixes separate the two fields: moving a byte between them changes the hash
        assert_eq!(recipe(b"ab", "c")[0], 50632);
        assert_eq!(recipe(b"a", "bc")[0], 49401);
    }

    #[test]
    fn spv3_profile_ports_wrap_and_ignore_source_pin() {
        // the shared window wraps inside the range, from the last port
        let w = window(SPAN - 1);
        assert_eq!(w.len(), 16);
        assert_eq!(w[0], 65535);
        assert_eq!(w[1], PORT_LOW);
        assert_eq!(w[15], PORT_LOW + 14);
        assert_eq!(window(0)[15], PORT_LOW + 15);
        let root = Path::new("/some/project");
        for i in 0..500u32 {
            let key = format!("k{i}");
            let c = profile_candidates(root, &key);
            assert_eq!(c.len(), 16, "{key}");
            assert!(c.iter().all(|p| *p >= PORT_LOW), "{c:?}");
            for pair in c.windows(2) {
                assert!(
                    pair[0].checked_add(1) == Some(pair[1])
                        || (pair[0] == 65535 && pair[1] == PORT_LOW),
                    "{c:?}"
                );
            }
        }
        // `serve.site_port` pins the source listener only
        assert_eq!(candidates(Some(4567), root), vec![4567]);
        assert_eq!(
            profile_candidates(root, "docs"),
            profile_candidates(root, "docs")
        );
        assert_eq!(profile_candidates(root, "docs").len(), 16);
        // the source's own list is the window at the derived port
        assert_eq!(
            candidates(None, root),
            window(derived_port(root) - PORT_LOW)
        );
    }
}
