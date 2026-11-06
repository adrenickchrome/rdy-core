//! Account identity on both sides of a connection, through the `sys` seam: on Windows, the peer
//! process of a named pipe and the full SID of its token; elsewhere, a socket's credentials. Each
//! side checks before a hello byte moves. The Windows crate does not build until every `sys` part
//! exists, so an isolated probe runs this file there until then.

use rdy_daemon::hello::{Hello, PROTO_VERSION, read_hello};
use rdy_daemon::session::{Proto, Sessions};
use rdy_daemon::sys::{self, Account};
use rdy_store::fileid::FileId;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A hung test fails here. It asserts no timing: every wait below ends on an event.
const HANG_LIMIT: Duration = Duration::from_secs(120);

async fn guarded<T>(test: impl Future<Output = T>) -> T {
    tokio::time::timeout(HANG_LIMIT, test)
        .await
        .expect("the test hung")
}

/// One endpoint name, unused by any other test or process.
struct Endpoint {
    path: PathBuf,
    /// Holds a Unix socket's directory.
    _dir: Option<tempfile::TempDir>,
}

static NEXT_ENDPOINT: AtomicUsize = AtomicUsize::new(0);

#[cfg(windows)]
fn endpoint() -> Endpoint {
    let n = NEXT_ENDPOINT.fetch_add(1, Ordering::SeqCst);
    Endpoint {
        path: PathBuf::from(format!(
            r"\\.\pipe\rdy-wx14-test-{}-{n}",
            std::process::id()
        )),
        _dir: None,
    }
}

#[cfg(not(windows))]
fn endpoint() -> Endpoint {
    NEXT_ENDPOINT.fetch_add(1, Ordering::SeqCst);
    // under /tmp, so the path fits `sun_path` on macOS
    let dir = tempfile::Builder::new()
        .prefix("rdy-wx14")
        .tempdir_in("/tmp")
        .unwrap();
    Endpoint {
        path: dir.path().join("s.sock"),
        _dir: Some(dir),
    }
}

/// An account this process does not run as: SYSTEM on Windows.
#[cfg(windows)]
fn another_account() -> Account {
    rdy_store::pio::Sid::parse(&[1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0]).unwrap()
}

/// An account this process does not run as.
#[cfg(not(windows))]
fn another_account() -> Account {
    sys::own_account().unwrap() + 1
}

/// What a client says of a daemon of the account `theirs`.
fn client_refusal(path: &Path, theirs: Account) -> String {
    #[cfg(windows)]
    {
        format!(
            "the daemon on {} belongs to another account ({theirs})",
            path.display()
        )
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        format!("the socket's peer is uid {theirs}, not ours")
    }
}

/// What a daemon logs of a client of the account `theirs`.
fn server_refusal(theirs: Account) -> String {
    #[cfg(windows)]
    {
        format!("refused a connection from account {theirs}")
    }
    #[cfg(not(windows))]
    {
        format!("refused a connection from uid {theirs}")
    }
}

fn hello_line() -> Vec<u8> {
    let hello = Hello {
        rdy: PROTO_VERSION,
        version: "t".into(),
        build: rdy_daemon::BUILD.into(),
        proto: Proto::Ctl,
        cwd: std::env::temp_dir(),
        project: FileId::lstat(&std::env::temp_dir()).unwrap(),
    };
    let mut line = serde_json::to_vec(&hello).unwrap();
    line.push(b'\n');
    line
}

/// What a client does on a new connection: the check, and only then the hello.
async fn check_then_hello(
    mut stream: sys::Stream,
    path: &Path,
    expected: Account,
) -> std::io::Result<sys::PeerProcess> {
    let process = sys::check_daemon(&stream, path, expected)?;
    stream.write_all(&hello_line()).await?;
    stream.flush().await?;
    Ok(process)
}

#[tokio::test]
async fn wx14_client_checks_server_sid_before_hello() {
    guarded(async {
        let e = endpoint();
        let mut listener = sys::bind(&e.path).unwrap();
        let me = sys::own_account().unwrap();

        // a spoof server: the name answers, and its account is not the one the client expects
        let client = sys::connect(&e.path).await.unwrap();
        let mut spoof = listener.accept().await.unwrap();
        let refused = check_then_hello(client, &e.path, another_account())
            .await
            .map(drop)
            .unwrap_err();
        assert_eq!(refused.to_string(), client_refusal(&e.path, me));
        // the client has left: it sent the spoof server nothing at all
        let mut heard = Vec::new();
        spoof.read_to_end(&mut heard).await.unwrap();
        assert!(heard.is_empty(), "the spoof server heard {heard:?}");

        // its own account's daemon gets the hello, and the pid is the system's
        let client = sys::connect(&e.path).await.unwrap();
        let mut server = listener.accept().await.unwrap();
        let process = check_then_hello(client, &e.path, me).await.unwrap();
        assert_eq!(process.pid(), Some(std::process::id()));
        assert_eq!(read_hello(&mut server).await.unwrap().proto, Proto::Ctl);
    })
    .await;
}

#[tokio::test]
async fn wx14_server_checks_client_sid_before_hello() {
    guarded(async {
        let e = endpoint();
        let mut listener = sys::bind(&e.path).unwrap();
        let me = sys::own_account().unwrap();

        // a client of the wrong account, whose hello is already on its way
        let mut client = sys::connect(&e.path).await.unwrap();
        client.write_all(&hello_line()).await.unwrap();
        client.flush().await.unwrap();
        let server = listener.accept().await.unwrap();
        assert_eq!(
            sys::check_client(&server, another_account()).map(drop),
            Err(server_refusal(me))
        );
        // the daemon closes it: no protocol reply reaches an unverified peer
        drop(server);
        let mut heard = Vec::new();
        client.read_to_end(&mut heard).await.unwrap();
        assert!(heard.is_empty(), "the refused client heard {heard:?}");

        // its own account: the check takes no byte of the hello that follows
        let mut client = sys::connect(&e.path).await.unwrap();
        client.write_all(&hello_line()).await.unwrap();
        client.flush().await.unwrap();
        let mut server = listener.accept().await.unwrap();
        let process = sys::check_client(&server, me).unwrap();
        assert_eq!(process.account(), me);
        assert_eq!(process.pid(), Some(std::process::id()));
        assert_eq!(read_hello(&mut server).await.unwrap().proto, Proto::Ctl);
    })
    .await;
}

#[test]
fn wx14_session_identity_matches_the_settled_schema() {
    let me = sys::own_account().unwrap();
    let session = Sessions::default().insert(
        Proto::Ctl,
        "t".into(),
        rdy_daemon::BUILD.into(),
        PathBuf::from("cwd"),
        me,
    );
    #[cfg(windows)]
    let (own_field, other_field, value) =
        ("peer_sid", "peer_uid", serde_json::json!(me.to_string()));
    #[cfg(not(windows))]
    let (own_field, other_field, value) = ("peer_uid", "peer_sid", serde_json::json!(me));
    let shown = serde_json::to_value(&session).unwrap();
    assert_eq!(shown[own_field], value);
    assert!(shown.get(other_field).is_none(), "{shown}");
    // the field stands where `peer_uid` always stood, and nothing else is new
    assert_eq!(
        serde_json::to_string(&session).unwrap(),
        format!(
            r#"{{"id":1,"proto":"ctl","version":"t","build":"{}","cwd":"cwd","{own_field}":{value},"connected_at_ms":{}}}"#,
            rdy_daemon::BUILD,
            session.connected_at_ms
        )
    );
}

/// The standard string form, whole: a SID of the largest size is never cut, and two SIDs that
/// differ only in their last sub-authority are two accounts.
#[cfg(windows)]
#[test]
fn wx14_native_a_sid_is_shown_and_compared_whole() {
    use rdy_store::pio::Sid;
    let sid = |last: u8| {
        let mut bytes = vec![1, 15, 0, 0, 0, 0, 0, 5];
        for n in 0..14u8 {
            bytes.extend([21 + n, 0, 0, 0]);
        }
        bytes.extend([last, 0, 0, 0]);
        Sid::parse(&bytes).unwrap()
    };
    let session = Sessions::default().insert(
        Proto::Ctl,
        "t".into(),
        rdy_daemon::BUILD.into(),
        PathBuf::from("cwd"),
        sid(1),
    );
    assert_eq!(
        serde_json::to_value(&session).unwrap()["peer_sid"],
        "S-1-5-21-22-23-24-25-26-27-28-29-30-31-32-33-34-1"
    );
    assert_ne!(sid(1), sid(2));
}

#[cfg(windows)]
mod peer_process {
    use super::*;
    use rdy_store::pio::Sid;
    use std::cell::RefCell;
    use std::io;
    use sys::{ProcessQuery, check_client_with, check_daemon_with, read_peer};
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE};

    const DENIED: i32 = ERROR_ACCESS_DENIED as i32;
    const GONE: i32 = ERROR_BROKEN_PIPE as i32;
    /// An OS error no case below expects by accident.
    const SOME_ERROR: i32 = 1234;

    fn os(code: i32) -> io::Error {
        io::Error::from_raw_os_error(code)
    }

    /// One peer as the system would describe it. Every answer can be an error.
    #[derive(Clone)]
    struct FakePeer {
        pid: Result<u32, i32>,
        open: Result<(), i32>,
        created: Result<u64, i32>,
        account: Result<Sid, i32>,
        exited: Result<bool, i32>,
        connected: Result<bool, i32>,
    }

    fn live_peer() -> FakePeer {
        FakePeer {
            pid: Ok(4242),
            open: Ok(()),
            created: Ok(1_000),
            account: Ok(sys::own_account().unwrap()),
            exited: Ok(false),
            connected: Ok(true),
        }
    }

    struct FakeQuery {
        peer: FakePeer,
        calls: RefCell<Vec<&'static str>>,
    }

    impl FakeQuery {
        fn new(peer: FakePeer) -> FakeQuery {
            FakeQuery {
                peer,
                calls: RefCell::new(Vec::new()),
            }
        }

        fn answer<T: Clone>(&self, call: &'static str, answer: &Result<T, i32>) -> io::Result<T> {
            self.calls.borrow_mut().push(call);
            answer.clone().map_err(os)
        }
    }

    impl ProcessQuery for FakeQuery {
        type Process = u32;

        fn peer_pid(&self) -> io::Result<u32> {
            self.answer("peer_pid", &self.peer.pid)
        }

        fn open(&self, pid: u32) -> io::Result<u32> {
            self.answer("open", &self.peer.open).map(|()| pid)
        }

        fn created(&self, _process: &u32) -> io::Result<u64> {
            self.answer("created", &self.peer.created)
        }

        fn account(&self, _process: &u32) -> io::Result<Sid> {
            self.answer("account", &self.peer.account)
        }

        fn has_exited(&self, _process: &u32) -> io::Result<bool> {
            self.answer("has_exited", &self.peer.exited)
        }

        fn connected(&self) -> io::Result<bool> {
            self.answer("connected", &self.peer.connected)
        }
    }

    /// Each answer that fails, or that shows the peer gone, and the OS error of the refusal.
    fn refused_peers() -> Vec<(&'static str, FakePeer, i32)> {
        let live = live_peer();
        vec![
            (
                "the pipe does not say",
                FakePeer {
                    pid: Err(SOME_ERROR),
                    ..live.clone()
                },
                SOME_ERROR,
            ),
            (
                "the process may not be opened",
                FakePeer {
                    open: Err(DENIED),
                    ..live.clone()
                },
                DENIED,
            ),
            (
                "its creation time cannot be read",
                FakePeer {
                    created: Err(SOME_ERROR),
                    ..live.clone()
                },
                SOME_ERROR,
            ),
            (
                "its token cannot be read",
                FakePeer {
                    account: Err(DENIED),
                    ..live.clone()
                },
                DENIED,
            ),
            (
                "its state cannot be read",
                FakePeer {
                    exited: Err(SOME_ERROR),
                    ..live.clone()
                },
                SOME_ERROR,
            ),
            (
                "the pipe's state cannot be read",
                FakePeer {
                    connected: Err(SOME_ERROR),
                    ..live.clone()
                },
                SOME_ERROR,
            ),
            (
                "it has exited",
                FakePeer {
                    exited: Ok(true),
                    ..live.clone()
                },
                GONE,
            ),
            (
                // the pid was reused: the process opened is another one, and the real peer is gone
                "the pipe's peer left before the process was held",
                FakePeer {
                    connected: Ok(false),
                    ..live
                },
                GONE,
            ),
        ]
    }

    #[test]
    fn wx14_unreadable_or_changed_peer_process_is_refused() {
        let me = sys::own_account().unwrap();
        let pipe = Path::new(r"\\.\pipe\rdy-x");
        for (case, peer, code) in refused_peers() {
            let error = os(code);
            // the client's words and the daemon's words, and no fallback to any other identity
            let refused = check_daemon_with(&FakeQuery::new(peer.clone()), pipe, me)
                .map(drop)
                .unwrap_err();
            assert_eq!(
                refused.to_string(),
                format!("could not read the daemon's account SID: {error}"),
                "{case}"
            );
            assert_eq!(refused.kind(), error.kind(), "{case}");
            assert_eq!(
                check_client_with(&FakeQuery::new(peer), me).map(drop),
                Err(format!(
                    "could not read a connection's account SID: {error}"
                )),
                "{case}"
            );
        }

        // a reused pid held by another account is that account, and it is refused by its SID
        let other = another_account();
        let reused = FakePeer {
            account: Ok(other),
            ..live_peer()
        };
        assert_eq!(
            check_daemon_with(&FakeQuery::new(reused.clone()), pipe, me)
                .map(drop)
                .unwrap_err()
                .to_string(),
            format!(
                "the daemon on {} belongs to another account ({other})",
                pipe.display()
            )
        );
        assert_eq!(
            check_client_with(&FakeQuery::new(reused), me).map(drop),
            Err(format!("refused a connection from account {other}"))
        );
    }

    #[test]
    fn wx14_native_the_process_is_held_before_the_pipe_proves_it() {
        let query = FakeQuery::new(live_peer());
        let peer = read_peer(&query).unwrap();
        assert_eq!(peer.pid(), Some(4242));
        assert_eq!(peer.created(), 1_000);
        assert_eq!(peer.account(), sys::own_account().unwrap());
        // the pipe's word comes after the handle pins the pid, or a reuse could pass unseen
        assert_eq!(
            *query.calls.borrow(),
            [
                "peer_pid",
                "open",
                "created",
                "account",
                "has_exited",
                "connected"
            ]
        );
    }

    #[test]
    fn wx14_native_a_reused_pid_is_another_process() {
        let first = read_peer(&FakeQuery::new(live_peer())).unwrap();
        assert!(first.is_process(4242, 1_000));
        // the same pid, started later
        let reused = FakePeer {
            created: Ok(2_000),
            ..live_peer()
        };
        let second = read_peer(&FakeQuery::new(reused)).unwrap();
        assert!(!second.is_process(4242, 1_000));
        assert!(!first.is_process(4243, 1_000));
    }

    #[tokio::test]
    async fn wx14_native_a_real_peer_is_read_from_the_system() {
        guarded(async {
            let e = endpoint();
            let mut listener = sys::bind(&e.path).unwrap();
            let me = sys::own_account().unwrap();
            let client = sys::connect(&e.path).await.unwrap();

            // no accept has run yet: a client checks as soon as its connect returns
            let from_client = sys::check_daemon(&client, &e.path, me).unwrap();
            let server = listener.accept().await.unwrap();
            assert_eq!(from_client.pid(), Some(std::process::id()));
            assert_eq!(from_client.account(), me);
            assert!(from_client.created() > 0);
            // this process, from either end: one pid, and one start
            let from_server = sys::check_client(&server, me).unwrap();
            assert_eq!(from_server.account(), me);
            assert!(from_server.is_process(std::process::id(), from_client.created()));
        })
        .await;
    }

    /// A peer that has left is refused, not read from whatever now has its pid. Its handle closes
    /// on the runtime's next turn, so this waits on the refusal itself.
    #[tokio::test]
    async fn wx14_native_a_peer_that_left_is_refused_on_both_sides() {
        guarded(async {
            let e = endpoint();
            let mut listener = sys::bind(&e.path).unwrap();
            let me = sys::own_account().unwrap();
            let gone = os(GONE);

            let client = sys::connect(&e.path).await.unwrap();
            let server = listener.accept().await.unwrap();
            drop(client);
            let refusal = loop {
                match sys::check_client(&server, me) {
                    Ok(_) => tokio::task::yield_now().await,
                    Err(words) => break words,
                }
            };
            assert_eq!(
                refusal,
                format!("could not read a connection's account SID: {gone}")
            );

            let client = sys::connect(&e.path).await.unwrap();
            let server = listener.accept().await.unwrap();
            drop(server);
            let refusal = loop {
                match sys::check_daemon(&client, &e.path, me) {
                    Ok(_) => tokio::task::yield_now().await,
                    Err(e) => break e.to_string(),
                }
            };
            assert_eq!(
                refusal,
                format!("could not read the daemon's account SID: {gone}")
            );
        })
        .await;
    }
}
