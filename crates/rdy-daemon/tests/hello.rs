//! The hello, starting/ready and the version handshake (hello1).
mod common;
use common::*;
use rdy_daemon::*;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn parsed(line: &str) -> Value {
    serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
}

#[tokio::test]
async fn hello1_a_ready_daemon_answers_ready_with_the_session_id() {
    let d = daemon("0.1.0", true, |_| {}).await;
    let (mut r, first) = connect_with(&d.run, &hello("0.1.0", "ctl", d.dir.path())).await;
    let v = parsed(&first);
    assert_eq!(v["status"], "ready");
    assert_eq!(v["version"], "0.1.0");
    assert_eq!(v["session"], 1);
    let v = ctl(&mut r, 1, "echo", json!({"a": 1})).await;
    assert_eq!(v["ok"]["session"], 1);
    assert_eq!(v["ok"]["cwd"], json!(d.dir.path()));
    assert_eq!(v["ok"]["args"]["a"], 1);
    drop(r);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        *d.echo.ended.lock().unwrap(),
        vec![SessionId(1)],
        "session_ended once"
    );
    assert_eq!(d.handle.sessions().len(), 0);
    d.handle.stop();
}

#[tokio::test]
async fn hello1_a_starting_daemon_streams_progress_then_ready() {
    let d = daemon("1", false, |_| {}).await;
    d.readiness.progress("catching up 1/3");
    let mut s = UnixStream::connect(d.run.socket()).await.unwrap();
    s.write_all(hello("1", "ctl", d.dir.path()).as_bytes())
        .await
        .unwrap();
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line).await.unwrap();
    let v = parsed(&line);
    assert_eq!(
        (v["status"].as_str(), v["progress"].as_str()),
        (Some("starting"), Some("catching up 1/3"))
    );
    tokio::time::sleep(Duration::from_millis(250)).await;
    d.readiness.progress("catching up 3/3");
    tokio::time::sleep(Duration::from_millis(250)).await;
    d.readiness.ready();
    let mut seen = vec![];
    loop {
        line.clear();
        r.read_line(&mut line).await.unwrap();
        let v = parsed(&line);
        seen.push(v["status"].as_str().unwrap().to_string());
        if v["status"] == "ready" {
            break;
        }
    }
    assert_eq!(
        seen,
        vec!["starting", "ready"],
        "one more starting, then ready"
    );
    assert_eq!(
        d.handle.sessions().len(),
        1,
        "a session exists only from ready"
    );
    d.handle.stop();
}

#[tokio::test]
async fn hello1_junk_and_oversized_hellos_are_refused_with_a_reason() {
    let d = daemon("1", true, |_| {}).await;
    let cases: Vec<(String, &str)> = vec![
        ("hello\n".to_string(), "the first line must be a hello"),
        (
            "{\"rdy\":1}\n".to_string(),
            "the first line must be a hello",
        ),
        (
            "{\"rdy\":2,\"version\":\"1\",\"proto\":\"ctl\",\"cwd\":\"/\"}\n".to_string(),
            "unsupported hello protocol version 2",
        ),
        (
            format!(
                "{{\"rdy\":1,\"version\":\"1\",\"proto\":\"ctl\",\"cwd\":\"/{}\"}}\n",
                "x".repeat(5000)
            ),
            "over 4096 bytes",
        ),
        ("{".repeat(5000), "over 4096 bytes"), // no newline at all
    ];
    for (line, needle) in &cases {
        let (mut r, first) = connect_with(&d.run, line).await;
        let v = parsed(&first);
        assert_eq!(v["status"], "refused", "{line:.60}");
        assert_eq!(v["error"]["code"], "E_BAD_INPUT", "{line:.60}");
        assert!(
            v["error"]["message"].as_str().unwrap().contains(needle),
            "{line:.60}: {v}"
        );
        let mut rest = String::new();
        assert_eq!(
            r.read_line(&mut rest).await.unwrap(),
            0,
            "closed after the refusal"
        );
    }
    assert_eq!(d.handle.sessions().len(), 0);
    assert!(
        d.echo.ended.lock().unwrap().is_empty(),
        "no session, no end hook"
    );
    d.handle.stop();
}

#[tokio::test]
async fn hello1_a_connection_that_never_sends_a_hello_is_closed_after_the_timeout() {
    let d = daemon("1", true, |_| {}).await;
    let s = UnixStream::connect(d.run.socket()).await.unwrap();
    let mut r = BufReader::new(s);
    let start = std::time::Instant::now();
    let mut out = String::new();
    r.read_line(&mut out).await.unwrap();
    let v = parsed(&out);
    assert_eq!(v["error"]["message"], "no hello within 5 s");
    assert!(start.elapsed() >= Duration::from_secs(5) && start.elapsed() < Duration::from_secs(8));
    tokio::time::sleep(Duration::from_millis(100)).await; // the connection task finishes after its reply
    assert_eq!(d.handle.connections(), 0);
    d.handle.stop();
}

#[tokio::test]
async fn hello1_another_version_with_sessions_is_refused_e_daemon_version() {
    let d = daemon("0.1.0", true, |_| {}).await;
    let (_keep, first) = connect_with(&d.run, &hello("0.1.0", "ctl", d.dir.path())).await;
    assert!(first.contains("ready"));
    let (_r, second) = connect_with(&d.run, &hello("0.2.0", "ctl", d.dir.path())).await;
    let v = parsed(&second);
    assert_eq!(v["status"], "refused");
    assert_eq!(v["error"]["code"], "E_DAEMON_VERSION");
    assert_eq!(
        v["error"]["message"],
        "the running daemon is rdy 0.1.0, this client is rdy 0.2.0"
    );
    assert_eq!(
        v["error"]["fix"],
        "close the other sessions or run rdy serve --stop"
    );
    assert_eq!(d.handle.sessions().len(), 1, "the newcomer got no session");
    d.handle.stop();
}

#[tokio::test]
async fn hello1_another_version_with_no_sessions_makes_the_daemon_exit() {
    let d = daemon("0.1.0", true, |_| {}).await;
    let (_r, first) = connect_with(&d.run, &hello("0.2.0", "ctl", d.dir.path())).await;
    let v = parsed(&first);
    assert_eq!(v["status"], "exiting");
    assert_eq!(v["version"], "0.1.0");
    let reason = tokio::time::timeout(Duration::from_secs(6), d.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(reason, ExitReason::VersionReplaced), "{reason:?}");
    assert!(
        !d.run.socket().exists(),
        "the socket is gone, so the client's next connect fails and it spawns"
    );
}

#[tokio::test]
async fn hello1_an_mcp_hello_hands_the_stream_to_the_handler_with_pipelined_bytes_intact() {
    use std::sync::{Arc, Mutex};
    struct Grab(Mutex<Option<tokio::sync::oneshot::Sender<(Proto, String)>>>);
    impl Handler for Grab {
        fn ctl(&self, _: &SessionInfo, _: CtlRequest) -> Result<Value, ErrorReply> {
            unreachable!()
        }
        fn mcp(
            &self,
            s: SessionInfo,
            stream: UnixStream,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
            let tx = self.0.lock().unwrap().take().unwrap();
            Box::pin(async move {
                let mut r = BufReader::new(stream);
                let mut line = String::new();
                r.read_line(&mut line).await.unwrap();
                let _ = tx.send((s.proto, line));
            })
        }
    }
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut d = Daemon::new(
        DaemonConfig::new(run.clone(), "1".into()),
        Arc::new(Grab(Mutex::new(Some(tx)))),
    );
    d.bind().await.unwrap();
    let (handle, readiness) = (d.handle(), d.readiness());
    readiness.ready();
    let task = tokio::spawn(d.run());
    let mut s = UnixStream::connect(run.socket()).await.unwrap();
    let both = hello("1", "mcp", dir.path())
        + "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n";
    s.write_all(both.as_bytes()).await.unwrap();
    let mut r = BufReader::new(&mut s);
    let mut first = String::new();
    r.read_line(&mut first).await.unwrap();
    assert!(first.contains("ready"), "{first}");
    let (proto, got) = tokio::time::timeout(Duration::from_secs(3), rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        got.trim(),
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}",
        "the byte after the hello reached the handler"
    );
    assert_eq!(proto, Proto::Mcp);
    handle.stop();
    task.await.unwrap().unwrap();
}
