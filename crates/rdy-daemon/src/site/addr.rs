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
    let start = derived_port(root) - PORT_LOW;
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
}
