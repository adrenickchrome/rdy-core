//! The event stream of an open live page (`/_rdy/events`): its head, its frames and its
//! keep-alive, written to the page's connection until the stream ends.
//!
//! The stream ends when its queue closes (the site dropped it, or closed), a write fails, the
//! reader goes away, or the daemon stops. While it runs, it holds the daemon up ([`IdleHold`]).
//! The page keeps its stream open only while it is in view (`live.js`, ruling task-6), so a page
//! in a background tab neither holds the daemon nor takes one of the browser's few connections
//! to the site.
use crate::daemon::IdleHold;
use crate::site::http::{BASE_HEADERS, http_date};
use crate::site::live::{LiveEvent, Subscription};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;

/// The reconnect delay the stream asks of the reader, in ms.
pub const RETRY: &[u8] = b"retry: 2000\n\n";
/// The comment a quiet stream sends, so a dead reader shows as a failed write.
pub const KEEP_ALIVE: &[u8] = b": keep-alive\n\n";
/// How long a stream stays quiet before it sends [`KEEP_ALIVE`].
pub const KEEP_ALIVE_EVERY: Duration = Duration::from_secs(15);

/// The stream's head: `200 OK`, `Date`, the `BASE_HEADERS` and the event-stream content type,
/// with no `Content-Length` (the response ends when the connection closes).
pub fn head(now_secs: i64) -> Vec<u8> {
    let mut head = format!("HTTP/1.1 200 OK\r\nDate: {}\r\n", http_date(now_secs));
    for (name, value) in BASE_HEADERS {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("Content-Type: text/event-stream; charset=utf-8\r\n\r\n");
    head.into_bytes()
}

/// The frame of `event`: `reload` with `{}`, or `label` with its text and the header's markup
/// as JSON strings, `text` first.
pub fn frame(event: &LiveEvent) -> Vec<u8> {
    match event {
        LiveEvent::Reload => b"event: reload\ndata: {}\n\n".to_vec(),
        LiveEvent::Label { text, html } => {
            let json = |s: &str| serde_json::Value::from(s).to_string();
            format!(
                "event: label\ndata: {{\"text\":{},\"html\":{}}}\n\n",
                json(text),
                json(html)
            )
            .into_bytes()
        }
    }
}

/// Writes the stream of `sub` to `conn` until it ends (see the module docs). `hold` keeps the
/// daemon up meanwhile; `stopped` turns true when the daemon stops.
pub async fn serve(
    mut conn: TcpStream,
    mut sub: Subscription,
    hold: IdleHold,
    mut stopped: watch::Receiver<bool>,
    now_secs: i64,
) {
    let _hold = hold;
    let mut first = head(now_secs);
    first.extend_from_slice(RETRY);
    let (mut read, mut write) = conn.split();
    if write.write_all(&first).await.is_err() {
        return;
    }
    let quiet = tokio::time::sleep(KEEP_ALIVE_EVERY);
    tokio::pin!(quiet);
    let mut scratch = [0u8; 512];
    loop {
        if *stopped.borrow_and_update() {
            return;
        }
        let out = tokio::select! {
            changed = stopped.changed() => {
                if changed.is_err() {
                    return;
                }
                continue;
            }
            event = sub.rx.recv() => match event {
                Some(event) => frame(&event),
                None => return,
            },
            () = &mut quiet => KEEP_ALIVE.to_vec(),
            // the reader sends nothing after its request: an end or an error is its going away
            got = read.read(&mut scratch) => match got {
                Ok(0) | Err(_) => return,
                Ok(_) => continue,
            },
        };
        if write.write_all(&out).await.is_err() {
            return;
        }
        quiet
            .as_mut()
            .reset(tokio::time::Instant::now() + KEEP_ALIVE_EVERY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::http::CSP;

    #[test]
    fn ls6_the_stream_bytes() {
        let head = String::from_utf8(head(0)).unwrap();
        assert_eq!(
            head,
            format!(
                "HTTP/1.1 200 OK\r\nDate: Thu, 01 Jan 1970 00:00:00 GMT\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: {CSP}\r\nConnection: close\r\nContent-Type: text/event-stream; charset=utf-8\r\n\r\n"
            )
        );
        assert_eq!(frame(&LiveEvent::Reload), b"event: reload\ndata: {}\n\n");
        assert_eq!(
            String::from_utf8(frame(&LiveEvent::Label {
                text: "v1.0.0 \"a\"\nb".into(),
                html: "<a>\"x\"</a>".into()
            }))
            .unwrap(),
            "event: label\ndata: {\"text\":\"v1.0.0 \\\"a\\\"\\nb\",\"html\":\"<a>\\\"x\\\"</a>\"}\n\n"
        );
    }

    #[test]
    fn sr5_a_label_event_carries_text_and_header() {
        let event = LiveEvent::Label {
            text: "v1.0.0".into(),
            html: "<a class=\"pill\">x</a>".into(),
        };
        assert_eq!(
            String::from_utf8(frame(&event)).unwrap(),
            "event: label\ndata: {\"text\":\"v1.0.0\",\"html\":\"<a class=\\\"pill\\\">x</a>\"}\n\n"
        );
    }
}
