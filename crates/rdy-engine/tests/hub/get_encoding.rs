//! r59013: the streamed get reply may come as zstd. The real transport runs against loopback TLS
//! fixtures with generated CAs; no sleep or elapsed-time assertion is used. The reply's frames are
//! made with the same encoder as a put body, or by hand where a test needs a frame it never
//! makes.
use crate::hub_tls;

use hub_tls::{Recorded, Reply, Server};
use rdy_engine::hub_http::{
    BodyEncoding, HttpHubTransport, ReplyEncoding, ReplySink, UPLOAD_BUFFER_BYTES,
    UploadHttpRequest, UploadMethod, zstd_frame,
};
use rdy_engine::hub_types::*;
use rdy_engine::{E_IO, Session, SessionKind};
use rdy_model::Role;
use std::io::{self, Cursor, Read, Write};

const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const SYNC_JSON: &str = "application/vnd.rdy.sync+json";
const GET_PATH: &str = "/sync/acme/docs/objects/get";
const REQUEST_BODY: &[u8] = br#"{"keys":[]}"#;

type Sent = Result<(u16, Vec<u8>, u64), HubFailure>;

fn claim() -> RequestClaim {
    RequestClaim::from_session(&Session {
        actor: "agent:claude".into(),
        run: Some("r7".into()),
        role: Role::Writer,
        kind: SessionKind::Agent,
    })
    .unwrap()
}

fn kind(f: &HubFailure) -> String {
    match f {
        HubFailure::Transport { may_have_sent } => format!("Transport({may_have_sent})"),
        HubFailure::Protocol => "Protocol".into(),
        HubFailure::Local(e) => format!("Local({})", e.code),
        other => format!("{other:?}"),
    }
}

/// What one streamed request left behind: its result, the bytes the sink took and the request
/// the hub read.
struct Run {
    result: Sent,
    out: Vec<u8>,
    request: Recorded,
}

impl Run {
    #[track_caller]
    fn ok(&self) -> (u16, u64) {
        match &self.result {
            Ok((status, _, n)) => (*status, *n),
            Err(f) => panic!("expected a reply, got {}", kind(f)),
        }
    }

    #[track_caller]
    fn failure(&self) -> String {
        match &self.result {
            Err(f) => kind(f),
            Ok((status, _, n)) => panic!("expected a failure, got {status} with {n} bytes"),
        }
    }
}

/// One POST to a new hub that answers `reply`, its 200 reply streamed to a sink of `limit`.
fn run(reply: Reply, encoding: ReplyEncoding, limit: u64) -> Run {
    run_into(reply, encoding, limit, &mut Vec::new())
}

fn run_into(reply: Reply, encoding: ReplyEncoding, limit: u64, out: &mut Vec<u8>) -> Run {
    let mut server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, server.ca_pem()).unwrap();
    let install = Origin::parse(server.origin()).unwrap();
    let t = HttpHubTransport::new(OriginFence::new(install.clone(), []).unwrap()).unwrap();
    let url = format!("{install}{GET_PATH}");
    let mut body = Cursor::new(REQUEST_BODY.to_vec());
    server.reply(reply);
    server.accept_n(1);
    let result = t.upload_streamed(
        &install,
        Some(ca.as_path()),
        &claim(),
        &Secret::new(ACCESS),
        UploadHttpRequest {
            method: UploadMethod::Post,
            destination: &url,
            content_type: SYNC_JSON,
            body: Some(&mut body),
            body_len: REQUEST_BODY.len() as u64,
            reply_limit: 1_048_576,
            encoding: BodyEncoding::Identity,
        },
        ReplySink {
            status: 200,
            out,
            limit,
            encoding,
        },
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "one request: {}", result_text(&result));
    let request = requests[0].clone();
    assert_eq!(server.finish(), 0);
    Run {
        result,
        out: std::mem::take(out),
        request,
    }
}

fn result_text(r: &Sent) -> String {
    match r {
        Ok((status, body, n)) => format!("{status} {} bytes in memory, {n} streamed", body.len()),
        Err(f) => kind(f),
    }
}

/// A 200 reply with the header lines `headers` and the raw `body`.
fn reply(headers: &[&str], body: &[u8]) -> Reply {
    reply_status(200, headers, body)
}

fn reply_status(status: u16, headers: &[&str], body: &[u8]) -> Reply {
    let mut lines: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
    lines.push("content-type: application/octet-stream".into());
    lines.push(format!("content-length: {}", body.len()));
    lines.push("connection: close".into());
    Reply::with_headers(status, &lines, body)
}

fn zstd_reply(body: &[u8]) -> Reply {
    reply(&["content-encoding: zstd"], body)
}

/// A 200 reply whose body goes as chunks of `step` bytes.
fn chunked(headers: &[&str], body: &[u8], step: usize) -> Reply {
    let mut lines: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
    lines.push("content-type: application/octet-stream".into());
    lines.push("transfer-encoding: chunked".into());
    lines.push("connection: close".into());
    let mut wire = Vec::new();
    for piece in body.chunks(step) {
        wire.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
        wire.extend_from_slice(piece);
        wire.extend_from_slice(b"\r\n");
    }
    wire.extend_from_slice(b"0\r\n\r\n");
    Reply::with_headers(200, &lines, &wire)
}

/// Bytes that do not compress: a xorshift stream.
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// Bytes that compress well: a repeated line with a counter.
fn lines(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n + 64);
    let mut i = 0u32;
    while out.len() < n {
        out.extend_from_slice(format!("record {i} of the batch, kept as text\n").as_bytes());
        i += 1;
    }
    out.truncate(n);
    out
}

/// The put framing of `body`: the frame, when `zstd_frame` finds one smaller than the body.
fn put_frame(body: &[u8]) -> Option<Vec<u8>> {
    let (mut file, n, encoding) = zstd_frame(&mut &body[..], body.len() as u64).unwrap();
    let mut framed = Vec::new();
    file.read_to_end(&mut framed).unwrap();
    assert_eq!(framed.len() as u64, n);
    match encoding {
        BodyEncoding::Zstd => Some(framed),
        BodyEncoding::Identity => {
            assert_eq!(framed, body);
            None
        }
    }
}

/// One frame of `body` at level 3 with its content size and checksum: what a put makes, but
/// always a frame, even when it is not smaller.
fn frame_of(body: &[u8]) -> Vec<u8> {
    let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    enc.include_checksum(true).unwrap();
    enc.set_pledged_src_size(Some(body.len() as u64)).unwrap();
    enc.write_all(body).unwrap();
    enc.finish().unwrap()
}

/// A skippable frame of `payload`.
fn skippable(payload: &[u8]) -> Vec<u8> {
    let mut out = 0x184D_2A50u32.to_le_bytes().to_vec();
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// A sink that takes `room` bytes, then refuses.
struct Full {
    room: usize,
}

impl Write for Full {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.room {
            return Err(io::Error::other("full"));
        }
        self.room -= buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// D-SEGWK, D-N8PR2: a zstd reply is decoded as it arrives. Compressible and incompressible
/// batches, a reply in one-byte pieces and several frames (with a skippable one between) give
/// the bytes that were framed, and the count is the decoded count. The frame is the put frame.
#[test]
fn r59013_zstd_get_keeps_checked_bytes() {
    let piece = UPLOAD_BUFFER_BYTES;
    let compressible = lines(3 * 1024 * 1024 + 17);
    let framed = put_frame(&compressible).expect("compressible text frames smaller");
    assert!(framed.len() < compressible.len() / 4, "{}", framed.len());
    let cases: Vec<(&str, Vec<u8>, Vec<u8>)> = vec![
        ("compressible", compressible.clone(), framed.clone()),
        ("small", lines(5), frame_of(&lines(5))),
        // a frame that is not smaller than its body, forced
        (
            "incompressible",
            noise(3 * piece + 11, 7),
            frame_of(&noise(3 * piece + 11, 7)),
        ),
        ("empty batch", Vec::new(), frame_of(&[])),
        (
            "exactly one buffer",
            noise(piece, 9),
            frame_of(&noise(piece, 9)),
        ),
        ("one buffer and a byte", lines(piece + 1), {
            frame_of(&lines(piece + 1))
        }),
    ];
    for (case, plain, wire) in &cases {
        let limit = (plain.len() as u64).max(wire.len() as u64);
        let r = run(zstd_reply(wire), ReplyEncoding::ZstdOrIdentity, limit);
        assert_eq!(r.ok(), (200, plain.len() as u64), "{case}");
        assert_eq!(&r.out, plain, "{case}");
        // the decoded count is what the reply carries
        let (_, body, n) = r.result.as_ref().unwrap();
        assert!(body.is_empty(), "{case}");
        assert_eq!(*n, plain.len() as u64, "{case}");
    }

    // the request's one offer: exactly one Accept-Encoding, zstd
    let r = run(
        zstd_reply(&framed),
        ReplyEncoding::ZstdOrIdentity,
        compressible.len() as u64,
    );
    assert_eq!(r.request.header("accept-encoding"), Some("zstd"));
    assert_eq!(
        r.request
            .headers
            .iter()
            .filter(|(n, _)| n == "accept-encoding")
            .count(),
        1
    );

    // a reply in pieces of one byte, and of uneven sizes: the same bytes
    let wire = frame_of(&lines(2_000));
    for step in [1usize, 3, 100] {
        let r = run(
            chunked(&["content-encoding: zstd"], &wire, step),
            ReplyEncoding::ZstdOrIdentity,
            2_000,
        );
        assert_eq!(r.ok(), (200, 2_000), "step {step}");
        assert_eq!(r.out, lines(2_000), "step {step}");
    }

    // frames one after another, with a skippable frame between: the bytes in order
    let (a, b, c) = (lines(70_000), noise(9_000, 3), lines(1));
    let mut wire = frame_of(&a);
    wire.extend_from_slice(&skippable(b"any bytes at all"));
    wire.extend_from_slice(&frame_of(&b));
    wire.extend_from_slice(&frame_of(&c));
    wire.extend_from_slice(&skippable(&[]));
    let whole: Vec<u8> = [&a[..], &b[..], &c[..]].concat();
    let r = run(
        zstd_reply(&wire),
        ReplyEncoding::ZstdOrIdentity,
        whole.len() as u64,
    );
    assert_eq!(r.ok(), (200, whole.len() as u64));
    assert_eq!(r.out, whole);
    for step in [1usize, 997] {
        let r = run(
            chunked(&["content-encoding: zstd"], &wire, step),
            ReplyEncoding::ZstdOrIdentity,
            whole.len() as u64,
        );
        assert_eq!(r.out, whole, "step {step}");
    }

    // the put framing: level 3, the content size and the checksum, a window within 8 MiB
    let reference = {
        let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        enc.include_checksum(true).unwrap();
        enc.set_pledged_src_size(Some(compressible.len() as u64))
            .unwrap();
        enc.write_all(&compressible).unwrap();
        enc.finish().unwrap()
    };
    assert_eq!(framed, reference, "level 3, checksum and content size");
    assert_eq!(&framed[..4], &0xFD2F_B528u32.to_le_bytes());
    let descriptor = framed[4];
    assert_ne!(descriptor & 0x04, 0, "checksum flag");
    assert_ne!(descriptor >> 6, 0, "content size is named");
    let window = if descriptor & 0x20 != 0 {
        // one segment: the window is the content size
        compressible.len() as u64
    } else {
        let w = framed[5];
        (1u64 << (10 + u32::from(w >> 3))) + (u64::from(w & 7) << (7 + u32::from(w >> 3)))
    };
    assert!(window <= 8_388_608, "{window}");

    // an identity reply to the same offer is copied as it always was
    for (case, headers) in [
        ("none", vec![]),
        ("identity", vec!["content-encoding: identity"]),
        ("spaced", vec!["content-encoding:   Identity \t"]),
    ] {
        let r = run(
            reply(&headers, &compressible),
            ReplyEncoding::ZstdOrIdentity,
            compressible.len() as u64,
        );
        assert_eq!(r.ok(), (200, compressible.len() as u64), "{case}");
        assert_eq!(r.out, compressible, "{case}");
    }
    // the token is not case-sensitive, and optional whitespace around it is trimmed
    let r = run(
        reply(&["content-encoding:  ZsTd \t"], &frame_of(&a)),
        ReplyEncoding::ZstdOrIdentity,
        a.len() as u64,
    );
    assert_eq!(r.ok(), (200, a.len() as u64));
    assert_eq!(r.out, a);
}

/// An encoding or a frame outside the contract is the protocol failure, with no successful
/// batch: an unsolicited coding, a duplicate field, a list, an empty or unknown coding, a
/// compressed refusal, a truncated or corrupt frame, trailing bytes and a window past 8 MiB.
#[test]
fn r59013_bad_encoding_and_frames_refuse() {
    let plain = lines(50_000);
    let good = frame_of(&plain);
    let limit = 1 << 20;
    let both = ReplyEncoding::ZstdOrIdentity;
    let fails = |case: &str, r: Reply, offer: ReplyEncoding| {
        let r = run(r, offer, limit);
        assert_eq!(r.failure(), "Protocol", "{case}");
    };

    // a coding the request did not offer: a dump asks for none
    fails("unsolicited", zstd_reply(&good), ReplyEncoding::Identity);
    let r = run(
        reply(&[], &plain),
        ReplyEncoding::Identity,
        plain.len() as u64,
    );
    assert_eq!(r.ok(), (200, plain.len() as u64));
    assert_eq!(r.request.header("accept-encoding"), None, "no offer");
    let r = run(
        reply(&["content-encoding: identity"], &plain),
        ReplyEncoding::Identity,
        plain.len() as u64,
    );
    assert_eq!(r.out, plain);

    // the field: duplicated, a list, empty, unknown, or a parameter
    fails(
        "duplicate",
        reply(&["content-encoding: zstd", "content-encoding: zstd"], &good),
        both,
    );
    fails(
        "duplicate identity",
        reply(
            &["content-encoding: identity", "content-encoding: identity"],
            &plain,
        ),
        both,
    );
    for value in [
        "zstd, identity",
        "identity, zstd",
        "zstd,zstd",
        "identity,identity",
        "gzip",
        "br",
        "deflate",
        "zstd;q=1",
        "zst",
        "zstdx",
        "",
        " ",
    ] {
        let line = format!("content-encoding: {value}");
        fails(value, reply(&[line.as_str()], &good), both);
    }

    // a refusal is identity JSON: a coded one is a failure, and its bytes are not read as one
    let json = br#"{"code":"E_HUB_NOT_FOUND","message":"m","fix":"f","request_id":"r1"}"#;
    let r = run(
        reply_status(404, &["content-encoding: identity"], json),
        both,
        limit,
    );
    let (status, body, n) = r.result.as_ref().unwrap();
    assert_eq!((*status, body.as_slice(), *n), (404, &json[..], 0));
    for value in ["zstd", "gzip", "zstd, identity"] {
        let line = format!("content-encoding: {value}");
        let coded = frame_of(json);
        let r = run(reply_status(404, &[line.as_str()], &coded), both, limit);
        assert_eq!(r.failure(), "Protocol", "refusal coded {value}");
        assert!(r.out.is_empty());
    }
    let r = run(
        reply_status(404, &["content-encoding: zstd"], &frame_of(json)),
        ReplyEncoding::Identity,
        limit,
    );
    assert_eq!(r.failure(), "Protocol", "refusal coded, no offer");

    // an empty body is no frame
    fails("empty", zstd_reply(&[]), both);

    // a frame cut short, at its header, its body and its checksum
    for cut in [
        1,
        3,
        5,
        9,
        good.len() / 2,
        good.len() - 5,
        good.len() - 4,
        good.len() - 1,
    ] {
        let r = run(zstd_reply(&good[..cut]), both, limit);
        assert_eq!(r.failure(), "Protocol", "cut at {cut}");
    }
    // a corrupt body, and a checksum that does not match
    let mut flipped = good.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0xFF;
    fails("corrupt body", zstd_reply(&flipped), both);
    let mut sum = good.clone();
    let last = sum.len() - 1;
    sum[last] ^= 0x01;
    fails("checksum", zstd_reply(&sum), both);
    // the first frame good and the second cut, or corrupt
    let mut two = good.clone();
    two.extend_from_slice(&good[..good.len() - 2]);
    fails("second frame cut", zstd_reply(&two), both);
    let mut two = good.clone();
    two.extend_from_slice(&sum);
    fails("second frame checksum", zstd_reply(&two), both);

    // bytes after the last frame that are no frame
    for tail in [&b"x"[..], b"xyz", b"trailing text", &[0u8; 4], &[0u8; 64]] {
        let mut wire = good.clone();
        wire.extend_from_slice(tail);
        fails(&format!("tail {tail:?}"), zstd_reply(&wire), both);
    }
    let mut wire = b"junk".to_vec();
    wire.extend_from_slice(&good);
    fails("leading bytes", zstd_reply(&wire), both);
    fails("not zstd at all", zstd_reply(&plain), both);

    // a window past 8 MiB (2^24, and the format's own largest), a frame that needs more
    // memory than the decoder allows
    for log in [24u32, 27] {
        let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        enc.window_log(log).unwrap();
        enc.write_all(&plain).unwrap();
        let wire = enc.finish().unwrap();
        fails(&format!("window log {log}"), zstd_reply(&wire), both);
    }
    // 2^23 is the largest that is taken
    let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    enc.window_log(23).unwrap();
    enc.include_checksum(true).unwrap();
    enc.write_all(&plain).unwrap();
    let wire = enc.finish().unwrap();
    let r = run(zstd_reply(&wire), both, limit);
    assert_eq!(r.ok(), (200, plain.len() as u64), "window log 23");
    assert_eq!(r.out, plain);

    // a transport cut keeps the read's own failure: the reply declares more than it sends
    let cut = Reply::with_headers(
        200,
        &[
            "content-encoding: zstd".into(),
            format!("content-length: {}", good.len() + 100),
            "connection: close".into(),
        ],
        &good,
    );
    let r = run(cut, both, limit);
    assert_eq!(r.failure(), "Transport(true)");
    let cut = Reply::with_headers(
        200,
        &[
            "content-encoding: zstd".into(),
            format!("content-length: {}", good.len() + 100),
            "connection: close".into(),
        ],
        &good[..good.len() / 2],
    );
    let r = run(cut, both, limit);
    assert_eq!(r.failure(), "Transport(true)");
}

/// The caps count the decoded bytes and the encoded bytes alike, each at the reply's cap: a tiny
/// frame of a huge body, the cap and one byte past it, a claimed size past the cap and a long
/// run of skippable bytes are within the caps and refused where they pass them, with no more
/// than the cap written to the sink.
#[test]
fn r59013_decoded_limits_are_not_wire_limits() {
    let both = ReplyEncoding::ZstdOrIdentity;

    // 8 MiB of zeros is a few hundred bytes on the wire: the wire is within any cap, the
    // decoded body is not
    let zeros = vec![0u8; 8 * 1024 * 1024];
    let tiny = frame_of(&zeros);
    assert!(tiny.len() < 1_000, "{}", tiny.len());
    for limit in [1u64 << 10, 64 * 1024, 1024 * 1024, zeros.len() as u64 - 1] {
        let r = run(zstd_reply(&tiny), both, limit);
        assert_eq!(r.failure(), "Protocol", "limit {limit}");
        assert!(r.out.len() as u64 <= limit, "{} > {limit}", r.out.len());
    }
    let r = run(zstd_reply(&tiny), both, zeros.len() as u64);
    assert_eq!(r.ok(), (200, zeros.len() as u64));
    assert!(r.out == zeros);

    // the cap, and one past it, decoded (the encoded body is well within it)
    let cap = 100_000usize;
    let at = lines(cap);
    let past = lines(cap + 1);
    let r = run(zstd_reply(&frame_of(&at)), both, cap as u64);
    assert_eq!(r.ok(), (200, cap as u64));
    assert_eq!(r.out, at);
    let r = run(zstd_reply(&frame_of(&past)), both, cap as u64);
    assert_eq!(r.failure(), "Protocol");
    assert!(r.out.len() <= cap, "{}", r.out.len());
    // and at a cap that is not a multiple of the read buffer, past it by one piece
    for limit in [65_535u64, 65_536, 65_537, 131_072] {
        let body = lines(limit as usize + 1);
        let r = run(zstd_reply(&frame_of(&body)), both, limit);
        assert_eq!(r.failure(), "Protocol", "limit {limit}");
        assert!(r.out.len() as u64 <= limit, "limit {limit}");
        let body = lines(limit as usize);
        let r = run(zstd_reply(&frame_of(&body)), both, limit);
        assert_eq!(r.ok(), (200, limit), "limit {limit}");
    }

    // the encoded bytes are held to the cap too: incompressible data of exactly the cap makes a
    // frame past it
    let hard = noise(cap, 5);
    let wire = frame_of(&hard);
    assert!(wire.len() > cap);
    let r = run(zstd_reply(&wire), both, cap as u64);
    assert_eq!(r.failure(), "Protocol");
    assert!(r.out.len() <= cap);
    // while a wire of exactly the cap decodes
    let r = run(zstd_reply(&wire), both, wire.len() as u64);
    assert_eq!(r.ok(), (200, cap as u64));
    let r = run(zstd_reply(&wire), both, wire.len() as u64 - 1);
    assert_eq!(r.failure(), "Protocol");

    // a claimed content size no body meets: a frame that says one terabyte, in one segment (its
    // window is past 8 MiB), and one that says it with a small window
    let mut one_segment = 0xFD2F_B528u32.to_le_bytes().to_vec();
    one_segment.push(0xE0); // 8-byte content size, one segment
    one_segment.extend_from_slice(&(1u64 << 40).to_le_bytes());
    one_segment.extend_from_slice(&[0x01, 0x00, 0x00]); // last block, raw, no bytes
    let r = run(zstd_reply(&one_segment), both, 1 << 20);
    assert_eq!(r.failure(), "Protocol", "one segment");
    assert!(r.out.is_empty());
    let mut windowed = 0xFD2F_B528u32.to_le_bytes().to_vec();
    windowed.push(0xC0); // 8-byte content size, a window byte
    windowed.push(0x00); // 1 KiB window
    windowed.extend_from_slice(&(1u64 << 40).to_le_bytes());
    windowed.extend_from_slice(&[0x29, 0x00, 0x00]); // last block, raw, 5 bytes
    windowed.extend_from_slice(b"hello");
    let r = run(zstd_reply(&windowed), both, 1 << 20);
    assert_eq!(r.failure(), "Protocol", "claimed size");
    assert!(r.out.len() <= 5, "{}", r.out.len());
    // skippable frames: taken between frames, and bounded by the encoded cap when long
    let body = lines(4_000);
    let mut wire = skippable(&[7u8; 1_000]);
    wire.extend_from_slice(&frame_of(&body));
    let r = run(zstd_reply(&wire), both, 1 << 20);
    assert_eq!(r.ok(), (200, 4_000));
    assert_eq!(r.out, body);
    let big = skippable(&vec![1u8; 4 * 1024 * 1024]);
    let mut wire = frame_of(&body);
    wire.extend_from_slice(&big);
    let r = run(zstd_reply(&wire), both, 1 << 20);
    assert_eq!(r.failure(), "Protocol", "skippable past the cap");
    // many small skippable frames, past the cap together
    let mut wire = frame_of(&body);
    for _ in 0..40_000 {
        wire.extend_from_slice(&skippable(&[0u8; 8]));
    }
    let r = run(zstd_reply(&wire), both, 64 * 1024);
    assert_eq!(r.failure(), "Protocol", "many skippable frames");
    // a skippable frame that claims more than the body holds
    let mut wire = frame_of(&body);
    wire.extend_from_slice(&0x184D_2A5Fu32.to_le_bytes());
    wire.extend_from_slice(&u32::MAX.to_le_bytes());
    wire.extend_from_slice(&[0u8; 32]);
    let r = run(zstd_reply(&wire), both, 1 << 20);
    assert_eq!(r.failure(), "Protocol", "skippable claim");

    // a sink that refuses a write keeps its own failure, for an identity and a zstd reply
    let wire = frame_of(&lines(200_000));
    let server_run = |reply: Reply, room: usize| {
        let mut server = Server::start();
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, server.ca_pem()).unwrap();
        let install = Origin::parse(server.origin()).unwrap();
        let t = HttpHubTransport::new(OriginFence::new(install.clone(), []).unwrap()).unwrap();
        let url = format!("{install}{GET_PATH}");
        let mut body = Cursor::new(REQUEST_BODY.to_vec());
        server.reply(reply);
        server.accept_n(1);
        let mut full = Full { room };
        let r = t.upload_streamed(
            &install,
            Some(ca.as_path()),
            &claim(),
            &Secret::new(ACCESS),
            UploadHttpRequest {
                method: UploadMethod::Post,
                destination: &url,
                content_type: SYNC_JSON,
                body: Some(&mut body),
                body_len: REQUEST_BODY.len() as u64,
                reply_limit: 1_048_576,
                encoding: BodyEncoding::Identity,
            },
            ReplySink {
                status: 200,
                out: &mut full,
                limit: 1 << 22,
                encoding: both,
            },
        );
        let _ = server.finish();
        r
    };
    for room in [0usize, 70_000] {
        let r = server_run(zstd_reply(&wire), room);
        assert_eq!(
            r.as_ref().map_err(kind).unwrap_err(),
            format!("Local({E_IO})"),
            "zstd, room {room}"
        );
        let r = server_run(reply(&[], &lines(200_000)), room);
        assert_eq!(
            r.as_ref().map_err(kind).unwrap_err(),
            format!("Local({E_IO})"),
            "identity, room {room}"
        );
    }
}
