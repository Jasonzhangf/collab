use crate::proto::{Req, Resp};
use anyhow::Context;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

pub fn connect(sock: &Path) -> std::io::Result<UnixStream> {
    UnixStream::connect(sock)
}

pub fn record_event(sock: &Path, kind: &str, detail: Value) {
    let Some(server_dir) = sock.parent() else {
        return;
    };
    let path = server_dir.join("events.jsonl");
    let record = serde_json::json!({"ts": chrono::Utc::now().timestamp_millis(), "kind": kind, "detail": detail});
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let mut line = serde_json::to_vec(&record).unwrap_or_default();
        line.push(b'\n');
        let _ = file.write_all(&line);
    }
}

/// Round-trip one request. Long-poll ops simply block on read; the server owns timeouts.
pub fn call<T: DeserializeOwned>(sock: &Path, req: &Req) -> anyhow::Result<T> {
    ensure_server(sock)?;
    let mut stream = connect(sock).with_context(|| {
        format!(
            "cannot reach collab server at {}; automatic restart failed (check server/log.txt or run `collab up`)",
            sock.display()
        )
    })?;
    let line = serde_json::to_string(req)?;
    use std::io::{BufRead, Write};
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut reader = std::io::BufReader::new(stream);
    let mut buf = String::new();
    reader.read_line(&mut buf)?;
    let resp: Resp = serde_json::from_str(buf.trim()).context("malformed response from server")?;
    if !resp.ok {
        anyhow::bail!(resp.error.unwrap_or_else(|| "unknown server error".into()));
    }
    serde_json::from_value(resp.data).with_context(|| "unexpected response shape")
}

pub fn ensure_server(sock: &Path) -> anyhow::Result<()> {
    let server_dir = sock
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid collab server socket path"))?;
    let down = server_dir.join("DOWN");
    if down.exists() {
        anyhow::bail!("collab daemon is explicitly down; run `collab up` first");
    }
    if alive(sock) {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let log_path = server_dir.join("log.txt");
    std::fs::create_dir_all(server_dir)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let err = log.try_clone()?;
    use std::os::unix::process::CommandExt;
    Command::new(exe)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err)
        .process_group(0)
        .spawn()?;
    record_event(
        sock,
        "daemon_restart_requested",
        serde_json::json!({"pid": std::process::id()}),
    );
    for _ in 0..40 {
        if alive(sock) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!(
        "collab daemon failed to restart; check {}",
        server_dir.join("log.txt").display()
    )
}

/// Check server liveness without full call.
pub fn alive(sock: &Path) -> bool {
    connect(sock).is_ok()
}
