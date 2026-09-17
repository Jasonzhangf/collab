//! Authorized retirement of a legacy project-local Collab control plane.
//!
//! This is the single offline reset owner. It is deliberately separate from
//! `collab migrate`, which preserves history, and it never claims any delivery,
//! review, install, or communication evidence. The operation is only reachable
//! with an explicit operator approval string, requires the host daemon to be
//! down, archives the retired bytes, removes the project-local control plane
//! plus its stale host route record, and rebuilds the current empty baseline.

use crate::scope::{self, HostPaths, Scope};
use anyhow::Context;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Legacy project-local control roots owned by Collab.
///
/// `.appsdk-control` belongs to the AppSDK governance transaction and is
/// deliberately not touched here. Business source, runtime data, `active/`,
/// and `protected/` are never listed.
const LEGACY_CONTROL_ROOTS: [&str; 2] = [".agent-collab", ".agent-collab-v2"];

pub struct ResetRequest {
    pub approval: String,
    pub discard_legacy: bool,
}

struct RetiredRoot {
    relative: String,
    absolute: PathBuf,
    staged: Option<PathBuf>,
    files: usize,
    sockets: Vec<String>,
    bytes: u64,
    digest: String,
}

fn archive_matches(entry: &RetiredRoot, files: usize, bytes: u64, digest: &str) -> bool {
    files == entry.files && bytes == entry.bytes && digest == entry.digest
}

fn source_matches(entry: &RetiredRoot, sockets: &[String]) -> bool {
    sockets == entry.sockets
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn fnv1a64(bytes: &[u8], seed: u64) -> u64 {
    let mut hash = seed;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Deterministic tree digest for regular files. Unix sockets have no durable
/// bytes and are returned separately so reset can retire a stale endpoint
/// without pretending that it copied socket state into the archive.
fn tree_digest(root: &Path) -> std::io::Result<(usize, Vec<String>, u64, String)> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    let mut count = 0usize;
    let mut sockets = Vec::new();
    let mut bytes = 0u64;
    let mut hash = 0xcbf29ce484222325_u64;
    for relative in &files {
        let path = root.join(relative);
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(control_tree_symlink_error(&path));
        } else if is_unix_socket(&metadata) {
            sockets.push(relative.clone());
            continue;
        } else {
            hash = fnv1a64(relative.as_bytes(), hash);
            let content = std::fs::read(&path)?;
            hash = fnv1a64(&content, hash);
        }
        count += 1;
        bytes += metadata.len();
    }
    Ok((count, sockets, bytes, format!("fnv1a64:{hash:016x}")))
}

fn is_unix_socket(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    metadata.file_type().is_socket()
}

fn collect_files(root: &Path, current: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(control_tree_symlink_error(&path));
        }
        if metadata.is_dir() {
            collect_files(root, &path, out)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            out.push(relative.to_string_lossy().into_owned());
        }
    }
    Ok(())
}

fn control_tree_symlink_error(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!(
            "RESET_CONTROL_TREE_SYMLINK_REJECTED: {} is a symlink; reset refuses to \
             create a non-self-contained archive",
            path.display()
        ),
    )
}

fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&from)?;
        if metadata.file_type().is_symlink() {
            return Err(control_tree_symlink_error(&from));
        } else if metadata.is_dir() {
            copy_tree(&from, &to)?;
        } else if is_unix_socket(&metadata) {
            continue;
        } else {
            std::fs::copy(&from, &to)?;
            std::fs::File::open(&to)?.sync_all()?;
        }
    }
    sync_dir(destination)?;
    Ok(())
}

fn sync_dir(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

fn reject_unsafe_control_roots(root: &Path) -> anyhow::Result<()> {
    for relative in LEGACY_CONTROL_ROOTS {
        let path = root.join(relative);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!(
                    "RESET_CONTROL_ROOT_SYMLINK_REJECTED: {} is a symlink; reset refuses \
                     to archive or write outside the project root",
                    path.display()
                );
            }
            Ok(metadata) if !metadata.is_dir() => {
                anyhow::bail!(
                    "RESET_CONTROL_ROOT_INVALID: {} is not a directory",
                    path.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn slugify(root: &Path) -> String {
    let text = root.to_string_lossy();
    let mut slug = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "project".to_string()
    } else {
        slug.chars().take(80).collect()
    }
}

fn append_reset_record(host_paths: &HostPaths, record: &serde_json::Value) -> anyhow::Result<()> {
    use std::io::Write;
    let path = host_paths.state_root().join("reset.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    file.write_all(&line)?;
    file.sync_data()?;
    Ok(())
}

struct RouteSnapshot {
    path: PathBuf,
    original: Option<Vec<u8>>,
}

fn snapshot_file(path: PathBuf) -> anyhow::Result<RouteSnapshot> {
    let original = match std::fs::read(&path) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    Ok(RouteSnapshot { path, original })
}

fn reject_symlinked_guidance(root: &Path) -> anyhow::Result<()> {
    for candidate in [root.join("docs"), root.join("docs/collab.md")] {
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!(
                    "RESET_DOC_SYMLINK_REJECTED: {} is a symlink; reset refuses to write \
                     outside the project-owned Collab guidance path",
                    candidate.display()
                );
            }
            Ok(_) | Err(_) => {}
        }
    }
    Ok(())
}

fn restore_file(snapshot: &RouteSnapshot) -> anyhow::Result<()> {
    match &snapshot.original {
        Some(content) => {
            use std::io::Write;
            let tmp = snapshot
                .path
                .with_file_name(format!("routes.jsonl.reset-restore-{}", std::process::id()));
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            file.write_all(content)?;
            file.sync_data()?;
            drop(file);
            std::fs::rename(&tmp, &snapshot.path)?;
            if let Some(parent) = snapshot.path.parent() {
                std::fs::File::open(parent)?.sync_all()?;
            }
        }
        None => match std::fs::remove_file(&snapshot.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        },
    }
    Ok(())
}

fn stage_retired_roots(retired: &mut [RetiredRoot], run_id: &str) -> anyhow::Result<()> {
    for entry in retired.iter_mut() {
        let staged = entry
            .absolute
            .with_file_name(format!("{}.reset-stage-{run_id}", entry.relative));
        std::fs::rename(&entry.absolute, &staged)?;
        entry.staged = Some(staged);
    }
    Ok(())
}

fn rollback_retired_roots(retired: &mut [RetiredRoot]) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    for entry in retired.iter_mut().rev() {
        let Some(staged) = entry.staged.take() else {
            continue;
        };
        if entry.absolute.is_dir() {
            if let Err(error) = std::fs::remove_dir_all(&entry.absolute) {
                errors.push(format!(
                    "remove replacement {}: {error}",
                    entry.absolute.display()
                ));
                continue;
            }
        }
        if let Err(error) = std::fs::rename(&staged, &entry.absolute) {
            errors.push(format!(
                "{} -> {}: {error}",
                staged.display(),
                entry.absolute.display()
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{}", errors.join("; "))
    }
}

fn discard_staged_roots(retired: &[RetiredRoot]) -> anyhow::Result<()> {
    for entry in retired {
        if let Some(staged) = &entry.staged {
            std::fs::remove_dir_all(staged)?;
        }
    }
    Ok(())
}

/// Rewrite the host route journal without the retired project's records.
///
/// The route table is host-wide durable state, so this is an atomic
/// replacement: a crash leaves either the previous valid table or the complete
/// new table. Unrelated routes are preserved byte-for-byte.
fn retire_host_routes(host_paths: &HostPaths, root: &Path) -> anyhow::Result<usize> {
    let root = std::fs::canonicalize(root)?;
    rewrite_host_routes(host_paths, |record| {
        let matches_project = record
            .get("canonical_root")
            .and_then(|value| value.as_str())
            .and_then(|value| std::fs::canonicalize(value).ok())
            .is_some_and(|value| value == root);
        matches_project.then_some(RouteDisposition::Retire)
    })
}

enum RouteDisposition {
    Keep,
    Retire,
}

/// Remove host routes whose canonical project root can no longer be used.
///
/// A route is provably stale only when its canonical root does not exist, or
/// when the root exists but no longer has the `.agent-collab` initialization
/// marker. Every other record is retained byte-for-byte. This is the
/// "ignore legacy residue" half of reset: stale routes must not keep a daemon
/// from replaying the current baseline.
fn prune_stale_host_routes(host_paths: &HostPaths) -> anyhow::Result<usize> {
    rewrite_host_routes(host_paths, |record| {
        let Some(canonical_root) = record
            .get("canonical_root")
            .and_then(|value| value.as_str())
        else {
            return Some(RouteDisposition::Retire);
        };
        match std::fs::canonicalize(canonical_root) {
            Ok(root) if root.join(".agent-collab").is_dir() => Some(RouteDisposition::Keep),
            Ok(_) => Some(RouteDisposition::Retire),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Some(RouteDisposition::Retire)
            }
            // Permission and I/O errors are unknown, not stale. Keep the
            // record so a transient mount failure cannot erase a valid route.
            Err(_) => Some(RouteDisposition::Keep),
        }
    })
}

fn rewrite_host_routes<F>(host_paths: &HostPaths, mut classify: F) -> anyhow::Result<usize>
where
    F: FnMut(&serde_json::Value) -> Option<RouteDisposition>,
{
    let path = host_paths.state_root().join("routes.jsonl");
    let content = match std::fs::read(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    if content.is_empty() {
        return Ok(0);
    }
    if !content.ends_with(b"\n") {
        anyhow::bail!(
            "HOST_ROUTE_DURABILITY_FAILED: route journal must end with a newline: {}",
            path.display()
        );
    }
    let mut kept = Vec::new();
    let mut removed = 0usize;
    for chunk in content.split_inclusive(|byte| *byte == b'\n') {
        let line = chunk.strip_suffix(b"\n").unwrap_or(chunk);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            anyhow::bail!(
                "HOST_ROUTE_DURABILITY_FAILED: empty route journal line in {}",
                path.display()
            );
        }
        let record: serde_json::Value = serde_json::from_slice(line)?;
        match classify(&record) {
            Some(RouteDisposition::Retire) => {
                removed += 1;
                continue;
            }
            Some(RouteDisposition::Keep) | None => {}
        }
        kept.extend_from_slice(chunk);
    }
    if removed == 0 {
        return Ok(0);
    }
    use std::io::Write;
    let tmp = path.with_file_name(format!("routes.jsonl.reset-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    file.write_all(&kept)?;
    file.sync_data()?;
    drop(file);
    std::fs::rename(&tmp, &path)?;
    std::fs::File::open(host_paths.state_root())?.sync_all()?;
    Ok(removed)
}

pub fn run(scope: &Scope, host_paths: &HostPaths, request: ResetRequest) -> anyhow::Result<()> {
    if !request.discard_legacy {
        anyhow::bail!(
            "RESET_AUTHORIZATION_REQUIRED: collab reset requires --discard-legacy; \
             use `collab migrate` to preserve history"
        );
    }
    if request.approval.trim().is_empty() {
        anyhow::bail!(
            "RESET_AUTHORIZATION_REQUIRED: collab reset requires a non-empty --approval \
             naming the operator authorization"
        );
    }
    let root = std::fs::canonicalize(&scope.root)?;

    // Prove exclusivity before touching any control plane. Keep both the
    // current host lock and every lock understood by a pre-host-endpoint
    // daemon alive through the complete transaction.
    let _lock =
        crate::server::acquire_reset_lock(&host_paths.lock_path(), &host_paths.socket_path())?;
    let _legacy_writer_fence =
        crate::server::acquire_legacy_writer_fence(&Scope { root: root.clone() }, host_paths)?;
    if crate::client::alive(&host_paths.socket_path()) {
        anyhow::bail!(
            "RESET_DAEMON_LIVE: a Collab daemon is reachable at {}; run `collab down` \
             before retiring the project control plane",
            host_paths.socket_path().display()
        );
    }

    let run_id = format!("reset-{}-{}", now_ms(), std::process::id());
    let archive_root = host_paths
        .state_root()
        .join("archives")
        .join(format!("{}-{run_id}", slugify(&root)));
    reject_unsafe_control_roots(&root)?;

    let mut retired = Vec::new();
    for relative in LEGACY_CONTROL_ROOTS {
        let absolute = root.join(relative);
        match std::fs::symlink_metadata(&absolute) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "RESET_CONTROL_ROOT_INSPECTION_FAILED: cannot inspect {}",
                        absolute.display()
                    )
                });
            }
        }
        // The current scaffold also creates `.agent-collab/`. An empty
        // current baseline has no durable reducer state and is not a legacy
        // control plane, so a repeated reset must be idempotent instead of
        // archiving and rebuilding that scaffold forever.
        if relative == ".agent-collab" && is_current_empty_baseline(&absolute) {
            continue;
        }
        let (files, sockets, bytes, digest) = tree_digest(&absolute)?;
        retired.push(RetiredRoot {
            relative: relative.to_string(),
            absolute,
            staged: None,
            files,
            sockets,
            bytes,
            digest,
        });
    }

    let already_reset = retired.is_empty();
    if !already_reset {
        // Archive first, verify byte equality, and only then remove. A failed
        // archive leaves the legacy control plane untouched.
        std::fs::create_dir_all(&archive_root)?;
        for entry in &retired {
            let destination = archive_root.join(&entry.relative);
            copy_tree(&entry.absolute, &destination)?;
            let (files, sockets, bytes, digest) = tree_digest(&destination)?;
            if !archive_matches(entry, files, bytes, &digest) {
                anyhow::bail!(
                    "RESET_ARCHIVE_MISMATCH: {} -> {} ({files} files, {} sockets, {bytes} bytes, \
                     {digest}) does not match the source ({} files, {} sockets, {} bytes, {})",
                    entry.absolute.display(),
                    destination.display(),
                    sockets.len(),
                    entry.files,
                    entry.sockets.len(),
                    entry.bytes,
                    entry.digest
                );
            }
            let source_sockets = tree_digest(&entry.absolute)?.1;
            if !source_matches(entry, &source_sockets) {
                anyhow::bail!(
                    "RESET_SOURCE_SOCKET_CHANGED: {} socket inventory changed during archive",
                    entry.absolute.display()
                );
            }
        }
        let manifest = json!({
            "schema": "collab-reset/v1",
            "run_id": run_id,
            "project_root": root,
            "approval": request.approval,
            "retired": retired
                .iter()
                .map(|entry| json!({
                    "path": entry.relative,
                    "files": entry.files,
                    "sockets": entry.sockets,
                    "bytes": entry.bytes,
                    "digest": entry.digest,
                }))
                .collect::<Vec<_>>(),
            "delivery_verified": false,
            "created_ms": now_ms(),
        });
        let manifest_path = archive_root.join("manifest.json");
        std::fs::write(
            &manifest_path,
            format!("{}\n", serde_json::to_string_pretty(&manifest)?),
        )?;
        std::fs::File::open(&manifest_path)?.sync_all()?;
        sync_dir(&archive_root)?;
        if let Some(archives_dir) = archive_root.parent() {
            sync_dir(archives_dir)?;
        }
        sync_dir(host_paths.state_root())?;
    }

    let routes_path = host_paths.state_root().join("routes.jsonl");
    let reset_record_path = host_paths.state_root().join("reset.jsonl");
    let route_snapshot = snapshot_file(routes_path)?;
    let reset_record_snapshot = snapshot_file(reset_record_path)?;
    reject_symlinked_guidance(&root)?;
    let docs_dir = root.join("docs");
    let docs_dir_existed = docs_dir.is_dir();
    let collab_doc_snapshot = snapshot_file(docs_dir.join("collab.md"))?;
    let baseline_existed = root.join(".agent-collab").exists();
    let mut removed_routes = 0usize;
    let mut removed_stale_routes = 0usize;
    let mut record = serde_json::Value::Null;
    let transaction = (|| -> anyhow::Result<()> {
        if !already_reset {
            stage_retired_roots(&mut retired, &run_id)?;
        }

        // Rebuild only the Collab-owned current empty baseline. The full init
        // path also edits project MCP/editor settings and global AppSDK
        // configuration; reset must not mutate those unrelated owners.
        scope::init_collab_baseline(&root)?;
        if !root.join(".agent-collab").is_dir() {
            anyhow::bail!(
                "RESET_BASELINE_INVALID: {} was not created by the current scaffold",
                root.join(".agent-collab").display()
            );
        }

        removed_routes = retire_host_routes(host_paths, &root)?;
        removed_stale_routes = prune_stale_host_routes(host_paths)?;
        // A reset must not retain an older project-local guidance file while
        // claiming that the current scaffold was rebuilt.
        std::fs::create_dir_all(root.join("docs"))?;
        std::fs::write(root.join("docs/collab.md"), scope::COLLAB_DOC)?;
        record = json!({
            "schema": "collab-reset/v1",
            "run_id": run_id,
            "at_ms": now_ms(),
            "project_root": root,
            "approval": request.approval,
            "already_reset": already_reset,
            "archive_root": if already_reset { None } else { Some(archive_root) },
            "removed_host_routes": removed_routes,
            "removed_stale_host_routes": removed_stale_routes,
            "retired": retired
                .iter()
                .map(|entry| json!({
                    "path": entry.relative,
                    "files": entry.files,
                    "sockets": entry.sockets,
                    "bytes": entry.bytes,
                    "digest": entry.digest,
                }))
                .collect::<Vec<_>>(),
            "archive_durable": !already_reset,
            "delivery_verified": false,
            "next": "run collab up, then collab init from this exact project root",
        });
        append_reset_record(host_paths, &record)?;
        discard_staged_roots(&retired)?;
        Ok(())
    })();

    if let Err(error) = transaction {
        let rollback_roots = rollback_retired_roots(&mut retired);
        let rollback_routes = restore_file(&route_snapshot);
        let rollback_record = restore_file(&reset_record_snapshot);
        let rollback_doc = restore_file(&collab_doc_snapshot);
        let rollback_baseline: anyhow::Result<()> = if baseline_existed {
            Ok(())
        } else {
            std::fs::remove_dir_all(root.join(".agent-collab"))
                .or_else(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })
                .map_err(anyhow::Error::from)
        };
        let rollback_docs_dir: anyhow::Result<()> = if docs_dir_existed {
            Ok(())
        } else {
            std::fs::remove_dir(&docs_dir)
                .or_else(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound
                        || error.kind() == std::io::ErrorKind::DirectoryNotEmpty
                    {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })
                .map_err(anyhow::Error::from)
        };
        if let Err(rollback_error) = rollback_roots
            .and(rollback_routes)
            .and(rollback_record)
            .and(rollback_doc)
            .and(rollback_baseline)
            .and(rollback_docs_dir)
        {
            let incomplete = json!({
                "schema": "collab-reset/v1",
                "run_id": run_id,
                "at_ms": now_ms(),
                "project_root": root,
                "approval": request.approval,
                "status": "incomplete",
                "delivery_verified": false,
                "error": error.to_string(),
                "rollback_error": rollback_error.to_string(),
            });
            let _ = append_reset_record(host_paths, &incomplete);
            anyhow::bail!("RESET_INCOMPLETE: {error}; rollback also failed: {rollback_error}");
        }
        return Err(error.context("reset rolled back; legacy control plane restored"));
    }

    println!("{}", serde_json::to_string_pretty(&record)?);
    Ok(())
}

fn is_current_empty_baseline(agent_collab: &Path) -> bool {
    let expected_roots = [
        "handoff",
        "mailbox",
        "mailboxes",
        "merge-queue",
        "messages",
        "runs",
        "server",
    ];
    let Ok(entries) = std::fs::read_dir(agent_collab) else {
        return false;
    };
    let mut roots = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let Ok(file_type) = entry.file_type() else {
            return false;
        };
        if !file_type.is_dir() {
            return false;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return false;
        };
        roots.push(name.to_owned());
    }
    roots.sort();
    if roots != expected_roots {
        return false;
    }
    for root in [
        "handoff",
        "mailbox",
        "mailboxes",
        "merge-queue",
        "messages",
        "runs",
    ] {
        let Ok(mut entries) = std::fs::read_dir(agent_collab.join(root)) else {
            return false;
        };
        if entries.next().is_some() {
            return false;
        }
    }

    let server = agent_collab.join("server");
    let Ok(entries) = std::fs::read_dir(&server) else {
        return false;
    };
    entries.filter_map(Result::ok).all(|entry| {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return false;
        };
        if !matches!(
            name,
            "daemon.lock" | "server.pid" | "DOWN" | "journal.jsonl" | "events.jsonl" | "log.txt"
        ) {
            return false;
        }
        let Ok(metadata) = entry.metadata() else {
            return false;
        };
        metadata.is_file() && metadata.len() == 0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        PathBuf::from(format!(
            "/tmp/cr-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ))
    }

    #[test]
    fn tree_digest_is_stable_and_content_sensitive() {
        let root = temp_root("digest");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("a.jsonl"), b"one\n").unwrap();
        std::fs::write(root.join("nested/b"), b"two\n").unwrap();
        let first = tree_digest(&root).unwrap();
        let second = tree_digest(&root).unwrap();
        assert_eq!(first, second);
        std::fs::write(root.join("nested/b"), b"changed\n").unwrap();
        assert_ne!(first, tree_digest(&root).unwrap());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn tree_digest_ignores_stale_unix_socket_bytes() {
        let root = temp_root("digest-socket");
        std::fs::create_dir_all(root.join("server")).unwrap();
        std::fs::write(root.join("server/journal.jsonl"), b"one\n").unwrap();
        let socket = root.join("server/server.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        drop(listener);

        let (files, sockets, bytes, _) = tree_digest(&root).unwrap();
        assert_eq!(files, 1);
        assert_eq!(sockets, vec![String::from("server/server.sock")]);
        assert_eq!(bytes, 4);

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn source_verification_requires_the_inspected_socket_inventory() {
        let entry = RetiredRoot {
            relative: ".agent-collab".into(),
            absolute: PathBuf::from("/tmp/source"),
            staged: None,
            files: 1,
            sockets: vec!["server/server.sock".into()],
            bytes: 4,
            digest: "fnv1a64:test".into(),
        };

        assert!(source_matches(&entry, &["server/server.sock".into()]));
        assert!(!source_matches(&entry, &[]));
        assert!(archive_matches(&entry, 1, 4, "fnv1a64:test"));
    }

    #[test]
    fn tree_digest_rejects_nested_symlinks() {
        let root = temp_root("digest-symlink");
        let target = temp_root("digest-symlink-target");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&target, b"external\n").unwrap();
        std::os::unix::fs::symlink(&target, root.join("journal.jsonl")).unwrap();

        let error = tree_digest(&root).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains("RESET_CONTROL_TREE_SYMLINK_REJECTED"),
            "{error}"
        );

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_file(target).ok();
    }

    #[test]
    fn retire_host_routes_preserves_unrelated_records() {
        let state = temp_root("routes-state");
        let project = temp_root("routes-project");
        let other = temp_root("routes-other");
        for path in [&state, &project, &other] {
            std::fs::create_dir_all(path).unwrap();
        }
        let host = HostPaths::from_state_root(&state).unwrap();
        let retired = json!({
            "version": 1,
            "op": "register",
            "app_scope_id": "appserver-cli",
            "project_scope": project.canonicalize().unwrap().to_string_lossy(),
            "canonical_root": project.canonicalize().unwrap().to_string_lossy(),
            "storage_root": project.canonicalize().unwrap().to_string_lossy(),
            "registered_ms": 1,
        });
        let kept = json!({
            "version": 1,
            "op": "register",
            "app_scope_id": "appserver-cli",
            "project_scope": other.canonicalize().unwrap().to_string_lossy(),
            "canonical_root": other.canonicalize().unwrap().to_string_lossy(),
            "storage_root": other.canonicalize().unwrap().to_string_lossy(),
            "registered_ms": 2,
        });
        std::fs::write(state.join("routes.jsonl"), format!("{retired}\n{kept}\n")).unwrap();

        assert_eq!(retire_host_routes(&host, &project).unwrap(), 1);
        let remaining = std::fs::read_to_string(state.join("routes.jsonl")).unwrap();
        assert!(remaining.contains(&other.canonicalize().unwrap().to_string_lossy().to_string()));
        assert!(!remaining.contains(
            &project
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .to_string()
        ));

        // Idempotent: a second pass removes nothing and leaves the table intact.
        assert_eq!(retire_host_routes(&host, &project).unwrap(), 0);
        assert_eq!(
            std::fs::read_to_string(state.join("routes.jsonl")).unwrap(),
            remaining
        );

        for path in [&state, &project, &other] {
            std::fs::remove_dir_all(path).ok();
        }
    }

    #[test]
    fn reset_requires_explicit_authorization() {
        let root = temp_root("auth");
        std::fs::create_dir_all(&root).unwrap();
        scope::init(&root).unwrap();
        let state = temp_root("auth-state");
        std::fs::create_dir_all(&state).unwrap();
        let host = HostPaths::from_state_root(&state).unwrap();
        let scope = Scope { root: root.clone() };

        let missing_flag = run(
            &scope,
            &host,
            ResetRequest {
                approval: "user text".into(),
                discard_legacy: false,
            },
        );
        assert!(missing_flag
            .unwrap_err()
            .to_string()
            .contains("RESET_AUTHORIZATION_REQUIRED"));

        let missing_approval = run(
            &scope,
            &host,
            ResetRequest {
                approval: "  ".into(),
                discard_legacy: true,
            },
        );
        assert!(missing_approval
            .unwrap_err()
            .to_string()
            .contains("RESET_AUTHORIZATION_REQUIRED"));

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(state).ok();
    }

    #[test]
    fn reset_rejects_a_held_legacy_project_writer_lock_before_archive() {
        use std::os::unix::io::AsRawFd;

        let root = temp_root("legacy-lock");
        std::fs::create_dir_all(root.join(".agent-collab/server")).unwrap();
        let legacy = b"{\"ev\":\"NotificationSubscribed\",\"subscription\":{}}\n";
        std::fs::write(root.join(".agent-collab/server/journal.jsonl"), legacy).unwrap();
        let lock_path = root.join(".agent-collab/server/daemon.lock");
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&lock_path)
            .unwrap();
        let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0);

        let state = temp_root("legacy-lock-state");
        std::fs::create_dir_all(&state).unwrap();
        let host = HostPaths::from_state_root(&state).unwrap();
        let scope = Scope { root: root.clone() };

        let error = run(
            &scope,
            &host,
            ResetRequest {
                approval: "operator authorized legacy retirement".into(),
                discard_legacy: true,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("DAEMON_MIGRATION_REQUIRED"));
        assert_eq!(
            std::fs::read(root.join(".agent-collab/server/journal.jsonl")).unwrap(),
            legacy
        );
        assert!(!state.join("archives").exists());
        assert!(!state.join("reset.jsonl").exists());

        drop(lock_file);
        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(state).ok();
    }

    #[test]
    fn reset_rebuilds_an_uninitialized_project_root() {
        let root = temp_root("uninitialized");
        std::fs::create_dir_all(&root).unwrap();
        let state = temp_root("uninitialized-state");
        std::fs::create_dir_all(&state).unwrap();
        let host = HostPaths::from_state_root(&state).unwrap();
        let scope = Scope { root: root.clone() };

        run(
            &scope,
            &host,
            ResetRequest {
                approval: "operator authorized current baseline repair".into(),
                discard_legacy: true,
            },
        )
        .unwrap();

        assert!(root.join(".agent-collab/server").is_dir());
        assert!(!root.join(".agent-collab/server/journal.jsonl").exists());
        let record = read_last_reset_record(&state);
        assert_eq!(record["already_reset"], json!(true));
        assert_eq!(record["retired"], json!([]));
        assert_eq!(record["delivery_verified"], json!(false));

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(state).ok();
    }

    #[test]
    fn prune_stale_host_routes_removes_only_provably_dead_roots() {
        let state = temp_root("prune-state");
        let live = temp_root("prune-live");
        let uninitialized = temp_root("prune-uninitialized");
        let missing = temp_root("prune-missing");
        for path in [&state, &live, &uninitialized] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::create_dir_all(live.join(".agent-collab")).unwrap();
        let host = HostPaths::from_state_root(&state).unwrap();

        let record = |root: &Path| {
            json!({
                "version": 1,
                "op": "register",
                "app_scope_id": "appserver-cli",
                "project_scope": root.canonicalize().unwrap().to_string_lossy(),
                "canonical_root": root.canonicalize().unwrap().to_string_lossy(),
                "storage_root": root.canonicalize().unwrap().to_string_lossy(),
                "registered_ms": 1,
            })
        };
        let missing_record = json!({
            "version": 1,
            "op": "register",
            "app_scope_id": "appserver-cli",
            "project_scope": missing.to_string_lossy(),
            "canonical_root": missing.to_string_lossy(),
            "storage_root": missing.to_string_lossy(),
            "registered_ms": 1,
        });
        std::fs::write(
            state.join("routes.jsonl"),
            format!(
                "{}\n{}\n{}\n",
                record(&live),
                record(&uninitialized),
                missing_record
            ),
        )
        .unwrap();

        assert_eq!(prune_stale_host_routes(&host).unwrap(), 2);
        let remaining = std::fs::read_to_string(state.join("routes.jsonl")).unwrap();
        assert!(remaining.contains(&live.canonicalize().unwrap().to_string_lossy().to_string()));
        assert!(!remaining.contains(
            &uninitialized
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .to_string()
        ));

        for path in [&state, &live, &uninitialized] {
            std::fs::remove_dir_all(path).ok();
        }
    }

    #[test]
    fn reset_retires_legacy_control_plane_and_rebuilds_baseline() {
        let root = temp_root("retire");
        std::fs::create_dir_all(root.join(".agent-collab/server")).unwrap();
        std::fs::write(
            root.join(".agent-collab/server/journal.jsonl"),
            b"{\"ev\":\"NotificationSubscribed\",\"subscription\":{}}\n",
        )
        .unwrap();

        let state = temp_root("retire-state");
        std::fs::create_dir_all(&state).unwrap();
        let host = HostPaths::from_state_root(&state).unwrap();
        let scope = Scope { root: root.clone() };

        run(
            &scope,
            &host,
            ResetRequest {
                approval: "operator authorized legacy retirement".into(),
                discard_legacy: true,
            },
        )
        .unwrap();

        // Current empty baseline exists and the legacy journal is gone.
        assert!(root.join(".agent-collab/server").is_dir());
        assert!(!root.join(".agent-collab/server/journal.jsonl").exists());

        // The audit record is explicit that this proves reset only.
        let record = read_last_reset_record(&state);
        assert_eq!(record["delivery_verified"], json!(false));
        assert_eq!(record["already_reset"], json!(false));
        assert_eq!(record["archive_durable"], json!(true));
        assert_eq!(record["schema"], json!("collab-reset/v1"));
        assert!(record["archive_root"].is_string());

        // Second run is idempotent and does not claim delivery.
        run(
            &scope,
            &host,
            ResetRequest {
                approval: "operator authorized legacy retirement".into(),
                discard_legacy: true,
            },
        )
        .unwrap();
        let record = read_last_reset_record(&state);
        assert_eq!(record["already_reset"], json!(true));
        assert_eq!(record["archive_durable"], json!(false));
        assert_eq!(record["delivery_verified"], json!(false));
        assert!(record["archive_root"].is_null());

        // Non-empty runtime artifacts are historical state too. They must be
        // archived and cleared rather than being accepted as a current empty
        // baseline.
        std::fs::write(root.join(".agent-collab/server/journal.jsonl"), b"").unwrap();
        std::fs::write(root.join(".agent-collab/server/events.jsonl"), b"{}\n").unwrap();
        std::fs::write(root.join(".agent-collab/server/log.txt"), b"started\n").unwrap();
        run(
            &scope,
            &host,
            ResetRequest {
                approval: "operator authorized legacy retirement".into(),
                discard_legacy: true,
            },
        )
        .unwrap();
        let record = read_last_reset_record(&state);
        assert_eq!(record["already_reset"], json!(false));
        assert_eq!(record["archive_durable"], json!(true));
        assert!(!record["retired"].as_array().unwrap().is_empty());
        assert!(!root.join(".agent-collab/server/events.jsonl").exists());
        assert!(!root.join(".agent-collab/server/log.txt").exists());
        assert_eq!(record["delivery_verified"], json!(false));

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(state).ok();
    }

    #[test]
    fn reset_retires_stale_project_socket_without_copying_socket_state() {
        use std::os::unix::net::UnixListener;

        let root = temp_root("retire-socket");
        std::fs::create_dir_all(root.join(".agent-collab/server")).unwrap();
        std::fs::write(
            root.join(".agent-collab/server/journal.jsonl"),
            b"{\"ev\":\"Sent\",\"msg\":{}}\n",
        )
        .unwrap();
        let legacy_socket = root.join(".agent-collab/server/server.sock");
        let listener = UnixListener::bind(&legacy_socket).unwrap();
        drop(listener);

        let state = temp_root("retire-socket-state");
        std::fs::create_dir_all(&state).unwrap();
        let host = HostPaths::from_state_root(&state).unwrap();
        let scope = Scope { root: root.clone() };

        run(
            &scope,
            &host,
            ResetRequest {
                approval: "operator authorized legacy retirement".into(),
                discard_legacy: true,
            },
        )
        .unwrap();

        assert!(!legacy_socket.exists());
        assert!(!root.join(".agent-collab/server/journal.jsonl").exists());
        let archive_entries = std::fs::read_dir(state.join("archives")).unwrap().count();
        assert!(archive_entries > 0);
        let manifest_path = std::fs::read_dir(state.join("archives"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("manifest.json");
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
        assert_eq!(
            manifest["retired"][0]["sockets"][0],
            json!("server/server.sock")
        );

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(state).ok();
    }

    #[test]
    fn reset_retires_nonempty_runtime_files_in_current_scaffold() {
        let root = temp_root("nonempty-runtime");
        scope::init(&root).unwrap();
        std::fs::write(root.join(".agent-collab/server/events.jsonl"), b"{}\n").unwrap();
        std::fs::write(root.join(".agent-collab/server/log.txt"), b"old log\n").unwrap();

        let state = temp_root("nonempty-runtime-state");
        std::fs::create_dir_all(&state).unwrap();
        let host = HostPaths::from_state_root(&state).unwrap();
        let scope = Scope { root: root.clone() };

        run(
            &scope,
            &host,
            ResetRequest {
                approval: "operator authorized legacy retirement".into(),
                discard_legacy: true,
            },
        )
        .unwrap();

        assert!(!root.join(".agent-collab/server/events.jsonl").exists());
        assert!(!root.join(".agent-collab/server/log.txt").exists());
        let record = read_last_reset_record(&state);
        assert_eq!(record["already_reset"], json!(false));
        assert_eq!(record["archive_durable"], json!(true));
        assert_eq!(record["delivery_verified"], json!(false));

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(state).ok();
    }

    #[test]
    fn reset_does_not_mutate_unrelated_project_or_global_configuration() {
        let root = temp_root("unrelated-config");
        std::fs::create_dir_all(root.join(".agent-collab/server")).unwrap();
        std::fs::write(
            root.join(".agent-collab/server/journal.jsonl"),
            b"{\"ev\":\"NotificationSubscribed\",\"subscription\":{}}\n",
        )
        .unwrap();
        std::fs::write(root.join(".mcp.json"), b"{\"keep\":true}\n").unwrap();
        std::fs::create_dir_all(root.join(".codex")).unwrap();
        std::fs::write(root.join(".codex/config.toml"), b"model = \"keep\"\n").unwrap();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(root.join(".claude/settings.json"), b"{\"keep\":true}\n").unwrap();

        let state = temp_root("unrelated-config-state");
        std::fs::create_dir_all(&state).unwrap();
        let host = HostPaths::from_state_root(&state).unwrap();
        let scope = Scope { root: root.clone() };

        run(
            &scope,
            &host,
            ResetRequest {
                approval: "operator authorized legacy retirement".into(),
                discard_legacy: true,
            },
        )
        .unwrap();

        assert_eq!(
            std::fs::read(root.join(".mcp.json")).unwrap(),
            b"{\"keep\":true}\n"
        );
        assert_eq!(
            std::fs::read(root.join(".codex/config.toml")).unwrap(),
            b"model = \"keep\"\n"
        );
        assert_eq!(
            std::fs::read(root.join(".claude/settings.json")).unwrap(),
            b"{\"keep\":true}\n"
        );

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(state).ok();
    }

    #[test]
    fn reset_does_not_treat_nonempty_project_state_as_empty_baseline() {
        let root = temp_root("nonempty-baseline");
        scope::init(&root).unwrap();
        std::fs::write(root.join(".agent-collab/runs/stale.jsonl"), b"{}\n").unwrap();
        std::fs::create_dir_all(root.join(".agent-collab/server/runtimes/old")).unwrap();
        std::fs::write(
            root.join(".agent-collab/server/runtimes/old/journal.jsonl"),
            b"legacy\n",
        )
        .unwrap();

        let state = temp_root("nonempty-baseline-state");
        std::fs::create_dir_all(&state).unwrap();
        let host = HostPaths::from_state_root(&state).unwrap();
        let scope = Scope { root: root.clone() };

        run(
            &scope,
            &host,
            ResetRequest {
                approval: "operator authorized legacy retirement".into(),
                discard_legacy: true,
            },
        )
        .unwrap();

        assert!(!root.join(".agent-collab/runs/stale.jsonl").exists());
        assert!(!root.join(".agent-collab/server/runtimes").exists());
        let record = read_last_reset_record(&state);
        assert_eq!(record["already_reset"], json!(false));
        assert_eq!(record["delivery_verified"], json!(false));

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(state).ok();
    }

    #[test]
    fn reset_rejects_symlinked_guidance_without_mutating_the_target() {
        let root = temp_root("symlink-guidance");
        let target = temp_root("symlink-guidance-target");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(&target, b"external guidance\n").unwrap();
        std::os::unix::fs::symlink(&target, root.join("docs/collab.md")).unwrap();

        let state = temp_root("symlink-guidance-state");
        std::fs::create_dir_all(&state).unwrap();
        let host = HostPaths::from_state_root(&state).unwrap();
        let scope = Scope { root: root.clone() };

        let error = run(
            &scope,
            &host,
            ResetRequest {
                approval: "operator authorized current baseline repair".into(),
                discard_legacy: true,
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("RESET_DOC_SYMLINK_REJECTED"),
            "{error:#}"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"external guidance\n");
        assert!(!state.join("reset.jsonl").exists());

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(state).ok();
        std::fs::remove_file(target).ok();
    }

    #[test]
    fn reset_rejects_symlinked_control_root_without_mutating_the_target() {
        let root = temp_root("symlink-control");
        let target = temp_root("symlink-control-target");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("journal.jsonl"), b"external\n").unwrap();
        std::os::unix::fs::symlink(&target, root.join(".agent-collab")).unwrap();

        let state = temp_root("symlink-control-state");
        std::fs::create_dir_all(&state).unwrap();
        let host = HostPaths::from_state_root(&state).unwrap();
        let scope = Scope { root: root.clone() };

        let error = run(
            &scope,
            &host,
            ResetRequest {
                approval: "operator authorized current baseline repair".into(),
                discard_legacy: true,
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("RESET_CONTROL_ROOT_SYMLINK_REJECTED"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(target.join("journal.jsonl")).unwrap(),
            b"external\n"
        );
        assert!(!state.join("archives").exists());
        assert!(!state.join("reset.jsonl").exists());

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(state).ok();
        std::fs::remove_dir_all(target).ok();
    }

    #[test]
    fn reset_rejects_symlink_inside_control_root_without_archiving() {
        let root = temp_root("symlink-control-entry");
        let target = temp_root("symlink-control-entry-target");
        std::fs::create_dir_all(root.join(".agent-collab/server")).unwrap();
        std::fs::write(&target, b"external\n").unwrap();
        std::os::unix::fs::symlink(&target, root.join(".agent-collab/server/journal.jsonl"))
            .unwrap();

        let state = temp_root("symlink-control-entry-state");
        std::fs::create_dir_all(&state).unwrap();
        let host = HostPaths::from_state_root(&state).unwrap();
        let scope = Scope { root: root.clone() };

        let error = run(
            &scope,
            &host,
            ResetRequest {
                approval: "operator authorized current baseline repair".into(),
                discard_legacy: true,
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("RESET_CONTROL_TREE_SYMLINK_REJECTED"),
            "{error:#}"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"external\n");
        assert!(root.join(".agent-collab/server/journal.jsonl").is_symlink());
        assert!(!state.join("archives").exists());
        assert!(!state.join("reset.jsonl").exists());

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(state).ok();
        std::fs::remove_file(target).ok();
    }

    #[test]
    fn reset_rolls_back_legacy_roots_when_route_rewrite_fails() {
        let root = temp_root("rollback-routes");
        std::fs::create_dir_all(root.join(".agent-collab/server")).unwrap();
        let legacy = b"{\"ev\":\"NotificationSubscribed\",\"subscription\":{}}\n";
        std::fs::write(root.join(".agent-collab/server/journal.jsonl"), legacy).unwrap();

        let state = temp_root("rollback-routes-state");
        std::fs::create_dir_all(&state).unwrap();
        let routes = state.join("routes.jsonl");
        std::fs::write(&routes, b"{not-json}\n").unwrap();
        let host = HostPaths::from_state_root(&state).unwrap();
        let scope = Scope { root: root.clone() };

        let error = run(
            &scope,
            &host,
            ResetRequest {
                approval: "operator authorized legacy retirement".into(),
                discard_legacy: true,
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("reset rolled back")
                || error.to_string().contains("DAEMON_MIGRATION_REQUIRED"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(root.join(".agent-collab/server/journal.jsonl")).unwrap(),
            legacy
        );
        assert_eq!(std::fs::read(&routes).unwrap(), b"{not-json}\n");
        assert!(!state.join("reset.jsonl").exists());
        assert!(root.join(".agent-collab").is_dir());

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(state).ok();
    }

    #[test]
    fn reset_rolls_back_legacy_roots_when_reset_record_write_fails() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("rollback-record");
        std::fs::create_dir_all(root.join(".agent-collab/server")).unwrap();
        let legacy = b"{\"ev\":\"NotificationSubscribed\",\"subscription\":{}}\n";
        std::fs::write(root.join(".agent-collab/server/journal.jsonl"), legacy).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        let original_doc = b"# Project-owned Collab notes\n";
        std::fs::write(root.join("docs/collab.md"), original_doc).unwrap();

        let state = temp_root("rollback-record-state");
        std::fs::create_dir_all(&state).unwrap();
        let reset_record = state.join("reset.jsonl");
        let original_record = b"{\"schema\":\"collab-reset/v1\",\"existing\":true}\n";
        std::fs::write(&reset_record, original_record).unwrap();
        let mut permissions = std::fs::metadata(&reset_record).unwrap().permissions();
        permissions.set_mode(0o444);
        std::fs::set_permissions(&reset_record, permissions).unwrap();

        let host = HostPaths::from_state_root(&state).unwrap();
        let scope = Scope { root: root.clone() };

        let error = run(
            &scope,
            &host,
            ResetRequest {
                approval: "operator authorized legacy retirement".into(),
                discard_legacy: true,
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("reset rolled back")
                || error.to_string().contains("DAEMON_MIGRATION_REQUIRED"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(root.join(".agent-collab/server/journal.jsonl")).unwrap(),
            legacy
        );
        assert_eq!(
            std::fs::read(root.join("docs/collab.md")).unwrap(),
            original_doc
        );
        assert!(root.join(".agent-collab").is_dir());
        assert!(root
            .read_dir()
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().contains("reset-stage")));
        assert_eq!(std::fs::read(&reset_record).unwrap(), original_record);

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(state).ok();
    }

    fn read_last_reset_record(state: &Path) -> serde_json::Value {
        let text = std::fs::read_to_string(state.join("reset.jsonl")).unwrap();
        let last = text.lines().last().unwrap();
        serde_json::from_str(last).unwrap()
    }
}
