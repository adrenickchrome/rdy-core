//! The site's listening-socket option (D-FPKPS): one call between a socket's creation and its
//! bind, for every listener of the site (full, LAN and profile-only).
//!
//! On Unix it is `SO_REUSEADDR`, so a successor daemon binds the port at once. On Windows it is
//! `SO_EXCLUSIVEADDRUSE`, so no other socket can bind the same address while the site holds it;
//! `SO_REUSEADDR` is never set there, because on Windows it lets a second socket take the port.
//!
//! This file builds on its own too: the isolated Windows probe includes it as it is, so its
//! tests run on Windows before the whole daemon builds there (FW-XP1 Phase 4).

use std::io;
use tokio::net::TcpSocket;

/// Sets the site's option on `socket`, before it binds: `SO_REUSEADDR`.
#[cfg(not(windows))]
pub(crate) fn configure_site_socket(socket: &TcpSocket) -> io::Result<()> {
    socket.set_reuseaddr(true)
}

/// Sets the site's option on `socket`, before it binds: `SO_EXCLUSIVEADDRUSE` = 1 at level
/// `SOL_SOCKET`, never `SO_REUSEADDR`. A failed call is the `WSAGetLastError` value.
#[cfg(windows)]
pub(crate) fn configure_site_socket(socket: &TcpSocket) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        SO_EXCLUSIVEADDRUSE, SOCKET, SOCKET_ERROR, SOL_SOCKET, WSAGetLastError, setsockopt,
    };
    let on: i32 = 1;
    #[allow(unsafe_code)]
    // SAFETY: the socket handle is open for the whole call (`socket` is borrowed and owns it),
    // and the option value points at a live i32 whose size is passed as its length, as
    // setsockopt requires for SO_EXCLUSIVEADDRUSE. WSAGetLastError reads this thread's last
    // WinSock error, right after the failed call on the same thread.
    unsafe {
        let rc = setsockopt(
            socket.as_raw_socket() as SOCKET,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            (&on as *const i32).cast::<u8>(),
            std::mem::size_of::<i32>() as i32,
        );
        if rc == SOCKET_ERROR {
            return Err(io::Error::from_raw_os_error(WSAGetLastError()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

    /// A configured socket for `at`'s family.
    fn configured(at: SocketAddr) -> TcpSocket {
        let socket = match at {
            SocketAddr::V4(_) => TcpSocket::new_v4().unwrap(),
            SocketAddr::V6(_) => TcpSocket::new_v6().unwrap(),
        };
        configure_site_socket(&socket).unwrap();
        socket
    }

    /// A first listener on an ephemeral port of `ip`, then a second configured socket on the
    /// same address: the first binds, the second does not. No wait: the first listens before
    /// the second binds.
    async fn second_bind_fails(ip: std::net::IpAddr) {
        let first = configured(SocketAddr::new(ip, 0));
        first.bind(SocketAddr::new(ip, 0)).unwrap();
        let at = first.local_addr().unwrap();
        let _listener = first.listen(64).unwrap();
        let second = configured(at);
        assert!(
            second.bind(at).is_err(),
            "a second socket must not bind {at} while the site listens there"
        );
    }

    /// Unix keeps the current option: `SO_REUSEADDR`, set before the bind; a second listener on
    /// the same address still fails.
    #[cfg(unix)]
    #[tokio::test]
    async fn ui4_unix_site_socket_sets_reuseaddr() {
        for ip in [
            std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
            std::net::IpAddr::V6(Ipv6Addr::LOCALHOST),
        ] {
            let socket = configured(SocketAddr::new(ip, 0));
            assert!(socket.reuseaddr().unwrap(), "{ip}");
            second_bind_fails(ip).await;
        }
    }

    /// The value of `SO_EXCLUSIVEADDRUSE` on `socket`.
    #[cfg(windows)]
    fn exclusive(socket: &TcpSocket) -> i32 {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::{
            SO_EXCLUSIVEADDRUSE, SOCKET, SOCKET_ERROR, SOL_SOCKET, getsockopt,
        };
        let mut value: i32 = 0;
        let mut len = std::mem::size_of::<i32>() as i32;
        #[allow(unsafe_code)]
        // SAFETY: the socket handle is open for the call (`socket` is borrowed), `value` is a
        // writable i32 and `len` holds its size, as getsockopt requires.
        let rc = unsafe {
            getsockopt(
                socket.as_raw_socket() as SOCKET,
                SOL_SOCKET,
                SO_EXCLUSIVEADDRUSE,
                (&mut value as *mut i32).cast::<u8>(),
                &mut len,
            )
        };
        assert_ne!(rc, SOCKET_ERROR, "getsockopt SO_EXCLUSIVEADDRUSE");
        value
    }

    /// D-FPKPS on Windows, run by the isolated probe: the production helper sets
    /// `SO_EXCLUSIVEADDRUSE` and never `SO_REUSEADDR`; the first bind succeeds and a second
    /// same-address bind fails, for IPv4 and IPv6.
    #[cfg(windows)]
    #[tokio::test]
    async fn ui4_windows_exclusive_bind_refuses_a_second_listener() {
        for ip in [
            std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
            std::net::IpAddr::V6(Ipv6Addr::LOCALHOST),
        ] {
            let socket = configured(SocketAddr::new(ip, 0));
            assert_eq!(exclusive(&socket), 1, "{ip}: SO_EXCLUSIVEADDRUSE");
            assert!(!socket.reuseaddr().unwrap(), "{ip}: no SO_REUSEADDR");
            second_bind_fails(ip).await;
        }
    }
}
