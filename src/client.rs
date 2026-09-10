#[path = "adapters/mod.rs"]
pub mod adapters;

use crate::identity::RuntimeIdentity;
use crate::proto::{ProjectContext, Req, RequestEnvelope, Resp};
use anyhow::Context;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const READINESS_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, serde::Deserialize)]
struct PingReadiness {
    workers: u64,
    messages: u64,
    tasks: u64,
    now: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonAvailability {
    Alive,
    Starting,
    Unavailable,
    Unknown,
}

impl DaemonAvailability {
    fn code(self) -> &'static str {
        match self {
            Self::Alive => "DAEMON_ALIVE",
            Self::Starting => "DAEMON_STARTING",
            Self::Unavailable => "DAEMON_UNAVAILABLE",
            Self::Unknown => "DAEMON_UNKNOWN",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockAvailability {
    Held,
    Unheld,
    Unknown,
}

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

/// Round-trip one request without changing daemon state.
///
/// The caller must have an already-running daemon. In particular, this path
/// never creates a server directory, starts a process, or records a client
/// event when the socket cannot be reached.
pub fn call<T: DeserializeOwned>(sock: &Path, req: &Req) -> anyhow::Result<T> {
    call_with_context(sock, req, None)
}

/// Round-trip one request with an explicit registered project context.  The
/// context is part of the wire envelope, while the v1 operation keeps its
/// original tagged shape for compatibility with existing adapters.
pub fn call_with_context<T: DeserializeOwned>(
    sock: &Path,
    req: &Req,
    project_context: Option<ProjectContext>,
) -> anyhow::Result<T> {
    if let Some(project_context) = project_context.as_ref() {
        project_context.validate()?;
    }
    let mut stream = connect(sock).map_err(|error| connection_error(sock, error))?;
    let line = serde_json::to_string(&RequestEnvelope::new(req.clone(), project_context))?;
    stream.write_all(line.as_bytes()).with_context(|| {
        format!(
            "DAEMON_UNKNOWN: failed to send request to {}",
            sock.display()
        )
    })?;
    stream.write_all(b"\n").with_context(|| {
        format!(
            "DAEMON_UNKNOWN: failed to send request to {}",
            sock.display()
        )
    })?;
    stream.flush().with_context(|| {
        format!(
            "DAEMON_UNKNOWN: failed to flush request to {}",
            sock.display()
        )
    })?;
    let mut reader = std::io::BufReader::new(stream);
    let mut buf = String::new();
    let bytes_read = reader.read_line(&mut buf).with_context(|| {
        format!(
            "DAEMON_UNKNOWN: failed to read response from {}",
            sock.display()
        )
    })?;
    if bytes_read == 0 {
        anyhow::bail!(
            "DAEMON_UNKNOWN: daemon closed the connection before replying at {}",
            sock.display()
        );
    }
    let resp: Resp = serde_json::from_str(buf.trim())
        .context("DAEMON_UNKNOWN: malformed response from server")?;
    if !resp.ok {
        anyhow::bail!(format!(
            "{}{}",
            if resp.error.is_none() {
                "DAEMON_UNKNOWN: "
            } else {
                ""
            },
            resp.error.unwrap_or_else(|| "unknown server error".into())
        ));
    }
    serde_json::from_value(resp.data).with_context(|| {
        format!(
            "DAEMON_UNKNOWN: unexpected response shape from {}",
            sock.display()
        )
    })
}

/// Round-trip a request using the appserver identity registered by the
/// caller.  The identity is typed and validated before it becomes route
/// context; role labels and other user-provided strings are never consulted.
pub fn call_with_runtime_identity<T: DeserializeOwned>(
    sock: &Path,
    req: &Req,
    identity: &RuntimeIdentity,
) -> anyhow::Result<T> {
    let root = crate::scope::project_root()?;
    call_with_runtime_identity_at_root(sock, req, &root, identity)
}

/// Variant for callers that already resolved the exact registered project
/// root.  Keeping root resolution explicit avoids searching ancestors or
/// silently selecting a project while constructing a route context.
pub fn call_with_runtime_identity_at_root<T: DeserializeOwned>(
    sock: &Path,
    req: &Req,
    root: &Path,
    identity: &RuntimeIdentity,
) -> anyhow::Result<T> {
    call_with_runtime_identity_at_root_selected_endpoint(sock, req, root, identity, None)
}

#[cfg(test)]
pub(crate) fn call_with_runtime_identity_at_root_for_endpoint<T: DeserializeOwned>(
    sock: &Path,
    req: &Req,
    root: &Path,
    identity: &RuntimeIdentity,
    endpoint: adapters::EndpointKind,
) -> anyhow::Result<T> {
    call_with_runtime_identity_at_root_selected_endpoint(sock, req, root, identity, Some(endpoint))
}

fn call_with_runtime_identity_at_root_selected_endpoint<T: DeserializeOwned>(
    sock: &Path,
    req: &Req,
    root: &Path,
    identity: &RuntimeIdentity,
    explicit_endpoint: Option<adapters::EndpointKind>,
) -> anyhow::Result<T> {
    let project_context = ProjectContext::for_registered_route(root, identity)?;
    let envelope = RequestEnvelope::new(req.clone(), Some(project_context));
    let binding = match explicit_endpoint {
        Some(endpoint) => Some(adapters::EndpointBinding::new(endpoint, identity)),
        None => adapters::binding_for_request(identity, &envelope)?,
    };
    if let Some(binding) = binding {
        // An explicitly selected AppServer owns this attempt. Adapter errors
        // are returned directly; the daemon remains a separate compatibility
        // route when no AppServer endpoint was selected.
        let adapter = adapters::AdapterRegistry::new().adapter(binding.endpoint);
        let receipt = adapters::submit_registered(&adapter, &binding, &envelope)?;
        return serde_json::from_value(receipt.response).with_context(|| {
            format!(
                "ADAPTER_UNKNOWN_RESPONSE: unexpected response shape from {} endpoint",
                binding.endpoint.as_str()
            )
        });
    }
    call_with_context(sock, req, envelope.project_context)
}

pub fn daemon_locked(server_dir: &Path) -> bool {
    matches!(lock_availability(server_dir), LockAvailability::Held)
}

pub fn ensure_server(sock: &Path) -> anyhow::Result<()> {
    ensure_server_with_launcher(sock, spawn_server)
}

fn ensure_server_with_launcher<F>(sock: &Path, mut launch: F) -> anyhow::Result<()>
where
    F: FnMut(&Path) -> anyhow::Result<()>,
{
    let server_dir = sock
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid collab server socket path"))?;
    let down = server_dir.join("DOWN");
    if down.exists() {
        anyhow::bail!(
            "DAEMON_UNAVAILABLE: collab daemon is explicitly down; run `collab up` first"
        );
    }
    match daemon_status(sock) {
        DaemonAvailability::Alive => Ok(()),
        DaemonAvailability::Starting => wait_for_server(sock),
        DaemonAvailability::Unavailable => {
            launch(sock)?;
            wait_for_server(sock)
        }
        DaemonAvailability::Unknown => Err(status_error(
            sock,
            DaemonAvailability::Unknown,
            "socket state is ambiguous; refusing to replace it",
        )),
    }
}

fn spawn_server(sock: &Path) -> anyhow::Result<()> {
    let server_dir = sock
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid collab server socket path"))?;
    let exe = std::env::current_exe()?;
    let log_path = server_dir.join("log.txt");
    std::fs::create_dir_all(server_dir)?;
    let log = OpenOptions::new()
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
    Ok(())
}

fn wait_for_server(sock: &Path) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        if daemon_status(sock) == DaemonAvailability::Alive {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let status = daemon_status(sock);
    let detail = match status {
        DaemonAvailability::Alive => return Ok(()),
        DaemonAvailability::Starting => {
            "daemon did not become reachable while its lock remained held"
        }
        DaemonAvailability::Unavailable => "daemon failed to become reachable after launch",
        DaemonAvailability::Unknown => "daemon reachability is unknown after launch",
    };
    Err(status_error(sock, status, detail))
}

fn readiness_probe(sock: &Path) -> io::Result<()> {
    let mut stream = connect(sock)?;
    stream.set_write_timeout(Some(READINESS_TIMEOUT))?;
    stream.set_read_timeout(Some(READINESS_TIMEOUT))?;
    let request = serde_json::to_vec(&Req::Ping).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cannot encode daemon readiness request: {error}"),
        )
    })?;
    stream.write_all(&request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut response = String::new();
    let bytes_read = io::BufReader::new(&mut stream).read_line(&mut response)?;
    if bytes_read == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "daemon closed the readiness connection before replying",
        ));
    }
    let response: Resp = serde_json::from_str(response.trim()).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("daemon readiness response was malformed: {error}"),
        )
    })?;
    if !response.ok {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            response
                .error
                .unwrap_or_else(|| "daemon readiness Ping failed".into()),
        ));
    }
    let readiness: PingReadiness = serde_json::from_value(response.data).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("daemon readiness response did not match the Ping contract: {error}"),
        )
    })?;
    let _ = (readiness.workers, readiness.messages, readiness.tasks);
    if readiness.now.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "daemon Ping returned an empty timestamp",
        ));
    }
    Ok(())
}

/// Check whether the daemon endpoint is accepting connections.
///
/// This intentionally does not perform a Ping.  The shutdown command uses
/// this predicate to decide whether it must send the shutdown request; a
/// readiness timeout must never be treated as proof that a listening daemon
/// has already stopped.
pub fn alive(sock: &Path) -> bool {
    connect(sock).is_ok()
}

#[cfg(test)]
mod route_context_tests {
    use super::*;
    use crate::identity::{AgentId, AppServerId, BindingId, NativeThreadId, RuntimeId};
    use std::os::unix::net::UnixListener;

    #[test]
    fn explicit_runtime_identity_is_project_context_source() {
        let root = std::env::temp_dir().join(format!(
            "collab-client-context-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let socket = std::path::PathBuf::from(format!(
            "/tmp/collab-r8-client-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        let identity = RuntimeIdentity {
            agent_id: AgentId::new("agent-1").unwrap(),
            runtime_id: RuntimeId::new("runtime-1").unwrap(),
            appserver_id: AppServerId::new("real-appserver-1").unwrap(),
            endpoint_generation: 1,
            binding_id: BindingId::new("binding-1").unwrap(),
            native_thread_id: Some(NativeThreadId::new("thread-1").unwrap()),
        };
        let expected_root = std::fs::canonicalize(&root).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let envelope: RequestEnvelope = serde_json::from_str(line.trim()).unwrap();
            let context = envelope.project_context.unwrap();
            assert_eq!(context.app_scope_id.as_str(), "real-appserver-1");
            assert_eq!(context.canonical_root, expected_root.to_str().unwrap());
            assert_eq!(
                context.project_scope.as_str(),
                expected_root.to_str().unwrap()
            );
            let mut stream = reader.into_inner();
            stream
                .write_all(
                    br#"{"ok":true,"answer":1}
"#,
                )
                .unwrap();
        });

        let response: Value =
            call_with_runtime_identity_at_root(&socket, &Req::Ping, &root, &identity).unwrap();
        assert_eq!(response["answer"], 1);
        server.join().unwrap();
        std::fs::remove_file(&socket).ok();
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn legacy_call_does_not_guess_an_app_scope() {
        let root = std::env::temp_dir().join(format!(
            "collab-client-no-context-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let socket = std::path::PathBuf::from(format!(
            "/tmp/collab-r8-no-context-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let envelope: RequestEnvelope = serde_json::from_str(line.trim()).unwrap();
            assert!(envelope.project_context.is_none());
            let mut stream = reader.into_inner();
            stream
                .write_all(
                    br#"{"ok":true,"answer":1}
"#,
                )
                .unwrap();
        });
        let response: Value = call(&socket, &Req::Ping).unwrap();
        assert_eq!(response["answer"], 1);
        server.join().unwrap();
        std::fs::remove_file(&socket).ok();
        std::fs::remove_dir_all(root).ok();
    }
}

/// Classify an existing daemon endpoint without creating or deleting any file.
pub fn daemon_status(sock: &Path) -> DaemonAvailability {
    match readiness_probe(sock) {
        Ok(()) => DaemonAvailability::Alive,
        Err(error) => failed_connection_status(sock, &error),
    }
}

fn connection_error(sock: &Path, error: io::Error) -> anyhow::Error {
    let status = failed_connection_status(sock, &error);
    anyhow::Error::new(error).context(format!(
        "{}: cannot reach collab daemon at {}",
        status.code(),
        sock.display()
    ))
}

fn status_error(sock: &Path, status: DaemonAvailability, detail: &str) -> anyhow::Error {
    anyhow::anyhow!("{}: {} at {}", status.code(), detail, sock.display())
}

fn failed_connection_status(sock: &Path, error: &io::Error) -> DaemonAvailability {
    if error.kind() != io::ErrorKind::NotFound {
        return DaemonAvailability::Unknown;
    }
    let socket_present = match std::fs::symlink_metadata(sock) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(_) => return DaemonAvailability::Unknown,
    };
    let Some(server_dir) = sock.parent() else {
        return DaemonAvailability::Unknown;
    };
    match (socket_present, lock_availability(server_dir)) {
        (false, LockAvailability::Held) => DaemonAvailability::Starting,
        (true, LockAvailability::Held) => DaemonAvailability::Unknown,
        (false, LockAvailability::Unheld) => DaemonAvailability::Unavailable,
        (true, LockAvailability::Unheld) => DaemonAvailability::Unknown,
        (_, LockAvailability::Unknown) => DaemonAvailability::Unknown,
    }
}

fn lock_availability(server_dir: &Path) -> LockAvailability {
    let lock_path = server_dir.join("daemon.lock");
    match std::fs::symlink_metadata(&lock_path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return LockAvailability::Unheld;
        }
        Err(_) => return LockAvailability::Unknown,
    }
    let file = match OpenOptions::new().read(true).write(true).open(lock_path) {
        Ok(file) => file,
        Err(_) => return LockAvailability::Unknown,
    };
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        let unlock_rc = unsafe { libc::flock(fd, libc::LOCK_UN) };
        return if unlock_rc == 0 {
            LockAvailability::Unheld
        } else {
            LockAvailability::Unknown
        };
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK
    ) {
        LockAvailability::Held
    } else {
        LockAvailability::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs::{self, File};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempServerDir(PathBuf);

    impl TempServerDir {
        fn new(test: &str) -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("collab-client-{test}-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create client fixture directory");
            Self(path)
        }

        fn socket(&self) -> PathBuf {
            self.0.join("server.sock")
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempServerDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn hold_lock(server_dir: &Path) -> File {
        let path = server_dir.join("daemon.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .expect("create lock fixture");
        use std::os::unix::io::AsRawFd;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0, "hold fixture lock");
        file
    }

    #[test]
    fn daemon_status_reports_active_socket_as_alive() {
        let fixture = TempServerDir::new("alive");
        let listener = UnixListener::bind(fixture.socket()).expect("bind active socket");
        let responder = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept readiness probe");
            stream
                .write_all(
                    br#"{"ok":true,"workers":0,"messages":0,"tasks":0,"now":"now"}
"#,
                )
                .expect("write readiness response");
            thread::sleep(Duration::from_millis(500));
        });

        assert_eq!(daemon_status(&fixture.socket()), DaemonAvailability::Alive);
        assert!(alive(&fixture.socket()));

        responder.join().expect("readiness responder");
    }

    #[test]
    fn ping_timeout_does_not_make_listening_socket_look_stopped_to_down() {
        let fixture = TempServerDir::new("down-ping-timeout");
        let listener = UnixListener::bind(fixture.socket()).expect("bind listening socket");
        let responder = thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = listener.accept().expect("accept liveness connection");
                thread::sleep(READINESS_TIMEOUT + Duration::from_millis(100));
                drop(stream);
            }
        });

        // Cmd::Down uses alive(), so a listening endpoint must still be
        // considered present even when its typed Ping is not answering.
        assert!(alive(&fixture.socket()));
        assert_eq!(
            daemon_status(&fixture.socket()),
            DaemonAvailability::Unknown
        );

        responder.join().expect("liveness responder");
    }

    #[test]
    fn daemon_status_reports_lock_held_without_socket_as_starting() {
        let fixture = TempServerDir::new("starting");
        let _lock = hold_lock(fixture.path());

        assert!(!fixture.socket().exists());
        assert_eq!(
            daemon_status(&fixture.socket()),
            DaemonAvailability::Starting
        );
    }

    #[test]
    fn daemon_status_reports_stale_socket_as_unknown() {
        let fixture = TempServerDir::new("stale");
        let listener = UnixListener::bind(fixture.socket()).expect("bind stale socket");
        drop(listener);

        assert!(fixture.socket().exists());
        assert_eq!(
            daemon_status(&fixture.socket()),
            DaemonAvailability::Unknown
        );
    }

    #[test]
    fn daemon_status_does_not_treat_stale_socket_with_lock_as_starting() {
        let fixture = TempServerDir::new("stale-locked");
        let listener = UnixListener::bind(fixture.socket()).expect("bind stale socket");
        drop(listener);
        let _lock = hold_lock(fixture.path());

        assert_eq!(
            daemon_status(&fixture.socket()),
            DaemonAvailability::Unknown
        );
    }

    #[test]
    fn call_missing_socket_is_unavailable_and_does_not_spawn_or_write_state() {
        let fixture = TempServerDir::new("no-spawn");
        let error = call::<serde_json::Value>(&fixture.socket(), &Req::Ping)
            .expect_err("missing daemon must fail closed");

        assert!(error.to_string().contains("DAEMON_UNAVAILABLE"));
        assert!(!fixture.socket().exists());
        for name in ["daemon.lock", "journal.jsonl", "events.jsonl", "log.txt"] {
            assert!(!fixture.path().join(name).exists(), "unexpected {name}");
        }
    }

    #[test]
    fn call_stale_socket_is_unknown_and_preserves_connect_error() {
        let fixture = TempServerDir::new("unknown");
        let listener = UnixListener::bind(fixture.socket()).expect("bind stale socket");
        drop(listener);

        let error = call::<serde_json::Value>(&fixture.socket(), &Req::Ping)
            .expect_err("stale daemon endpoint must fail closed");

        assert!(error.to_string().contains("DAEMON_UNKNOWN"));
        assert!(error
            .chain()
            .any(|cause| cause.to_string().contains("refused")));
        assert!(
            fixture.socket().exists(),
            "client must not delete stale socket"
        );
    }

    #[test]
    fn ensure_server_launcher_seam_preserves_explicit_start_path() {
        let fixture = TempServerDir::new("launcher");
        let listener = Arc::new(Mutex::new(None));
        let retained = Arc::clone(&listener);

        ensure_server_with_launcher(&fixture.socket(), move |sock| {
            let bound = UnixListener::bind(sock)?;
            let responder = bound.try_clone()?;
            thread::spawn(move || {
                let (mut stream, _) = responder.accept().expect("accept readiness probe");
                let mut request = String::new();
                std::io::BufReader::new(&mut stream)
                    .read_line(&mut request)
                    .expect("read readiness request");
                stream
                    .write_all(
                        b"{\"ok\":true,\"workers\":0,\"messages\":0,\"tasks\":0,\"now\":\"now\"}\n",
                    )
                    .expect("write readiness response");
            });
            *retained.lock().expect("listener fixture lock") = Some(bound);
            Ok(())
        })
        .expect("explicit launcher should satisfy ensure_server");

        assert!(listener.lock().expect("listener fixture lock").is_some());
    }

    #[test]
    fn ensure_server_refuses_to_replace_stale_socket() {
        let fixture = TempServerDir::new("stale-ensure");
        let listener = UnixListener::bind(fixture.socket()).expect("bind stale socket");
        drop(listener);
        let launched = Arc::new(Mutex::new(false));
        let observed = Arc::clone(&launched);

        let error = ensure_server_with_launcher(&fixture.socket(), move |_| {
            *observed.lock().expect("launcher fixture lock") = true;
            Ok(())
        })
        .expect_err("ambiguous socket must fail closed");

        assert!(error.to_string().contains("DAEMON_UNKNOWN"));
        assert!(!*launched.lock().expect("launcher fixture lock"));
        assert!(
            fixture.socket().exists(),
            "client must not delete stale socket"
        );
    }

    #[test]
    fn call_active_socket_round_trips_without_starting_a_daemon() {
        let fixture = TempServerDir::new("round-trip");
        let listener = UnixListener::bind(fixture.socket()).expect("bind active socket");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client");
            let mut request = String::new();
            std::io::BufReader::new(&mut stream)
                .read_line(&mut request)
                .expect("read request");
            assert!(request.contains("\"op\":\"Ping\""));
            stream
                .write_all(b"{\"ok\":true,\"pong\":true}\n")
                .expect("write response");
        });

        let response: serde_json::Value = call(&fixture.socket(), &Req::Ping).expect("call");
        server.join().expect("server thread");
        assert_eq!(response, json!({"pong": true}));
    }
}
