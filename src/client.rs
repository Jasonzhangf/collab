use crate::proto::{Req, Resp};
use anyhow::Context;
use serde::de::DeserializeOwned;
use std::os::unix::net::UnixStream;
use std::path::Path;

pub fn connect(sock: &Path) -> std::io::Result<UnixStream> {
    UnixStream::connect(sock)
}

/// Round-trip one request. Long-poll ops simply block on read; the server owns timeouts.
pub fn call<T: DeserializeOwned>(sock: &Path, req: &Req) -> anyhow::Result<T> {
    let mut stream = connect(sock)
        .with_context(|| format!("cannot reach collab server at {}; is it up? (`collab up`)", sock.display()))?;
    let line = serde_json::to_string(req)?;
    use std::io::{BufRead, Write};
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut reader = std::io::BufReader::new(stream);
    let mut buf = String::new();
    reader.read_line(&mut buf)?;
    let resp: Resp = serde_json::from_str(buf.trim())
        .context("malformed response from server")?;
    if !resp.ok {
        anyhow::bail!(resp.error.unwrap_or_else(|| "unknown server error".into()));
    }
    serde_json::from_value(resp.data).with_context(|| "unexpected response shape")
}

/// Check server liveness without full call.
pub fn alive(sock: &Path) -> bool {
    connect(sock).is_ok()
}
