//! The common daemon harness (wx21a): it reaches the daemon only through the `sys` seam, and its
//! fixtures leave no handle open.
mod common;

use common::{Conn, connect_with, ctl, daemon, hello};
use rdy_daemon::ExitReason;
use rdy_daemon::sys::{self, Liveness};
use serde_json::{Value, json};
use tokio::io::BufReader;

#[tokio::test]
async fn wx21a_common_harness_uses_native_transport() {
    let d = daemon("1.0.0", true, |_| {}).await;
    let cwd = d.dir.path().to_path_buf();
    // the reader is over the seam's own stream: a Unix socket or a named pipe
    let (mut r, first): (BufReader<Conn>, String) =
        connect_with(&d.run, &hello("1.0.0", "ctl", &cwd)).await;
    let first: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(first["status"], json!("ready"), "{first}");
    let reply = ctl(&mut r, 1, "echo", json!({"k": "v"})).await;
    assert_eq!(reply["ok"]["args"], json!({"k": "v"}), "{reply}");
    drop(r);
    // the record helper writes the grammar the daemon itself wrote
    let project = d.run.project_id().unwrap();
    let socket = d.run.recorded_socket().expect("the daemon's record");
    std::fs::remove_file(d.run.socket_record()).unwrap();
    common::write_record(&d.run, &socket, &project);
    assert_eq!(d.run.recorded_socket(), Some(socket));
    assert!(matches!(d.finish().await, ExitReason::Stopped));
}

#[tokio::test]
async fn wx21a_fixture_cleanup_closes_every_handle() {
    let d = daemon("1.0.0", true, |_| {}).await;
    let cwd = d.dir.path().to_path_buf();
    let socket = d.run.socket();
    let (mut r, _) = connect_with(&d.run, &hello("1.0.0", "ctl", &cwd)).await;
    ctl(&mut r, 1, "echo", json!({})).await;
    drop(r);
    // `finish` removes the directory: it fails while the daemon or the fixture holds a handle
    d.finish().await;
    assert!(!cwd.exists());
    assert_eq!(sys::endpoint_liveness(&socket).await, Liveness::Gone);
}
