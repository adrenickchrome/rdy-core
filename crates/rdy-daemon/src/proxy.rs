//! `rdy mcp`: a stdio-to-socket proxy. It connects to the project's daemon as an `mcp` session
//! (starting the daemon if none runs) and pumps bytes both ways, one task each way.
//!
//! Nothing here can reorder replies: one task reads stdin and writes the socket (through
//! `pump_lines`, which refuses an over-long line before the daemon sees it), one task reads the
//! socket and writes stdout, and the socket is a byte stream. The daemon answers each connection
//! in request order (its tool calls are stamped at the transport: `rdy_mcp::order`).

use crate::client::{StartOptions, connect_or_start};
use crate::ctl::MAX_CTL_LINE_BYTES;
use crate::layout::RunDir;
use crate::session::Proto;
use crate::spawn::{serve_command, spawn_serve};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const GONE: &str = "rdy mcp: the daemon closed the connection (E_DAEMON_GONE); check rdy status; a write may or may not have committed";

/// Copies the socket's read half to stdout, flushing after each chunk, until the socket's EOF
/// (or an error either side).
async fn socket_to_stdout(mut sock_r: tokio::net::unix::OwnedReadHalf) {
    let mut stdout = tokio::io::stdout();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match sock_r.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if stdout.write_all(&buf[..n]).await.is_err() || stdout.flush().await.is_err() {
            return;
        }
    }
}

/// Runs the proxy for `project` until stdin's EOF (then the daemon's session ends and the socket
/// drains: exit code 0) or until the daemon closes the connection first, or cannot be reached
/// (exit code 1, with the reason on stderr). The caller runs it on a current-thread runtime and
/// should not wait for that runtime's blocking pool at exit: a stdin read may still be pending.
pub async fn run_stdio(project: PathBuf, version: String, start_timeout: Duration) -> i32 {
    let spawn_project = project.clone();
    let opts = StartOptions {
        version,
        proto: Proto::Mcp,
        cwd: project.clone(),
        start_timeout,
        spawn: Box::new(move |run| {
            spawn_serve(&mut serve_command(
                &std::env::current_exe()?,
                &spawn_project,
                run,
            )?)
        }),
        progress: Box::new(|p| eprintln!("rdy mcp: starting: {p}")),
    };
    let client = match connect_or_start(&RunDir::of_project(&project), opts).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rdy mcp: {e}");
            return 1;
        }
    };
    let (sock_r, mut sock_w) = client.into_stream().into_split();
    // stdin to the socket; `Ok` only at stdin's EOF, after the write half is shut down
    let mut a = tokio::spawn(async move {
        rdy_mcp::server::pump_lines(
            tokio::io::stdin(),
            &mut sock_w,
            MAX_CTL_LINE_BYTES,
            &mut |n, why| eprintln!("rdy mcp: dropped a {n}-byte request line: {why}"),
        )
        .await?;
        sock_w.shutdown().await
    });
    let b = socket_to_stdout(sock_r);
    tokio::pin!(b);
    tokio::select! {
        biased;
        r = &mut a => match r {
            Ok(Ok(())) => {
                // the daemon's session ends at the EOF and closes the socket
                b.await;
                0
            }
            // a write to the socket failed: the daemon is gone
            Ok(Err(_)) | Err(_) => {
                eprintln!("{GONE}");
                1
            }
        },
        () = &mut b => {
            eprintln!("{GONE}");
            a.abort();
            1
        }
    }
}
