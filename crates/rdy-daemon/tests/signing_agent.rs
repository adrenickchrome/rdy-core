#![cfg(unix)]
// Unix only: a fake SSH agent listens on a Unix socket. FW-XP1 Phase 4 ports it.
//! The caller's SSH agent over its socket (sig3): bounded, complete packets, and one agent per
//! caller. No test reads or sets `SSH_AUTH_SOCK`: each agent is injected by its socket path.

use rdy_daemon::signing_agent::CallerAgent;
use rdy_engine::signing_crypto::{SigningAgent, resolve_key, sign_sshsig};
use rdy_engine::{E_NO_AGENT, RdyError};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Barrier};
use std::thread;

/// What the fake agent writes for one request (the request's message type and body): the raw
/// reply bytes, length prefix included, so a test can send a malformed frame.
type Script = Arc<dyn Fn(u8, &[u8]) -> Vec<u8> + Send + Sync>;

/// A fake agent on its own socket: it answers each connection's one request with `script`,
/// reports each request on the returned channel, then closes the connection.
struct Fake {
    path: PathBuf,
    requests: Receiver<Vec<u8>>,
    _dir: tempfile::TempDir,
}

fn fake(script: Script) -> Fake {
    let (tx, rx) = channel();
    fake_on(script, tx, rx)
}

/// [`fake`] reporting on `tx`, whose receiver `rx` the returned value holds.
fn fake_on(script: Script, tx: Sender<Vec<u8>>, rx: Receiver<Vec<u8>>) -> Fake {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.sock");
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || serve(listener, script, tx));
    Fake {
        path,
        requests: rx,
        _dir: dir,
    }
}

fn serve(listener: UnixListener, script: Script, tx: Sender<Vec<u8>>) {
    for conn in listener.incoming() {
        let Ok(mut conn) = conn else { return };
        let mut len = [0u8; 4];
        if conn.read_exact(&mut len).is_err() {
            continue;
        }
        let mut req = vec![0u8; u32::from_be_bytes(len) as usize];
        if conn.read_exact(&mut req).is_err() {
            continue;
        }
        let mut whole = len.to_vec();
        whole.extend(&req);
        if tx.send(whole).is_err() {
            return;
        }
        let reply = script(req[0], &req[1..]);
        let _ = conn.write_all(&reply);
    }
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = (payload.len() as u32).to_be_bytes().to_vec();
    out.extend(payload);
    out
}

fn string(bytes: &[u8]) -> Vec<u8> {
    let mut out = (bytes.len() as u32).to_be_bytes().to_vec();
    out.extend(bytes);
    out
}

/// An `ssh-ed25519` public-key blob with the 32 bytes `[n; 32]`.
fn ed_key(n: u8) -> Vec<u8> {
    let mut out = string(b"ssh-ed25519");
    out.extend(string(&[n; 32]));
    out
}

/// A well-encoded (but not valid) Ed25519 signature blob.
fn ed_signature() -> Vec<u8> {
    let mut out = string(b"ssh-ed25519");
    out.extend(string(&[7u8; 64]));
    out
}

/// An identities answer: type 12, the count, then each key with `comment`.
fn identities_answer(keys: &[Vec<u8>], comment: &[u8]) -> Vec<u8> {
    let mut out = vec![12u8];
    out.extend((keys.len() as u32).to_be_bytes());
    for k in keys {
        out.extend(string(k));
        out.extend(string(comment));
    }
    out
}

/// A sign response: type 14 and the signature string.
fn sign_response(sig: &[u8]) -> Vec<u8> {
    let mut out = vec![14u8];
    out.extend(string(sig));
    out
}

/// The fingerprint of `blob` as rdy spells it, through the engine's own parser.
fn fp(blob: &[u8]) -> String {
    rdy_engine::signing_crypto::blob_fingerprint(blob)
}

fn assert_cause(r: Result<impl std::fmt::Debug, RdyError>, cause: &str, what: &str) {
    let e = r.expect_err(what);
    assert_eq!(e.code, E_NO_AGENT, "{what}: {e}");
    assert_eq!(
        e.message,
        format!("the SSH agent could not sign: {cause}"),
        "{what}"
    );
}

/// The identities call and the sign call against an agent that always writes `raw`.
fn both(raw: Vec<u8>) -> (Result<usize, RdyError>, Result<Vec<u8>, RdyError>) {
    let f = fake(Arc::new(move |_, _| raw.clone()));
    let agent = CallerAgent::with_socket(&f.path);
    let ids = agent.identities().map(|v| v.len());
    let sig = agent.sign(&ed_key(1), b"payload");
    (ids, sig)
}

#[test]
fn sig3_agent_packets_are_bounded_and_complete() {
    // The requests are exact: identities is type 11; sign is type 13, key, payload, flags 0.
    let key = ed_key(1);
    let (k2, sig) = (key.clone(), ed_signature());
    let f = fake(Arc::new(move |ty, _| match ty {
        11 => frame(&identities_answer(std::slice::from_ref(&k2), b"me@host")),
        13 => frame(&sign_response(&sig)),
        _ => vec![],
    }));
    let agent = CallerAgent::with_socket(&f.path);
    let ids = agent.identities().unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0].public_key, key, "the comment is dropped");
    assert_eq!(f.requests.recv().unwrap(), vec![0, 0, 0, 1, 11]);
    assert_eq!(agent.sign(&key, b"payload").unwrap(), ed_signature());
    let mut sign_req = vec![13u8];
    sign_req.extend(string(&key));
    sign_req.extend(string(b"payload"));
    sign_req.extend([0, 0, 0, 0]);
    assert_eq!(f.requests.recv().unwrap(), frame(&sign_req));

    // Refusals: types 5, 30 and 102 are the shared `request refused`, for either request.
    for ty in [5u8, 30, 102] {
        let (ids, sig) = both(frame(&[ty]));
        assert_cause(ids, "request refused", &format!("identities, type {ty}"));
        assert_cause(sig, "request refused", &format!("sign, type {ty}"));
        // A refusal is the bare type: any byte after it is an invalid reply.
        for body in [&[99u8, 100][..], &[0][..]] {
            let (ids, sig) = both(frame(&[&[ty][..], body].concat()));
            let what = format!("type {ty} with trailing {body:?}");
            assert_cause(ids, "invalid reply", &format!("identities, {what}"));
            assert_cause(sig, "invalid reply", &format!("sign, {what}"));
        }
    }
    // Every other unexpected type is an invalid reply.
    for ty in 0u8..=255 {
        if [5, 30, 102].contains(&ty) {
            continue;
        }
        let f = fake(Arc::new(move |_, _| frame(&[ty])));
        let agent = CallerAgent::with_socket(&f.path);
        // (type 12 with no count is a cut answer, so invalid too)
        assert_cause(agent.identities(), "invalid reply", &format!("ids {ty}"));
        assert_cause(
            agent.sign(&ed_key(1), b"p"),
            "invalid reply",
            &format!("sign {ty}"),
        );
    }

    // Truncation: a frame cut short, or a cut length prefix, is a lost connection.
    let mut cut = frame(&identities_answer(&[ed_key(1)], b""));
    cut.truncate(cut.len() - 3);
    let (ids, sig) = both(cut);
    assert_cause(ids, "connection failed", "cut frame");
    assert_cause(sig, "connection failed", "cut frame");
    let (ids, sig) = both(vec![0, 0]);
    assert_cause(ids, "connection failed", "cut prefix");
    assert_cause(sig, "connection failed", "cut prefix");
    let (ids, sig) = both(vec![]);
    assert_cause(ids, "connection failed", "no reply");
    assert_cause(sig, "connection failed", "no reply");
    // No agent at that path at all.
    let dir = tempfile::tempdir().unwrap();
    let gone = CallerAgent::with_socket(dir.path().join("none.sock"));
    assert_cause(gone.identities(), "connection failed", "no socket");
    assert_cause(
        gone.sign(&ed_key(1), b"p"),
        "connection failed",
        "no socket",
    );

    // The packet cap comes before any read of the body: 262145 is refused unread; an empty
    // packet is invalid; a body of exactly 262144 bytes is read (and then judged on its content).
    let (ids, sig) = both(262_145u32.to_be_bytes().to_vec());
    assert_cause(ids, "reply exceeds 262144 bytes", "over cap");
    assert_cause(sig, "reply exceeds 262144 bytes", "over cap");
    let (ids, sig) = both(u32::MAX.to_be_bytes().to_vec());
    assert_cause(ids, "reply exceeds 262144 bytes", "2^32-1");
    assert_cause(sig, "reply exceeds 262144 bytes", "2^32-1");
    let (ids, sig) = both(frame(&[]));
    assert_cause(ids, "invalid reply", "empty packet");
    assert_cause(sig, "invalid reply", "empty packet");
    let mut at_cap = identities_answer(&[], b"");
    at_cap.resize(262_144, 0);
    let (ids, _) = both(frame(&at_cap));
    assert_cause(ids, "invalid reply", "at the cap, trailing zeros");

    // Bad lengths inside a whole packet.
    let mut huge_key = vec![12u8, 0, 0, 0, 1];
    huge_key.extend(u32::MAX.to_be_bytes());
    huge_key.extend([0u8; 16]);
    let mut past_end = vec![12u8, 0, 0, 0, 1];
    past_end.extend(40u32.to_be_bytes());
    past_end.extend([0u8; 39]);
    let mut comment_past_end = vec![12u8, 0, 0, 0, 1];
    comment_past_end.extend(string(&ed_key(1)));
    comment_past_end.extend(10u32.to_be_bytes());
    comment_past_end.extend(b"short");
    let mut count_past_end = vec![12u8, 0, 0, 0, 2];
    count_past_end.extend(string(&ed_key(1)));
    count_past_end.extend(string(b""));
    for (what, body) in [
        ("key length 2^32-1", huge_key),
        ("key past the end", past_end),
        ("comment past the end", comment_past_end),
        ("count past the end", count_past_end),
        ("count cut", vec![12u8, 0, 0]),
    ] {
        let f = fake(Arc::new(move |_, _| frame(&body)));
        assert_cause(
            CallerAgent::with_socket(&f.path).identities(),
            "invalid reply",
            what,
        );
    }

    // Identity count, key and comment bounds: the limit is accepted, one more is refused.
    let tiny: Vec<Vec<u8>> = (0..1024).map(|_| vec![1u8]).collect();
    let f = fake(Arc::new(move |_, _| frame(&identities_answer(&tiny, b""))));
    assert_eq!(
        CallerAgent::with_socket(&f.path)
            .identities()
            .unwrap()
            .len(),
        1024
    );
    let mut over = vec![12u8];
    over.extend(1025u32.to_be_bytes());
    for _ in 0..1025 {
        over.extend(string(&[1]));
        over.extend(string(b""));
    }
    let f = fake(Arc::new(move |_, _| frame(&over)));
    assert_cause(
        CallerAgent::with_socket(&f.path).identities(),
        "invalid reply",
        "1025 identities",
    );
    for (comment_len, ok) in [(4096usize, true), (4097, false)] {
        let comment = vec![b'c'; comment_len];
        let f = fake(Arc::new(move |_, _| {
            frame(&identities_answer(&[ed_key(1)], &comment))
        }));
        let r = CallerAgent::with_socket(&f.path).identities();
        if ok {
            assert_eq!(r.unwrap().len(), 1, "a 4096-byte comment");
        } else {
            assert_cause(r, "invalid reply", "a 4097-byte comment");
        }
    }
    for (key_len, ok) in [(16384usize, true), (16385, false)] {
        let key = vec![b'k'; key_len];
        let f = fake(Arc::new(move |_, _| {
            frame(&identities_answer(std::slice::from_ref(&key), b""))
        }));
        let r = CallerAgent::with_socket(&f.path).identities();
        if ok {
            assert_eq!(r.unwrap()[0].public_key.len(), 16384);
        } else {
            assert_cause(r, "invalid reply", "a 16385-byte key");
        }
    }

    // Trailing bytes after a complete answer, and a wrong answer type.
    let mut ids_trailing = identities_answer(&[ed_key(1)], b"");
    ids_trailing.push(0);
    let mut sign_trailing = sign_response(&ed_signature());
    sign_trailing.push(0);
    let f = fake(Arc::new(move |ty, _| match ty {
        11 => frame(&ids_trailing),
        _ => frame(&sign_trailing),
    }));
    let agent = CallerAgent::with_socket(&f.path);
    assert_cause(agent.identities(), "invalid reply", "identities trailing");
    assert_cause(
        agent.sign(&ed_key(1), b"p"),
        "invalid reply",
        "sign trailing",
    );
    let f = fake(Arc::new(move |ty, _| match ty {
        11 => frame(&sign_response(&ed_signature())),
        _ => frame(&identities_answer(&[], b"")),
    }));
    let agent = CallerAgent::with_socket(&f.path);
    assert_cause(
        agent.identities(),
        "invalid reply",
        "sign answer to identities",
    );
    assert_cause(
        agent.sign(&ed_key(1), b"p"),
        "invalid reply",
        "identities answer to sign",
    );
    // A sign response whose signature string runs past the end, or over 16384 bytes.
    let mut sig_past_end = vec![14u8];
    sig_past_end.extend(100u32.to_be_bytes());
    sig_past_end.extend([0u8; 10]);
    let f = fake(Arc::new(move |_, _| frame(&sig_past_end)));
    assert_cause(
        CallerAgent::with_socket(&f.path).sign(&ed_key(1), b"p"),
        "invalid reply",
        "signature past the end",
    );
    let f = fake(Arc::new(move |_, _| frame(&sign_response(&[0u8; 16385]))));
    assert_cause(
        CallerAgent::with_socket(&f.path).sign(&ed_key(1), b"p"),
        "invalid reply",
        "a 16385-byte signature",
    );
}

#[test]
fn sig3_agent_access_stays_inside_the_caller() {
    // Two callers, each with its own agent socket injected; neither reads the environment.
    let (ka, kb) = (ed_key(0xaa), ed_key(0xbb));
    let script = |key: Vec<u8>| -> Script {
        Arc::new(move |ty, _| match ty {
            11 => frame(&identities_answer(std::slice::from_ref(&key), b"")),
            13 => frame(&sign_response(&ed_signature())),
            _ => vec![],
        })
    };
    let fa = fake(script(ka.clone()));
    let fb = fake(script(kb.clone()));
    let a = Arc::new(CallerAgent::with_socket(&fa.path));
    let b = Arc::new(CallerAgent::with_socket(&fb.path));

    // Both callers resolve and sign at once.
    let barrier = Arc::new(Barrier::new(2));
    let run = |agent: Arc<CallerAgent>, mine: Vec<u8>, theirs: Vec<u8>, barrier: Arc<Barrier>| {
        thread::spawn(move || {
            barrier.wait();
            let key = resolve_key("signing.key", &fp(&mine), agent.as_ref()).unwrap();
            assert_eq!(key.public_key, mine);
            let e = resolve_key("signing.key", &fp(&theirs), agent.as_ref()).unwrap_err();
            assert_eq!(e.message, "the SSH agent could not sign: key is not held");
            sign_sshsig(agent.as_ref(), &key, "rdy-approval-v1", b"statement").unwrap()
        })
    };
    let ta = run(a.clone(), ka.clone(), kb.clone(), barrier.clone());
    let tb = run(b.clone(), kb.clone(), ka.clone(), barrier);
    let (sa, sb) = (ta.join().unwrap(), tb.join().unwrap());
    // Each SSHSIG embeds its caller's key.
    assert!(contains(&sa, &ka) && !contains(&sa, &kb));
    assert!(contains(&sb, &kb) && !contains(&sb, &ka));

    // Each agent saw only its own caller: two identities requests and one sign request, each
    // on its own connection, and the sign request names that caller's key.
    for (f, key) in [(&fa, &ka), (&fb, &kb)] {
        let reqs: Vec<Vec<u8>> = f.requests.try_iter().collect();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0], vec![0, 0, 0, 1, 11]);
        assert_eq!(reqs[1], vec![0, 0, 0, 1, 11]);
        assert_eq!(reqs[2][4], 13);
        assert!(contains(&reqs[2], key));
    }
}

#[test]
fn sig3_cancel_closes_the_agent_stream() {
    // The fake agent reports the request, then holds the reply until released; the caller
    // cancels in between and the waiting call ends as `request cancelled`.
    let (held_tx, held_rx) = channel::<()>();
    let held_rx = Arc::new(std::sync::Mutex::new(held_rx));
    // The fake agent and the waiting call report on one channel: a request, or (empty) the end
    // of the call, so a call that ends early fails the test instead of hanging it.
    let (tx, rx) = channel();
    let f = fake_on(
        Arc::new(move |_, _| {
            let _ = held_rx.lock().unwrap().recv();
            frame(&identities_answer(&[], b""))
        }),
        tx.clone(),
        rx,
    );
    let agent = Arc::new(CallerAgent::with_socket(&f.path));
    let waiting = {
        let agent = agent.clone();
        thread::spawn(move || {
            let r = agent.identities().map(|v| v.len());
            let _ = tx.send(vec![]);
            r
        })
    };
    // The request reached the agent: the call is now waiting for its reply.
    let first = f.requests.recv().unwrap();
    assert!(
        !first.is_empty(),
        "the call ended before the agent saw a request: {:?}",
        waiting.join()
    );
    agent.cancel();
    assert_cause(
        waiting.join().unwrap(),
        "request cancelled",
        "cancelled call",
    );
    drop(held_tx);
    // A cancel applies to the calls in flight only: the next call is answered.
    let f = fake(Arc::new(|_, _| frame(&identities_answer(&[], b""))));
    let agent = CallerAgent::with_socket(&f.path);
    agent.cancel();
    assert_eq!(agent.identities().unwrap().len(), 0);
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}
