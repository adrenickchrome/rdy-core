//! The hello: the first line of every connection, and the daemon's replies to it.

use crate::error::{E_BAD_INPUT, ErrorReply};
use crate::session::{Proto, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

/// The hello protocol version this daemon speaks.
pub const PROTO_VERSION: u32 = 1;
/// The longest hello line, newline included.
pub const MAX_HELLO_BYTES: usize = 4096;
/// How long a new connection has to send its hello.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// `{"rdy":1,"version":"…","proto":"ctl"|"mcp","cwd":"…"}`. Unknown fields are ignored:
/// later versions may add some.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub rdy: u32,
    pub version: String,
    pub proto: Proto,
    pub cwd: PathBuf,
}

/// The daemon's answer to a hello, one JSON line each. Zero or more `starting`, then exactly one
/// of `ready`, `refused` or `exiting`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum HelloReply {
    Starting { progress: String },
    Ready { version: String, session: SessionId },
    Refused { error: ErrorReply },
    Exiting { version: String },
}

const SEND_HELLO_FIRST: &str = "send the hello line first";

fn not_a_hello() -> ErrorReply {
    ErrorReply::new(
        E_BAD_INPUT,
        r#"the first line must be a hello: {"rdy":1,"version":"…","proto":"ctl"|"mcp","cwd":"…"}"#,
        SEND_HELLO_FIRST,
    )
}

fn no_hello() -> ErrorReply {
    ErrorReply::new(E_BAD_INPUT, "no hello within 5 s", SEND_HELLO_FIRST)
}

enum Line {
    Done(Vec<u8>),
    Over,
}

/// Reads one byte at a time up to and including the newline, so nothing after the hello is
/// consumed (an MCP client may pipeline its `initialize` right behind it).
async fn read_line<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Line> {
    let mut buf = Vec::with_capacity(256);
    loop {
        let b = r.read_u8().await?;
        if b == b'\n' {
            return Ok(Line::Done(buf));
        }
        buf.push(b);
        if buf.len() >= MAX_HELLO_BYTES {
            return Ok(Line::Over);
        }
    }
}

/// Reads and checks the hello: at most `MAX_HELLO_BYTES` bytes, within `HELLO_TIMEOUT`, never
/// past its newline. EOF before the newline counts as no hello.
pub async fn read_hello<R: AsyncRead + Unpin>(r: &mut R) -> Result<Hello, ErrorReply> {
    let line = match tokio::time::timeout(HELLO_TIMEOUT, read_line(r)).await {
        Ok(Ok(Line::Done(line))) => line,
        Ok(Ok(Line::Over)) => {
            return Err(ErrorReply::new(
                E_BAD_INPUT,
                format!("the hello line is over {MAX_HELLO_BYTES} bytes"),
                SEND_HELLO_FIRST,
            ));
        }
        // EOF or a read error: the caller closes either way
        Ok(Err(_)) | Err(_) => return Err(no_hello()),
    };
    let value: Value = serde_json::from_slice(&line).map_err(|_| not_a_hello())?;
    // the version is checked before the shape, so a later protocol's hello is told why
    if let Some(n) = value.get("rdy").and_then(Value::as_u64)
        && n != u64::from(PROTO_VERSION)
    {
        return Err(ErrorReply::new(
            E_BAD_INPUT,
            format!("unsupported hello protocol version {n}; this daemon speaks {PROTO_VERSION}"),
            "upgrade rdy",
        ));
    }
    serde_json::from_value(value).map_err(|_| not_a_hello())
}
