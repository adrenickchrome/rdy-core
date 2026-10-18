//! The ctl line codec and the built-in ops (ctl1).
use rdy_daemon::*;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

struct Echo;
impl Handler for Echo {
    fn ctl(
        &self,
        s: &SessionInfo,
        r: CtlRequest,
        _agent: Arc<dyn rdy_engine::signing_crypto::SigningAgent>,
    ) -> Result<Value, ErrorReply> {
        match r.op.as_str() {
            "echo" => Ok(json!({"session": s.id, "args": r.args})),
            "fail" => Err(ErrorReply::new("E_TEST", "asked to fail", "don't")),
            "panic" => panic!("boom"),
            other => Err(ErrorReply::new(
                E_BAD_INPUT,
                format!("unknown op {other:?}"),
                "use echo",
            )),
        }
    }
}

fn info() -> DaemonInfo {
    DaemonInfo {
        pid: std::process::id(),
        version: "t".into(),
        started_at: std::time::Instant::now(),
        sessions: Sessions::default(),
        connections: Arc::new(AtomicUsize::new(1)),
        stop: Arc::new(tokio::sync::watch::channel(false).0),
    }
}

/// A served ctl connection over an in-memory duplex: (client end, the serve task).
fn serve(info: DaemonInfo) -> (DuplexStream, tokio::task::JoinHandle<std::io::Result<()>>) {
    let (client, server) = tokio::io::duplex(1 << 20);
    let session = info.sessions.insert(Proto::Ctl, "t".into(), "/".into(), 0);
    let (r, w) = tokio::io::split(server);
    let h = tokio::spawn(serve_ctl(r, w, session, Arc::new(Echo), info));
    (client, h)
}

async fn roundtrip(c: &mut DuplexStream, line: &str) -> Value {
    c.write_all(line.as_bytes()).await.unwrap();
    c.write_all(b"\n").await.unwrap();
    let mut r = BufReader::new(c);
    let mut out = String::new();
    r.read_line(&mut out).await.unwrap();
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out:?}"))
}

#[tokio::test]
async fn ctl1_a_request_round_trips_with_its_id_and_args() {
    let (mut c, _h) = serve(info());
    let v = roundtrip(&mut c, r#"{"id":7,"op":"echo","args":{"x":[1,2]}}"#).await;
    assert_eq!(v["id"], 7);
    assert_eq!(v["ok"]["args"]["x"], json!([1, 2]));
    assert_eq!(v["ok"]["session"], 1);
    let v = roundtrip(&mut c, r#"{"id":"s-1","op":"echo"}"#).await;
    assert_eq!(v["id"], "s-1");
    assert_eq!(v["ok"]["args"], json!({}), "args default to {{}}");
    let v = roundtrip(&mut c, r#"{"id":8,"op":"fail"}"#).await;
    assert_eq!(v["error"]["code"], "E_TEST");
    assert_eq!(v["error"]["fix"], "don't");
}

#[tokio::test]
async fn ctl1_a_malformed_line_gets_a_bad_input_reply_and_the_connection_lives() {
    let (mut c, _h) = serve(info());
    for bad in [
        r#"{"op":"echo"}"#,
        r#"{"id":true,"op":"echo"}"#,
        "[1,2]",
        "{not json",
    ] {
        let v = roundtrip(&mut c, bad).await;
        assert_eq!(v["id"], Value::Null, "{bad}");
        assert_eq!(v["error"]["code"], "E_BAD_INPUT", "{bad}");
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("a ctl request is one JSON object per line"),
            "{bad}"
        );
    }
    // ruling Q13: a line that has an id keeps it in the reply
    let v = roundtrip(&mut c, r#"{"id":1}"#).await;
    assert_eq!(v["id"], 1, "{v}");
    assert_eq!(v["error"]["code"], "E_BAD_INPUT", "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("a ctl request is one JSON object per line"),
        "{v}"
    );
    let v = roundtrip(&mut c, r#"{"id":2,"op":"echo"}"#).await;
    assert_eq!(v["id"], 2, "still served after bad lines");
}

#[tokio::test]
async fn ctl1_an_empty_line_is_skipped() {
    let (mut c, _h) = serve(info());
    c.write_all(b"\n\r\n").await.unwrap();
    let v = roundtrip(&mut c, r#"{"id":3,"op":"echo"}"#).await;
    assert_eq!(v["id"], 3);
}

#[tokio::test]
async fn ctl1_an_oversized_line_gets_e_limit_and_the_connection_closes() {
    let (mut c, h) = serve(info());
    let big = format!(
        r#"{{"id":9,"op":"echo","args":{{"x":"{}"}}}}"#,
        "y".repeat(MAX_CTL_LINE_BYTES)
    );
    c.write_all(big.as_bytes()).await.unwrap();
    c.write_all(b"\n").await.unwrap();
    let mut r = BufReader::new(&mut c);
    let mut out = String::new();
    r.read_line(&mut out).await.unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["id"], Value::Null);
    assert_eq!(v["error"]["code"], "E_LIMIT");
    assert_eq!(v["error"]["message"], "request line is over 16 MiB");
    out.clear();
    assert_eq!(
        r.read_line(&mut out).await.unwrap(),
        0,
        "closed after the reply"
    );
    h.await.unwrap().unwrap();
}

#[tokio::test]
async fn ctl1_a_line_of_exactly_the_limit_is_served() {
    let (mut c, _h) = serve(info());
    let head = r#"{"id":10,"op":"echo","args":{"x":""#;
    let tail = r#""}}"#;
    let line = format!(
        "{head}{}{tail}",
        "z".repeat(MAX_CTL_LINE_BYTES - head.len() - tail.len())
    );
    assert_eq!(line.len(), MAX_CTL_LINE_BYTES);
    let v = roundtrip(&mut c, &line).await;
    assert_eq!(v["id"], 10, "{v}");
}

#[tokio::test]
async fn ctl1_replies_follow_request_order_when_pipelined() {
    let (mut c, _h) = serve(info());
    let mut lines = String::new();
    for i in 0..200 {
        lines.push_str(&format!(r#"{{"id":{i},"op":"echo","args":{{"i":{i}}}}}"#));
        lines.push('\n');
    }
    c.write_all(lines.as_bytes()).await.unwrap();
    let mut r = BufReader::new(&mut c);
    for i in 0..200 {
        let mut out = String::new();
        r.read_line(&mut out).await.unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["id"], i, "reply {i} out of order: {out}");
    }
}

#[tokio::test]
async fn ctl1_builtin_ops_answer_without_the_handler() {
    let info = info();
    let (mut c, h) = serve(info.clone());
    let v = roundtrip(&mut c, r#"{"id":1,"op":"ping"}"#).await;
    assert_eq!(v["ok"]["pid"], std::process::id());
    assert_eq!(v["ok"]["version"], "t");
    assert_eq!(v["ok"]["sessions"], 1);
    assert_eq!(v["ok"]["connections"], 1);
    assert!(v["ok"]["uptime_ms"].is_number());
    let v = roundtrip(&mut c, r#"{"id":2,"op":"sessions"}"#).await;
    let list = v["ok"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["proto"], "ctl");
    assert_eq!(list[0]["id"], 1);
    assert!(!*info.stop.borrow());
    let v = roundtrip(&mut c, r#"{"id":3,"op":"stop"}"#).await;
    assert_eq!(v["ok"]["stopping"], true);
    assert!(*info.stop.borrow(), "stop is signalled after the reply");
    let mut r = BufReader::new(&mut c);
    let mut out = String::new();
    assert_eq!(
        r.read_line(&mut out).await.unwrap(),
        0,
        "the loop drains: closed after the stop reply"
    );
    h.await.unwrap().unwrap();
}

#[tokio::test]
async fn ctl1_an_idle_connection_closes_at_once_when_stop_is_signalled() {
    let info = info();
    let (mut c, h) = serve(info.clone());
    let v = roundtrip(&mut c, r#"{"id":1,"op":"echo"}"#).await;
    assert_eq!(v["id"], 1);
    info.stop.send(true).unwrap();
    let mut r = BufReader::new(&mut c);
    let mut out = String::new();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), r.read_line(&mut out))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    h.await.unwrap().unwrap();
}

#[tokio::test]
async fn ctl1_a_panicking_handler_answers_e_internal_and_serves_on() {
    let (mut c, _h) = serve(info());
    let v = roundtrip(&mut c, r#"{"id":1,"op":"panic"}"#).await;
    assert_eq!(v["error"]["code"], "E_INTERNAL");
    assert!(
        v["error"]["message"].as_str().unwrap().contains("boom"),
        "{v}"
    );
    let v = roundtrip(&mut c, r#"{"id":2,"op":"echo"}"#).await;
    assert_eq!(v["id"], 2);
}

#[tokio::test]
async fn ctl1_a_connection_closed_mid_line_ends_the_loop_cleanly() {
    let (mut c, h) = serve(info());
    c.write_all(br#"{"id":1,"op":"echo","args":{"half"#)
        .await
        .unwrap();
    drop(c);
    h.await.unwrap().unwrap();
}

#[test]
fn ctl1_error_reply_serializes_in_the_m1_shape() {
    let mut e = ErrorReply::new(E_LIMIT, "too big", "shrink");
    e.extra.insert("issues".into(), json!([{"code": "x"}]));
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(
        v,
        json!({"code": "E_LIMIT", "message": "too big", "fix": "shrink", "issues": [{"code": "x"}]})
    );
    let back: ErrorReply = serde_json::from_value(v).unwrap();
    assert_eq!(back, e);
    assert_eq!(e.to_string(), "E_LIMIT: too big (fix: shrink)");
    let _: &dyn std::error::Error = &e;
}

/// After the E_LIMIT reply, what the client already pipelined is read off before the close:
/// closing a socket with unread input makes Linux reset the connection, which can hide the reply.
#[tokio::test]
async fn ctl1_an_oversized_line_drains_pipelined_input_before_the_close() {
    let mut input = vec![b'a'; MAX_CTL_LINE_BYTES + 10];
    input.push(b'\n');
    // ~42 KiB queued behind the oversized line: more than one read buffer
    let pipelined = (json!({"id": 2, "op": "echo"}).to_string() + "\n").repeat(2000);
    input.extend_from_slice(pipelined.as_bytes());
    let mut r: &[u8] = &input;
    let mut out: Vec<u8> = Vec::new();
    let i = info();
    let session = i.sessions.insert(Proto::Ctl, "t".into(), "/".into(), 0);
    serve_ctl(&mut r, &mut out, session, Arc::new(Echo), i)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["error"]["code"], "E_LIMIT");
    assert!(r.is_empty(), "{} pipelined bytes left unread", r.len());
}

/// `Echo`, plus a notice on every reply.
struct Noticing;
impl Handler for Noticing {
    fn ctl(
        &self,
        s: &SessionInfo,
        r: CtlRequest,
        agent: Arc<dyn rdy_engine::signing_crypto::SigningAgent>,
    ) -> Result<Value, ErrorReply> {
        Echo.ctl(s, r, agent)
    }

    fn notice(&self, _session: &SessionInfo) -> Option<String> {
        Some("n".into())
    }
}

#[tokio::test]
async fn reload1_a_reply_carries_the_handlers_notice() {
    let served = |handler: Arc<dyn Handler>| {
        let info = info();
        let (client, server) = tokio::io::duplex(1 << 20);
        let session = info.sessions.insert(Proto::Ctl, "t".into(), "/".into(), 0);
        let (r, w) = tokio::io::split(server);
        tokio::spawn(serve_ctl(r, w, session, handler, info));
        client
    };
    let mut c = served(Arc::new(Noticing));
    let v = roundtrip(&mut c, r#"{"id":1,"op":"echo","args":{}}"#).await;
    assert_eq!(v["ok"]["args"], json!({}));
    assert_eq!(v["notice"], "n", "{v}");
    let v = roundtrip(&mut c, r#"{"id":2,"op":"ping"}"#).await;
    assert!(v["ok"]["pid"].is_number(), "{v}");
    assert_eq!(v["notice"], "n", "built-ins too: {v}");
    let mut c = served(Arc::new(Echo));
    for line in [r#"{"id":1,"op":"echo"}"#, r#"{"id":2,"op":"ping"}"#] {
        let v = roundtrip(&mut c, line).await;
        assert!(v.get("ok").is_some(), "{v}");
        assert!(v.get("notice").is_none(), "no notice key by default: {v}");
    }
}

#[tokio::test]
async fn hd3_a_ctl_request_with_an_unknown_field_is_refused_with_its_id() {
    let (mut c, _h) = serve(info());
    let v = roundtrip(&mut c, r#"{"id":4,"op":"echo","arg":{"x":1}}"#).await;
    assert_eq!(v["id"], 4, "the reply keeps the request's id: {v}");
    assert_eq!(v["error"]["code"], "E_BAD_INPUT", "{v}");
    assert_eq!(v["error"]["message"], "ctl request: unknown field \"arg\"");
    assert_eq!(
        v["error"]["fix"],
        "did you mean \"args\"? the fields are id, op, args"
    );
    assert_eq!(
        roundtrip(&mut c, r#"{"id":5,"op":"echo"}"#).await["id"],
        5,
        "the connection lives"
    );
}
