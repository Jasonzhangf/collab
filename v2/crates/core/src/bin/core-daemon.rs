use collab_v2_core::{
    plan_legacy_beta_migration, CoreCommand, CoreError, CoreState, JournalEntry,
    LegacyMigrationPlan,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct CommandEnvelope {
    command_id: String,
    #[serde(flatten)]
    command: CoreCommand,
}

struct WriterLock(PathBuf);

impl WriterLock {
    fn acquire(state_path: &Path) -> Result<Self, String> {
        let path = state_path.with_extension("lock");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("DuplicateWriter: {error}"))?;
        writeln!(file, "{}", std::process::id()).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        Ok(Self(path))
    }
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn journal_path(state_path: &Path) -> PathBuf {
    state_path.with_file_name("journal.jsonl")
}

fn read_journal(path: &Path) -> Result<Vec<JournalEntry>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    BufReader::new(file)
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let line = line.map_err(|error| error.to_string())?;
            serde_json::from_str(&line)
                .map_err(|error| format!("MalformedJournal line {}: {error}", index + 1))
        })
        .collect()
}

fn append_journal(path: &Path, entry: &JournalEntry) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut file, entry).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

fn write_snapshot(path: &Path, state: &CoreState) -> Result<(), String> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut file, state).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn migration_plan(state_path: Option<&PathBuf>) -> Result<LegacyMigrationPlan, String> {
    let state_path = state_path.ok_or_else(|| "migration requires --state".to_owned())?;
    let legacy_path = state_path.with_file_name("legacy-state.json");
    let raw = fs::read_to_string(&legacy_path)
        .map_err(|error| format!("LegacyStateUnavailable: {error}"))?;
    plan_legacy_beta_migration(&raw).map_err(|error| format!("LegacyStateInvalid: {error}"))
}

fn error_value(error: CoreError) -> Value {
    json!({"ok": false, "error": format!("{error:?}")})
}

fn run() -> Result<(), String> {
    let state_path = std::env::args()
        .position(|arg| arg == "--state")
        .and_then(|index| std::env::args().nth(index + 1))
        .map(PathBuf::from);
    if let Some(path) = state_path.as_ref() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
    }
    let _writer_lock = state_path
        .as_ref()
        .map(|path| WriterLock::acquire(path))
        .transpose()?;
    let mut entries = state_path
        .as_ref()
        .map(|path| read_journal(&journal_path(path)))
        .transpose()?
        .unwrap_or_default();
    let mut state =
        CoreState::replay(&entries).map_err(|error| format!("ReplayFailed: {error:?}"))?;
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let raw: Value = match serde_json::from_str(&line) {
            Ok(raw) => raw,
            Err(error) => {
                write_response(
                    json!({"ok": false, "error": "InvalidCommand", "message": error.to_string()}),
                )?;
                continue;
            }
        };
        let op = raw.get("op").and_then(Value::as_str);
        if op == Some("snapshot") {
            write_response(
                json!({"ok": true, "state": state, "snapshot_sha256": state.snapshot_sha256().map_err(|error| error.to_string())?}),
            )?;
            continue;
        }
        if matches!(op, Some("migration_inspect") | Some("migration_plan")) {
            match migration_plan(state_path.as_ref()) {
                Ok(plan) if op == Some("migration_plan") && !plan.issues.is_empty() => {
                    write_response(
                        json!({"ok": false, "error": "MigrationBlocked", "issues": plan.issues}),
                    )?
                }
                Ok(plan) => write_response(json!({"ok": true, "plan": plan}))?,
                Err(error) => write_response(
                    json!({"ok": false, "error": "MigrationInspectFailed", "message": error}),
                )?,
            }
            continue;
        }
        let envelope: CommandEnvelope = if matches!(
            op,
            Some("migration_apply") | Some("migration_verify") | Some("migration_resume")
        ) {
            let command_id = raw
                .get("command_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let command = match op {
                Some("migration_apply") => match migration_plan(state_path.as_ref()) {
                    Ok(plan) => CoreCommand::ApplyMigration { plan },
                    Err(error) => {
                        write_response(
                            json!({"ok": false, "error": "MigrationInspectFailed", "message": error}),
                        )?;
                        continue;
                    }
                },
                Some("migration_verify") => CoreCommand::VerifyMigration,
                Some("migration_resume") => CoreCommand::ResumeMigration,
                _ => unreachable!(),
            };
            CommandEnvelope {
                command_id,
                command,
            }
        } else {
            match serde_json::from_value(raw) {
                Ok(command) => command,
                Err(error) => {
                    write_response(
                        json!({"ok": false, "error": "InvalidCommand", "message": error.to_string()}),
                    )?;
                    continue;
                }
            }
        };
        if envelope.command_id.is_empty() {
            write_response(
                json!({"ok": false, "error": "InvalidCommand", "message": "command_id is required"}),
            )?;
            continue;
        }
        if let Some(existing) = entries
            .iter()
            .find(|entry| entry.command_id == envelope.command_id)
        {
            if existing.command == envelope.command {
                write_response(
                    json!({"ok": true, "idempotent": true, "sequence": existing.sequence}),
                )?;
            } else {
                write_response(error_value(CoreError::DuplicateCommand))?;
            }
            continue;
        }
        let mut candidate = state.clone();
        let outcome = match candidate.apply(&envelope.command) {
            Ok(outcome) => outcome,
            Err(error) => {
                write_response(error_value(error))?;
                continue;
            }
        };
        let entry = JournalEntry {
            sequence: state.sequence + 1,
            command_id: envelope.command_id,
            command: envelope.command,
        };
        candidate.sequence = entry.sequence;
        if let Some(path) = state_path.as_ref() {
            append_journal(&journal_path(path), &entry)?;
            write_snapshot(path, &candidate)?;
        }
        entries.push(entry);
        state = candidate;
        write_response(
            json!({"ok": true, "sequence": state.sequence, "disposition": outcome, "snapshot_sha256": state.snapshot_sha256().map_err(|error| error.to_string())?}),
        )?;
    }
    Ok(())
}

fn write_response(response: Value) -> Result<(), String> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer(&mut stdout, &response).map_err(|error| error.to_string())?;
    stdout.write_all(b"\n").map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(2);
    }
}
